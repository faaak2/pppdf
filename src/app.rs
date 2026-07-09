//! Application state machine + page cache. Pure logic — no OS types.

use crate::layout::{
    clamp_scroll, fit_scale, max_zoom, page_rect, pdf_rect_to_window, render_size, unrotate_point,
    zoom_scroll, PointF, RectF, SizeU, SCROLL_STEP,
};
use crate::overlay::OutlineState;
use crate::types::{FieldKind, FormField, LinkTarget, OutlineItem, PageLink};
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::PathBuf;

/// One rendered page: BGRA pixels + that page's links (PDF coords).
pub struct CachedPage {
    pub page: u16,
    pub px: SizeU,
    /// Clockwise quarter-turns the bitmap was rendered at. Part of the cache
    /// identity: a 180° render has the SAME px as the unrotated one, so
    /// matching on (page, px) alone would serve stale pixels forever.
    pub rot: u8,
    pub bgra: Vec<u8>,
    pub links: Vec<PageLink>,
    pub fields: Vec<FormField>,
    /// Monotonic id stamped by `App::rendered()` — part of the renderer's
    /// device-bitmap memo key. Form filling re-renders the SAME
    /// (page, px, rot) with NEW pixels; without gen the memo would keep
    /// serving the stale bitmap (same trap as the `rot` note above).
    pub gen: u64,
}

/// How far (px per axis) a cached size may differ from a requested size and
/// still count as the same render — absorbs rounding differences between our
/// fit math and pdfium's output so requests can never loop.
const SIZE_TOLERANCE: i32 = 2;

fn close(a: SizeU, b: SizeU) -> bool {
    (a.w as i32 - b.w as i32).abs() <= SIZE_TOLERANCE
        && (a.h as i32 - b.h as i32).abs() <= SIZE_TOLERANCE
}

/// Upper bound on total cached pixel bytes (~384 MB). Entry-count alone is
/// no budget: one zoomed 4K render is ~350 MB.
const CACHE_BYTE_BUDGET: usize = 384 * 1024 * 1024;

/// Tiny insertion-ordered cache keyed by (page, pixel size). Front = newest.
pub struct PageCache {
    entries: VecDeque<CachedPage>,
    cap: usize,
    max_bytes: usize,
}

impl PageCache {
    pub fn new(cap: usize) -> Self {
        Self { entries: VecDeque::new(), cap, max_bytes: CACHE_BYTE_BUDGET }
    }

    /// Used by tests to exercise the byte-budget eviction with small numbers.
    pub fn with_budget(cap: usize, max_bytes: usize) -> Self {
        Self { entries: VecDeque::new(), cap, max_bytes }
    }

    fn total_bytes(&self) -> usize {
        self.entries.iter().map(|e| e.bgra.len()).sum()
    }

    /// Freshest cached render of this page at any size (soft preview).
    /// Rotation must match exactly — drawing a differently-rotated bitmap
    /// into the destination rect would show sideways/stretched content.
    pub fn get_any(&self, page: u16, rot: u8) -> Option<&CachedPage> {
        self.entries.iter().find(|e| e.page == page && e.rot == rot)
    }

    /// Entry within SIZE_TOLERANCE of `want` if one exists, else the freshest.
    pub fn get_best(&self, page: u16, rot: u8, want: SizeU) -> Option<&CachedPage> {
        self.entries
            .iter()
            .find(|e| e.page == page && e.rot == rot && close(e.px, want))
            .or_else(|| self.get_any(page, rot))
    }

    pub fn has_close(&self, page: u16, rot: u8, px: SizeU) -> bool {
        self.entries.iter().any(|e| e.page == page && e.rot == rot && close(e.px, px))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Insert `entry`, then evict down to the cap/byte budget — but never
    /// evict a page in `protected`. `protected` must be exactly the set
    /// `wanted_renders()` would re-request (the current page plus its
    /// immediate neighbors): evicting one of those pages here would make it
    /// look uncached to `wanted_renders`, which re-requests it, whose reply
    /// then evicts a sibling protected entry in turn — an infinite
    /// render/evict loop. Protecting the whole working set means eviction can
    /// leave the cache over budget, but only by a BOUNDED amount (at most
    /// `cap` entries across at most 3 pages — same-(page, px) puts replace in
    /// place, and a page flip resets zoom so old sizes age out normally).
    /// That bounded overshoot is the price of guaranteed convergence.
    pub fn put(&mut self, entry: CachedPage, protected: &[u16]) {
        self.entries
            .retain(|e| !(e.page == entry.page && e.px == entry.px && e.rot == entry.rot));
        self.entries.push_front(entry);
        while (self.entries.len() > self.cap || self.total_bytes() > self.max_bytes)
            && self.entries.len() > 1
        {
            // Scan from the back (oldest) for the first evictable entry.
            match self.entries.iter().rposition(|e| !protected.contains(&e.page)) {
                Some(idx) => {
                    self.entries.remove(idx);
                }
                None => break, // every remaining entry is protected: bounded overshoot
            }
        }
        // Keep at most the newest 2 entries per page: wanted_renders needs at
        // most one size per page at any instant (always the newest same-page
        // insert), and the second slot absorbs a stray stale reply — so this
        // cannot break request convergence, while stopping a slow zoom crawl
        // from retaining every intermediate size of a protected page.
        let mut per_page: HashMap<u16, usize> = HashMap::new();
        self.entries.retain(|e| {
            let n = per_page.entry(e.page).or_insert(0);
            *n += 1;
            *n <= 2
        });
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

pub struct DocState {
    pub path: PathBuf,
    pub pages: u16,
    pub page_sizes: Vec<(f32, f32)>,
    pub outline: Vec<OutlineItem>,
    pub has_forms: bool,
    pub current: u16,
    /// 1.0 = fit-to-window.
    pub zoom: f32,
    pub scroll: PointF,
    /// Some = TOC overlay is open.
    pub overlay: Option<OutlineState>,
    /// Clockwise quarter-turns (0..=3) the document view is rotated by.
    /// Applies to every page; reset by opening another document.
    pub rot: u8,
    /// Some = the "G" go-to-page prompt is open, holding the typed digits.
    pub goto_entry: Option<String>,
}

impl DocState {
    /// Current page size in PDF points, as authored (unrotated).
    fn page_pt(&self) -> (f32, f32) {
        self.page_sizes
            .get(self.current as usize)
            .copied()
            .unwrap_or((612.0, 792.0))
    }

    /// Current page size as displayed: axes swap at 90°/270°. ALL fit/zoom/
    /// scroll geometry must use this; `page_pt()` is only for link rects,
    /// which pdfium reports in unrotated page space.
    fn page_pt_rot(&self) -> (f32, f32) {
        let (w, h) = self.page_pt();
        if self.rot % 2 == 1 { (h, w) } else { (w, h) }
    }
}

pub enum Mode {
    Empty,
    Loading {
        path: PathBuf,
    },
    /// Typing a password into the borderless window. `prev` stashes the doc
    /// that was open when the prompt appeared, so cancel restores it.
    Password {
        path: PathBuf,
        buf: String,
        prev: Option<Box<DocState>>,
    },
    Doc(DocState),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderReq {
    pub page: u16,
    pub px: SizeU,
    pub rot: u8,
    pub visible: bool,
}

#[derive(Debug, PartialEq)]
pub enum WheelOutcome {
    None,
    Redraw,
    Flipped,
}

pub struct App {
    pub mode: Mode,
    pub cache: PageCache,
    /// Render requests sent to the PDF thread but not yet answered,
    /// keyed (page, w, h, rot).
    pub pending: HashSet<(u16, u32, u32, u8)>,
    /// Pages whose render genuinely failed — never re-requested (avoids a
    /// request/fail hot loop on malformed pages). Cleared on document load.
    pub failed: HashSet<u16>,
    pub client: SizeU,
    pub dpi: u32,
    /// F1 help overlay. Global (not per-document): available in every mode,
    /// so it lives here rather than on DocState like the TOC overlay.
    pub help_visible: bool,
    /// Source of `CachedPage::gen`; bumped on every render reply.
    render_gen: u64,
    /// Pro: a form field currently has keyboard focus (typing mode). The UI
    /// owns this state — it initiates every focus change (click, Tab, Esc).
    #[cfg(feature = "pro")]
    pub form_focus: bool,
    /// Pro: index (into the current cache entry's fields) last focused via
    /// click or Tab — the anchor for Tab cycling.
    #[cfg(feature = "pro")]
    pub form_tab_idx: Option<usize>,
    /// Pro: unsaved form changes exist (dirty dot + close-prompt gate).
    #[cfg(feature = "pro")]
    pub form_dirty: bool,
    /// Pro: how form widgets are marked on the page ('F' cycles it).
    /// Sticky for the window's lifetime — a viewer preference like
    /// `help_visible`, so `doc_loaded` intentionally leaves it alone.
    #[cfg(feature = "pro")]
    pub field_indication: FieldIndication,
}

/// Pro: visual marking of form widgets, cycled by 'F'. Drawn client-side by
/// d2d over the cached page bitmap — never baked into rendered pixels, so
/// toggling repaints instantly without touching the render cache.
#[cfg(feature = "pro")]
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FieldIndication {
    /// Translucent blue wash over each widget (the Chromium look). Default.
    Highlight,
    /// Dotted outline around each widget.
    Frame,
    Off,
}

impl App {
    pub fn new() -> Self {
        Self {
            mode: Mode::Empty,
            cache: PageCache::new(6),
            pending: HashSet::new(),
            failed: HashSet::new(),
            client: SizeU { w: 0, h: 0 },
            dpi: 96,
            help_visible: false,
            render_gen: 0,
            #[cfg(feature = "pro")]
            form_focus: false,
            #[cfg(feature = "pro")]
            form_tab_idx: None,
            #[cfg(feature = "pro")]
            form_dirty: false,
            #[cfg(feature = "pro")]
            field_indication: FieldIndication::Highlight,
        }
    }

    /// Advance the field-indication mode and return the new one (toast text).
    #[cfg(feature = "pro")]
    pub fn cycle_field_indication(&mut self) -> FieldIndication {
        self.field_indication = match self.field_indication {
            FieldIndication::Highlight => FieldIndication::Frame,
            FieldIndication::Frame => FieldIndication::Off,
            FieldIndication::Off => FieldIndication::Highlight,
        };
        self.field_indication
    }

    /// Flip the F1 help overlay. Available in every mode.
    pub fn toggle_help(&mut self) {
        self.help_visible = !self.help_visible;
    }

    /// Unsaved form changes exist (always false in the free edition).
    pub fn dirty_dot_visible(&self) -> bool {
        #[cfg(feature = "pro")]
        {
            self.form_dirty
        }
        #[cfg(not(feature = "pro"))]
        {
            false
        }
    }

    pub fn doc(&self) -> Option<&DocState> {
        match &self.mode {
            Mode::Doc(d) => Some(d),
            _ => None,
        }
    }

    pub fn doc_mut(&mut self) -> Option<&mut DocState> {
        match &mut self.mode {
            Mode::Doc(d) => Some(d),
            _ => None,
        }
    }

    pub fn set_client(&mut self, client: SizeU) {
        self.client = client;
    }

    pub fn doc_loaded(
        &mut self,
        path: PathBuf,
        pages: u16,
        page_sizes: Vec<(f32, f32)>,
        outline: Vec<OutlineItem>,
        has_forms: bool,
    ) {
        self.cache.clear();
        self.pending.clear();
        self.failed.clear();
        #[cfg(feature = "pro")]
        {
            self.form_focus = false;
            self.form_tab_idx = None;
            self.form_dirty = false;
            // field_indication deliberately NOT reset: it's a viewer
            // preference (like help_visible), not document state — turning
            // the marks off must survive opening the next form.
        }
        self.mode = Mode::Doc(DocState {
            path,
            pages,
            page_sizes,
            outline,
            has_forms,
            current: 0,
            zoom: 1.0,
            scroll: PointF::default(),
            overlay: None,
            rot: 0,
            goto_entry: None,
        });
    }

    /// Change page by delta; returns true if the page changed. Resets zoom/scroll.
    pub fn flip(&mut self, delta: i32) -> bool {
        let Some(d) = self.doc_mut() else { return false };
        if d.pages == 0 {
            return false;
        }
        let target = (d.current as i32 + delta).clamp(0, d.pages as i32 - 1) as u16;
        if target == d.current {
            return false;
        }
        d.current = target;
        d.zoom = 1.0;
        d.scroll = PointF::default();
        true
    }

    pub fn goto(&mut self, page: u16) -> bool {
        let Some(d) = self.doc() else { return false };
        let delta = page.min(d.pages.saturating_sub(1)) as i32 - d.current as i32;
        self.flip(delta)
    }

    /// notches > 0 = wheel down (toward the next page).
    pub fn wheel(&mut self, notches: i32) -> WheelOutcome {
        let client = self.client;
        let Some(d) = self.doc_mut() else { return WheelOutcome::None };
        if notches == 0 {
            return WheelOutcome::None;
        }
        if d.zoom <= 1.0 + 1e-4 {
            return if self.flip(notches.signum()) {
                WheelOutcome::Flipped
            } else {
                WheelOutcome::None
            };
        }
        let px = render_size(d.page_pt_rot(), client, d.zoom);
        let max_y = (px.h as f32 - client.h as f32).max(0.0);
        let at_bottom = d.scroll.y >= max_y - 0.5;
        let at_top = d.scroll.y <= 0.5;
        if (notches > 0 && at_bottom) || (notches < 0 && at_top) {
            return if self.flip(notches.signum()) {
                WheelOutcome::Flipped
            } else {
                WheelOutcome::None
            };
        }
        d.scroll.y += notches as f32 * SCROLL_STEP;
        d.scroll = clamp_scroll(px, client, d.scroll);
        WheelOutcome::Redraw
    }

    /// Multiply zoom by `factor`, keeping the page point under `cursor` fixed.
    /// Returns true if anything changed.
    pub fn zoom_by(&mut self, cursor: PointF, factor: f32) -> bool {
        let client = self.client;
        let Some(d) = self.doc_mut() else { return false };
        let page_pt = d.page_pt_rot();
        let old_zoom = d.zoom;
        let new_zoom = (old_zoom * factor).clamp(1.0, max_zoom(page_pt, client));
        if (new_zoom - old_zoom).abs() < 1e-4 {
            return false;
        }
        if new_zoom <= 1.0 + 1e-4 {
            d.zoom = 1.0;
            d.scroll = PointF::default();
            return true;
        }
        let old_px = render_size(page_pt, client, old_zoom);
        let old_rect = page_rect(old_px, client, d.scroll);
        let new_px = render_size(page_pt, client, new_zoom);
        d.scroll = zoom_scroll(cursor, old_rect, new_zoom / old_zoom, new_px, client);
        d.zoom = new_zoom;
        true
    }

    pub fn reset_zoom(&mut self) -> bool {
        let Some(d) = self.doc_mut() else { return false };
        if d.zoom == 1.0 {
            return false;
        }
        d.zoom = 1.0;
        d.scroll = PointF::default();
        true
    }

    pub fn toggle_overlay(&mut self) {
        let Some(d) = self.doc_mut() else { return };
        d.overlay = match d.overlay.take() {
            Some(_) => None,
            None => Some(OutlineState::new(d.outline.clone())),
        };
    }

    /// Rotate the view 90° clockwise (cycles back to upright after four).
    /// Resets to fit like a page flip does — cursor-anchored zoom math has no
    /// meaningful anchor across a rotation. Returns true if a doc is open.
    pub fn rotate_cw(&mut self) -> bool {
        let Some(d) = self.doc_mut() else { return false };
        d.rot = (d.rot + 1) % 4;
        d.zoom = 1.0;
        d.scroll = PointF::default();
        true
    }

    /// Pixel size the current page renders at (current zoom).
    pub fn current_px(&self) -> Option<SizeU> {
        let d = self.doc()?;
        Some(render_size(d.page_pt_rot(), self.client, d.zoom))
    }

    /// Window-space rect the current page occupies.
    pub fn current_page_rect(&self) -> Option<RectF> {
        let d = self.doc()?;
        Some(page_rect(self.current_px()?, self.client, d.scroll))
    }

    /// Crisp render of the current page first, then prefetch next/prev at fit.
    pub fn wanted_renders(&self) -> Vec<RenderReq> {
        let Some(d) = self.doc() else { return Vec::new() };
        let rot = d.rot;
        let mut out = Vec::new();
        let mut push_if_needed = |page: u16, px: SizeU, visible: bool| {
            if !self.cache.has_close(page, rot, px)
                && !self.pending.contains(&(page, px.w, px.h, rot))
                && !self.failed.contains(&page)
            {
                out.push(RenderReq { page, px, rot, visible });
            }
        };
        push_if_needed(d.current, render_size(d.page_pt_rot(), self.client, d.zoom), true);
        for neighbor in [d.current as i32 + 1, d.current as i32 - 1] {
            if neighbor < 0 || neighbor >= d.pages as i32 {
                continue;
            }
            let page = neighbor as u16;
            let (w, h) = d.page_sizes.get(page as usize).copied().unwrap_or((612.0, 792.0));
            let pt = if rot % 2 == 1 { (h, w) } else { (w, h) };
            push_if_needed(page, render_size(pt, self.client, 1.0), false);
        }
        drop(push_if_needed);
        out
    }

    pub fn mark_pending(&mut self, req: &RenderReq) {
        self.pending.insert((req.page, req.px.w, req.px.h, req.rot));
    }

    /// A render reply arrived: clear the page's pending entries (by page, not
    /// exact size — pdfium may round differently than layout::render_size).
    pub fn rendered(&mut self, mut page: CachedPage) {
        // Every reply gets a fresh generation, even for an identical
        // (page, px, rot) key — the pixels may differ (form filling).
        self.render_gen += 1;
        page.gen = self.render_gen;
        crate::dlog!(
            "app: rendered page={} px={}x{} rot={} gen={} fields={}",
            page.page,
            page.px.w,
            page.px.h,
            page.rot,
            page.gen,
            page.fields.len()
        );
        self.pending.retain(|k| k.0 != page.page);
        let protected: Vec<u16> = self
            .doc()
            .map(|d| {
                let c = d.current;
                let mut p = vec![c];
                if c > 0 {
                    p.push(c - 1);
                }
                if c + 1 < d.pages {
                    p.push(c + 1);
                }
                p
            })
            .unwrap_or_default();
        self.cache.put(page, &protected);
    }

    /// RenderFailed reply. Superseded requests may be re-issued; a genuine
    /// pdfium failure marks the page so it is never hot-loop re-requested
    /// (the spec accepts corrupt pages silently).
    pub fn render_failed(&mut self, page: u16, superseded: bool) {
        self.pending.retain(|k| k.0 != page);
        if !superseded {
            self.failed.insert(page);
        }
    }

    /// Project a window-space point into the current page's unrotated bitmap
    /// frame. Returns (point in unrotated bitmap px, px-per-pt scale, page
    /// size in points). None when no doc/render or the point is off-page.
    fn page_hit_frame(&self, p: PointF) -> Option<(PointF, f32, (f32, f32))> {
        let d = self.doc()?;
        // Cache presence gate kept from link_at: hit-testing against a page
        // that has never rendered would use stale geometry.
        self.cache.get_any(d.current, d.rot)?;
        let pr = self.current_page_rect()?;
        // Points outside the displayed page can't be on a link — and must be
        // rejected BEFORE unrotating: negating coordinates flips the half-open
        // page interval, which would otherwise turn the first point past one
        // edge into an exact hit on the opposite edge.
        if !pr.contains(p) {
            return None;
        }
        let page_pt = d.page_pt();
        // Same scalar in both frames — rotation is rigid.
        let px_per_pt = fit_scale(d.page_pt_rot(), self.client) * d.zoom;
        // Window point -> rotated-bitmap coords -> unrotated-bitmap coords.
        // The same interval flip makes an exact on-edge point land exactly ON
        // the far bound (q == dims), where half-open `contains` misses a link
        // that bleeds to the page edge; nudge it back inside.
        let q = unrotate_point(
            PointF { x: p.x - pr.left, y: p.y - pr.top },
            pr.width(),
            pr.height(),
            d.rot,
        );
        let q = PointF {
            x: q.x.min(page_pt.0 * px_per_pt - 1e-3),
            y: q.y.min(page_pt.1 * px_per_pt - 1e-3),
        };
        Some((q, px_per_pt, page_pt))
    }

    /// Link under a window-space point on the current page, if any. Link
    /// rects live in unrotated page space, so the window point is first
    /// carried back through the view rotation.
    pub fn link_at(&self, p: PointF) -> Option<LinkTarget> {
        let d = self.doc()?;
        let entry = self.cache.get_any(d.current, d.rot)?;
        let (q, px_per_pt, page_pt) = self.page_hit_frame(p)?;
        let unrot =
            RectF { left: 0.0, top: 0.0, right: page_pt.0 * px_per_pt, bottom: page_pt.1 * px_per_pt };
        for link in &entry.links {
            let wr = pdf_rect_to_window(link.rect_pt, page_pt.1, px_per_pt, unrot);
            if wr.contains(q) {
                return Some(link.target.clone());
            }
        }
        None
    }

    /// Form field under a window-space point: (index into the cache entry's
    /// `fields`, kind). Same projection as link_at.
    pub fn field_hit(&self, p: PointF) -> Option<(usize, FieldKind)> {
        let d = self.doc()?;
        let entry = self.cache.get_any(d.current, d.rot)?;
        let (q, px_per_pt, page_pt) = self.page_hit_frame(p)?;
        let unrot =
            RectF { left: 0.0, top: 0.0, right: page_pt.0 * px_per_pt, bottom: page_pt.1 * px_per_pt };
        for (i, f) in entry.fields.iter().enumerate() {
            let wr = pdf_rect_to_window(f.rect_pt, page_pt.1, px_per_pt, unrot);
            if wr.contains(q) {
                return Some((i, f.kind));
            }
        }
        None
    }

    pub fn field_at(&self, p: PointF) -> Option<FieldKind> {
        self.field_hit(p).map(|(_, k)| k)
    }

    /// Window-space rects of the current page's form widgets under the
    /// current layout (zoom, scroll, rotation) — the drawing-side complement
    /// of `field_hit`: that one unrotates the probe POINT, this rotates the
    /// RECTS into the displayed frame.
    #[cfg(feature = "pro")]
    pub fn field_rects_window(&self) -> Vec<RectF> {
        let Some(d) = self.doc() else { return Vec::new() };
        let Some(entry) = self.cache.get_any(d.current, d.rot) else { return Vec::new() };
        let Some(pr) = self.current_page_rect() else { return Vec::new() };
        let page_pt = d.page_pt();
        let px_per_pt = fit_scale(d.page_pt_rot(), self.client) * d.zoom;
        // Unrotated-bitmap frame, origin (0,0) — same frame field_hit tests
        // in; the page offset is applied AFTER rotation, because pr is where
        // the ROTATED bitmap sits.
        let unrot = RectF {
            left: 0.0,
            top: 0.0,
            right: page_pt.0 * px_per_pt,
            bottom: page_pt.1 * px_per_pt,
        };
        entry
            .fields
            .iter()
            .map(|f| {
                let r = pdf_rect_to_window(f.rect_pt, page_pt.1, px_per_pt, unrot);
                let rr = crate::layout::rotate_rect(r, unrot.right, unrot.bottom, d.rot);
                RectF {
                    left: pr.left + rr.left,
                    top: pr.top + rr.top,
                    right: pr.left + rr.right,
                    bottom: pr.top + rr.bottom,
                }
            })
            .collect()
    }

    /// Window point -> PDF page coordinates (points, origin bottom-left,
    /// y up) — the space FORM_On* events and field rects live in.
    pub fn pdf_point_at(&self, p: PointF) -> Option<PointF> {
        let (q, px_per_pt, page_pt) = self.page_hit_frame(p)?;
        Some(PointF { x: q.x / px_per_pt, y: page_pt.1 - q.y / px_per_pt })
    }

    /// Advance Tab focus through the current page's fields in reading order
    /// (top-to-bottom, then left-to-right; PDF y is up, so top DESCENDING).
    /// Returns the next field's center in PDF points for a synthesized click.
    #[cfg(feature = "pro")]
    pub fn form_tab_target(&mut self, backwards: bool) -> Option<PointF> {
        let d = self.doc()?;
        let entry = self.cache.get_any(d.current, d.rot)?;
        if entry.fields.is_empty() {
            return None;
        }
        let mut order: Vec<usize> = (0..entry.fields.len()).collect();
        order.sort_by(|&a, &b| {
            let (ra, rb) = (&entry.fields[a].rect_pt, &entry.fields[b].rect_pt);
            rb.top.total_cmp(&ra.top).then(ra.left.total_cmp(&rb.left))
        });
        let pos = self.form_tab_idx.and_then(|cur| order.iter().position(|&i| i == cur));
        let next_pos = match (pos, backwards) {
            (Some(p), false) => (p + 1) % order.len(),
            (Some(p), true) => (p + order.len() - 1) % order.len(),
            (None, false) => 0,
            (None, true) => order.len() - 1,
        };
        let idx = order[next_pos];
        self.form_tab_idx = Some(idx);
        let r = entry.fields[idx].rect_pt;
        Some(PointF { x: (r.left + r.right) / 2.0, y: (r.top + r.bottom) / 2.0 })
    }

    // --- go-to-page entry (G, digits, Enter — drawn as a small prompt) ---

    /// Open the go-to-page prompt. Only meaningful with a document.
    pub fn goto_entry_start(&mut self) -> bool {
        let Some(d) = self.doc_mut() else { return false };
        d.goto_entry = Some(String::new());
        true
    }

    pub fn goto_entry_active(&self) -> bool {
        self.doc().is_some_and(|d| d.goto_entry.is_some())
    }

    /// Append a typed digit (anything else is ignored; length capped so the
    /// buffer can't grow without bound).
    pub fn goto_entry_char(&mut self, c: char) {
        if let Some(buf) = self.doc_mut().and_then(|d| d.goto_entry.as_mut()) {
            if c.is_ascii_digit() && buf.len() < 6 {
                buf.push(c);
            }
        }
    }

    pub fn goto_entry_backspace(&mut self) {
        if let Some(buf) = self.doc_mut().and_then(|d| d.goto_entry.as_mut()) {
            buf.pop();
        }
    }

    pub fn goto_entry_cancel(&mut self) {
        if let Some(d) = self.doc_mut() {
            d.goto_entry = None;
        }
    }

    /// Enter: jump to the typed 1-based page, clamped to the document.
    /// An empty buffer just closes the prompt. Returns true if the page changed.
    pub fn goto_entry_commit(&mut self) -> bool {
        let Some(d) = self.doc_mut() else { return false };
        let Some(buf) = d.goto_entry.take() else { return false };
        let Ok(n) = buf.parse::<u32>() else { return false };
        let page_1 = n.clamp(1, d.pages.max(1) as u32) as u16;
        self.goto(page_1 - 1)
    }

    // --- password entry (typed straight into the borderless window) ---

    pub fn password_char(&mut self, c: char) {
        if let Mode::Password { buf, .. } = &mut self.mode {
            if !c.is_control() {
                buf.push(c);
            }
        }
    }

    pub fn password_backspace(&mut self) {
        if let Mode::Password { buf, .. } = &mut self.mode {
            buf.pop();
        }
    }

    /// Take the typed password (caller sends PdfCmd::Load with it). The
    /// stashed doc — if any — keeps showing while the retry loads; otherwise
    /// show the Loading screen.
    pub fn password_take(&mut self) -> Option<(PathBuf, String)> {
        if let Mode::Password { path, buf, prev } = &mut self.mode {
            let (path, pw) = (path.clone(), buf.clone());
            self.mode = match prev.take() {
                Some(d) => Mode::Doc(*d),
                None => Mode::Loading { path: path.clone() },
            };
            return Some((path, pw));
        }
        None
    }

    /// Cancel the password prompt: restore the stashed doc, or Empty.
    pub fn password_cancel(&mut self) {
        if let Mode::Password { prev, .. } = &mut self.mode {
            self.mode = match prev.take() {
                Some(d) => Mode::Doc(*d),
                None => Mode::Empty,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LETTER: (f32, f32) = (612.0, 792.0);

    fn app_with_doc(pages: u16) -> App {
        let mut a = App::new();
        a.set_client(SizeU { w: 1000, h: 1000 });
        a.doc_loaded(
            PathBuf::from("x.pdf"),
            pages,
            vec![LETTER; pages as usize],
            vec![],
            false,
        );
        a
    }

    fn cached(page: u16, px: SizeU) -> CachedPage {
        CachedPage { page, px, rot: 0, bgra: vec![0; 4], links: vec![], fields: vec![], gen: 0 }
    }

    // --- paging ---

    #[test]
    fn help_toggles() {
        let mut a = App::new();
        assert!(!a.help_visible);
        a.toggle_help();
        assert!(a.help_visible);
        a.toggle_help();
        assert!(!a.help_visible);
    }

    #[test]
    fn flip_clamps_at_both_ends() {
        let mut a = app_with_doc(3);
        assert!(!a.flip(-1), "already at first page");
        assert!(a.flip(1));
        assert_eq!(a.doc().unwrap().current, 1);
        assert!(a.flip(5)); // clamps to last
        assert_eq!(a.doc().unwrap().current, 2);
        assert!(!a.flip(1));
    }

    #[test]
    fn flip_resets_zoom_and_scroll() {
        let mut a = app_with_doc(3);
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 2.0);
        assert!(a.doc().unwrap().zoom > 1.0);
        a.flip(1);
        let d = a.doc().unwrap();
        assert_eq!(d.zoom, 1.0);
        assert_eq!(d.scroll, PointF { x: 0.0, y: 0.0 });
    }

    #[test]
    fn goto_clamps_out_of_range() {
        let mut a = app_with_doc(3);
        a.goto(99);
        assert_eq!(a.doc().unwrap().current, 2);
    }

    #[test]
    fn zero_page_doc_navigation_is_safe() {
        let mut a = App::new();
        a.set_client(SizeU { w: 1000, h: 1000 });
        a.doc_loaded(PathBuf::from("empty.pdf"), 0, vec![], vec![], false);
        assert!(!a.flip(1));
        assert!(!a.flip(-1));
        assert!(!a.goto(5));
        assert_eq!(a.wheel(1), WheelOutcome::None);
    }

    // --- wheel ---

    #[test]
    fn wheel_at_fit_flips() {
        let mut a = app_with_doc(3);
        assert_eq!(a.wheel(1), WheelOutcome::Flipped);
        assert_eq!(a.doc().unwrap().current, 1);
        assert_eq!(a.wheel(-1), WheelOutcome::Flipped);
        assert_eq!(a.doc().unwrap().current, 0);
    }

    #[test]
    fn wheel_zoomed_scrolls_then_flips_only_from_edge() {
        let mut a = app_with_doc(3);
        // Zoom deep so the page is much taller than the client.
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 4.0);
        let max_y = {
            let d = a.doc().unwrap();
            let px = render_size(LETTER, a.client, d.zoom);
            px.h as f32 - a.client.h as f32
        };
        assert!(max_y > SCROLL_STEP, "test premise: page overflows by more than one step");
        // Scroll to the bottom edge with many notches: must NOT flip.
        let notches = (max_y / SCROLL_STEP).ceil() as i32 + 1;
        for _ in 0..notches {
            if a.doc().unwrap().scroll.y >= max_y {
                break;
            }
            assert_eq!(a.wheel(1), WheelOutcome::Redraw);
        }
        assert!((a.doc().unwrap().scroll.y - max_y).abs() < 0.5);
        assert_eq!(a.doc().unwrap().current, 0, "scrolling to the edge must not flip");
        // The NEXT notch, already at the edge, flips and resets to fit.
        assert_eq!(a.wheel(1), WheelOutcome::Flipped);
        let d = a.doc().unwrap();
        assert_eq!(d.current, 1);
        assert_eq!(d.zoom, 1.0);
    }

    #[test]
    fn wheel_zoomed_up_at_top_flips_back() {
        let mut a = app_with_doc(3);
        a.flip(1);
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 3.0);
        // scroll.y is clamped >= 0; possibly already 0 at top depending on cursor. Force top:
        a.doc_mut().unwrap().scroll.y = 0.0;
        assert_eq!(a.wheel(-1), WheelOutcome::Flipped);
        assert_eq!(a.doc().unwrap().current, 0);
    }

    #[test]
    fn wheel_without_doc_is_none() {
        let mut a = App::new();
        assert_eq!(a.wheel(1), WheelOutcome::None);
    }

    // --- zoom ---

    #[test]
    fn zoom_clamps_to_one_and_max() {
        let mut a = app_with_doc(1);
        assert!(!a.zoom_by(PointF { x: 0.0, y: 0.0 }, 0.5), "already at fit");
        assert_eq!(a.doc().unwrap().zoom, 1.0);
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 100.0);
        let d = a.doc().unwrap();
        let mz = max_zoom(LETTER, a.client);
        assert!((d.zoom - mz).abs() < 1e-4);
    }

    #[test]
    fn zoom_back_to_fit_zeroes_scroll() {
        let mut a = app_with_doc(1);
        a.zoom_by(PointF { x: 700.0, y: 700.0 }, 3.0);
        a.doc_mut().unwrap().scroll = PointF { x: 50.0, y: 400.0 };
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 0.01); // clamps to 1.0
        let d = a.doc().unwrap();
        assert_eq!(d.zoom, 1.0);
        assert_eq!(d.scroll, PointF { x: 0.0, y: 0.0 });
    }

    #[test]
    fn reset_zoom_only_reports_change() {
        let mut a = app_with_doc(1);
        assert!(!a.reset_zoom());
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 2.0);
        assert!(a.reset_zoom());
        assert_eq!(a.doc().unwrap().zoom, 1.0);
    }

    // --- wanted_renders / pending / cache ---

    #[test]
    fn refreshed_render_gets_a_new_generation() {
        // Form filling re-renders the SAME (page, px, rot) key with new
        // pixels; the d2d device-bitmap memo keys on gen to notice. Two
        // same-key renders must never share a generation.
        let mut a = app_with_doc(1);
        let px = SizeU { w: 10, h: 10 };
        a.rendered(cached(0, px));
        let g1 = a.cache.get_any(0, 0).unwrap().gen;
        a.rendered(cached(0, px));
        let g2 = a.cache.get_any(0, 0).unwrap().gen;
        assert!(g2 > g1, "refreshed pixels must bump gen: {g1} vs {g2}");
        assert!(g1 > 0, "assigned gens start at 1 so a fresh memo can't collide");
    }

    #[test]
    fn wanted_renders_current_then_next_then_prev() {
        let mut a = app_with_doc(5);
        a.flip(2); // current = 2
        let reqs = a.wanted_renders();
        assert_eq!(reqs.len(), 3);
        assert_eq!((reqs[0].page, reqs[0].visible), (2, true));
        assert_eq!((reqs[1].page, reqs[1].visible), (3, false));
        assert_eq!((reqs[2].page, reqs[2].visible), (1, false));
        let fit_px = render_size(LETTER, a.client, 1.0);
        assert_eq!(reqs[0].px, fit_px);
    }

    #[test]
    fn wanted_renders_skips_cached_and_pending_and_ends() {
        let mut a = app_with_doc(2); // pages 0, 1; current 0 -> no prev
        let fit_px = render_size(LETTER, a.client, 1.0);
        a.rendered(cached(0, fit_px)); // current cached
        let reqs = a.wanted_renders();
        assert_eq!(reqs.len(), 1, "only the next-page prefetch remains");
        assert_eq!(reqs[0].page, 1);
        a.mark_pending(&reqs[0]);
        assert!(a.wanted_renders().is_empty());
        // Rendered reply clears pending.
        a.rendered(cached(1, fit_px));
        assert!(a.wanted_renders().is_empty()); // now cached instead of pending
        assert!(a.pending.is_empty());
    }

    #[test]
    fn wanted_renders_empty_without_doc() {
        let a = App::new();
        assert!(a.wanted_renders().is_empty());
    }

    #[test]
    fn cache_evicts_oldest_beyond_cap() {
        let mut c = PageCache::new(2);
        let px = SizeU { w: 10, h: 10 };
        c.put(cached(0, px), &[]);
        c.put(cached(1, px), &[]);
        c.put(cached(2, px), &[]);
        assert!(c.get_any(0, 0).is_none(), "0 was evicted");
        assert!(c.get_any(1, 0).is_some());
        assert!(c.get_any(2, 0).is_some());
    }

    #[test]
    fn cache_evicts_by_byte_budget_keeping_newest() {
        let mut c = PageCache::with_budget(6, 100);
        let big = |page: u16, bytes: usize| CachedPage {
            page,
            px: SizeU { w: 1, h: 1 },
            rot: 0,
            bgra: vec![0; bytes],
            links: vec![],
            fields: vec![],
            gen: 0,
        };
        c.put(big(0, 60), &[]);
        c.put(big(1, 60), &[]); // 120 > 100: page 0 evicted
        assert!(c.get_any(0, 0).is_none());
        assert!(c.get_any(1, 0).is_some());
        c.put(big(2, 300), &[]); // oversized alone: still kept (newest never evicted)
        assert!(c.get_any(2, 0).is_some());
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn cache_replaces_same_key_instead_of_duplicating() {
        let mut c = PageCache::new(4);
        c.put(cached(0, SizeU { w: 10, h: 10 }), &[]);
        c.put(cached(0, SizeU { w: 10, h: 10 }), &[]);
        assert_eq!(c.len(), 1);
        c.put(cached(0, SizeU { w: 20, h: 20 }), &[]); // different size = separate entry
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn cache_has_close_tolerates_render_rounding() {
        let mut c = PageCache::new(4);
        c.put(cached(0, SizeU { w: 800, h: 1000 }), &[]);
        assert!(c.has_close(0, 0, SizeU { w: 801, h: 999 }));
        assert!(!c.has_close(0, 0, SizeU { w: 850, h: 1000 }));
        assert!(!c.has_close(1, 0, SizeU { w: 800, h: 1000 }));
        assert!(!c.has_close(0, 2, SizeU { w: 800, h: 1000 }), "rot mismatch is a miss");
    }

    #[test]
    fn cache_get_best_prefers_size_match_then_freshest() {
        let mut c = PageCache::new(4);
        c.put(cached(0, SizeU { w: 100, h: 100 }), &[]);
        c.put(cached(0, SizeU { w: 200, h: 200 }), &[]); // freshest
        let best = c.get_best(0, 0, SizeU { w: 101, h: 100 }).unwrap();
        assert_eq!(best.px, SizeU { w: 100, h: 100 }, "size match wins over freshness");
        let fallback = c.get_best(0, 0, SizeU { w: 999, h: 999 }).unwrap();
        assert_eq!(fallback.px, SizeU { w: 200, h: 200 }, "no size match -> freshest");
        assert!(c.get_best(0, 1, SizeU { w: 100, h: 100 }).is_none(), "other rot never previews");
    }

    #[test]
    fn cache_keeps_at_most_two_sizes_per_page() {
        let mut c = PageCache::with_budget(6, usize::MAX);
        for i in 0..12u32 {
            c.put(
                CachedPage {
                    page: 1,
                    px: SizeU { w: 100 + i, h: 100 + i },
                    rot: 0,
                    bgra: vec![0; 10],
                    links: vec![],
                    fields: vec![],
                    gen: 0,
                },
                &[1], // page 1 is protected — the crawl must still be bounded
            );
        }
        assert!(c.len() <= 2, "len = {}", c.len());
        assert!(c.has_close(1, 0, SizeU { w: 111, h: 111 }), "newest size must survive");
    }

    #[test]
    fn render_reply_loop_converges_under_budget_pressure() {
        let mut a = App::new();
        a.set_client(SizeU { w: 1000, h: 1000 });
        a.cache = PageCache::with_budget(6, 100);
        a.doc_loaded(PathBuf::from("x.pdf"), 3, vec![LETTER; 3], vec![], false);
        a.flip(1); // current = 1; working set {0, 1, 2}
        let mut rounds = 0;
        loop {
            let reqs = a.wanted_renders();
            if reqs.is_empty() {
                break;
            }
            rounds += 1;
            assert!(rounds <= 4, "render request loop failed to converge (eviction livelock)");
            for r in reqs {
                a.mark_pending(&r);
                let bytes = if r.visible { 60 } else { 30 }; // 60+30+30 = 120 > budget 100
                a.rendered(CachedPage {
                    page: r.page,
                    px: r.px,
                    rot: r.rot,
                    bgra: vec![0; bytes],
                    links: vec![],
                    fields: vec![], gen: 0,
                });
            }
        }
        assert!(a.cache.get_any(1, 0).is_some(), "visible page must remain cached");
    }

    #[test]
    fn superseded_render_failure_allows_rerequest() {
        let mut a = app_with_doc(2);
        let reqs = a.wanted_renders();
        for r in &reqs {
            a.mark_pending(r);
        }
        assert!(a.wanted_renders().is_empty());
        a.render_failed(0, true); // superseded by the PDF thread: retry is fine
        let again = a.wanted_renders();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].page, 0);
    }

    #[test]
    fn real_render_failure_is_not_rerequested() {
        let mut a = app_with_doc(2);
        for r in a.wanted_renders() {
            a.mark_pending(&r);
        }
        a.render_failed(1, false); // genuine pdfium failure on the prefetch page
        assert!(a.wanted_renders().iter().all(|r| r.page != 1), "failed page must not hot-loop");
        // A fresh document clears the failed set.
        a.doc_loaded(PathBuf::from("y.pdf"), 2, vec![LETTER; 2], vec![], false);
        assert!(a.wanted_renders().iter().any(|r| r.page == 1));
    }

    // --- links ---

    #[test]
    fn link_at_transforms_pdf_coords() {
        let mut a = app_with_doc(1);
        let fit_px = render_size(LETTER, a.client, 1.0);
        // Link occupying the top-left quarter of the page (PDF coords: y up).
        let link = PageLink {
            rect_pt: RectF { left: 0.0, top: 792.0, right: 306.0, bottom: 396.0 },
            target: LinkTarget::Page(0),
        };
        a.rendered(CachedPage {
            page: 0,
            px: fit_px,
            rot: 0,
            bgra: vec![0; 4],
            links: vec![link],
            fields: vec![], gen: 0,
        });
        let pr = a.current_page_rect().unwrap();
        // A window point just inside the page's top-left corner is on the link.
        let p = PointF { x: pr.left + 5.0, y: pr.top + 5.0 };
        assert_eq!(a.link_at(p), Some(LinkTarget::Page(0)));
        // Bottom-left corner of the page is NOT on the link.
        let q = PointF { x: pr.left + 5.0, y: pr.bottom - 5.0 };
        assert_eq!(a.link_at(q), None);
    }

    #[test]
    fn link_at_follows_the_rotated_view() {
        let mut a = app_with_doc(1);
        // Same top-left-quarter link as above, but the view is rotated 90° CW,
        // so that quarter now shows in the window's TOP-RIGHT.
        let link = PageLink {
            rect_pt: RectF { left: 0.0, top: 792.0, right: 306.0, bottom: 396.0 },
            target: LinkTarget::Page(0),
        };
        assert!(a.rotate_cw());
        let px = a.current_px().unwrap();
        assert!(px.w > px.h, "rotated portrait page must be landscape");
        a.rendered(CachedPage { page: 0, px, rot: 1, bgra: vec![0; 4], links: vec![link], fields: vec![], gen: 0 });
        let pr = a.current_page_rect().unwrap();
        let top_right = PointF { x: pr.right - 5.0, y: pr.top + 5.0 };
        assert_eq!(a.link_at(top_right), Some(LinkTarget::Page(0)));
        let top_left = PointF { x: pr.left + 5.0, y: pr.top + 5.0 };
        assert_eq!(a.link_at(top_left), None, "unrotated hotspot must have moved");
    }

    #[test]
    fn field_hit_transforms_like_links() {
        let mut a = app_with_doc(1);
        let fit_px = render_size(LETTER, a.client, 1.0);
        let field = crate::types::FormField {
            rect_pt: RectF { left: 0.0, top: 792.0, right: 306.0, bottom: 396.0 },
            kind: crate::types::FieldKind::Text,
        };
        a.rendered(CachedPage {
            page: 0, px: fit_px, rot: 0, bgra: vec![0; 4], links: vec![], fields: vec![field], gen: 0,
        });
        let pr = a.current_page_rect().unwrap();
        let p = PointF { x: pr.left + 5.0, y: pr.top + 5.0 };
        assert_eq!(a.field_hit(p), Some((0, crate::types::FieldKind::Text)));
        let q = PointF { x: pr.left + 5.0, y: pr.bottom - 5.0 };
        assert_eq!(a.field_at(q), None);
    }

    #[test]
    fn pdf_point_at_round_trips_pdf_space() {
        let mut a = app_with_doc(1);
        let fit_px = render_size(LETTER, a.client, 1.0);
        a.rendered(CachedPage {
            page: 0, px: fit_px, rot: 0, bgra: vec![0; 4], links: vec![], fields: vec![], gen: 0,
        });
        let pr = a.current_page_rect().unwrap();
        // Window point 10% in from the page's top-left corner.
        let p = PointF { x: pr.left + pr.width() * 0.1, y: pr.top + pr.height() * 0.1 };
        let pt = a.pdf_point_at(p).unwrap();
        assert!((pt.x - 61.2).abs() < 2.0, "x: {}", pt.x);
        assert!((pt.y - (792.0 - 79.2)).abs() < 2.0, "y-up flip: {}", pt.y);
    }

    // --- rotation ---

    #[test]
    fn rotate_cycles_and_resets_to_fit() {
        let mut a = app_with_doc(2);
        assert!(!App::new().rotate_cw(), "no doc: no-op");
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 2.0);
        assert!(a.rotate_cw());
        let d = a.doc().unwrap();
        assert_eq!((d.rot, d.zoom), (1, 1.0));
        assert_eq!(d.scroll, PointF::default());
        for want in [2, 3, 0, 1] {
            a.rotate_cw();
            assert_eq!(a.doc().unwrap().rot, want);
        }
    }

    #[test]
    fn rotated_renders_swap_axes_and_carry_rot() {
        let mut a = app_with_doc(2);
        let upright = a.wanted_renders()[0];
        a.rotate_cw();
        let reqs = a.wanted_renders();
        assert_eq!(reqs[0].rot, 1);
        // In a square client the fit scale is symmetric, so the rotated
        // request is exactly the upright one with its axes swapped.
        assert_eq!((reqs[0].px.w, reqs[0].px.h), (upright.px.h, upright.px.w));
        // A portrait page fit into a square client: upright is taller than
        // wide, rotated is wider than tall.
        assert!(upright.px.h > upright.px.w);
        assert!(reqs[0].px.w > reqs[0].px.h);
        // The prefetch request is rotated too.
        assert!(reqs.iter().all(|r| r.rot == 1));
        // An upright cached entry must NOT satisfy a rotated request, even at
        // 180° where the pixel size is identical.
        a.rotate_cw(); // rot = 2: same px as upright
        let px180 = a.current_px().unwrap();
        a.rendered(CachedPage { page: 0, px: px180, rot: 0, bgra: vec![0; 4], links: vec![], fields: vec![], gen: 0 });
        assert!(
            a.wanted_renders().iter().any(|r| r.page == 0 && r.rot == 2),
            "180° must re-render despite identical px"
        );
    }

    // --- go-to-page entry ---

    #[test]
    fn goto_entry_flow_commits_clamped_1_based() {
        let mut a = app_with_doc(5);
        assert!(!a.goto_entry_active());
        assert!(a.goto_entry_start());
        assert!(a.goto_entry_active());
        a.goto_entry_char('x'); // non-digit ignored
        a.goto_entry_char('4');
        a.goto_entry_char('2');
        a.goto_entry_backspace();
        assert_eq!(a.doc().unwrap().goto_entry.as_deref(), Some("4"));
        assert!(a.goto_entry_commit(), "page 4 (1-based) = index 3");
        assert_eq!(a.doc().unwrap().current, 3);
        assert!(!a.goto_entry_active(), "commit closes the prompt");
        // Out-of-range clamps to the last page; "0" clamps to the first.
        a.goto_entry_start();
        for c in "999".chars() {
            a.goto_entry_char(c);
        }
        assert!(a.goto_entry_commit());
        assert_eq!(a.doc().unwrap().current, 4);
        a.goto_entry_start();
        a.goto_entry_char('0');
        assert!(a.goto_entry_commit());
        assert_eq!(a.doc().unwrap().current, 0);
    }

    #[test]
    fn goto_entry_cancel_and_empty_commit_close_without_jump() {
        let mut a = app_with_doc(5);
        a.flip(2);
        a.goto_entry_start();
        a.goto_entry_char('5');
        a.goto_entry_cancel();
        assert!(!a.goto_entry_active());
        assert_eq!(a.doc().unwrap().current, 2, "cancel must not jump");
        a.goto_entry_start();
        assert!(!a.goto_entry_commit(), "empty Enter just closes");
        assert!(!a.goto_entry_active());
        assert_eq!(a.doc().unwrap().current, 2);
        // Buffer length is capped at 6 digits.
        a.goto_entry_start();
        for c in "1234567890".chars() {
            a.goto_entry_char(c);
        }
        assert_eq!(a.doc().unwrap().goto_entry.as_deref(), Some("123456"));
        a.goto_entry_cancel();
    }

    // --- password mode ---

    #[test]
    fn password_mode_buffer_ops() {
        let mut a = App::new();
        a.mode = Mode::Password { path: PathBuf::from("x.pdf"), buf: String::new(), prev: None };
        a.password_char('h');
        a.password_char('i');
        a.password_backspace();
        match &a.mode {
            Mode::Password { buf, .. } => assert_eq!(buf, "h"),
            _ => panic!(),
        }
        let (path, pw) = a.password_take().unwrap();
        assert_eq!(path, PathBuf::from("x.pdf"));
        assert_eq!(pw, "h");
        assert!(matches!(a.mode, Mode::Loading { .. }), "no stashed doc -> Loading screen");
    }

    #[test]
    fn password_cancel_and_take_restore_stashed_doc() {
        // A doc is open when a password-protected file is opened on top of it:
        // the prompt stashes the DocState; cancel restores it (spec: keep the
        // current doc), and Enter/take ALSO restores it (it keeps showing
        // while the retry loads).
        let mut a = app_with_doc(3);
        a.flip(1);
        let prev = match std::mem::replace(&mut a.mode, Mode::Empty) {
            Mode::Doc(d) => Some(Box::new(d)),
            _ => None,
        };
        a.mode = Mode::Password { path: PathBuf::from("locked.pdf"), buf: String::new(), prev };
        a.password_cancel();
        assert_eq!(a.doc().unwrap().current, 1, "cancel restores the doc at its page");
        // Without a stashed doc, cancel falls back to Empty.
        let mut b = App::new();
        b.mode = Mode::Password { path: PathBuf::from("locked.pdf"), buf: String::new(), prev: None };
        b.password_cancel();
        assert!(matches!(b.mode, Mode::Empty));
    }

    #[cfg(feature = "pro")]
    mod pro_tests {
        use super::*;
        use crate::types::{FieldKind, FormField};

        fn field(l: f32, b: f32, r: f32, t: f32) -> FormField {
            FormField { rect_pt: RectF { left: l, top: t, right: r, bottom: b }, kind: FieldKind::Text }
        }

        #[test]
        fn field_indication_cycles_highlight_frame_off() {
            let mut a = App::new();
            assert_eq!(a.field_indication, FieldIndication::Highlight, "default is highlight");
            assert_eq!(a.cycle_field_indication(), FieldIndication::Frame);
            assert_eq!(a.cycle_field_indication(), FieldIndication::Off);
            assert_eq!(a.cycle_field_indication(), FieldIndication::Highlight);
        }

        /// The drawn indication and the click hit-test must agree: the center
        /// of every drawn rect must field_hit() back to that field, at every
        /// rotation (rects live in unrotated PDF space; drawing rotates them,
        /// hit-testing unrotates the point — the two must be exact inverses).
        #[test]
        fn field_rects_window_inverts_field_hit_across_rotations() {
            let mut a = app_with_doc(1);
            let f = field(100.0, 650.0, 300.0, 700.0);
            for _ in 0..4 {
                let req = a.wanted_renders()[0];
                a.rendered(CachedPage {
                    page: 0,
                    px: req.px,
                    rot: req.rot,
                    bgra: vec![0; 4],
                    links: vec![],
                    fields: vec![f.clone()],
                    gen: 0,
                });
                let rects = a.field_rects_window();
                assert_eq!(rects.len(), 1, "rot {}", req.rot);
                let r = rects[0];
                assert!(r.left < r.right && r.top < r.bottom, "rot {}: {r:?}", req.rot);
                let c = PointF { x: (r.left + r.right) / 2.0, y: (r.top + r.bottom) / 2.0 };
                assert_eq!(
                    a.field_hit(c),
                    Some((0, FieldKind::Text)),
                    "rot {}: center {c:?} of drawn rect {r:?} must hit the field",
                    req.rot
                );
                a.rotate_cw();
            }
        }

        #[test]
        fn dirty_flag_lifecycle() {
            let mut a = app_with_doc(1);
            assert!(!a.dirty_dot_visible());
            a.form_dirty = true;
            assert!(a.dirty_dot_visible());
            // Stashing the doc behind a password prompt (opening a second,
            // locked PDF) must NOT clear the flag — the edits still exist in
            // the stashed doc. The renderer hides the dot outside Doc mode;
            // this accessor keeps reporting the truth.
            let prev = match std::mem::replace(&mut a.mode, Mode::Empty) {
                Mode::Doc(d) => Some(Box::new(d)),
                _ => None,
            };
            a.mode =
                Mode::Password { path: PathBuf::from("locked.pdf"), buf: String::new(), prev };
            assert!(a.dirty_dot_visible(), "mode changes alone never clear dirty");
            // A fresh document clears it.
            a.doc_loaded(PathBuf::from("y.pdf"), 1, vec![LETTER], vec![], false);
            assert!(!a.dirty_dot_visible());
        }

        #[test]
        fn tab_cycles_fields_in_reading_order_and_wraps() {
            let mut a = app_with_doc(1);
            let fit_px = render_size(LETTER, a.client, 1.0);
            // Three fields: top (y 700..720), middle (500..520), bottom (300..320).
            a.rendered(CachedPage {
                page: 0, px: fit_px, rot: 0, bgra: vec![0; 4], links: vec![],
                fields: vec![field(72.0, 500.0, 300.0, 520.0),   // middle
                             field(72.0, 700.0, 300.0, 720.0),   // top
                             field(72.0, 300.0, 300.0, 320.0)],  // bottom
                gen: 0,
            });
            // From nothing selected, Tab goes to the TOP field (max top).
            let p1 = a.form_tab_target(false).unwrap();
            assert!((p1.y - 710.0).abs() < 1.0, "first tab -> top field, got y {}", p1.y);
            let p2 = a.form_tab_target(false).unwrap();
            assert!((p2.y - 510.0).abs() < 1.0, "then middle");
            let p3 = a.form_tab_target(false).unwrap();
            assert!((p3.y - 310.0).abs() < 1.0, "then bottom");
            let p4 = a.form_tab_target(false).unwrap();
            assert!((p4.y - 710.0).abs() < 1.0, "wraps to top");
            let p5 = a.form_tab_target(true).unwrap();
            assert!((p5.y - 310.0).abs() < 1.0, "shift-tab goes back (wraps to bottom)");
        }
    }
}

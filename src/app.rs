//! Application state machine + page cache. Pure logic — no OS types.

use crate::layout::{
    clamp_scroll, fit_scale, max_zoom, page_rect, pdf_rect_to_window, render_size, zoom_scroll,
    PointF, RectF, SizeU, SCROLL_STEP,
};
use crate::overlay::OutlineState;
use crate::types::{LinkTarget, OutlineItem, PageLink};
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::PathBuf;

/// One rendered page: BGRA pixels + that page's links (PDF coords).
pub struct CachedPage {
    pub page: u16,
    pub px: SizeU,
    pub bgra: Vec<u8>,
    pub links: Vec<PageLink>,
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
    pub fn get_any(&self, page: u16) -> Option<&CachedPage> {
        self.entries.iter().find(|e| e.page == page)
    }

    /// Entry within SIZE_TOLERANCE of `want` if one exists, else the freshest.
    pub fn get_best(&self, page: u16, want: SizeU) -> Option<&CachedPage> {
        self.entries
            .iter()
            .find(|e| e.page == page && close(e.px, want))
            .or_else(|| self.get_any(page))
    }

    pub fn has_close(&self, page: u16, px: SizeU) -> bool {
        self.entries.iter().any(|e| e.page == page && close(e.px, px))
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
        self.entries.retain(|e| !(e.page == entry.page && e.px == entry.px));
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
    pub current: u16,
    /// 1.0 = fit-to-window.
    pub zoom: f32,
    pub scroll: PointF,
    /// Some = TOC overlay is open.
    pub overlay: Option<OutlineState>,
}

impl DocState {
    fn page_pt(&self) -> (f32, f32) {
        self.page_sizes
            .get(self.current as usize)
            .copied()
            .unwrap_or((612.0, 792.0))
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
    /// Render requests sent to the PDF thread but not yet answered.
    pub pending: HashSet<(u16, u32, u32)>,
    /// Pages whose render genuinely failed — never re-requested (avoids a
    /// request/fail hot loop on malformed pages). Cleared on document load.
    pub failed: HashSet<u16>,
    pub client: SizeU,
    pub dpi: u32,
    /// F1 help overlay. Global (not per-document): available in every mode,
    /// so it lives here rather than on DocState like the TOC overlay.
    pub help_visible: bool,
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
        }
    }

    /// Flip the F1 help overlay. Available in every mode.
    pub fn toggle_help(&mut self) {
        self.help_visible = !self.help_visible;
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
    ) {
        self.cache.clear();
        self.pending.clear();
        self.failed.clear();
        self.mode = Mode::Doc(DocState {
            path,
            pages,
            page_sizes,
            outline,
            current: 0,
            zoom: 1.0,
            scroll: PointF::default(),
            overlay: None,
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
        let px = render_size(d.page_pt(), client, d.zoom);
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
        let page_pt = d.page_pt();
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

    /// Pixel size the current page renders at (current zoom).
    pub fn current_px(&self) -> Option<SizeU> {
        let d = self.doc()?;
        Some(render_size(d.page_pt(), self.client, d.zoom))
    }

    /// Window-space rect the current page occupies.
    pub fn current_page_rect(&self) -> Option<RectF> {
        let d = self.doc()?;
        Some(page_rect(self.current_px()?, self.client, d.scroll))
    }

    /// Crisp render of the current page first, then prefetch next/prev at fit.
    pub fn wanted_renders(&self) -> Vec<RenderReq> {
        let Some(d) = self.doc() else { return Vec::new() };
        let mut out = Vec::new();
        let mut push_if_needed = |page: u16, px: SizeU, visible: bool| {
            if !self.cache.has_close(page, px)
                && !self.pending.contains(&(page, px.w, px.h))
                && !self.failed.contains(&page)
            {
                out.push(RenderReq { page, px, visible });
            }
        };
        push_if_needed(d.current, render_size(d.page_pt(), self.client, d.zoom), true);
        for neighbor in [d.current as i32 + 1, d.current as i32 - 1] {
            if neighbor < 0 || neighbor >= d.pages as i32 {
                continue;
            }
            let page = neighbor as u16;
            let pt = d.page_sizes.get(page as usize).copied().unwrap_or((612.0, 792.0));
            push_if_needed(page, render_size(pt, self.client, 1.0), false);
        }
        drop(push_if_needed);
        out
    }

    pub fn mark_pending(&mut self, req: &RenderReq) {
        self.pending.insert((req.page, req.px.w, req.px.h));
    }

    /// A render reply arrived: clear the page's pending entries (by page, not
    /// exact size — pdfium may round differently than layout::render_size).
    pub fn rendered(&mut self, page: CachedPage) {
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

    /// Link under a window-space point on the current page, if any.
    pub fn link_at(&self, p: PointF) -> Option<LinkTarget> {
        let d = self.doc()?;
        let entry = self.cache.get_any(d.current)?;
        let pr = self.current_page_rect()?;
        let page_pt = d.page_pt();
        let px_per_pt = fit_scale(page_pt, self.client) * d.zoom;
        for link in &entry.links {
            let wr = pdf_rect_to_window(link.rect_pt, page_pt.1, px_per_pt, pr);
            if wr.contains(p) {
                return Some(link.target.clone());
            }
        }
        None
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
        );
        a
    }

    fn cached(page: u16, px: SizeU) -> CachedPage {
        CachedPage { page, px, bgra: vec![0; 4], links: vec![] }
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
        a.doc_loaded(PathBuf::from("empty.pdf"), 0, vec![], vec![]);
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
        assert!(c.get_any(0).is_none(), "0 was evicted");
        assert!(c.get_any(1).is_some());
        assert!(c.get_any(2).is_some());
    }

    #[test]
    fn cache_evicts_by_byte_budget_keeping_newest() {
        let mut c = PageCache::with_budget(6, 100);
        let big = |page: u16, bytes: usize| CachedPage {
            page,
            px: SizeU { w: 1, h: 1 },
            bgra: vec![0; bytes],
            links: vec![],
        };
        c.put(big(0, 60), &[]);
        c.put(big(1, 60), &[]); // 120 > 100: page 0 evicted
        assert!(c.get_any(0).is_none());
        assert!(c.get_any(1).is_some());
        c.put(big(2, 300), &[]); // oversized alone: still kept (newest never evicted)
        assert!(c.get_any(2).is_some());
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
        assert!(c.has_close(0, SizeU { w: 801, h: 999 }));
        assert!(!c.has_close(0, SizeU { w: 850, h: 1000 }));
        assert!(!c.has_close(1, SizeU { w: 800, h: 1000 }));
    }

    #[test]
    fn cache_get_best_prefers_size_match_then_freshest() {
        let mut c = PageCache::new(4);
        c.put(cached(0, SizeU { w: 100, h: 100 }), &[]);
        c.put(cached(0, SizeU { w: 200, h: 200 }), &[]); // freshest
        let best = c.get_best(0, SizeU { w: 101, h: 100 }).unwrap();
        assert_eq!(best.px, SizeU { w: 100, h: 100 }, "size match wins over freshness");
        let fallback = c.get_best(0, SizeU { w: 999, h: 999 }).unwrap();
        assert_eq!(fallback.px, SizeU { w: 200, h: 200 }, "no size match -> freshest");
    }

    #[test]
    fn cache_keeps_at_most_two_sizes_per_page() {
        let mut c = PageCache::with_budget(6, usize::MAX);
        for i in 0..12u32 {
            c.put(
                CachedPage {
                    page: 1,
                    px: SizeU { w: 100 + i, h: 100 + i },
                    bgra: vec![0; 10],
                    links: vec![],
                },
                &[1], // page 1 is protected — the crawl must still be bounded
            );
        }
        assert!(c.len() <= 2, "len = {}", c.len());
        assert!(c.has_close(1, SizeU { w: 111, h: 111 }), "newest size must survive");
    }

    #[test]
    fn render_reply_loop_converges_under_budget_pressure() {
        let mut a = App::new();
        a.set_client(SizeU { w: 1000, h: 1000 });
        a.cache = PageCache::with_budget(6, 100);
        a.doc_loaded(PathBuf::from("x.pdf"), 3, vec![LETTER; 3], vec![]);
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
                a.rendered(CachedPage { page: r.page, px: r.px, bgra: vec![0; bytes], links: vec![] });
            }
        }
        assert!(a.cache.get_any(1).is_some(), "visible page must remain cached");
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
        a.doc_loaded(PathBuf::from("y.pdf"), 2, vec![LETTER; 2], vec![]);
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
        a.rendered(CachedPage { page: 0, px: fit_px, bgra: vec![0; 4], links: vec![link] });
        let pr = a.current_page_rect().unwrap();
        // A window point just inside the page's top-left corner is on the link.
        let p = PointF { x: pr.left + 5.0, y: pr.top + 5.0 };
        assert_eq!(a.link_at(p), Some(LinkTarget::Page(0)));
        // Bottom-left corner of the page is NOT on the link.
        let q = PointF { x: pr.left + 5.0, y: pr.bottom - 5.0 };
        assert_eq!(a.link_at(q), None);
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
}

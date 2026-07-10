//! Direct2D/DirectWrite: device management and ALL drawing.

use crate::app::{App, Mode};
use crate::layout::{RectF, SizeU};
use windows::core::*;
use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

const BG: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.102, g: 0.102, b: 0.102, a: 1.0 }; // #1a1a1a

/// F1 help: the full input reference, drawn as a two-column table. Order and
/// wording mirror the README key map so the two never drift.
const HELP_ROWS: &[(&str, &str)] = &[
    ("Drag anywhere", "Move the window (Aero Snap works)"),
    ("Drag near an edge (8 px)", "Resize"),
    ("PgDn / \u{2192} / Space / wheel", "Next page"),
    ("PgUp / \u{2190} / Shift+Space / wheel up", "Previous page"),
    ("Home / End", "First / last page"),
    ("G, then a number, Enter", "Go to a page"),
    ("Alt+\u{2190} / Backspace / mouse back", "Back to where you jumped from"),
    ("Alt+\u{2192} / Shift+Backspace / mouse forward", "Forward again (re-do the jump)"),
    ("Ctrl+F", "Find text in the book (Esc closes the bar)"),
    ("Enter / F3 \u{b7} Shift steps back", "Next / previous match"),
    ("Alt+C / Alt+W", "Match case / whole words while finding"),
    ("Tab (or right-click menu)", "Table of contents overlay"),
    ("\u{2191} \u{2193} \u{2192} \u{2190} / Enter / Esc", "Navigate the open overlay"),
    ("Click a link", "Follow it (web links open in your browser)"),
    ("Ctrl+wheel", "Zoom at the cursor"),
    ("Drag (zoomed)", "Pan \u{b7} Ctrl+drag moves the window"),
    ("Double-click or 0", "Back to fit"),
    ("R", "Rotate 90\u{b0} clockwise"),
    ("Wheel (zoomed)", "Scroll; one more notch at the edge flips the page"),
    ("Ctrl+O", "Open file \u{b7} drop a PDF works too"),
    ("Ctrl+N", "Open a new empty window"),
    ("Ctrl+Shift+O", "Open a file in a new window"),
    ("F11", "Fill the screen (work area) / restore"),
    ("F1", "Show / hide the help overlay"),
    ("Esc / Ctrl+W", "Quit (Esc closes the overlay first)"),
];

/// Pro-only rows appended to the help table (kept out of the free binary).
#[cfg(feature = "pro")]
const PRO_HELP_ROWS: &[(&str, &str)] = &[
    ("Click a form field", "Fill it \u{b7} Tab/Shift+Tab next/previous \u{b7} Esc leaves"),
    ("F", "Mark form fields: highlight / frames / off"),
    ("Ctrl+S", "Save filled form in place"),
];

fn color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

fn rectf(r: RectF) -> D2D_RECT_F {
    D2D_RECT_F { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

pub struct Renderer {
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    rt: Option<ID2D1HwndRenderTarget>,
    /// Memo of the last uploaded page bitmap: (page, px, rot, gen) -> device
    /// bitmap. `rot` matters: a 180° render has the same px as the upright
    /// one, and without it the memo would keep serving the stale pixels.
    /// `gen` matters for the same reason: form filling re-renders the SAME
    /// (page, px, rot) with new pixels, and only the generation stamped by
    /// App::rendered() tells the two apart.
    page_bitmap: Option<(u16, SizeU, u8, u64, ID2D1Bitmap)>,
    dpi: u32,
    /// ((size_dip*10) as u32, centered) -> format; rebuilt when DPI changes.
    formats: Vec<((u32, bool), IDWriteTextFormat)>,
    /// Dotted stroke for the form-field frame indication. Factory resource
    /// (device-independent): survives render-target loss, created once.
    #[cfg(feature = "pro")]
    dot_stroke: Option<ID2D1StrokeStyle>,
}

impl Renderer {
    pub fn new() -> Result<Renderer> {
        unsafe {
            let factory: ID2D1Factory =
                D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Renderer {
                factory,
                dwrite,
                rt: None,
                page_bitmap: None,
                dpi: 96,
                formats: Vec::new(),
                #[cfg(feature = "pro")]
                dot_stroke: None,
            })
        }
    }

    pub fn set_dpi(&mut self, dpi: u32) {
        self.dpi = dpi;
        self.formats.clear();
    }

    /// Forget the memoized device bitmap. Called when a new document loads.
    /// The memo key's `gen` already rules out serving the old document's
    /// pixels (generations are never reset across loads), so this is
    /// defense-in-depth that also frees the old GPU bitmap promptly.
    pub fn clear_page_cache(&mut self) {
        self.page_bitmap = None;
    }

    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if let Some(rt) = &self.rt {
            unsafe {
                let _ = rt.Resize(&D2D_SIZE_U { width: w.max(1), height: h.max(1) });
            }
        }
    }

    fn ensure_rt(&mut self, hwnd: HWND) -> Result<ID2D1HwndRenderTarget> {
        if let Some(rt) = &self.rt {
            return Ok(rt.clone());
        }
        unsafe {
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                // 96 dpi = 1 DIP == 1 physical px; we do our own DPI scaling.
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd,
                pixelSize: D2D_SIZE_U {
                    width: (rc.right - rc.left).max(1) as u32,
                    height: (rc.bottom - rc.top).max(1) as u32,
                },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let rt = self.factory.CreateHwndRenderTarget(&props, &hwnd_props)?;
            self.rt = Some(rt.clone());
            Ok(rt)
        }
    }

    fn text_format(&mut self, size_dip: f32, centered: bool) -> Result<IDWriteTextFormat> {
        let key = ((size_dip * 10.0) as u32, centered);
        if let Some((_, f)) = self.formats.iter().find(|(k, _)| *k == key) {
            return Ok(f.clone());
        }
        unsafe {
            let make = |family: PCWSTR| {
                self.dwrite.CreateTextFormat(
                    family,
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size_dip * self.scale(),
                    w!("en-us"),
                )
            };
            // Spec: Segoe UI Variable, with plain Segoe UI as the fallback.
            let f = make(w!("Segoe UI Variable")).or_else(|_| make(w!("Segoe UI")))?;
            if centered {
                f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            } else {
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?; // vertical centering in row rects
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            }
            self.formats.push((key, f.clone()));
            Ok(f)
        }
    }

    fn draw_text(
        &self,
        rt: &ID2D1HwndRenderTarget,
        text: &str,
        fmt: &IDWriteTextFormat,
        rect: RectF,
        col: D2D1_COLOR_F,
    ) -> Result<()> {
        unsafe {
            let brush = rt.CreateSolidColorBrush(&col, None)?;
            let utf16: Vec<u16> = text.encode_utf16().collect();
            rt.DrawText(
                &utf16,
                fmt,
                &rectf(rect),
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
        Ok(())
    }

    fn page_bitmap(
        &mut self,
        rt: &ID2D1HwndRenderTarget,
        page: u16,
        px: SizeU,
        rot: u8,
        gen: u64,
        bgra: &[u8],
    ) -> Result<ID2D1Bitmap> {
        if let Some((p, s, r, g, bmp)) = &self.page_bitmap {
            if *p == page && *s == px && *r == rot && *g == gen {
                return Ok(bmp.clone());
            }
        }
        crate::dlog!("d2d: upload page={page} px={}x{} rot={rot} gen={gen}", px.w, px.h);
        unsafe {
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            let bmp = rt.CreateBitmap(
                D2D_SIZE_U { width: px.w, height: px.h },
                Some(bgra.as_ptr() as *const core::ffi::c_void),
                px.w * 4,
                &props,
            )?;
            self.page_bitmap = Some((page, px, rot, gen, bmp.clone()));
            Ok(bmp)
        }
    }

    /// Draw the whole scene. Returns `Ok(false)` on a normal frame.
    ///
    /// On device loss (`D2DERR_RECREATE_TARGET`, e.g. a driver reset or GPU
    /// contention from other apps) it drops the device resources and returns
    /// `Ok(true)` — a signal that the caller MUST schedule another paint so the
    /// render target is recreated. Without that, a state that never invalidates
    /// on its own (Empty/Password mode, or a Doc with no pending render reply)
    /// would keep the now-dropped target and leave the window permanently
    /// stale/white after a single transient loss.
    pub fn draw(&mut self, hwnd: HWND, app: &App, badge: Option<(&str, f32)>) -> Result<bool> {
        let rt = self.ensure_rt(hwnd)?;
        let result = self.draw_inner(&rt, app, badge);
        match result {
            Err(e) if e.code() == D2DERR_RECREATE_TARGET => {
                self.rt = None;
                self.page_bitmap = None;
                Ok(true)
            }
            Ok(()) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn draw_inner(
        &mut self,
        rt: &ID2D1HwndRenderTarget,
        app: &App,
        badge: Option<(&str, f32)>,
    ) -> Result<()> {
        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&BG));
            let client = app.client;
            let full = RectF { left: 0.0, top: 0.0, right: client.w as f32, bottom: client.h as f32 };

            match &app.mode {
                Mode::Empty => {
                    let fmt = self.text_format(16.0, true)?;
                    self.draw_text(rt, "Right-click or drop a PDF", &fmt, full, color(1.0, 1.0, 1.0, 0.35))?;
                }
                Mode::Loading { .. } => {
                    let fmt = self.text_format(16.0, true)?;
                    self.draw_text(rt, "Loading…", &fmt, full, color(1.0, 1.0, 1.0, 0.35))?;
                }
                Mode::Password { buf, .. } => {
                    let fmt = self.text_format(16.0, true)?;
                    let dots = "•".repeat(buf.chars().count().max(0));
                    let msg =
                        format!("Password required\n\n{dots}_\n\nEnter to open — Esc to cancel");
                    self.draw_text(rt, &msg, &fmt, full, color(1.0, 1.0, 1.0, 0.8))?;
                }
                Mode::Doc(d) => {
                    if let (Some(px), Some(dest)) = (app.current_px(), app.current_page_rect()) {
                        if let Some(entry) = app.cache.get_best(d.current, d.rot, px) {
                            let bmp = self.page_bitmap(
                                rt, entry.page, entry.px, entry.rot, entry.gen, &entry.bgra,
                            )?;
                            let crisp = entry.px.w as f32 == dest.width().round()
                                && entry.px.h as f32 == dest.height().round();
                            let mode = if crisp {
                                D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR
                            } else {
                                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR
                            };
                            rt.DrawBitmap(&bmp, Some(&rectf(dest)), 1.0, mode, None);
                            // Form-field indication ('F' cycles): drawn over
                            // the page bitmap, under overlay/badge/toasts.
                            // Client-side on purpose — rendered pixels stay
                            // indication-free, so toggling is a repaint, not
                            // a render-cache invalidation.
                            #[cfg(feature = "pro")]
                            self.draw_field_indication(rt, app)?;
                            // Search-match highlights: client-side over the
                            // cached bitmap (toggling/retyping is a repaint,
                            // never a render), only when the page bitmap is
                            // present — same gating as field indication.
                            self.draw_search_highlights(rt, app)?;
                        }
                    }
                    if let Some(ov) = &d.overlay {
                        self.draw_overlay(rt, ov, client)?;
                    }
                    if let Some((text, alpha)) = badge {
                        self.draw_badge(rt, text, alpha, client)?;
                    }
                    if let Some(buf) = &d.goto_entry {
                        self.draw_goto(rt, buf, d.pages, client)?;
                    }
                    if let Some(st) = &d.search {
                        self.draw_search_bar(rt, st, client)?;
                    }
                }
            }
            // Unsaved-changes dot (pro): small, top-right, deliberately quiet.
            // Doc mode only — the flag can survive into the Password screen
            // (opening a second locked PDF over a dirty doc), where the dot
            // would be noise over a prompt that isn't about that document.
            // D2D1_ELLIPSE.point is windows_numerics::Vector2 in this windows-rs
            // version (not D2D_POINT_2F) — set via Default + field access so we
            // don't need windows-numerics as a direct dependency just to name it.
            if app.dirty_dot_visible() && matches!(app.mode, Mode::Doc(_)) {
                let s = self.scale();
                let (radius, m) = (4.0 * s, 10.0 * s);
                let mut ell = D2D1_ELLIPSE { radiusX: radius, radiusY: radius, ..Default::default() };
                ell.point.X = client.w as f32 - m - radius;
                ell.point.Y = m + radius;
                let b = rt.CreateSolidColorBrush(&color(1.0, 0.72, 0.2, 0.55), None)?;
                rt.FillEllipse(&ell, &b);
            }
            // F1 help sits above everything (page, overlay, badge) in every mode.
            if app.help_visible {
                self.draw_help(rt, client)?;
            }
            rt.EndDraw(None, None)
        }
    }

    /// Translucent wash or dotted outline over every form widget on the
    /// current page, per `App::field_indication` ('F' cycles it).
    #[cfg(feature = "pro")]
    fn draw_field_indication(&mut self, rt: &ID2D1HwndRenderTarget, app: &App) -> Result<()> {
        use crate::app::FieldIndication;
        if app.field_indication == FieldIndication::Off {
            return Ok(());
        }
        let rects = app.field_rects_window();
        if rects.is_empty() {
            return Ok(());
        }
        unsafe {
            match app.field_indication {
                FieldIndication::Highlight => {
                    // Chromium-style pale blue wash; low alpha keeps the
                    // widget's own text and borders readable through it.
                    let b = rt.CreateSolidColorBrush(&color(0.47, 0.62, 1.0, 0.22), None)?;
                    for r in &rects {
                        rt.FillRectangle(&rectf(*r), &b);
                    }
                }
                FieldIndication::Frame => {
                    let stroke = self.dot_stroke()?;
                    let b = rt.CreateSolidColorBrush(&color(0.35, 0.52, 0.95, 0.85), None)?;
                    let w = 1.0 * self.scale();
                    for r in &rects {
                        rt.DrawRectangle(&rectf(*r), &b, w, &stroke);
                    }
                }
                FieldIndication::Off => {}
            }
        }
        Ok(())
    }

    /// Lazily-built dotted stroke style. Dot dashes have ZERO length — with
    /// the default flat cap nothing renders at all; round caps make the dots.
    /// Factory (device-independent) resource: survives render-target loss.
    #[cfg(feature = "pro")]
    fn dot_stroke(&mut self) -> Result<ID2D1StrokeStyle> {
        if let Some(s) = &self.dot_stroke {
            return Ok(s.clone());
        }
        let props = D2D1_STROKE_STYLE_PROPERTIES {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            dashCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_MITER,
            miterLimit: 10.0,
            dashStyle: D2D1_DASH_STYLE_DOT,
            dashOffset: 0.0,
        };
        let s = unsafe { self.factory.CreateStrokeStyle(&props, None)? };
        self.dot_stroke = Some(s.clone());
        Ok(s)
    }

    /// Translucent washes over the current page's search matches — yellow
    /// for every match, a stronger orange for the current one (Chromium's
    /// scheme, distinct from Pro's pale-blue field wash).
    fn draw_search_highlights(&mut self, rt: &ID2D1HwndRenderTarget, app: &App) -> Result<()> {
        let Some(d) = app.doc() else { return Ok(()) };
        let Some(st) = &d.search else { return Ok(()) };
        let Some(pm) = st.matches_on(d.current) else { return Ok(()) };
        unsafe {
            let all = rt.CreateSolidColorBrush(&color(1.0, 0.85, 0.2, 0.28), None)?;
            let cur = rt.CreateSolidColorBrush(&color(1.0, 0.55, 0.1, 0.45), None)?;
            for (i, m) in pm.matches.iter().enumerate() {
                let brush = if st.current == Some((d.current, i)) { &cur } else { &all };
                // Every rect of a multi-rect (line-wrapped) match is painted.
                for r in app.page_rects_window(&m.rects) {
                    rt.FillRectangle(&rectf(r), brush);
                }
            }
        }
        Ok(())
    }

    /// The Ctrl+F find bar: a top-right rounded pill (goto-pill family)
    /// showing the query (head-elided so the tail stays visible), the
    /// "n / m" counter (ellipsis while scanning, red for a finished no-hit
    /// scan, hidden while the query is under 2 chars), and Aa / W option
    /// indicators (lit when active).
    fn draw_search_bar(
        &mut self,
        rt: &ID2D1HwndRenderTarget,
        st: &crate::search::SearchState,
        client: SizeU,
    ) -> Result<()> {
        let s = self.scale();
        let r = crate::search::bar_rect(client, s);
        unsafe {
            let rr = D2D1_ROUNDED_RECT { rect: rectf(r), radiusX: 8.0 * s, radiusY: 8.0 * s };
            let bg = rt.CreateSolidColorBrush(&color(0.0, 0.0, 0.0, 0.85), None)?;
            rt.FillRoundedRectangle(&rr, &bg);
            let border = rt.CreateSolidColorBrush(&color(1.0, 1.0, 1.0, 0.25), None)?;
            rt.DrawRoundedRectangle(&rr, &border, 1.0 * s, None);
        }
        let fmt = self.text_format(14.0, false)?;
        let pad = 12.0 * s;
        // Right zone: counter + Aa/W indicators; the query gets the rest.
        let right_w = (150.0 * s).min(r.width() * 0.5);
        let query_rect =
            RectF { left: r.left + pad, top: r.top, right: r.right - right_w, bottom: r.bottom };
        // Head-elide the query so its TAIL stays visible (~7 px per char at
        // 14 DIP; exactness doesn't matter, NO_WRAP clips the rest).
        let budget = ((query_rect.width() / s - 45.0) / 7.0).max(4.0) as usize;
        let n = st.query.chars().count();
        let shown: String = if n > budget {
            format!("…{}", st.query.chars().skip(n - budget).collect::<String>())
        } else {
            st.query.clone()
        };
        self.draw_text(rt, &format!("Find: {shown}_"), &fmt, query_rect, color(1.0, 1.0, 1.0, 0.92))?;
        // Counter — hidden while the query is illegal (< 2 chars).
        if st.legal() {
            let text = format!(
                "{} / {}{}",
                st.ordinal().unwrap_or(0),
                st.total(),
                if st.scanning { "…" } else { "" }
            );
            let col = if !st.scanning && st.total() == 0 {
                color(1.0, 0.45, 0.45, 0.95) // scan finished with no hits
            } else {
                color(1.0, 1.0, 1.0, 0.7)
            };
            let counter_rect = RectF {
                left: r.right - right_w,
                top: r.top,
                right: r.right - pad - 44.0 * s,
                bottom: r.bottom,
            };
            self.draw_text(rt, &text, &fmt, counter_rect, col)?;
        }
        // Aa / W indicators: lit when the option is on, dim when off.
        let ind_fmt = self.text_format(12.0, false)?;
        let lit = color(1.0, 0.85, 0.2, 0.95);
        let dim = color(1.0, 1.0, 1.0, 0.3);
        let aa_rect = RectF {
            left: r.right - pad - 40.0 * s,
            top: r.top,
            right: r.right - pad - 16.0 * s,
            bottom: r.bottom,
        };
        let w_rect =
            RectF { left: r.right - pad - 14.0 * s, top: r.top, right: r.right - pad, bottom: r.bottom };
        self.draw_text(rt, "Aa", &ind_fmt, aa_rect, if st.options.match_case { lit } else { dim })?;
        self.draw_text(rt, "W", &ind_fmt, w_rect, if st.options.whole_word { lit } else { dim })?;
        Ok(())
    }

    /// Full-window F1 help: a dimmed backdrop with a centered two-column table
    /// of every input binding. Laid out in physical px and clamped so the whole
    /// table always fits the window (rows shrink before they'd overflow).
    fn draw_help(&mut self, rt: &ID2D1HwndRenderTarget, client: SizeU) -> Result<()> {
        let s = self.scale();
        let (cw, ch) = (client.w as f32, client.h as f32);
        let full = RectF { left: 0.0, top: 0.0, right: cw, bottom: ch };
        unsafe {
            // Near-opaque backdrop: readable over any page, still hints at it.
            let backdrop = rt.CreateSolidColorBrush(&color(0.09, 0.09, 0.09, 0.97), None)?;
            rt.FillRectangle(&rectf(full), &backdrop);
        }

        // Base rows plus the pro-only ones (absent from the free binary).
        #[allow(unused_mut)]
        let mut rows: Vec<(&str, &str)> = HELP_ROWS.to_vec();
        #[cfg(feature = "pro")]
        rows.extend_from_slice(PRO_HELP_ROWS);
        let n_rows = (rows.len() + 1) as f32; // + header
        let margin = 28.0 * s;
        let avail_w = (cw - 2.0 * margin).max(40.0);
        let avail_h = (ch - 2.0 * margin).max(40.0);
        let table_w = avail_w.min(880.0 * s);
        let row_h = (34.0 * s).min(avail_h / n_rows).max(1.0);
        let table_h = row_h * n_rows;
        let left = ((cw - table_w) / 2.0).max(margin);
        let top = ((ch - table_h) / 2.0).max(margin);
        let right = left + table_w;
        let bottom = top + table_h;
        let col_x = left + table_w * 0.40; // Input | Action divider
        // row_h is physical px; text_format wants a logical DIP size.
        let font_dip = (row_h / s * 0.42).clamp(9.0, 15.0);
        let pad = 16.0 * s;

        // Grid lines are drawn as thin filled rects (windows-rs DrawLine wants
        // windows_numerics::Vector2, a crate we intentionally don't depend on).
        let lw = (1.0 * s).max(1.0);
        unsafe {
            let line = rt.CreateSolidColorBrush(&color(1.0, 1.0, 1.0, 0.08), None)?;
            for i in 1..(n_rows as i32) {
                let y = top + row_h * i as f32;
                let sep = RectF { left, top: y - lw * 0.5, right, bottom: y + lw * 0.5 };
                rt.FillRectangle(&rectf(sep), &line);
            }
            let divider = RectF { left: col_x - lw * 0.5, top, right: col_x + lw * 0.5, bottom };
            rt.FillRectangle(&rectf(divider), &line);
            let border = rt.CreateSolidColorBrush(&color(1.0, 1.0, 1.0, 0.14), None)?;
            let rr = D2D1_ROUNDED_RECT {
                rect: rectf(RectF { left, top, right, bottom }),
                radiusX: 6.0 * s,
                radiusY: 6.0 * s,
            };
            rt.DrawRoundedRectangle(&rr, &border, 1.5 * s, None);
        }

        // Header (centered, accent-colored).
        let hdr_fmt = self.text_format(font_dip + 1.0, true)?;
        let accent = color(0.74, 0.78, 0.96, 1.0);
        self.draw_text(rt, "Input", &hdr_fmt, RectF { left, top, right: col_x, bottom: top + row_h }, accent)?;
        self.draw_text(rt, "Action", &hdr_fmt, RectF { left: col_x, top, right, bottom: top + row_h }, accent)?;

        // Data rows (left-aligned, vertically centered in each row).
        let fmt = self.text_format(font_dip, false)?;
        let ink_in = color(1.0, 1.0, 1.0, 0.85);
        let ink_ac = color(1.0, 1.0, 1.0, 0.92);
        for (i, (inp, act)) in rows.iter().enumerate() {
            let ry = top + row_h * (i as f32 + 1.0);
            let in_rect = RectF { left: left + pad, top: ry, right: col_x - pad * 0.5, bottom: ry + row_h };
            let ac_rect = RectF { left: col_x + pad, top: ry, right: right - pad * 0.5, bottom: ry + row_h };
            self.draw_text(rt, inp, &fmt, in_rect, ink_in)?;
            self.draw_text(rt, act, &fmt, ac_rect, ink_ac)?;
        }
        Ok(())
    }

    fn draw_overlay(
        &mut self,
        rt: &ID2D1HwndRenderTarget,
        ov: &crate::overlay::OutlineState,
        client: SizeU,
    ) -> Result<()> {
        use crate::overlay::{CHEVRON_W, INDENT, PANEL_W, ROW_H};
        let s = self.scale();
        let panel =
            RectF { left: 0.0, top: 0.0, right: PANEL_W * s, bottom: client.h as f32 };
        unsafe {
            let bg = rt.CreateSolidColorBrush(&color(0.055, 0.055, 0.055, 0.85), None)?;
            rt.FillRectangle(&rectf(panel), &bg);
        }
        if ov.items.is_empty() {
            let fmt = self.text_format(14.0, true)?;
            return self.draw_text(rt, "No table of contents", &fmt, panel, color(1.0, 1.0, 1.0, 0.4));
        }
        let fmt = self.text_format(14.0, false)?;
        let panel_h_dip = client.h as f32 / s;
        for (pos, &idx) in ov.visible().iter().enumerate() {
            let top_dip = ov.row_top(pos);
            if top_dip + ROW_H < 0.0 {
                continue;
            }
            if top_dip > panel_h_dip {
                break;
            }
            let it = &ov.items[idx];
            let y = top_dip * s;
            let row = RectF { left: 0.0, top: y, right: PANEL_W * s, bottom: y + ROW_H * s };
            if idx == ov.selected {
                unsafe {
                    let sel = rt.CreateSolidColorBrush(&color(1.0, 1.0, 1.0, 0.12), None)?;
                    rt.FillRectangle(&rectf(row), &sel);
                }
            }
            let indent = it.depth as f32 * INDENT * s;
            if it.has_children {
                let chev_rect = RectF {
                    left: indent + 4.0 * s,
                    top: row.top,
                    right: indent + CHEVRON_W * s,
                    bottom: row.bottom,
                };
                let glyph = if ov.is_expanded(idx) { "▾" } else { "▸" };
                self.draw_text(rt, glyph, &fmt, chev_rect, color(1.0, 1.0, 1.0, 0.55))?;
            }
            let title_rect = RectF {
                left: indent + CHEVRON_W * s + 2.0 * s,
                top: row.top,
                right: PANEL_W * s - 8.0 * s,
                bottom: row.bottom,
            };
            self.draw_text(rt, &it.title, &fmt, title_rect, color(1.0, 1.0, 1.0, 0.9))?;
        }
        Ok(())
    }

    /// The "G" go-to-page prompt: a bottom-centered pill showing the typed
    /// digits, styled like the page badge so it reads as the same family.
    fn draw_goto(
        &mut self,
        rt: &ID2D1HwndRenderTarget,
        buf: &str,
        pages: u16,
        client: SizeU,
    ) -> Result<()> {
        let s = self.scale();
        // Never wider than the window: the 200-DIP minimum window is narrower
        // than the pill's natural width, and an overhanging pill gets clipped
        // by the window edges.
        let (bw, bh, m) = ((240.0 * s).min(client.w as f32 - 16.0 * s), 36.0 * s, 24.0 * s);
        let cx = client.w as f32 / 2.0;
        let r = RectF {
            left: cx - bw / 2.0,
            top: client.h as f32 - bh - m,
            right: cx + bw / 2.0,
            bottom: client.h as f32 - m,
        };
        unsafe {
            let rr = D2D1_ROUNDED_RECT { rect: rectf(r), radiusX: bh / 2.0, radiusY: bh / 2.0 };
            let bg = rt.CreateSolidColorBrush(&color(0.0, 0.0, 0.0, 0.8), None)?;
            rt.FillRoundedRectangle(&rr, &bg);
            let border = rt.CreateSolidColorBrush(&color(1.0, 1.0, 1.0, 0.25), None)?;
            rt.DrawRoundedRectangle(&rr, &border, 1.0 * s, None);
        }
        let fmt = self.text_format(14.0, true)?;
        let text = format!("Go to page: {buf}_ ({pages} pages)");
        self.draw_text(rt, &text, &fmt, r, color(1.0, 1.0, 1.0, 0.92))
    }

    fn draw_badge(
        &mut self,
        rt: &ID2D1HwndRenderTarget,
        text: &str,
        alpha: f32,
        client: SizeU,
    ) -> Result<()> {
        if alpha <= 0.0 {
            return Ok(());
        }
        let s = self.scale();
        // Width tracks the text so longer toasts (save confirmation, the
        // free-edition Pro hint) fit; the page badge keeps its old size.
        let bw = ((text.chars().count() as f32 * 8.0 + 44.0) * s)
            .max(110.0 * s)
            .min(client.w as f32 - 16.0 * s);
        let (bh, m) = (36.0 * s, 24.0 * s);
        let r = RectF {
            left: client.w as f32 - bw - m,
            top: client.h as f32 - bh - m,
            right: client.w as f32 - m,
            bottom: client.h as f32 - m,
        };
        unsafe {
            let rr = D2D1_ROUNDED_RECT { rect: rectf(r), radiusX: bh / 2.0, radiusY: bh / 2.0 };
            let bg = rt.CreateSolidColorBrush(&color(0.0, 0.0, 0.0, 0.7 * alpha), None)?;
            rt.FillRoundedRectangle(&rr, &bg);
        }
        let fmt = self.text_format(14.0, true)?;
        self.draw_text(rt, text, &fmt, r, color(1.0, 1.0, 1.0, 0.9 * alpha))
    }
}

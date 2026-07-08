//! Pure geometry: fit, zoom, pan, hit zones. No OS types allowed in this file.

pub const MAX_RENDER_DIM: u32 = 16_384;
pub const MAX_ZOOM: f32 = 5.0;
/// Pixels one wheel notch scrolls a zoomed page.
pub const SCROLL_STEP: f32 = 120.0;

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct SizeU {
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct PointF {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct RectF {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl RectF {
    pub fn contains(&self, p: PointF) -> bool {
        p.x >= self.left && p.x < self.right && p.y >= self.top && p.y < self.bottom
    }
    pub fn width(&self) -> f32 {
        self.right - self.left
    }
    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }
}

/// Scale that aspect-fits a page (in points) into the client (in px): px per pt.
pub fn fit_scale(page_pt: (f32, f32), client: SizeU) -> f32 {
    if page_pt.0 <= 0.0 || page_pt.1 <= 0.0 || client.w == 0 || client.h == 0 {
        return 1.0;
    }
    (client.w as f32 / page_pt.0).min(client.h as f32 / page_pt.1)
}

/// Pixel size to render the page at for the given zoom (1.0 = fit), capped at MAX_RENDER_DIM.
pub fn render_size(page_pt: (f32, f32), client: SizeU, zoom: f32) -> SizeU {
    let s = fit_scale(page_pt, client) * zoom;
    let mut w = (page_pt.0 * s).round().max(1.0);
    let mut h = (page_pt.1 * s).round().max(1.0);
    let cap = MAX_RENDER_DIM as f32;
    if w > cap || h > cap {
        let k = (cap / w).min(cap / h);
        w = (w * k).floor().max(1.0);
        h = (h * k).floor().max(1.0);
    }
    SizeU { w: w as u32, h: h as u32 }
}

/// Largest usable zoom for this page/client (<= MAX_ZOOM, honors MAX_RENDER_DIM).
pub fn max_zoom(page_pt: (f32, f32), client: SizeU) -> f32 {
    let s = fit_scale(page_pt, client);
    if s <= 0.0 {
        return 1.0;
    }
    let cap_w = MAX_RENDER_DIM as f32 / (page_pt.0 * s);
    let cap_h = MAX_RENDER_DIM as f32 / (page_pt.1 * s);
    MAX_ZOOM.min(cap_w).min(cap_h).max(1.0)
}

/// Clamp scroll to the page's overhang; axes where the page fits snap to 0.
pub fn clamp_scroll(page_px: SizeU, client: SizeU, scroll: PointF) -> PointF {
    let mx = (page_px.w as f32 - client.w as f32).max(0.0);
    let my = (page_px.h as f32 - client.h as f32).max(0.0);
    PointF { x: scroll.x.clamp(0.0, mx), y: scroll.y.clamp(0.0, my) }
}

/// Where the page bitmap lands in window coords (centered per axis when smaller).
pub fn page_rect(page_px: SizeU, client: SizeU, scroll: PointF) -> RectF {
    let x = if page_px.w <= client.w {
        ((client.w - page_px.w) as f32) / 2.0
    } else {
        -scroll.x
    };
    let y = if page_px.h <= client.h {
        ((client.h - page_px.h) as f32) / 2.0
    } else {
        -scroll.y
    };
    RectF { left: x, top: y, right: x + page_px.w as f32, bottom: y + page_px.h as f32 }
}

/// New scroll such that the page point under `cursor` stays under it after zooming
/// by `zoom_ratio` (new_zoom / old_zoom). Result is clamped.
pub fn zoom_scroll(
    cursor: PointF,
    old_rect: RectF,
    zoom_ratio: f32,
    new_page_px: SizeU,
    client: SizeU,
) -> PointF {
    let want_x = (cursor.x - old_rect.left) * zoom_ratio - cursor.x;
    let want_y = (cursor.y - old_rect.top) * zoom_ratio - cursor.y;
    clamp_scroll(new_page_px, client, PointF { x: want_x, y: want_y })
}

/// PDF-point rect (origin bottom-left, y up, top > bottom) -> window px rect.
/// `page_rect` is where the page bitmap sits in the window (from `page_rect()`).
pub fn pdf_rect_to_window(rect_pt: RectF, page_h_pt: f32, px_per_pt: f32, page_rect: RectF) -> RectF {
    RectF {
        left: page_rect.left + rect_pt.left * px_per_pt,
        top: page_rect.top + (page_h_pt - rect_pt.top) * px_per_pt,
        right: page_rect.left + rect_pt.right * px_per_pt,
        bottom: page_rect.top + (page_h_pt - rect_pt.bottom) * px_per_pt,
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Zone {
    TopLeft,
    Top,
    TopRight,
    Left,
    Interior,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

/// Resolve a client-space point to a resize-grip zone (corners win over edges).
pub fn hit_zone(p: PointF, client: SizeU, grip: f32) -> Zone {
    let l = p.x < grip;
    let r = p.x >= client.w as f32 - grip;
    let t = p.y < grip;
    let b = p.y >= client.h as f32 - grip;
    match (l, r, t, b) {
        (true, _, true, _) => Zone::TopLeft,
        (_, true, true, _) => Zone::TopRight,
        (true, _, _, true) => Zone::BottomLeft,
        (_, true, _, true) => Zone::BottomRight,
        (true, ..) => Zone::Left,
        (_, true, ..) => Zone::Right,
        (_, _, true, _) => Zone::Top,
        (_, _, _, true) => Zone::Bottom,
        _ => Zone::Interior,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LETTER: (f32, f32) = (612.0, 792.0); // US Letter, points

    fn sz(w: u32, h: u32) -> SizeU {
        SizeU { w, h }
    }
    fn pt(x: f32, y: f32) -> PointF {
        PointF { x, y }
    }

    // --- fit_scale / render_size ---

    #[test]
    fn fit_portrait_page_in_wide_client_is_height_bound() {
        // client 2000x1000: height binds: 1000/792
        let s = fit_scale(LETTER, sz(2000, 1000));
        assert!((s - 1000.0 / 792.0).abs() < 1e-5);
    }

    #[test]
    fn fit_portrait_page_in_tall_client_is_width_bound() {
        let s = fit_scale(LETTER, sz(500, 5000));
        assert!((s - 500.0 / 612.0).abs() < 1e-5);
    }

    #[test]
    fn fit_degenerate_inputs_yield_scale_one() {
        assert_eq!(fit_scale((0.0, 792.0), sz(100, 100)), 1.0);
        assert_eq!(fit_scale(LETTER, sz(0, 100)), 1.0);
    }

    #[test]
    fn render_size_at_fit_matches_client_on_binding_axis() {
        let px = render_size(LETTER, sz(2000, 1000), 1.0);
        assert_eq!(px.h, 1000); // height-bound
        assert!((px.w as f32 - 612.0 * (1000.0 / 792.0)).abs() <= 1.0);
    }

    #[test]
    fn render_size_never_zero() {
        let px = render_size(LETTER, sz(1, 1), 1.0);
        assert!(px.w >= 1 && px.h >= 1);
    }

    #[test]
    fn render_size_capped_at_max_dim() {
        let px = render_size(LETTER, sz(16_000, 16_000), 5.0);
        assert!(px.w <= MAX_RENDER_DIM && px.h <= MAX_RENDER_DIM);
    }

    #[test]
    fn max_zoom_capped_by_render_dim() {
        let mz = max_zoom(LETTER, sz(16_000, 16_000));
        let px = render_size(LETTER, sz(16_000, 16_000), mz);
        assert!(px.w <= MAX_RENDER_DIM && px.h <= MAX_RENDER_DIM);
        assert!(mz >= 1.0 && mz <= MAX_ZOOM);
    }

    // --- clamp_scroll / page_rect ---

    #[test]
    fn scroll_clamps_to_zero_when_page_fits() {
        let s = clamp_scroll(sz(500, 500), sz(1000, 1000), pt(50.0, -50.0));
        assert_eq!(s, pt(0.0, 0.0));
    }

    #[test]
    fn scroll_clamps_to_overhang() {
        let s = clamp_scroll(sz(1500, 2000), sz(1000, 1000), pt(9999.0, -5.0));
        assert_eq!(s, pt(500.0, 0.0));
    }

    #[test]
    fn page_rect_centers_when_smaller() {
        let r = page_rect(sz(500, 800), sz(1000, 1000), pt(0.0, 0.0));
        assert_eq!((r.left, r.top), (250.0, 100.0));
        assert_eq!((r.width(), r.height()), (500.0, 800.0));
    }

    #[test]
    fn page_rect_applies_scroll_when_larger() {
        let r = page_rect(sz(2000, 3000), sz(1000, 1000), pt(300.0, 700.0));
        assert_eq!((r.left, r.top), (-300.0, -700.0));
    }

    // --- zoom_scroll: the page point under the cursor stays put ---

    #[test]
    fn zoom_keeps_cursor_point_stationary() {
        let client = sz(1000, 1000);
        let old_px = render_size(LETTER, client, 1.0);
        let old_rect = page_rect(old_px, client, pt(0.0, 0.0));
        let cursor = pt(500.0, 500.0);
        let ratio = 2.0;
        let new_px = render_size(LETTER, client, 2.0);
        let scroll = zoom_scroll(cursor, old_rect, ratio, new_px, client);
        let new_rect = page_rect(new_px, client, scroll);
        // page-space position under cursor before == after (in new-zoom px, so *ratio)
        let before = ((cursor.x - old_rect.left) * ratio, (cursor.y - old_rect.top) * ratio);
        let after = (cursor.x - new_rect.left, cursor.y - new_rect.top);
        assert!((before.0 - after.0).abs() < 1.5, "x drifted: {before:?} vs {after:?}");
        assert!((before.1 - after.1).abs() < 1.5, "y drifted: {before:?} vs {after:?}");
    }

    // --- pdf_rect_to_window: y flips ---

    #[test]
    fn pdf_rect_transform_flips_y() {
        // A rect at the TOP of a 792pt page (top=792, bottom=772) must land at
        // the TOP of the window-space page rect.
        let link = RectF { left: 72.0, top: 792.0, right: 200.0, bottom: 772.0 };
        let pr = RectF { left: 0.0, top: 0.0, right: 612.0, bottom: 792.0 }; // 1 px/pt
        let w = pdf_rect_to_window(link, 792.0, 1.0, pr);
        assert_eq!((w.left, w.top, w.right, w.bottom), (72.0, 0.0, 200.0, 20.0));
    }

    #[test]
    fn pdf_rect_transform_scales_and_offsets() {
        let link = RectF { left: 100.0, top: 400.0, right: 200.0, bottom: 300.0 };
        let pr = RectF { left: 50.0, top: 10.0, right: 50.0 + 1224.0, bottom: 10.0 + 1584.0 }; // 2 px/pt
        let w = pdf_rect_to_window(link, 792.0, 2.0, pr);
        assert_eq!(w.left, 50.0 + 200.0);
        assert_eq!(w.top, 10.0 + (792.0 - 400.0) * 2.0);
    }

    // --- hit_zone: corners take precedence over edges ---

    #[test]
    fn hit_zone_corners_beat_edges() {
        let c = sz(1000, 1000);
        assert_eq!(hit_zone(pt(3.0, 3.0), c, 8.0), Zone::TopLeft);
        assert_eq!(hit_zone(pt(996.0, 3.0), c, 8.0), Zone::TopRight);
        assert_eq!(hit_zone(pt(3.0, 996.0), c, 8.0), Zone::BottomLeft);
        assert_eq!(hit_zone(pt(996.0, 996.0), c, 8.0), Zone::BottomRight);
    }

    #[test]
    fn hit_zone_edges_and_interior() {
        let c = sz(1000, 1000);
        assert_eq!(hit_zone(pt(500.0, 3.0), c, 8.0), Zone::Top);
        assert_eq!(hit_zone(pt(500.0, 996.0), c, 8.0), Zone::Bottom);
        assert_eq!(hit_zone(pt(3.0, 500.0), c, 8.0), Zone::Left);
        assert_eq!(hit_zone(pt(996.0, 500.0), c, 8.0), Zone::Right);
        assert_eq!(hit_zone(pt(500.0, 500.0), c, 8.0), Zone::Interior);
    }

    #[test]
    fn rectf_contains_is_half_open() {
        let r = RectF { left: 0.0, top: 0.0, right: 10.0, bottom: 10.0 };
        assert!(r.contains(pt(0.0, 0.0)));
        assert!(!r.contains(pt(10.0, 5.0)));
    }
}

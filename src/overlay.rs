//! TOC overlay state: visibility, selection, expansion, scrolling, hit-testing.
//! Pure logic — all coordinates are panel-local DIPs. No OS types allowed.

use crate::layout::PointF;
use crate::types::OutlineItem;

pub const ROW_H: f32 = 28.0;
pub const PANEL_W: f32 = 320.0;
pub const INDENT: f32 = 16.0;
pub const CHEVRON_W: f32 = 22.0;
pub const PANEL_PAD: f32 = 8.0;

#[derive(Debug)]
pub enum OverlayHit {
    None,
    Chevron(usize),
    Title(usize),
}

pub struct OutlineState {
    pub items: Vec<OutlineItem>,
    expanded: Vec<bool>,
    /// Index into `items`; always a visible row (invariant kept by all methods).
    pub selected: usize,
    /// DIP scroll offset into the row list, >= 0.
    pub scroll: f32,
    /// Index of the "Bookmarks" section-header row, when one exists.
    /// Set by App::toggle_overlay (never by new()); d2d styles that row.
    pub header_idx: Option<usize>,
}

impl OutlineState {
    pub fn new(items: Vec<OutlineItem>) -> Self {
        let expanded = items.iter().map(|it| it.depth == 0).collect();
        Self { items, expanded, selected: 0, scroll: 0.0, header_idx: None }
    }

    /// Indices into `items` of the rows currently shown, in display order.
    pub fn visible(&self) -> Vec<usize> {
        let mut out = Vec::new();
        let mut hide_deeper_than: Option<u8> = None;
        for (i, it) in self.items.iter().enumerate() {
            if let Some(d) = hide_deeper_than {
                if it.depth > d {
                    continue;
                }
                hide_deeper_than = None;
            }
            out.push(i);
            if it.has_children && !self.expanded[i] {
                hide_deeper_than = Some(it.depth);
            }
        }
        out
    }

    pub fn select(&mut self, item_idx: usize) {
        if item_idx < self.items.len() {
            self.selected = item_idx;
        }
    }

    pub fn is_expanded(&self, item_idx: usize) -> bool {
        self.expanded.get(item_idx).copied().unwrap_or(false)
    }

    pub fn move_selection(&mut self, delta: i32, panel_h: f32) {
        let vis = self.visible();
        if vis.is_empty() {
            return;
        }
        let pos = vis.iter().position(|&i| i == self.selected).unwrap_or(0) as i32;
        let new = (pos + delta).clamp(0, vis.len() as i32 - 1) as usize;
        self.selected = vis[new];
        self.ensure_visible(panel_h);
    }

    pub fn expand_selected(&mut self) {
        if self.items.get(self.selected).map_or(false, |it| it.has_children) {
            self.expanded[self.selected] = true;
        }
    }

    /// Collapse the selected row; if it has no children or is already
    /// collapsed, move selection to its parent instead.
    pub fn collapse_selected(&mut self) {
        let Some(it) = self.items.get(self.selected) else { return };
        if it.has_children && self.expanded[self.selected] {
            self.expanded[self.selected] = false;
        } else if let Some(parent) = self.parent_of(self.selected) {
            self.selected = parent;
        }
    }

    pub fn toggle(&mut self, item_idx: usize) {
        if self.items.get(item_idx).map_or(false, |it| it.has_children) {
            self.expanded[item_idx] = !self.expanded[item_idx];
            // Selection must never rest on a row that just got hidden.
            let vis = self.visible();
            if !vis.contains(&self.selected) {
                self.selected = item_idx;
            }
        }
    }

    pub fn activate(&self) -> Option<u16> {
        self.items.get(self.selected).and_then(|it| it.page)
    }

    fn parent_of(&self, item_idx: usize) -> Option<usize> {
        let depth = self.items.get(item_idx)?.depth;
        if depth == 0 {
            return None;
        }
        (0..item_idx).rev().find(|&i| self.items[i].depth == depth - 1)
    }

    /// DIP y of the top of the row at position `visible_pos` in `visible()`.
    pub fn row_top(&self, visible_pos: usize) -> f32 {
        PANEL_PAD + visible_pos as f32 * ROW_H - self.scroll
    }

    pub fn hit_test(&self, p: PointF, panel_h: f32) -> OverlayHit {
        if p.y < 0.0 || p.y > panel_h {
            return OverlayHit::None;
        }
        let vis = self.visible();
        let row_f = (p.y + self.scroll - PANEL_PAD) / ROW_H;
        if row_f < 0.0 {
            return OverlayHit::None;
        }
        let pos = row_f as usize;
        let Some(&idx) = vis.get(pos) else { return OverlayHit::None };
        let it = &self.items[idx];
        let chevron_x = it.depth as f32 * INDENT;
        if it.has_children && p.x >= chevron_x && p.x < chevron_x + CHEVRON_W {
            OverlayHit::Chevron(idx)
        } else {
            OverlayHit::Title(idx)
        }
    }

    fn max_scroll(&self, panel_h: f32) -> f32 {
        let content = 2.0 * PANEL_PAD + self.visible().len() as f32 * ROW_H;
        (content - panel_h).max(0.0)
    }

    pub fn scroll_by(&mut self, dy: f32, panel_h: f32) {
        self.scroll = (self.scroll + dy).clamp(0.0, self.max_scroll(panel_h));
    }

    /// Scroll just enough that the selected row is fully inside the panel.
    fn ensure_visible(&mut self, panel_h: f32) {
        let vis = self.visible();
        let Some(pos) = vis.iter().position(|&i| i == self.selected) else { return };
        let top = self.row_top(pos);
        if top < 0.0 {
            self.scroll += top;
        } else if top + ROW_H > panel_h {
            self.scroll += top + ROW_H - panel_h;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll(panel_h));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, page: u16, depth: u8, has_children: bool) -> OutlineItem {
        OutlineItem { title: title.into(), page: Some(page), depth, has_children }
    }

    /// A(0)+ -> A1(1)+ -> A1a(2), A2(1); B(0)   ("+" = has children)
    fn tree() -> Vec<OutlineItem> {
        vec![
            item("A", 0, 0, true),
            item("A1", 2, 1, true),
            item("A1a", 4, 2, false),
            item("A2", 6, 1, false),
            item("B", 8, 0, false),
        ]
    }

    #[test]
    fn default_visibility_top_level_expanded_deeper_collapsed() {
        let s = OutlineState::new(tree());
        assert_eq!(s.visible(), vec![0, 1, 3, 4]); // A, A1, A2, B — A1a hidden
    }

    #[test]
    fn expanding_a1_reveals_grandchild() {
        let mut s = OutlineState::new(tree());
        s.select(1);
        s.expand_selected();
        assert_eq!(s.visible(), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn collapsing_root_hides_subtree() {
        let mut s = OutlineState::new(tree());
        s.select(0);
        s.collapse_selected();
        assert_eq!(s.visible(), vec![0, 4]); // A, B
    }

    #[test]
    fn collapse_on_leaf_jumps_to_parent() {
        let mut s = OutlineState::new(tree());
        s.select(3); // A2, no children
        s.collapse_selected();
        assert_eq!(s.selected, 0); // parent A
    }

    #[test]
    fn selection_moves_over_visible_rows_and_clamps() {
        let mut s = OutlineState::new(tree());
        assert_eq!(s.selected, 0);
        s.move_selection(1, 1000.0);
        assert_eq!(s.selected, 1); // A1
        s.move_selection(1, 1000.0);
        assert_eq!(s.selected, 3); // A2 (skips hidden A1a)
        s.move_selection(10, 1000.0);
        assert_eq!(s.selected, 4); // clamped at B
        s.move_selection(-10, 1000.0);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn toggling_ancestor_never_leaves_selection_on_hidden_row() {
        let mut s = OutlineState::new(tree());
        s.select(3); // A2 (child of A) is selected
        s.toggle(0); // collapse A -> A1, A2 disappear
        assert_eq!(s.selected, 0, "selection must jump to the collapsed ancestor");
        assert!(s.visible().contains(&s.selected));
    }

    #[test]
    fn activate_returns_selected_page() {
        let mut s = OutlineState::new(tree());
        s.move_selection(1, 1000.0);
        assert_eq!(s.activate(), Some(2)); // A1 -> page 2
    }

    #[test]
    fn hit_test_resolves_chevron_vs_title_vs_none() {
        let s = OutlineState::new(tree());
        // Row 1 in visible order is A1 (depth 1). Its chevron zone starts at
        // INDENT*1 = 16 and is CHEVRON_W wide.
        let row1_y = PANEL_PAD + ROW_H * 1.0 + ROW_H / 2.0;
        match s.hit_test(PointF { x: 20.0, y: row1_y }, 1000.0) {
            OverlayHit::Chevron(i) => assert_eq!(i, 1),
            other => panic!("expected Chevron, got {other:?}"),
        }
        match s.hit_test(PointF { x: 150.0, y: row1_y }, 1000.0) {
            OverlayHit::Title(i) => assert_eq!(i, 1),
            other => panic!("expected Title, got {other:?}"),
        }
        // Leaf rows have no chevron: x inside chevron zone still yields Title.
        let row3_y = PANEL_PAD + ROW_H * 3.0 + ROW_H / 2.0; // B, depth 0
        match s.hit_test(PointF { x: 4.0, y: row3_y }, 1000.0) {
            OverlayHit::Title(i) => assert_eq!(i, 4),
            other => panic!("expected Title, got {other:?}"),
        }
        // Below the last row: None.
        assert!(matches!(
            s.hit_test(PointF { x: 50.0, y: PANEL_PAD + ROW_H * 10.0 }, 1000.0),
            OverlayHit::None
        ));
    }

    #[test]
    fn scroll_clamps_to_content() {
        let mut s = OutlineState::new(tree()); // 4 visible rows
        let panel_h = 100.0; // content = 2*PANEL_PAD + 4*28 = 128
        s.scroll_by(9999.0, panel_h);
        assert!((s.scroll - 28.0).abs() < 0.01, "scroll = {}", s.scroll);
        s.scroll_by(-9999.0, panel_h);
        assert_eq!(s.scroll, 0.0);
        // taller panel than content: no scrolling at all
        let mut s2 = OutlineState::new(tree());
        s2.scroll_by(50.0, 1000.0);
        assert_eq!(s2.scroll, 0.0);
    }

    #[test]
    fn ensure_visible_scrolls_selection_into_view() {
        let mut s = OutlineState::new(tree());
        let panel_h = 2.0 * PANEL_PAD + ROW_H * 2.0; // fits 2 rows
        s.move_selection(3, panel_h); // to B, visible pos 3
        // row_top of B must now be inside [PANEL_PAD, panel_h - PANEL_PAD - ROW_H]
        let pos = s.visible().iter().position(|&i| i == s.selected).unwrap();
        let top = s.row_top(pos);
        assert!(top >= 0.0 && top + ROW_H <= panel_h, "top = {top}");
    }

    #[test]
    fn empty_outline_is_harmless() {
        let mut s = OutlineState::new(vec![]);
        assert!(s.visible().is_empty());
        s.move_selection(1, 100.0);
        assert_eq!(s.activate(), None);
        assert!(matches!(s.hit_test(PointF { x: 5.0, y: 5.0 }, 100.0), OverlayHit::None));
    }

    #[test]
    fn pageless_header_row_activates_to_none_and_header_idx_defaults_none() {
        let mut s = OutlineState::new(vec![
            OutlineItem { title: "Bookmarks".into(), page: None, depth: 0, has_children: true },
            item("B1", 3, 1, false),
        ]);
        assert_eq!(s.header_idx, None, "callers opt in; new() never sets it");
        assert_eq!(s.activate(), None, "header row has no page");
        s.move_selection(1, 1000.0);
        assert_eq!(s.activate(), Some(3));
        // The depth-0 default-expansion rule covers the header for free.
        assert_eq!(s.visible(), vec![0, 1]);
    }
}

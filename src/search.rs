//! Find-bar state machine + bar geometry. Pure logic — all coordinates are
//! DIPs (bar geometry) or PDF points (match rects). No OS types allowed.

use crate::layout::{RectF, SizeU};
use crate::types::{SearchMatch, SearchOptions};

/// Natural bar width / height / margin from the top-right corner, in DIPs.
pub const BAR_W: f32 = 360.0;
pub const BAR_H: f32 = 40.0;
pub const BAR_MARGIN: f32 = 12.0;
/// Hard cap on the query length (chars) — bounds pasted text.
pub const QUERY_CAP: usize = 256;
/// Queries shorter than this are treated exactly like an empty one.
pub const MIN_QUERY: usize = 2;

/// Where the bar sits, in window PHYSICAL px (top-right, width clamped so
/// the 200-DIP minimum window still fits it). Drawing (d2d) and the
/// wants_client_mouse hit-test (window.rs) must both use THIS rect — the
/// clamped drawn rect IS the hit-test rect.
pub fn bar_rect(client: SizeU, scale: f32) -> RectF {
    let m = BAR_MARGIN * scale;
    let w = (BAR_W * scale).min((client.w as f32 - 2.0 * m).max(40.0));
    let right = client.w as f32 - m;
    RectF { left: right - w, top: m, right, bottom: m + BAR_H * scale }
}

/// One page's hits, in document order within the page.
pub struct PageMatches {
    pub page: u16,
    pub matches: Vec<SearchMatch>,
}

/// What a query mutation asks the caller (window.rs) to do.
#[derive(Debug, PartialEq, Eq)]
pub enum QueryEffect {
    /// Query is legal: issue a fresh `PdfCmd::Search` (via `begin_scan`).
    Rescan,
    /// Query became illegal (< MIN_QUERY): results were cleared locally;
    /// send `PdfCmd::CancelSearch`.
    Cancel,
    Nothing,
}

/// What a next/prev step asks the caller to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Navigate to this (page, index-within-page) match.
    Jump(u16, usize),
    /// `pending_jump` was armed — the jump fires when a suitable hit
    /// arrives (or SearchDone wraps).
    Armed,
    Nothing,
}

pub struct SearchState {
    pub query: String,
    pub options: SearchOptions,
    /// Id of the scan whose replies we accept. Bumped via `begin_scan`.
    pub id: u32,
    /// Sorted by page; one entry per page with hits.
    pub matches: Vec<PageMatches>,
    /// (page, index within that page's matches). Auto-selected to the first
    /// hit received for the current scan; auto-selection never navigates.
    pub current: Option<(u16, usize)>,
    pub scanning: bool,
    /// A jump has landed during this scan (first Enter reveals `current`;
    /// only afterwards does Enter advance).
    pub landed: bool,
    /// Pseudo-select-all: the next typed/pasted char REPLACES the whole
    /// query; Backspace clears it. Armed by opens that restore a query and
    /// by Ctrl+F while the bar is open.
    pub replace_on_type: bool,
    /// A jump is waiting for a hit to arrive (Enter before the first hit,
    /// or stepping forward past the streaming frontier).
    pub pending_jump: bool,
}

impl SearchState {
    /// Open the bar, restoring the remembered query/options if any. A
    /// restored non-empty query arms replace-on-type (typing replaces it,
    /// browser-style).
    pub fn new(restore: Option<(String, SearchOptions)>) -> SearchState {
        let (query, options) = restore.unwrap_or_default();
        SearchState {
            replace_on_type: !query.is_empty(),
            query,
            options,
            id: 0,
            matches: Vec::new(),
            current: None,
            scanning: false,
            landed: false,
            pending_jump: false,
        }
    }

    pub fn legal(&self) -> bool {
        self.query.chars().count() >= MIN_QUERY
    }

    /// The atomic new-scan reset: results from the previous query must never
    /// survive into (or merge with) the new scan — the reply id-check alone
    /// only stops in-flight stragglers.
    pub fn begin_scan(&mut self, id: u32) {
        self.id = id;
        self.matches.clear();
        self.current = None;
        self.landed = false;
        self.pending_jump = false;
        self.scanning = true;
    }

    /// Local reset for an illegal/cleared query (no scan running afterwards).
    pub fn clear_results(&mut self) {
        self.matches.clear();
        self.current = None;
        self.landed = false;
        self.pending_jump = false;
        self.scanning = false;
    }

    fn edited(&mut self) -> QueryEffect {
        if self.legal() {
            QueryEffect::Rescan
        } else {
            self.clear_results();
            QueryEffect::Cancel
        }
    }

    /// Append a typed char (control chars are the caller's problem — window.rs
    /// filters them). While replace-on-type is armed, the char REPLACES the
    /// whole query.
    pub fn push_char(&mut self, c: char) -> QueryEffect {
        if std::mem::take(&mut self.replace_on_type) {
            self.query.clear();
        }
        if self.query.chars().count() >= QUERY_CAP {
            return QueryEffect::Nothing;
        }
        self.query.push(c);
        self.edited()
    }

    /// While replace-on-type is armed, Backspace clears the whole query.
    pub fn backspace(&mut self) -> QueryEffect {
        if std::mem::take(&mut self.replace_on_type) {
            self.query.clear();
            return self.edited();
        }
        if self.query.pop().is_none() {
            return QueryEffect::Nothing;
        }
        self.edited()
    }

    /// Ctrl+V. Control chars (incl. newlines) are stripped; the cap applies.
    /// An armed replace that ends up appending nothing (control-only
    /// clipboard) still cleared the query and MUST route through edited()
    /// so the stale results are cleared and a CancelSearch goes out.
    pub fn paste(&mut self, text: &str) -> QueryEffect {
        let replaced = std::mem::take(&mut self.replace_on_type);
        if replaced {
            self.query.clear();
        }
        let mut appended = false;
        for c in text.chars().filter(|c| !c.is_control()) {
            if self.query.chars().count() >= QUERY_CAP {
                break;
            }
            self.query.push(c);
            appended = true;
        }
        if appended || replaced {
            self.edited()
        } else {
            QueryEffect::Nothing
        }
    }

    pub fn toggle_case(&mut self) -> QueryEffect {
        self.options.match_case = !self.options.match_case;
        if self.legal() {
            QueryEffect::Rescan
        } else {
            QueryEffect::Nothing
        }
    }

    pub fn toggle_word(&mut self) -> QueryEffect {
        self.options.whole_word = !self.options.whole_word;
        if self.legal() {
            QueryEffect::Rescan
        } else {
            QueryEffect::Nothing
        }
    }

    /// Flattened document-order position of `current` (0-based).
    fn flat_pos(&self, target: (u16, usize)) -> Option<usize> {
        let mut n = 0;
        for pm in &self.matches {
            if pm.page == target.0 {
                if target.1 < pm.matches.len() {
                    return Some(n + target.1);
                }
                return None;
            }
            n += pm.matches.len();
        }
        None
    }

    fn flat_get(&self, mut pos: usize) -> Option<(u16, usize)> {
        for pm in &self.matches {
            if pos < pm.matches.len() {
                return Some((pm.page, pos));
            }
            pos -= pm.matches.len();
        }
        None
    }

    pub fn total(&self) -> usize {
        self.matches.iter().map(|pm| pm.matches.len()).sum()
    }

    /// 1-based ordinal of the current match, for the "3 / 17" counter.
    pub fn ordinal(&self) -> Option<usize> {
        self.current.and_then(|c| self.flat_pos(c)).map(|p| p + 1)
    }

    pub fn matches_on(&self, page: u16) -> Option<&PageMatches> {
        self.matches.iter().find(|pm| pm.page == page)
    }

    /// Union bounding box of one match's rects (PDF points) — the
    /// navigation target; individual rects are for painting.
    pub fn union_bbox(&self, page: u16, idx: usize) -> Option<RectF> {
        let m = self.matches_on(page)?.matches.get(idx)?;
        let mut it = m.rects.iter();
        let first = *it.next()?;
        Some(it.fold(first, |a, r| RectF {
            left: a.left.min(r.left),
            // PDF points: top > bottom numerically.
            top: a.top.max(r.top),
            right: a.right.max(r.right),
            bottom: a.bottom.min(r.bottom),
        }))
    }

    /// A page's hits arrived. Inserts in page-sorted order, auto-selects
    /// `current` on the first hit of the scan, and fires `pending_jump` when
    /// a suitable target exists. Returns the (page, idx) to NAVIGATE to when
    /// the pending jump fires, else None. Stale ids are dropped.
    pub fn hits(&mut self, id: u32, page: u16, matches: Vec<SearchMatch>) -> Option<(u16, usize)> {
        if id != self.id || matches.is_empty() {
            return None;
        }
        let pos = self.matches.partition_point(|pm| pm.page < page);
        // One SearchHits per page per scan — same-page duplicates would be a
        // worker bug; replace defensively rather than duplicating.
        if self.matches.get(pos).is_some_and(|pm| pm.page == page) {
            self.matches[pos] = PageMatches { page, matches };
        } else {
            self.matches.insert(pos, PageMatches { page, matches });
        }
        if self.current.is_none() {
            // First hit received for this scan (wrap scan order makes this
            // the nearest hit at-or-after start_page).
            self.current = Some((page, 0));
        }
        if self.pending_jump {
            if !self.landed {
                // Enter/F3 fired before any hit existed: land on the first.
                self.pending_jump = false;
                self.landed = true;
                return self.current;
            }
            // Frontier case: fire on the first match strictly PAST current
            // in document order, if this batch supplied one.
            if let Some(cur_pos) = self.current.and_then(|c| self.flat_pos(c)) {
                if let Some(next) = self.flat_get(cur_pos + 1) {
                    self.pending_jump = false;
                    self.current = Some(next);
                    return self.current;
                }
            }
        }
        None
    }

    /// The scan covered every page. Clears `scanning`; a still-armed pending
    /// jump wraps to the first match (or just clears when there are none).
    pub fn done(&mut self, id: u32) -> Option<(u16, usize)> {
        if id != self.id {
            return None;
        }
        self.scanning = false;
        if std::mem::take(&mut self.pending_jump) && self.total() > 0 {
            self.current = self.flat_get(0);
            self.landed = true;
            return self.current;
        }
        None
    }

    /// Enter / Shift+Enter / F3 / Shift+F3.
    pub fn step(&mut self, backward: bool) -> Step {
        if !self.legal() {
            return Step::Nothing;
        }
        if self.total() == 0 {
            if self.scanning {
                // Enter before the first hit arrived: land when it does.
                self.pending_jump = true;
                return Step::Armed;
            }
            return Step::Nothing;
        }
        let cur = match self.current {
            Some(c) => c,
            None => return Step::Nothing, // unreachable: hits() auto-selects
        };
        if !self.landed {
            // First activation reveals the current match instead of skipping
            // past it — and makes the Enter-vs-first-hit race unobservable.
            self.landed = true;
            return Step::Jump(cur.0, cur.1);
        }
        let pos = match self.flat_pos(cur) {
            Some(p) => p,
            None => return Step::Nothing,
        };
        let n = self.total();
        if backward {
            // Prev past the first wraps to the last RECEIVED match
            // (mid-scan: the streaming frontier — accepted).
            let target = if pos == 0 { n - 1 } else { pos - 1 };
            self.current = self.flat_get(target);
        } else if pos + 1 < n {
            self.current = self.flat_get(pos + 1);
        } else if self.scanning {
            // Streaming frontier: arm; fires on the next hit past current,
            // or wraps at SearchDone.
            self.pending_jump = true;
            return Step::Armed;
        } else {
            self.current = self.flat_get(0); // wrap
        }
        match self.current {
            Some((p, i)) => Step::Jump(p, i),
            None => Step::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(n: usize) -> Vec<SearchMatch> {
        (0..n)
            .map(|i| SearchMatch {
                rects: vec![RectF {
                    left: 10.0 * i as f32,
                    top: 700.0,
                    right: 10.0 * i as f32 + 8.0,
                    bottom: 690.0,
                }],
            })
            .collect()
    }

    fn scanning_state(query: &str) -> SearchState {
        let mut st = SearchState::new(None);
        for c in query.chars() {
            st.push_char(c);
        }
        st.begin_scan(1);
        st
    }

    // --- query editing / legality ---

    #[test]
    fn two_char_legality_threshold() {
        let mut st = SearchState::new(None);
        assert!(!st.legal());
        assert_eq!(st.push_char('d'), QueryEffect::Cancel, "1 char is illegal");
        assert_eq!(st.push_char('r'), QueryEffect::Rescan, "2 chars are legal");
        assert_eq!(st.backspace(), QueryEffect::Cancel, "back to 1 char cancels");
    }

    #[test]
    fn query_cap_bounds_typing_and_paste() {
        let mut st = SearchState::new(None);
        for _ in 0..QUERY_CAP + 10 {
            st.push_char('x');
        }
        assert_eq!(st.query.chars().count(), QUERY_CAP);
        let mut st2 = SearchState::new(None);
        let long: String = "y".repeat(QUERY_CAP + 50);
        st2.paste(&long);
        assert_eq!(st2.query.chars().count(), QUERY_CAP);
    }

    #[test]
    fn paste_strips_control_chars() {
        let mut st = SearchState::new(None);
        assert_eq!(st.paste("dra\r\ngon"), QueryEffect::Rescan);
        assert_eq!(st.query, "dragon");
    }

    #[test]
    fn armed_paste_of_only_control_chars_clears_and_cancels() {
        // Regression: an armed replace clears the query even when the
        // clipboard held nothing printable — the old query's results must
        // not survive under an empty "Find: _" bar.
        let mut st = SearchState::new(Some(("dragon".into(), SearchOptions::default())));
        st.begin_scan(1);
        st.hits(1, 0, m(2));
        assert_eq!(st.paste("\r\n"), QueryEffect::Cancel);
        assert_eq!(st.query, "");
        assert_eq!(st.total(), 0, "stale highlights must be gone");
        assert!(!st.scanning);
    }

    #[test]
    fn replace_on_type_first_char_replaces_backspace_clears() {
        let mut st = SearchState::new(Some(("dragon".into(), SearchOptions::default())));
        assert!(st.replace_on_type, "restored query arms replace");
        st.push_char('w');
        assert_eq!(st.query, "w", "first char replaced the whole query");
        assert!(!st.replace_on_type, "disarmed after use");

        let mut st = SearchState::new(Some(("dragon".into(), SearchOptions::default())));
        assert_eq!(st.backspace(), QueryEffect::Cancel);
        assert_eq!(st.query, "", "armed Backspace clears everything");

        let mut st = SearchState::new(Some(("dragon".into(), SearchOptions::default())));
        st.paste("wizard king");
        assert_eq!(st.query, "wizard king", "armed paste replaces");
    }

    #[test]
    fn open_without_restore_is_empty_and_unarmed() {
        let st = SearchState::new(None);
        assert_eq!(st.query, "");
        assert!(!st.replace_on_type);
        assert!(!st.scanning && !st.pending_jump && !st.landed);
    }

    // --- new-scan reset (the blocker invariant) ---

    #[test]
    fn begin_scan_clears_prior_results_before_first_hit() {
        let mut st = scanning_state("dragon");
        st.hits(1, 3, m(2));
        st.step(false); // landed on (3,0)
        assert_eq!(st.total(), 2);
        st.push_char('s'); // "dragons" -> Rescan expected by caller
        st.begin_scan(2);
        assert_eq!(st.total(), 0, "old query's matches must not survive");
        assert_eq!(st.current, None);
        assert!(!st.landed && !st.pending_jump);
        assert!(st.scanning);
        // Stale id-1 stragglers are dropped.
        assert_eq!(st.hits(1, 5, m(3)), None);
        assert_eq!(st.total(), 0);
        assert_eq!(st.done(1), None);
        assert!(st.scanning, "stale done must not clear scanning");
    }

    // --- streaming insertion / counter ---

    #[test]
    fn hits_insert_sorted_and_ordinal_counts_across_pages() {
        let mut st = scanning_state("dragon");
        // Wrap order: start_page 5 -> pages 5, 9, then wrapped 1.
        st.hits(1, 5, m(2));
        st.hits(1, 9, m(1));
        st.hits(1, 1, m(3));
        let pages: Vec<u16> = st.matches.iter().map(|pm| pm.page).collect();
        assert_eq!(pages, vec![1, 5, 9], "sorted by page regardless of arrival");
        assert_eq!(st.total(), 6);
        assert_eq!(st.current, Some((5, 0)), "first hit RECEIVED, not first in doc order");
        assert_eq!(st.ordinal(), Some(4), "3 matches on page 1 precede it");
    }

    // --- first activation / stepping / wrap ---

    #[test]
    fn first_step_reveals_current_then_advances() {
        let mut st = scanning_state("dragon");
        st.hits(1, 5, m(2));
        assert_eq!(st.step(false), Step::Jump(5, 0), "first Enter reveals, not skips");
        assert_eq!(st.step(false), Step::Jump(5, 1), "second Enter advances");
    }

    #[test]
    fn step_wraps_when_done() {
        let mut st = scanning_state("dragon");
        st.hits(1, 2, m(1));
        st.hits(1, 4, m(1));
        st.done(1);
        st.step(false); // reveal (2,0)
        st.step(false); // (4,0)
        assert_eq!(st.step(false), Step::Jump(2, 0), "wraps to first");
        assert_eq!(st.step(true), Step::Jump(4, 0), "prev wraps to last");
    }

    #[test]
    fn forward_step_at_frontier_arms_and_fires_on_next_hit() {
        let mut st = scanning_state("dragon");
        st.hits(1, 2, m(1));
        st.step(false); // reveal (2,0) — now at the last received match
        assert_eq!(st.step(false), Step::Armed, "frontier: arm instead of wrapping");
        assert!(st.pending_jump);
        // A hit past current fires the pending jump.
        assert_eq!(st.hits(1, 6, m(2)), Some((6, 0)));
        assert!(!st.pending_jump);
        assert_eq!(st.current, Some((6, 0)));
    }

    #[test]
    fn frontier_pending_jump_wraps_at_done() {
        let mut st = scanning_state("dragon");
        st.hits(1, 4, m(1));
        st.step(false); // reveal
        assert_eq!(st.step(false), Step::Armed);
        // Wrapped-region hits (page < current) must NOT fire a forward jump.
        assert_eq!(st.hits(1, 1, m(2)), None);
        assert!(st.pending_jump, "hit before current in doc order keeps it armed");
        assert_eq!(st.done(1), Some((1, 0)), "SearchDone wraps to the first match");
        assert!(!st.scanning && !st.pending_jump);
    }

    #[test]
    fn enter_before_first_hit_arms_and_lands_on_it() {
        let mut st = scanning_state("dragon");
        assert_eq!(st.step(false), Step::Armed, "no hits yet: arm");
        assert_eq!(st.hits(1, 7, m(1)), Some((7, 0)), "first hit lands the jump");
        assert!(st.landed);
        assert_eq!(st.step(false), Step::Armed, "at frontier again");
    }

    #[test]
    fn pending_jump_never_survives_an_edit() {
        let mut st = scanning_state("dragon");
        st.step(false); // Armed (no hits yet)
        assert!(st.pending_jump);
        st.push_char('s');
        st.begin_scan(2); // what window.rs does on Rescan
        assert!(!st.pending_jump, "typing never moves the view");
        assert_eq!(st.hits(2, 3, m(1)), None, "new scan's first hit must not auto-jump");
    }

    #[test]
    fn zero_hit_scan_then_retype_does_not_autojump() {
        let mut st = scanning_state("qqqq");
        st.step(false); // Armed
        assert_eq!(st.done(1), None, "no hits: pending clears without a jump");
        assert!(!st.pending_jump);
        st.push_char('x');
        st.begin_scan(2);
        assert_eq!(st.hits(2, 0, m(1)), None, "fresh scan: no inherited jump");
    }

    #[test]
    fn step_on_empty_done_scan_is_nothing() {
        let mut st = scanning_state("zz");
        st.done(1);
        assert_eq!(st.step(false), Step::Nothing);
        assert_eq!(st.step(true), Step::Nothing);
        let mut illegal = SearchState::new(None);
        illegal.push_char('z');
        assert_eq!(illegal.step(false), Step::Nothing, "illegal query never steps");
    }

    // --- toggles / done / bbox / geometry ---

    #[test]
    fn toggles_flip_options_and_request_rescan_when_legal() {
        let mut st = scanning_state("dragon");
        assert_eq!(st.toggle_case(), QueryEffect::Rescan);
        assert!(st.options.match_case);
        assert_eq!(st.toggle_word(), QueryEffect::Rescan);
        assert!(st.options.whole_word);
        let mut short = SearchState::new(None);
        short.push_char('a');
        assert_eq!(short.toggle_case(), QueryEffect::Nothing);
    }

    #[test]
    fn done_clears_scanning_for_matching_id_only() {
        let mut st = scanning_state("dragon");
        assert!(st.scanning);
        assert_eq!(st.done(99), None);
        assert!(st.scanning);
        st.done(1);
        assert!(!st.scanning);
    }

    #[test]
    fn union_bbox_covers_multi_rect_matches() {
        let mut st = scanning_state("dragon");
        st.hits(
            1,
            2,
            vec![SearchMatch {
                rects: vec![
                    RectF { left: 400.0, top: 700.0, right: 540.0, bottom: 688.0 },
                    RectF { left: 72.0, top: 684.0, right: 130.0, bottom: 672.0 },
                ],
            }],
        );
        let b = st.union_bbox(2, 0).unwrap();
        assert_eq!((b.left, b.top, b.right, b.bottom), (72.0, 700.0, 540.0, 672.0));
        assert_eq!(st.union_bbox(2, 5), None);
        assert_eq!(st.union_bbox(9, 0), None);
    }

    #[test]
    fn bar_rect_clamps_to_tiny_windows() {
        let s = 1.0;
        let r = bar_rect(SizeU { w: 1200, h: 800 }, s);
        assert_eq!(r.right, 1200.0 - BAR_MARGIN);
        assert_eq!(r.width(), BAR_W);
        // 200-px window: the bar must fit inside with both margins.
        let tiny = bar_rect(SizeU { w: 200, h: 200 }, s);
        assert!(tiny.left >= BAR_MARGIN - 1e-3, "left: {}", tiny.left);
        assert!(tiny.width() > 0.0);
        // Hit-test agreement is by construction: this IS the drawn rect.
    }
}

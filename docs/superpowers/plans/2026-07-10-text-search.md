# Full-Text Search Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Browser-style Ctrl+F find bar: live whole-document streaming search on the PDF worker, match highlighting over the page, Enter/F3 next-prev navigation that keeps zoom and scrolls matches into view.

**Architecture:** A new `PdfCmd::Search` streams per-page `SearchHits` replies from the worker (one page scanned per loop pass, preempt-polled like renders); the UI accumulates them into a pure `SearchState` on `DocState` (new module `src/search.rs`). Highlights reuse the field-indication rect pipeline (un-gated into shared `App::page_rects_window`); navigation goes through a new `App::goto_match` that shares `goto`'s history recording but preserves zoom via a new `layout::scroll_to_reveal` primitive.

**Tech Stack:** Rust, windows-rs (Win32 + Direct2D/DirectWrite), vendored pdfium-render 0.9.2 (`PdfPageText::search()` — safe API, zero vendor patches).

**Authoritative spec:** `docs/superpowers/specs/2026-07-10-text-search-design.md`. If a detail here seems ambiguous, the spec wins.

## Global Constraints

- Both editions must stay green after EVERY task: `cargo build` and `cargo test`, each with and without `--features pro`.
- Search is a **both-editions** feature: no code in `src/pro/`, no new `cfg(feature = "pro")` except the two places this plan explicitly marks (the Ctrl+F arm inside the already-gated form rung; the Pro scan-loop timer duties in pdf.rs).
- `app.rs`, `layout.rs`, `search.rs` are pure: no OS types, no Win32 imports.
- `vendor/` must not be edited at all.
- Rect convention everywhere: PDF points, unrotated page frame, origin bottom-left (`top > bottom` numerically).
- Every worker reply must be sent with a following `wake()` call.
- Nothing repaints on its own: every UI state mutation ends with `self.invalidate()` (and `sync_renders()` when page pixels may change).
- Minimum legal query length is 2 chars; query capped at 256 chars.
- Commit after every task with a conventional-commit message.

---

### Task 1: Protocol types (types.rs)

**Files:**
- Modify: `src/types.rs`

**Interfaces:**
- Consumes: `crate::layout::RectF` (already imported in types.rs).
- Produces (later tasks rely on these exact shapes):
  - `pub struct SearchOptions { pub match_case: bool, pub whole_word: bool }` (`Clone, Copy, Debug, PartialEq, Eq, Default`)
  - `pub struct SearchMatch { pub rects: Vec<RectF> }` (`Clone, Debug, PartialEq`)
  - `PdfCmd::Search { id: u32, query: String, options: SearchOptions, start_page: u16 }`
  - `PdfCmd::CancelSearch`
  - `PdfReply::SearchHits { id: u32, page: u16, matches: Vec<SearchMatch> }`
  - `PdfReply::SearchDone { id: u32 }`

- [ ] **Step 1: Add the types**

In `src/types.rs`, insert after the `FormField` struct (before the `FormEvent` enum):

```rust
/// Search options forwarded to PDFium's native matcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SearchOptions {
    pub match_case: bool,
    pub whole_word: bool,
}

/// One search hit. `rects` uses the same convention as `PageLink::rect_pt`
/// (PDF points, origin bottom-left, top > bottom) — a Vec because one hit
/// can span line wraps or font changes (PDFium merges boxes only within a
/// line/style run).
#[derive(Clone, Debug, PartialEq)]
pub struct SearchMatch {
    pub rects: Vec<RectF>,
}
```

Add to `PdfCmd` (after the `Render` variant, before the Pro-gated `Form`):

```rust
    /// Start (or replace) the streaming text scan. `start_page` = the page
    /// the scan begins at; it wraps around the document end. Both editions.
    Search { id: u32, query: String, options: SearchOptions, start_page: u16 },
    /// Kill the active scan and any `Search` queued before this command.
    CancelSearch,
```

Add to `PdfReply` (after `RenderFailed`, before the Pro-gated `FormDirty`):

```rust
    /// One scanned page's hits (sent only for pages WITH hits). Stale `id`s
    /// are dropped by the UI.
    SearchHits { id: u32, page: u16, matches: Vec<SearchMatch> },
    /// The scan with this `id` covered every page.
    SearchDone { id: u32 },
```

- [ ] **Step 2: Stub the two exhaustive matches so both editions keep compiling**

Two existing `match`es are exhaustive over these enums and would fail with
E0004 the moment the variants exist. Add **named placeholder arms** (never
`_` catch-alls — future variants must keep exhaustiveness protection):

1. `src/pdf.rs`, in the coalescer's `match cmd` (inside `run()`), after the
`PdfCmd::Render { .., visible: false }` arm:

```rust
                // Placeholder: replaced wholesale by the scan-engine task.
                PdfCmd::Search { .. } | PdfCmd::CancelSearch => {}
```

2. `src/window.rs`, in `pump_pdf_replies`' `match reply`, after the
`PdfReply::RenderFailed { .. }` arm:

```rust
                // Placeholder: replaced wholesale by the window-wiring task.
                PdfReply::SearchHits { .. } | PdfReply::SearchDone { .. } => {}
```

- [ ] **Step 3: Verify both editions compile**

Run: `cargo build` then `cargo build --features pro`
Expected: both succeed with no new warnings.

- [ ] **Step 4: Commit**

```bash
git add src/types.rs src/pdf.rs src/window.rs
git commit -m "feat(search): add Search/CancelSearch commands and SearchHits/SearchDone replies"
```

---

### Task 2: `layout::scroll_to_reveal`

**Files:**
- Modify: `src/layout.rs` (function + unit tests in the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `clamp_scroll`, `PointF`, `RectF`, `SizeU` (same file).
- Produces: `pub fn scroll_to_reveal(scroll: PointF, target: RectF, page_px: SizeU, client: SizeU, margin: f32) -> PointF`
  - `target` is in **window space as drawn under the input `scroll`**.
  - Returns the new scroll, passed through `clamp_scroll(page_px, client, ·)`.
  - No-op (returns `scroll` clamped) when the target already lies fully inside the viewport inset by `margin`.
  - A target larger than the viewport aligns its left/top edge to `margin`.

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests` in `src/layout.rs`:

```rust
    // --- scroll_to_reveal: minimal scroll bringing a rect into view ---

    #[test]
    fn reveal_noop_when_already_visible() {
        let client = sz(1000, 1000);
        let page = sz(3000, 3000);
        let scroll = pt(500.0, 500.0);
        let target = RectF { left: 200.0, top: 200.0, right: 400.0, bottom: 300.0 };
        assert_eq!(scroll_to_reveal(scroll, target, page, client, 48.0), scroll);
    }

    #[test]
    fn reveal_scrolls_right_and_down_minimally() {
        let client = sz(1000, 1000);
        let page = sz(3000, 3000);
        let scroll = pt(0.0, 0.0);
        // Off the right/bottom edges: right = 1200 > 1000 - 48, bottom = 1100.
        let target = RectF { left: 1100.0, top: 1050.0, right: 1200.0, bottom: 1100.0 };
        let s = scroll_to_reveal(scroll, target, page, client, 48.0);
        // Minimal: bring right edge to client.w - margin, bottom to client.h - margin.
        assert!((s.x - (1200.0 - (1000.0 - 48.0))).abs() < 1e-3, "x: {}", s.x);
        assert!((s.y - (1100.0 - (1000.0 - 48.0))).abs() < 1e-3, "y: {}", s.y);
    }

    #[test]
    fn reveal_scrolls_left_and_up_minimally() {
        let client = sz(1000, 1000);
        let page = sz(3000, 3000);
        let scroll = pt(800.0, 800.0);
        // Off the left/top edges (negative window coords).
        let target = RectF { left: -100.0, top: -50.0, right: 20.0, bottom: 0.0 };
        let s = scroll_to_reveal(scroll, target, page, client, 48.0);
        // left must land at margin: new.x = 800 - (48 - (-100)) = 652.
        assert!((s.x - 652.0).abs() < 1e-3, "x: {}", s.x);
        assert!((s.y - 702.0).abs() < 1e-3, "y: {}", s.y);
    }

    #[test]
    fn reveal_result_is_clamped() {
        let client = sz(1000, 1000);
        let page = sz(1200, 1200); // max scroll 200
        let scroll = pt(0.0, 0.0);
        let target = RectF { left: 5000.0, top: 5000.0, right: 5100.0, bottom: 5050.0 };
        let s = scroll_to_reveal(scroll, target, page, client, 48.0);
        assert_eq!(s, pt(200.0, 200.0), "must clamp to the overhang");
    }

    #[test]
    fn reveal_oversized_rect_aligns_start_edge() {
        let client = sz(1000, 1000);
        let page = sz(5000, 5000);
        let scroll = pt(0.0, 0.0);
        // Wider AND taller than the viewport: align left/top to margin.
        let target = RectF { left: 2000.0, top: 2000.0, right: 3500.0, bottom: 3500.0 };
        let s = scroll_to_reveal(scroll, target, page, client, 48.0);
        assert!((s.x - (2000.0 - 48.0)).abs() < 1e-3, "x: {}", s.x);
        assert!((s.y - (2000.0 - 48.0)).abs() < 1e-3, "y: {}", s.y);
    }

    #[test]
    fn reveal_snaps_to_zero_on_fitting_axis() {
        let client = sz(1000, 1000);
        let page = sz(800, 3000); // x fits: clamp snaps x to 0
        let s = scroll_to_reveal(
            pt(0.0, 0.0),
            RectF { left: 100.0, top: 2000.0, right: 200.0, bottom: 2100.0 },
            page,
            client,
            48.0,
        );
        assert_eq!(s.x, 0.0);
        assert!(s.y > 0.0);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib layout::tests::reveal`
Expected: compile error — `scroll_to_reveal` not found.

- [ ] **Step 3: Implement**

Insert after `zoom_scroll` in `src/layout.rs`:

```rust
/// Minimal scroll change that brings `target` — a rect in WINDOW space as
/// drawn under the input `scroll` — fully into the viewport with a comfort
/// `margin` on every side. No-op when it is already fully visible. A rect
/// larger than the viewport aligns its left/top edge to the margin (the
/// start of the content is what the user is looking for). The result passes
/// through `clamp_scroll`, so axes where the page fits snap to 0.
pub fn scroll_to_reveal(
    scroll: PointF,
    target: RectF,
    page_px: SizeU,
    client: SizeU,
    margin: f32,
) -> PointF {
    // Increasing scroll.x moves the page (and the target rect) LEFT in the
    // window, so a rect off the right edge needs scroll += overshoot.
    let reveal_axis = |scroll: f32, lo: f32, hi: f32, extent: f32| -> f32 {
        let (min, max) = (margin, extent - margin);
        if hi - lo > max - min {
            // Oversized: align the start edge to the margin.
            scroll + (lo - min)
        } else if lo < min {
            scroll - (min - lo)
        } else if hi > max {
            scroll + (hi - max)
        } else {
            scroll
        }
    };
    let want = PointF {
        x: reveal_axis(scroll.x, target.left, target.right, client.w as f32),
        y: reveal_axis(scroll.y, target.top, target.bottom, client.h as f32),
    };
    clamp_scroll(page_px, client, want)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib layout::tests::reveal`
Expected: 6 passed. Also run the full lib suite once: `cargo test --lib` — all pass.

- [ ] **Step 5: Commit**

```bash
git add src/layout.rs
git commit -m "feat(search): layout::scroll_to_reveal - minimal scroll to bring a rect into view"
```

### Task 3: `src/search.rs` — the pure search state machine

**Files:**
- Create: `src/search.rs`
- Modify: `src/lib.rs` (add `pub mod search;` in alphabetical order, between `pub mod register;` and `pub mod types;` — keep the existing sorted style: app, d2d, dbglog, edition, layout, overlay, pdf, pro, register, **search**, types, window)

**Interfaces:**
- Consumes: `crate::layout::{PointF, RectF, SizeU}`, `crate::types::{SearchOptions, SearchMatch}`.
- Produces (window.rs/d2d.rs/app.rs rely on these exact names):
  - constants `BAR_W: f32 = 360.0`, `BAR_H: f32 = 40.0`, `BAR_MARGIN: f32 = 12.0` (DIPs), `QUERY_CAP: usize = 256`, `MIN_QUERY: usize = 2`
  - `pub fn bar_rect(client: SizeU, scale: f32) -> RectF` (window px, top-right, width clamped to the client)
  - `pub struct PageMatches { pub page: u16, pub matches: Vec<SearchMatch> }`
  - `pub struct SearchState { pub query: String, pub options: SearchOptions, pub id: u32, pub matches: Vec<PageMatches>, pub current: Option<(u16, usize)>, pub scanning: bool, pub landed: bool, pub replace_on_type: bool, pub pending_jump: bool }`
  - `pub enum QueryEffect { Rescan, Cancel, Nothing }`
  - `pub enum Step { Jump(u16, usize), Armed, Nothing }`
  - methods: `new(restore) -> Self`, `legal(&self) -> bool`, `push_char(&mut, char) -> QueryEffect`, `backspace(&mut) -> QueryEffect`, `paste(&mut, &str) -> QueryEffect`, `toggle_case(&mut) -> QueryEffect`, `toggle_word(&mut) -> QueryEffect`, `begin_scan(&mut, id: u32)`, `clear_results(&mut)`, `hits(&mut, id: u32, page: u16, matches: Vec<SearchMatch>) -> Option<(u16, usize)>`, `done(&mut, id: u32) -> Option<(u16, usize)>`, `step(&mut, backward: bool) -> Step`, `total(&self) -> usize`, `ordinal(&self) -> Option<usize>`, `union_bbox(&self, page: u16, idx: usize) -> Option<RectF>`, `matches_on(&self, page: u16) -> Option<&PageMatches>`

- [ ] **Step 1: Write the module skeleton with the state machine**

Create `src/search.rs`:

```rust
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
}
```

Note: in `new()` the `replace_on_type: !query.is_empty()` initializer must
appear BEFORE the `query` field moves the String (borrow-then-move order, as
written above).

- [ ] **Step 2: Add results/stepping logic**

Append to the `impl SearchState` block:

```rust
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
```

- [ ] **Step 3: Register the module**

In `src/lib.rs` add `pub mod search;` between `register` and `types`. Run `cargo build` — must compile clean.

- [ ] **Step 4: Write the unit tests**

Append to `src/search.rs`:

```rust
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
```

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib search::`
Expected: every test in `search::tests` passes (do not anchor on a specific
count). Then `cargo test --lib` and `cargo test --lib --features pro` — all
green.

- [ ] **Step 6: Commit**

```bash
git add src/search.rs src/lib.rs
git commit -m "feat(search): pure SearchState machine + bar geometry in src/search.rs"
```

### Task 4: app.rs integration — state fields, shared rect pipeline, `goto_match`

**Files:**
- Modify: `src/app.rs`

**Interfaces:**
- Consumes: `crate::search::SearchState`, `crate::types::SearchOptions`, `crate::layout::{scroll_to_reveal, rotate_rect, max_zoom}` (extend the existing `use crate::layout::{...}` import list with `rotate_rect` and `scroll_to_reveal`; `max_zoom` and `pdf_rect_to_window` are already imported).
- Produces (window.rs/d2d.rs rely on these exact signatures):
  - `DocState.search: Option<crate::search::SearchState>` (pub), `DocState.last_search: Option<(String, SearchOptions)>` (pub)
  - `App::next_search_id(&mut self) -> u32` (starts at 1, increments)
  - `App::page_rects_window(&self, rects_pt: &[RectF]) -> Vec<RectF>` — **pure geometry, NO cache-presence gate**
  - `App::goto_match(&mut self, page: u16, bbox_pt: RectF) -> bool` — returns true iff the page changed
  - `App::search_open(&mut self) -> bool` (true = bar newly opened or re-armed; false = no doc)
  - `App::search_close(&mut self) -> bool` (true = a bar was open and is now closed+stashed)
  - `App::search_active(&self) -> bool`

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests` in `src/app.rs` (NOT inside `pro_tests`):

```rust
    // --- text search: goto_match / page_rects_window / open-close ---

    #[test]
    fn goto_match_records_history_iff_page_changes() {
        let mut a = app_with_doc(10);
        let bbox = RectF { left: 100.0, top: 700.0, right: 200.0, bottom: 688.0 };
        assert!(a.goto_match(5, bbox), "cross-page jump");
        assert_eq!(a.doc().unwrap().current, 5);
        assert!(!a.goto_match(5, bbox), "same-page step: no page change");
        assert!(a.back(), "the cross-page jump was recorded");
        assert_eq!(a.doc().unwrap().current, 0);
        assert!(!a.back(), "the same-page step was NOT recorded");
    }

    #[test]
    fn goto_match_preserves_zoom_clamped_to_target_page() {
        let mut a = app_with_doc(3);
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 3.0);
        let z = a.doc().unwrap().zoom;
        assert!(z > 1.0);
        let bbox = RectF { left: 300.0, top: 400.0, right: 350.0, bottom: 390.0 };
        a.goto_match(2, bbox);
        let d = a.doc().unwrap();
        assert_eq!(d.current, 2);
        assert!((d.zoom - z).abs() < 1e-4, "zoom carried across the jump");
        let mz = max_zoom(LETTER, a.client);
        assert!(d.zoom <= mz + 1e-4, "and clamped to the target page's max");
    }

    #[test]
    fn goto_match_cross_page_reveals_from_zero_baseline() {
        let mut a = app_with_doc(3);
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 4.0);
        // Scroll far into page 0 so a stale-baseline bug would be visible.
        a.doc_mut().unwrap().scroll = PointF { x: 900.0, y: 1500.0 };
        // Match near the TOP-LEFT of page 2 (PDF y-up: top ~ 780).
        let bbox = RectF { left: 20.0, top: 780.0, right: 90.0, bottom: 770.0 };
        a.goto_match(2, bbox);
        let d = a.doc().unwrap();
        // A top-left match from a zero baseline needs (near-)zero scroll;
        // the old page's scroll must not leak in.
        assert!(d.scroll.x < 100.0, "x: {}", d.scroll.x);
        assert!(d.scroll.y < 100.0, "y: {}", d.scroll.y);
        // And the match must actually be inside the viewport now.
        let pr = a.current_page_rect().unwrap();
        let w = a.page_rects_window(&[bbox]);
        let r = w[0];
        assert!(r.left >= 0.0 && r.right <= a.client.w as f32, "{r:?} vs client");
        assert!(r.top >= 0.0 && r.bottom <= a.client.h as f32, "{r:?} in {pr:?}");
    }

    #[test]
    fn goto_match_same_page_scrolls_match_into_view() {
        let mut a = app_with_doc(1);
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 4.0);
        a.doc_mut().unwrap().scroll = PointF { x: 0.0, y: 0.0 };
        // A match near the BOTTOM of the page (PDF y-up: bottom area ~ y=50).
        let bbox = RectF { left: 300.0, top: 60.0, right: 380.0, bottom: 50.0 };
        assert!(!a.goto_match(0, bbox), "same page");
        let r = a.page_rects_window(&[bbox])[0];
        assert!(
            r.bottom <= a.client.h as f32 && r.top >= 0.0,
            "match must be scrolled into the viewport: {r:?}"
        );
        assert!(a.doc().unwrap().zoom > 1.0, "zoom untouched");
    }

    #[test]
    fn goto_match_works_with_an_empty_cache() {
        // No rendered() calls at all: the bbox transform must be pure
        // geometry (its progenitor field_rects_window gates on the cache).
        let mut a = app_with_doc(5);
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 2.0);
        let bbox = RectF { left: 500.0, top: 100.0, right: 560.0, bottom: 90.0 };
        assert!(a.goto_match(4, bbox));
        assert_eq!(a.doc().unwrap().current, 4);
        let r = a.page_rects_window(&[bbox])[0];
        assert!(r.left >= 0.0 && r.right <= a.client.w as f32, "{r:?}");
    }

    #[test]
    fn page_rects_window_matches_field_pipeline_under_rotation() {
        // page_rects_window must agree with the hit-test inverse the same
        // way field_rects_window does (see the pro test of the same shape).
        let mut a = app_with_doc(1);
        let rect_pt = RectF { left: 100.0, top: 700.0, right: 300.0, bottom: 650.0 };
        for _ in 0..4 {
            let req = a.wanted_renders()[0];
            a.rendered(CachedPage {
                page: 0,
                px: req.px,
                rot: req.rot,
                bgra: vec![0; 4],
                links: vec![],
                fields: vec![],
                gen: 0,
            });
            let rects = a.page_rects_window(&[rect_pt]);
            assert_eq!(rects.len(), 1);
            let r = rects[0];
            assert!(r.left < r.right && r.top < r.bottom, "normalized at rot {}", a.doc().unwrap().rot);
            let c = PointF { x: (r.left + r.right) / 2.0, y: (r.top + r.bottom) / 2.0 };
            let pdf = a.pdf_point_at(c).unwrap();
            assert!((pdf.x - 200.0).abs() < 2.0, "rot {}: x {}", a.doc().unwrap().rot, pdf.x);
            assert!((pdf.y - 675.0).abs() < 2.0, "rot {}: y {}", a.doc().unwrap().rot, pdf.y);
            a.rotate_cw();
        }
    }

    #[test]
    fn search_state_survives_password_stash() {
        let mut a = app_with_doc(5);
        assert!(a.search_open());
        a.doc_mut().unwrap().search.as_mut().unwrap().push_char('d');
        a.doc_mut().unwrap().search.as_mut().unwrap().push_char('r');
        let prev = match std::mem::replace(&mut a.mode, Mode::Empty) {
            Mode::Doc(d) => Some(Box::new(d)),
            _ => None,
        };
        a.mode = Mode::Password { path: PathBuf::from("locked.pdf"), buf: String::new(), prev };
        assert!(!a.search_active(), "no doc mode: not active");
        a.password_cancel();
        assert!(a.search_active(), "search state came back with the stashed doc");
        assert_eq!(a.doc().unwrap().search.as_ref().unwrap().query, "dr");
    }

    #[test]
    fn search_open_close_stashes_and_restores_the_query() {
        let mut a = app_with_doc(5);
        assert!(!App::new().search_open(), "no doc: refuses");
        assert!(a.search_open());
        {
            let st = a.doc_mut().unwrap().search.as_mut().unwrap();
            st.push_char('o');
            st.push_char('r');
            st.push_char('c');
            assert!(!st.replace_on_type, "typing disarmed it");
        }
        assert!(a.search_close());
        assert!(!a.search_active());
        assert_eq!(
            a.doc().unwrap().last_search,
            Some(("orc".into(), crate::types::SearchOptions::default()))
        );
        assert!(a.search_open(), "reopen restores");
        let st = a.doc().unwrap().search.as_ref().unwrap();
        assert_eq!(st.query, "orc");
        assert!(st.replace_on_type, "restored query arms replace-on-type");
        // Ctrl+F while already open re-arms (search_open on an open bar).
        a.doc_mut().unwrap().search.as_mut().unwrap().replace_on_type = false;
        assert!(a.search_open());
        assert!(a.doc().unwrap().search.as_ref().unwrap().replace_on_type);
        // A fresh document has neither bar nor memory.
        a.doc_loaded(PathBuf::from("y.pdf"), 2, vec![LETTER; 2], vec![], false);
        assert!(!a.search_active());
        assert_eq!(a.doc().unwrap().last_search, None);
    }

    #[test]
    fn next_search_id_is_monotonic() {
        let mut a = App::new();
        let a1 = a.next_search_id();
        let a2 = a.next_search_id();
        assert!(a1 >= 1 && a2 > a1);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib app::tests::goto_match_records_history_iff_page_changes`
Expected: compile errors (missing fields/methods).

- [ ] **Step 3: Implement the DocState fields and App additions**

In `src/app.rs`:

1. Extend the layout import (top of file) to include `rotate_rect` and `scroll_to_reveal`:

```rust
use crate::layout::{
    clamp_scroll, fit_scale, max_zoom, page_rect, pdf_rect_to_window, render_size, rotate_rect,
    scroll_to_reveal, unrotate_point, zoom_scroll, PointF, RectF, SizeU, SCROLL_STEP,
};
```

2. Add to `DocState` (after `history: History`):

```rust
    /// Some = the find bar is open (Ctrl+F).
    pub search: Option<crate::search::SearchState>,
    /// Last query/options, stashed when the bar closes so reopening
    /// restores them. Per-document: dies with the DocState on doc load.
    pub last_search: Option<(String, crate::types::SearchOptions)>,
```

3. Initialize both to `None` in `doc_loaded`'s `DocState { ... }` literal (after `history: History::default(),`):

```rust
            search: None,
            last_search: None,
```

4. Add to `App` (after `render_gen: u64,`):

```rust
    /// Source of search-scan ids; bumped per issued PdfCmd::Search. The
    /// reply id-check drops in-flight stragglers from superseded scans.
    search_gen: u32,
```

and `search_gen: 0,` in `App::new()` (after `render_gen: 0,`).

5. Add these methods to `impl App` (place after `password_cancel`, at the end of the impl):

```rust
    // --- text search (Ctrl+F find bar) ---

    /// Fresh id for a PdfCmd::Search. Monotonic per App (like render_gen).
    pub fn next_search_id(&mut self) -> u32 {
        self.search_gen = self.search_gen.wrapping_add(1);
        self.search_gen
    }

    pub fn search_active(&self) -> bool {
        self.doc().is_some_and(|d| d.search.is_some())
    }

    /// Open the find bar (restoring the remembered query), or re-arm
    /// replace-on-type when it is already open (Ctrl+F while open).
    /// Returns false without a document.
    pub fn search_open(&mut self) -> bool {
        let Some(d) = self.doc_mut() else { return false };
        match &mut d.search {
            Some(st) => st.replace_on_type = true,
            None => d.search = Some(crate::search::SearchState::new(d.last_search.clone())),
        }
        true
    }

    /// Close the bar, remembering a non-empty query for the next open.
    /// Returns true iff a bar was open (the caller then sends CancelSearch).
    pub fn search_close(&mut self) -> bool {
        let Some(d) = self.doc_mut() else { return false };
        let Some(st) = d.search.take() else { return false };
        if !st.query.is_empty() {
            d.last_search = Some((st.query, st.options));
        }
        true
    }

    /// Window-space rects for PDF-point rects on the CURRENT page under the
    /// current zoom/scroll/rotation. Pure geometry — deliberately NO
    /// cache-presence gate (unlike its progenitor field_rects_window):
    /// goto_match needs bboxes for pages that have never rendered, and
    /// current_page_rect is pure. The bitmap gate belongs at highlight-DRAW
    /// call sites only.
    pub fn page_rects_window(&self, rects_pt: &[RectF]) -> Vec<RectF> {
        let Some(d) = self.doc() else { return Vec::new() };
        let Some(pr) = self.current_page_rect() else { return Vec::new() };
        let page_pt = d.page_pt();
        // Same scalar basis as render_size: zoom is clamped to max_zoom
        // everywhere it is set, so fit_scale * zoom never exceeds the
        // MAX_RENDER_DIM cap and highlights can't drift off the bitmap.
        let px_per_pt = fit_scale(d.page_pt_rot(), self.client) * d.zoom;
        let unrot = RectF {
            left: 0.0,
            top: 0.0,
            right: page_pt.0 * px_per_pt,
            bottom: page_pt.1 * px_per_pt,
        };
        rects_pt
            .iter()
            .map(|rect| {
                let r = pdf_rect_to_window(*rect, page_pt.1, px_per_pt, unrot);
                let rr = rotate_rect(r, unrot.right, unrot.bottom, d.rot);
                RectF {
                    left: pr.left + rr.left,
                    top: pr.top + rr.top,
                    right: pr.left + rr.right,
                    bottom: pr.top + rr.bottom,
                }
            })
            .collect()
    }

    /// Jump to a search match: shares goto's history contract (records iff
    /// the page changes) but is the "landing variant" — zoom is PRESERVED
    /// (clamped to the target page's max_zoom) and the match's union bbox is
    /// scrolled into view instead of resetting to fit. Cross-page landings
    /// compute the reveal from a zero-scroll baseline (the old page's scroll
    /// is meaningless in the new page's overhang range); same-page steps
    /// reveal from the live scroll. Returns true iff the page changed.
    pub fn goto_match(&mut self, page: u16, bbox_pt: RectF) -> bool {
        let client = self.client;
        let margin = 48.0 * (self.dpi as f32 / 96.0);
        let Some(d) = self.doc_mut() else { return false };
        if d.pages == 0 {
            return false;
        }
        let page = page.min(d.pages - 1);
        let from = d.current;
        let changed = page != from;
        if changed {
            d.history.record_jump(from);
            d.current = page;
            d.zoom = d.zoom.clamp(1.0, max_zoom(d.page_pt_rot(), client));
            d.scroll = PointF::default();
        }
        drop(d);
        let Some(target) = self.page_rects_window(&[bbox_pt]).into_iter().next() else {
            return changed;
        };
        let page_px = render_size(
            self.doc().map(|d| d.page_pt_rot()).unwrap_or((612.0, 792.0)),
            client,
            self.doc().map(|d| d.zoom).unwrap_or(1.0),
        );
        if let Some(d) = self.doc_mut() {
            d.scroll = scroll_to_reveal(d.scroll, target, page_px, client, margin);
        }
        changed
    }
```

Note: `drop(d)` on a `&mut` reborrow is a no-op statement — if the borrow
checker complains about the `self.page_rects_window` call after mutating
through `d`, restructure exactly like this (end the `doc_mut()` borrow in a
block, then call the pure helpers):

```rust
    pub fn goto_match(&mut self, page: u16, bbox_pt: RectF) -> bool {
        let client = self.client;
        let margin = 48.0 * (self.dpi as f32 / 96.0);
        let changed = {
            let Some(d) = self.doc_mut() else { return false };
            if d.pages == 0 {
                return false;
            }
            let page = page.min(d.pages - 1);
            let from = d.current;
            let changed = page != from;
            if changed {
                d.history.record_jump(from);
                d.current = page;
                d.zoom = d.zoom.clamp(1.0, max_zoom(d.page_pt_rot(), client));
                d.scroll = PointF::default();
            }
            changed
        };
        let Some(target) = self.page_rects_window(&[bbox_pt]).into_iter().next() else {
            return changed;
        };
        let page_px = match self.doc() {
            Some(d) => render_size(d.page_pt_rot(), client, d.zoom),
            None => return changed,
        };
        if let Some(d) = self.doc_mut() {
            d.scroll = scroll_to_reveal(d.scroll, target, page_px, client, margin);
        }
        changed
    }
```

Use the second (block-scoped) form — it is the one that compiles; the first
is shown only to make the intent obvious.

6. Refactor the Pro `field_rects_window` to delegate (replace its whole body;
the `#[cfg(feature = "pro")]` attribute and doc comment stay, plus a new
sentence "Keeps its cache gate: the rects themselves live on the cache
entry."):

```rust
    #[cfg(feature = "pro")]
    pub fn field_rects_window(&self) -> Vec<RectF> {
        let Some(d) = self.doc() else { return Vec::new() };
        let Some(entry) = self.cache.get_any(d.current, d.rot) else { return Vec::new() };
        let rects: Vec<RectF> = entry.fields.iter().map(|f| f.rect_pt).collect();
        self.page_rects_window(&rects)
    }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib` and `cargo test --lib --features pro`
Expected: all pass — including the pre-existing pro test
`field_rects_window_inverts_field_hit_across_rotations`, which now exercises
the shared pipeline.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "feat(search): DocState search state, shared page_rects_window, App::goto_match landing variant"
```

### Task 5: pdf.rs scan engine + text fixture + core integration tests

**Files:**
- Modify: `src/pdf.rs`
- Modify: `tests/common/mod.rs`
- Test: `tests/pdf_integration.rs`

**Interfaces:**
- Consumes: `PdfCmd::Search/CancelSearch`, `PdfReply::SearchHits/SearchDone`, `SearchMatch`, `SearchOptions` (Task 1); pdfium-render's `PdfSearchOptions` / `PdfPageText::search()` (already in scope via the existing `use pdfium_render::prelude::*`).
- Produces: worker behavior later tasks and tests rely on — one page scanned per loop pass, renders always before scan resumption, wrap order from `start_page`, `SearchHits` only for pages with hits, exactly one `SearchDone{id}` per scan that runs to completion, every reply followed by `wake()`; order-sensitive `CancelSearch` coalescing; `Load`/`Shutdown` kill scans regardless of batch order. Also `tests/common/mod.rs::text_fixture_path()` and `build_text_pdf()`.

- [ ] **Step 1: Add the text fixture to tests/common/mod.rs**

Append to `tests/common/mod.rs`:

```rust
/// Three pages of real text for search tests (Helvetica, a standard-14
/// font — nothing to embed). Ground truth for the query "dragon":
///   page 0: "The dragon sleeps" / "Another dragon here"  -> 2 hits
///   page 1: "Nothing to see here"                        -> 0 hits
///   page 2: "Dragon rider" / "dragonfire burns"          -> case/word probes
/// Totals: default (insensitive substring) = 4; match_case = 3 (drops
/// "Dragon"); whole_word = 3 (drops "dragonfire"); both = 2.
pub fn build_text_pdf() -> Vec<u8> {
    fn stream_obj(content: &str) -> String {
        format!("<< /Length {} >>\nstream\n{}\nendstream", content.len(), content)
    }
    let font = "/Resources << /Font << /F1 9 0 R >> >>";
    let objs: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
        (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>".into()),
        (3, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R {font} >>")),
        (4, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R {font} >>")),
        (5, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 8 0 R {font} >>")),
        (
            6,
            stream_obj(
                "BT /F1 24 Tf 72 700 Td (The dragon sleeps) Tj 0 -50 Td (Another dragon here) Tj ET",
            ),
        ),
        (7, stream_obj("BT /F1 24 Tf 72 700 Td (Nothing to see here) Tj ET")),
        (
            8,
            stream_obj(
                "BT /F1 24 Tf 72 700 Td (Dragon rider) Tj 0 -50 Td (dragonfire burns) Tj ET",
            ),
        ),
        (9, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into()),
    ];
    serialize_pdf(&objs)
}

/// Write the text fixture to a temp file and return its path.
pub fn text_fixture_path() -> std::path::PathBuf {
    let path = std::env::temp_dir().join("pppdf_text_fixture.pdf");
    std::fs::write(&path, build_text_pdf()).expect("write text fixture pdf");
    path
}

/// A WIDE fixture for genuinely-mid-scan tests: `n` pages, a "dragon" hit
/// on page 0 only. The first SearchHits arrives almost immediately while
/// the scan still has n-1 pages to cover — long enough (at 200 pages,
/// hundreds of ms) that a CancelSearch sent after that first hit reliably
/// lands mid-scan. Object layout: 1 catalog, 2 pages, 3..3+n page objects,
/// 3+n..3+2n content streams, 3+2n the font — contiguous ids, as
/// serialize_pdf's xref builder requires.
pub fn build_wide_text_pdf(n: u32) -> Vec<u8> {
    fn stream_obj(content: &str) -> String {
        format!("<< /Length {} >>\nstream\n{}\nendstream", content.len(), content)
    }
    let font_id = 3 + 2 * n;
    let kids: String =
        (0..n).map(|i| format!("{} 0 R", 3 + i)).collect::<Vec<_>>().join(" ");
    let mut objs: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
        (2, format!("<< /Type /Pages /Kids [{kids}] /Count {n} >>")),
    ];
    for i in 0..n {
        objs.push((
            3 + i,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R \
                 /Resources << /Font << /F1 {font_id} 0 R >> >> >>",
                3 + n + i
            ),
        ));
    }
    for i in 0..n {
        let text =
            if i == 0 { "Here be a dragon".to_string() } else { format!("Filler page {i}") };
        objs.push((3 + n + i, stream_obj(&format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET"))));
    }
    objs.push((font_id, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into()));
    serialize_pdf(&objs)
}

/// Write the 200-page wide fixture to a temp file and return its path.
pub fn wide_text_fixture_path() -> std::path::PathBuf {
    let path = std::env::temp_dir().join("pppdf_wide_text_fixture.pdf");
    std::fs::write(&path, build_wide_text_pdf(200)).expect("write wide text fixture pdf");
    path
}
```

- [ ] **Step 2: Write the failing integration tests**

Append to `tests/pdf_integration.rs` (top of file already imports `PdfCmd`, `PdfReply`, `SizeU`; extend the types import to `use pppdf::types::{LinkTarget, PdfCmd, PdfReply, SearchMatch, SearchOptions};`):

```rust
/// Drain replies until SearchDone{want} arrives, returning that scan's
/// SearchHits in ARRIVAL order. Skips unrelated replies. Panics on timeout.
fn collect_hits(
    reply_rx: &mpsc::Receiver<PdfReply>,
    want: u32,
) -> Vec<(u16, Vec<SearchMatch>)> {
    let mut hits = Vec::new();
    loop {
        match reply_rx.recv_timeout(WAIT).expect("search reply before timeout") {
            PdfReply::SearchHits { id, page, matches } if id == want => hits.push((page, matches)),
            PdfReply::SearchDone { id } if id == want => return hits,
            _ => {}
        }
    }
}

#[test]
fn search_streams_hits_in_wrap_order_with_sane_rects() {
    let _pdfium = pdfium_lock();
    let (reply_tx, reply_rx) = mpsc::channel();
    // Counting wake closure: every search reply must be followed by wake(),
    // or the UI counter freezes until an unrelated message arrives.
    let wakes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let w = std::sync::Arc::clone(&wakes);
    let cmd_tx = pdf::spawn(reply_tx, move || {
        w.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });
    cmd_tx.send(PdfCmd::Load { path: common::text_fixture_path(), password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { pages, .. } => assert_eq!(pages, 3),
        other => panic!("expected Loaded, got {other:?}"),
    }

    // From page 0: hits stream in page order 0, 2.
    cmd_tx
        .send(PdfCmd::Search {
            id: 1,
            query: "dragon".into(),
            options: SearchOptions::default(),
            start_page: 0,
        })
        .unwrap();
    let hits = collect_hits(&reply_rx, 1);
    let pages: Vec<u16> = hits.iter().map(|(p, _)| *p).collect();
    assert_eq!(pages, vec![0, 2]);
    assert_eq!(hits[0].1.len(), 2, "two 'dragon' hits on page 0");
    assert_eq!(hits[1].1.len(), 2, "'Dragon' + 'dragonfire' on page 2");
    for (_, matches) in &hits {
        for m in matches {
            assert!(!m.rects.is_empty(), "every hit ships at least one rect");
            for r in &m.rects {
                assert!(r.top > r.bottom, "PDF y-up convention: {r:?}");
                assert!(r.right > r.left, "{r:?}");
                assert!(
                    r.left >= 0.0 && r.right <= 612.0 && r.bottom >= 0.0 && r.top <= 792.0,
                    "rect must lie inside the page: {r:?}"
                );
            }
        }
    }

    // From page 2: wrap order — page 2's hits arrive FIRST, then page 0's.
    cmd_tx
        .send(PdfCmd::Search {
            id: 2,
            query: "dragon".into(),
            options: SearchOptions::default(),
            start_page: 2,
        })
        .unwrap();
    let hits = collect_hits(&reply_rx, 2);
    let pages: Vec<u16> = hits.iter().map(|(p, _)| *p).collect();
    assert_eq!(pages, vec![2, 0], "scan starts at start_page and wraps");

    cmd_tx.send(PdfCmd::Shutdown).unwrap();
    join_pdf_thread(&reply_rx);
    // Assert wakes only AFTER the join: the worker sends each reply and
    // THEN wakes, so a mid-stream count races that last wake. The joined
    // thread proves every wake executed. Two scans ran:
    // 1 Loaded + 2 × (2 SearchHits + 1 SearchDone) = 7 wakes minimum —
    // a missing Hits or Done wake shows up as a shortfall here.
    assert!(
        wakes.load(std::sync::atomic::Ordering::Relaxed) >= 7,
        "every search reply must be sent with wake(): {} wakes",
        wakes.load(std::sync::atomic::Ordering::Relaxed)
    );
}

#[test]
fn search_honors_case_and_whole_word_options() {
    let _pdfium = pdfium_lock();
    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    cmd_tx.send(PdfCmd::Load { path: common::text_fixture_path(), password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { .. } => {}
        other => panic!("expected Loaded, got {other:?}"),
    }
    // (match_case, whole_word) -> expected total across the document.
    for (id, case, word, want) in
        [(10u32, false, false, 4usize), (11, true, false, 3), (12, false, true, 3), (13, true, true, 2)]
    {
        cmd_tx
            .send(PdfCmd::Search {
                id,
                query: "dragon".into(),
                options: SearchOptions { match_case: case, whole_word: word },
                start_page: 0,
            })
            .unwrap();
        let total: usize = collect_hits(&reply_rx, id).iter().map(|(_, m)| m.len()).sum();
        assert_eq!(total, want, "case={case} word={word}");
    }
    // A term that is absent: no hits, but SearchDone still arrives.
    cmd_tx
        .send(PdfCmd::Search {
            id: 14,
            query: "wizard".into(),
            options: SearchOptions::default(),
            start_page: 0,
        })
        .unwrap();
    assert!(collect_hits(&reply_rx, 14).is_empty());
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
    join_pdf_thread(&reply_rx);
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test --test pdf_integration search_streams`
Expected: FAIL — the worker's Task-1 placeholder arm silently swallows
`Search`, so `SearchDone` never arrives and `collect_hits` panics on the
20-second `WAIT` timeout ("search reply before timeout"). That timeout
panic IS the expected red state.

- [ ] **Step 4: Implement the scan engine in pdf.rs**

All changes to `src/pdf.rs`:

1. Extend the types import:

```rust
use crate::types::{
    FieldKind, FormField, LinkTarget, OutlineItem, PageLink, PdfCmd, PdfReply, SearchMatch,
    SearchOptions,
};
```

2. Add near the top (after the `TMP_SEQ` static), the scan state:

```rust
/// The in-progress streaming text scan (PdfCmd::Search). One page is
/// scanned per main-loop pass so commands keep page-granularity latency,
/// exactly like the render preempt point.
struct ScanState {
    id: u32,
    query: String,
    options: SearchOptions,
    /// First page of the scan; it walks start..pages then wraps 0..start.
    start: u16,
    /// Pages covered so far; the scan is done when this reaches the count.
    scanned: u16,
}
```

3. In `run()`, add the state alongside `carry` (line ~155):

```rust
    // The streaming text scan, advanced one page per idle loop pass.
    let mut active_search: Option<ScanState> = None;
```

4. Replace the loop-head `let first = match carry.take() { ... };` block with
an Option-yielding version that never blocks while a scan is active, and
scans one page when no command is waiting:

```rust
        let first: Option<PdfCmd> = match carry.take() {
            Some(c) => Some(c),
            // A scan is active: never block — poll, and when nothing is
            // queued, advance the scan one page instead.
            None if active_search.is_some() => match cmd_rx.try_recv() {
                Ok(c) => Some(c),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => break 'outer,
            },
            None => {
                #[cfg(feature = "pro")]
                {
                    match wait_cmd(
                        &cmd_rx,
                        &mut engine,
                        &doc,
                        last_visible,
                        &mut last_unsolicited,
                        &reply_tx,
                        &wake,
                    ) {
                        Some(c) => Some(c),
                        None => break 'outer,
                    }
                }
                #[cfg(not(feature = "pro"))]
                {
                    match cmd_rx.recv() {
                        Ok(c) => Some(c),
                        Err(_) => break 'outer, // UI hung up
                    }
                }
            }
        };
        let Some(first) = first else {
            // Idle pass with an active scan: cover exactly one page.
            scan_step(&mut active_search, &doc, &reply_tx, &wake);
            // wait_cmd's timeout-arm duties must not starve while the scan
            // spins outside it: keep form timers firing (caret blink) and
            // flush throttle-deferred repaints. maybe_render_unsolicited is
            // internally throttled (33 ms), so per-page calls are safe.
            #[cfg(feature = "pro")]
            if let Some(eng) = engine.as_mut() {
                eng.fire_due_timers();
                maybe_render_unsolicited(
                    eng,
                    &doc,
                    last_visible,
                    &mut last_unsolicited,
                    &reply_tx,
                    &wake,
                );
            }
            continue 'outer;
        };
```

5. In the coalescer: add a local above the `for cmd in batch` loop —

```rust
        let mut queued_search: Option<ScanState> = None;
```

— and **REPLACE the Task-1 placeholder arm**
(`PdfCmd::Search { .. } | PdfCmd::CancelSearch => {}`) with these two arms:

```rust
                PdfCmd::Search { id, query, options, start_page } => {
                    // Only the LAST queued Search survives (visible-render
                    // semantics); earlier ones are superseded silently — the
                    // UI already moved on when it issued the newer id.
                    queued_search =
                        Some(ScanState { id, query, options, start: start_page, scanned: 0 });
                }
                PdfCmd::CancelSearch => {
                    // Order-sensitive, like Load: kills the active scan and
                    // any Search queued BEFORE it in this batch; a Search
                    // AFTER it in the same batch survives (Esc-then-Ctrl+F
                    // burst must not strand the new scan).
                    queued_search = None;
                    active_search = None;
                }
```

6. Directly after the `for page in superseded { ... }` supersede-reply loop,
apply the search coalescing results:

```rust
        // A Load replaces the document: any scan (active or queued, either
        // side of it in the batch) targets the OLD document and dies with
        // it. The UI drops stale ids anyway; no reply is owed.
        if load.is_some() {
            active_search = None;
            queued_search = None;
        }
        if let Some(s) = queued_search.take() {
            active_search = Some(s);
        }
```

7. Add the two functions after `describe` (before `walk_bookmark`):

```rust
/// Advance the streaming scan by exactly ONE page: send SearchHits for a
/// page with hits, and SearchDone when every page has been covered. Every
/// reply is followed by wake().
fn scan_step(
    scan: &mut Option<ScanState>,
    doc: &Option<PdfDocument<'static>>,
    reply_tx: &Sender<PdfReply>,
    wake: &impl Fn(),
) {
    let Some(s) = scan.as_mut() else { return };
    let Some(d) = doc else {
        // Unreachable in practice (the UI only searches an open document
        // and a Load kills the scan) — but never strand the UI's spinner.
        let _ = reply_tx.send(PdfReply::SearchDone { id: s.id });
        wake();
        *scan = None;
        return;
    };
    let pages = d.pages().len().clamp(0, u16::MAX as i32) as u16;
    if pages == 0 || s.scanned >= pages {
        let _ = reply_tx.send(PdfReply::SearchDone { id: s.id });
        wake();
        *scan = None;
        return;
    }
    // Wrap order: start..end then 0..start — the hits nearest the reader
    // stream in first, so Enter can jump almost immediately mid-scan.
    let page = ((s.start as u32 + s.scanned as u32) % pages as u32) as u16;
    s.scanned += 1;
    let done = s.scanned >= pages;
    let id = s.id;
    let matches = search_page(d, page, &s.query, s.options);
    if !matches.is_empty() {
        let _ = reply_tx.send(PdfReply::SearchHits { id, page, matches });
        wake();
    }
    if done {
        let _ = reply_tx.send(PdfReply::SearchDone { id });
        wake();
        *scan = None;
    }
}

/// All hits on one page, in document order, as plain rect lists (PDF
/// points, bottom-left origin — the PageLink convention). A page whose
/// text fails to load is skipped silently, per the corrupt-page policy.
fn search_page(
    doc: &PdfDocument,
    page_idx: u16,
    query: &str,
    options: SearchOptions,
) -> Vec<SearchMatch> {
    let Ok(page) = doc.pages().get(page_idx as PdfPageIndex) else { return Vec::new() };
    let Ok(text) = page.text() else { return Vec::new() };
    let opts = PdfSearchOptions::new()
        .match_case(options.match_case)
        .match_whole_word(options.whole_word);
    let Ok(search) = text.search(query, &opts) else { return Vec::new() };
    let mut out = Vec::new();
    while let Some(seg) = search.find_next() {
        // One hit can span multiple rects (line wraps / font changes).
        let mut rects = Vec::new();
        for i in seg.as_range() {
            if let Ok(segment) = seg.get(i) {
                let b = segment.bounds();
                rects.push(RectF {
                    left: b.left().value,
                    top: b.top().value,
                    right: b.right().value,
                    bottom: b.bottom().value,
                });
            }
        }
        if !rects.is_empty() {
            out.push(SearchMatch { rects });
        }
    }
    out
}
```

(If `page.text()` in this pdfium-render version returns `Result` via a
different accessor name, it does not — `PdfPage::text()` →
`Result<PdfPageText, PdfiumError>` is the 0.9.2 signature; `PdfSearchOptions`
and `PdfRect` methods `left()/top()/right()/bottom()` (each returning
`PdfPoints` with `.value`) come through the already-imported prelude, the
same accessor style `extract_links` uses.)

- [ ] **Step 5: Run the tests**

Run: `cargo test --test pdf_integration` (free) and
`cargo test --features pro --test pdf_integration` (Pro).
Expected: all pass, including all pre-existing tests — the loop restructure
must not disturb render/form behavior.

- [ ] **Step 6: Commit**

```bash
git add src/pdf.rs tests/common/mod.rs tests/pdf_integration.rs
git commit -m "feat(search): streaming per-page scan on the pdf worker + text fixture and core tests"
```

---

### Task 6: Integration tests — supersede, cancel-burst, load-kill, render priority

**Files:**
- Test: `tests/pdf_integration.rs` (additions only, plus any pdf.rs fixes they force)

**Interfaces:**
- Consumes: everything from Task 5, plus `common::wide_text_fixture_path()`
  (Task 5's 200-page fixture). The "busy render" trick makes batch grouping
  deterministic: a large visible render (1224×1584) takes long enough that
  commands sent immediately after it are drained into ONE later batch.
  Note the coverage split: the burst tests exercise the ORDER-SENSITIVE
  COALESCING of scans that are still queued; `cancel_lands_mid_scan_on_a_
  wide_document` is the one that kills a scan which is provably ACTIVE
  (streaming) — both paths of the spec's kill rules.

- [ ] **Step 1: Write the tests**

Append to `tests/pdf_integration.rs`:

```rust
/// A render big enough to keep the worker busy while a burst of commands
/// queues up behind it — the burst then lands in one batch, making
/// coalescing deterministic.
const BUSY_PX: SizeU = SizeU { w: 1224, h: 1584 };

fn load_text_fixture(cmd_tx: &mpsc::Sender<PdfCmd>, reply_rx: &mpsc::Receiver<PdfReply>) {
    cmd_tx.send(PdfCmd::Load { path: common::text_fixture_path(), password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { pages, .. } => assert_eq!(pages, 3),
        other => panic!("expected Loaded, got {other:?}"),
    }
}

fn dragon_search(id: u32) -> PdfCmd {
    PdfCmd::Search {
        id,
        query: "dragon".into(),
        options: SearchOptions::default(),
        start_page: 0,
    }
}

#[test]
fn later_search_supersedes_queued_one_silently() {
    let _pdfium = pdfium_lock();
    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    load_text_fixture(&cmd_tx, &reply_rx);
    cmd_tx.send(PdfCmd::Render { page: 0, px: BUSY_PX, rot: 0, visible: true }).unwrap();
    cmd_tx.send(dragon_search(3)).unwrap();
    cmd_tx.send(dragon_search(4)).unwrap();
    // Drain to SearchDone{4}: id 3 must never produce a reply (it was
    // superseded in-batch; the UI's id-check needs no supersede signal).
    let mut saw_4_hits = 0;
    loop {
        match reply_rx.recv_timeout(WAIT).unwrap() {
            PdfReply::SearchHits { id, .. } => {
                assert_ne!(id, 3, "superseded scan must be silent");
                if id == 4 {
                    saw_4_hits += 1;
                }
            }
            PdfReply::SearchDone { id } => {
                assert_ne!(id, 3, "superseded scan must send no Done");
                if id == 4 {
                    break;
                }
            }
            _ => {} // Rendered / RenderFailed from the busy render
        }
    }
    assert_eq!(saw_4_hits, 2, "the surviving scan runs to completion");
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
    join_pdf_thread(&reply_rx);
}

#[test]
fn cancel_then_search_in_one_burst_keeps_the_later_scan() {
    let _pdfium = pdfium_lock();
    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    load_text_fixture(&cmd_tx, &reply_rx);
    cmd_tx.send(PdfCmd::Render { page: 0, px: BUSY_PX, rot: 0, visible: true }).unwrap();
    // Esc-then-Ctrl+F reflex: Search 5, Cancel, Search 6 in one batch.
    cmd_tx.send(dragon_search(5)).unwrap();
    cmd_tx.send(PdfCmd::CancelSearch).unwrap();
    cmd_tx.send(dragon_search(6)).unwrap();
    let mut pages_6 = Vec::new();
    loop {
        match reply_rx.recv_timeout(WAIT).unwrap() {
            PdfReply::SearchHits { id, page, .. } => {
                assert_ne!(id, 5, "cancelled scan must be silent");
                if id == 6 {
                    pages_6.push(page);
                }
            }
            PdfReply::SearchDone { id } => {
                assert_ne!(id, 5);
                if id == 6 {
                    break;
                }
            }
            _ => {}
        }
    }
    assert_eq!(pages_6, vec![0, 2], "the post-cancel scan is fully live");
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
    join_pdf_thread(&reply_rx);
}

#[test]
fn cancel_mid_scan_stops_replies_but_worker_stays_live() {
    let _pdfium = pdfium_lock();
    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    load_text_fixture(&cmd_tx, &reply_rx);
    cmd_tx.send(PdfCmd::Render { page: 0, px: BUSY_PX, rot: 0, visible: true }).unwrap();
    cmd_tx.send(dragon_search(7)).unwrap();
    cmd_tx.send(PdfCmd::CancelSearch).unwrap();
    // The worker must still answer renders afterwards.
    cmd_tx
        .send(PdfCmd::Render { page: 1, px: SizeU { w: 612, h: 792 }, rot: 0, visible: true })
        .unwrap();
    loop {
        match reply_rx.recv_timeout(WAIT).unwrap() {
            PdfReply::SearchHits { .. } | PdfReply::SearchDone { .. } => {
                panic!("cancelled scan must produce no search replies")
            }
            PdfReply::Rendered { page: 1, .. } => break,
            _ => {}
        }
    }
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
    join_pdf_thread(&reply_rx);
}

#[test]
fn load_kills_the_scan_without_a_done() {
    let _pdfium = pdfium_lock();
    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    load_text_fixture(&cmd_tx, &reply_rx);
    cmd_tx.send(PdfCmd::Render { page: 0, px: BUSY_PX, rot: 0, visible: true }).unwrap();
    cmd_tx.send(dragon_search(8)).unwrap();
    cmd_tx.send(PdfCmd::Load { path: common::fixture_path(), password: None }).unwrap();
    // Drain to the second Loaded; no search reply may appear before OR
    // after it (drain a settling window to be sure).
    loop {
        match reply_rx.recv_timeout(WAIT).unwrap() {
            PdfReply::SearchHits { .. } | PdfReply::SearchDone { .. } => {
                panic!("Load must kill the scan silently")
            }
            PdfReply::Loaded { .. } => break,
            _ => {}
        }
    }
    match reply_rx.recv_timeout(Duration::from_millis(500)) {
        Err(_) => {} // silence: correct
        Ok(PdfReply::SearchHits { .. }) | Ok(PdfReply::SearchDone { .. }) => {
            panic!("no search replies may straggle after the Load")
        }
        Ok(_) => {} // e.g. a late RenderFailed{"superseded"} is fine
    }
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
    join_pdf_thread(&reply_rx);
}

#[test]
fn cancel_lands_mid_scan_on_a_wide_document() {
    // The burst tests above kill the scan while it is still QUEUED (one
    // batch). This one cancels a scan that is provably ACTIVE: the wide
    // fixture's only hit is on page 0, so the first SearchHits arrives
    // while ~199 pages are still unscanned (hundreds of ms of work) — the
    // CancelSearch sent after it reliably lands mid-scan and exercises the
    // `active_search = None` path.
    let _pdfium = pdfium_lock();
    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    cmd_tx.send(PdfCmd::Load { path: common::wide_text_fixture_path(), password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { pages, .. } => assert_eq!(pages, 200),
        other => panic!("expected Loaded, got {other:?}"),
    }
    cmd_tx.send(dragon_search(20)).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::SearchHits { id: 20, page: 0, .. } => {}
        other => panic!("expected the page-0 hit first, got {other:?}"),
    }
    cmd_tx.send(PdfCmd::CancelSearch).unwrap();
    // Settle window: the cancelled scan must go silent — in particular, NO
    // SearchDone{20} (a Done arriving here would mean the scan outran the
    // cancel; if this ever flakes, widen the fixture, never add sleeps).
    loop {
        match reply_rx.recv_timeout(Duration::from_millis(600)) {
            Err(_) => break, // silence: cancelled
            Ok(PdfReply::SearchHits { .. }) | Ok(PdfReply::SearchDone { .. }) => {
                panic!("cancelled ACTIVE scan must produce no further search replies")
            }
            Ok(_) => {}
        }
    }
    // The worker is still alive and serving.
    cmd_tx
        .send(PdfCmd::Render { page: 0, px: SizeU { w: 612, h: 792 }, rot: 0, visible: true })
        .unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Rendered { page: 0, .. } => {}
        other => panic!("expected Rendered, got {other:?}"),
    }
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
    join_pdf_thread(&reply_rx);
}

#[test]
fn renders_take_priority_over_an_active_scan() {
    let _pdfium = pdfium_lock();
    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    load_text_fixture(&cmd_tx, &reply_rx);
    cmd_tx.send(PdfCmd::Render { page: 0, px: BUSY_PX, rot: 0, visible: true }).unwrap();
    // Search and a page-flip render land in one batch: the render must be
    // answered before ANY hit of that scan (renders run before the scan
    // resumes).
    cmd_tx.send(dragon_search(9)).unwrap();
    cmd_tx
        .send(PdfCmd::Render { page: 1, px: SizeU { w: 612, h: 792 }, rot: 0, visible: true })
        .unwrap();
    let mut saw_page1_render = false;
    loop {
        match reply_rx.recv_timeout(WAIT).unwrap() {
            PdfReply::Rendered { page: 1, .. } => saw_page1_render = true,
            PdfReply::SearchHits { id: 9, .. } => {
                assert!(saw_page1_render, "hits must not precede the queued render");
            }
            PdfReply::SearchDone { id: 9 } => break,
            _ => {}
        }
    }
    assert!(saw_page1_render);
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
    join_pdf_thread(&reply_rx);
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test --test pdf_integration` and
`cargo test --features pro --test pdf_integration`
Expected: all pass. If `later_search_supersedes_queued_one_silently` or the
burst tests flake because the burst straddled two batches, the busy render
is too small — bump `BUSY_PX` (e.g. 2448×3168); do NOT add sleeps.

- [ ] **Step 3: Commit**

```bash
git add tests/pdf_integration.rs
git commit -m "test(search): supersede/cancel-burst/load-kill/render-priority integration coverage"
```

### Task 7: window.rs wiring — Ctrl+F, the search rung, WM_CHAR, replies, mouse

**Files:**
- Modify: `src/window.rs`

**Interfaces:**
- Consumes: `App::{search_open, search_close, search_active, next_search_id, goto_match}` (Task 4), `SearchState` methods + `crate::search::{bar_rect, QueryEffect, Step}` (Task 3), `PdfCmd::{Search, CancelSearch}`, `PdfReply::{SearchHits, SearchDone}` (Tasks 1/5).
- Produces: the complete input surface. No new public API — everything is `Ui`-internal.

There are no unit tests for this task (window.rs needs a real HWND); all
logic already lives tested in search.rs/app.rs. Verification is: both
editions compile, the full test suites stay green, and the Task 9 manual
checklist covers the UX. Keep every branch THIN — anything conditional
beyond routing belongs in search.rs/app.rs instead. One conscious coverage
decision: the spec's "stash-restore re-issue rule" names an app.rs unit
test, but the re-issue lives here (`resync_open_search`, a two-line shim
over tested code) — it is covered by the app.rs stash-survival test plus
the password-stash manual row, not by a window.rs unit test.

- [ ] **Step 1: Import VK_F3 and un-gate clipboard_text**

1. Extend the main VK import list (the `use windows::Win32::UI::Input::KeyboardAndMouse::{...}` block) with `VK_F3` (alphabetical: after `VK_F11`).
2. Remove the `#[cfg(feature = "pro")]` attribute from `fn clipboard_text` (bottom of the file) — the find bar pastes in both editions. Update its doc comment to: `/// Plain-text clipboard contents (CF_UNICODETEXT), if any. Used by Pro form paste and the find bar.`

- [ ] **Step 2: Add the search helper block**

Add these methods to the second `impl Ui` block (after `form_paste`/`save_doc`, before `sync_renders`):

```rust
    // --- text search (Ctrl+F find bar) ---

    /// Open the find bar (Ctrl+F / F3): closes the TOC panel first — the
    /// bar owns Enter/arrows while open, and a keyboard-dead panel is worse
    /// than a closed one (same courtesy as cancelling the goto prompt).
    /// Re-issues the remembered query's scan when the bar newly opens.
    fn search_open_ui(&mut self) {
        if self.app.doc().is_none() {
            return;
        }
        if let Some(d) = self.app.doc_mut() {
            d.overlay = None;
        }
        let was_active = self.app.search_active();
        if self.app.search_open() {
            if !was_active {
                self.sync_search();
            }
            self.invalidate();
        }
    }

    /// Close the bar, remembering the query (Esc path).
    fn search_close_ui(&mut self) {
        if self.app.search_close() {
            self.send_pdf(PdfCmd::CancelSearch);
            self.invalidate();
        }
    }

    /// Issue the scan matching the bar's current query, or cancel when it
    /// is illegal (< 2 chars). The single choke point every query mutation
    /// funnels through — begin_scan's atomic reset is what keeps a new
    /// query's results from merging with the old ones.
    fn sync_search(&mut self) {
        let Some((current, legal)) = self
            .app
            .doc()
            .and_then(|d| d.search.as_ref().map(|st| (d.current, st.legal())))
        else {
            return;
        };
        if legal {
            let id = self.app.next_search_id();
            let payload = self.app.doc_mut().and_then(|d| d.search.as_mut()).map(|st| {
                st.begin_scan(id);
                (st.query.clone(), st.options)
            });
            if let Some((query, options)) = payload {
                self.send_pdf(PdfCmd::Search { id, query, options, start_page: current });
            }
        } else {
            if let Some(st) = self.app.doc_mut().and_then(|d| d.search.as_mut()) {
                st.clear_results();
            }
            self.send_pdf(PdfCmd::CancelSearch);
        }
    }

    /// Apply a QueryEffect returned by a bar edit/toggle.
    fn apply_search_effect(&mut self, eff: crate::search::QueryEffect) {
        match eff {
            crate::search::QueryEffect::Rescan => self.sync_search(),
            // clear_results already ran inside SearchState (edited()).
            crate::search::QueryEffect::Cancel => self.send_pdf(PdfCmd::CancelSearch),
            crate::search::QueryEffect::Nothing => {}
        }
        self.invalidate();
    }

    /// Enter / Shift+Enter / F3 / Shift+F3 while the bar is open.
    fn search_step_ui(&mut self, backward: bool) {
        let step = match self.app.doc_mut().and_then(|d| d.search.as_mut()) {
            Some(st) => st.step(backward),
            None => return,
        };
        match step {
            crate::search::Step::Jump(page, idx) => self.search_jump(page, idx),
            crate::search::Step::Armed | crate::search::Step::Nothing => self.invalidate(),
        }
    }

    /// Land on a match: goto_match keeps zoom and reveals the union bbox;
    /// a page change records history and badges like every jump source.
    fn search_jump(&mut self, page: u16, idx: usize) {
        let bbox = self
            .app
            .doc()
            .and_then(|d| d.search.as_ref())
            .and_then(|st| st.union_bbox(page, idx));
        let Some(bbox) = bbox else { return };
        let changed = self.app.goto_match(page, bbox);
        self.sync_renders();
        if changed {
            self.show_badge();
        }
        self.invalidate();
    }

    /// Any path that restores a stashed DocState (password cancel, a failed
    /// load over an open doc) re-issues an open search with a fresh id: the
    /// Load that displaced the doc killed its scan WITHOUT a SearchDone, so
    /// the restored `scanning` flag would otherwise spin forever.
    ///
    /// password_take (Enter with a typed password) ALSO restores the stash,
    /// but deliberately does NOT resync: the retry Load it sends right after
    /// would kill the fresh scan immediately, and every continuation repairs
    /// the stale flag anyway — Loaded rebuilds DocState, LoadFailed lands in
    /// the resync below, NeedsPassword re-stashes until a cancel does.
    fn resync_open_search(&mut self) {
        if self.app.search_active() {
            self.sync_search();
        }
    }

    /// Keys while the find bar is open. Returns true when consumed. The bar
    /// is semi-modal: PgUp/PgDn still page the document, and every Ctrl
    /// chord except Ctrl+V/Ctrl+F falls through (Ctrl+W quits, Ctrl+O/N
    /// open, Ctrl+S saves — browser find-bar behavior).
    fn search_key(&mut self, vk: u16) -> bool {
        if !self.app.search_active() {
            return false;
        }
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        match vk {
            _ if vk == VK_ESCAPE.0 => self.search_close_ui(),
            _ if vk == VK_RETURN.0 || vk == VK_F3.0 => self.search_step_ui(shift),
            _ if vk == VK_BACK.0 && !ctrl => {
                let eff =
                    self.app.doc_mut().and_then(|d| d.search.as_mut()).map(|st| st.backspace());
                if let Some(eff) = eff {
                    self.apply_search_effect(eff);
                }
            }
            0x56 if ctrl => {
                // Ctrl+V: paste into the query.
                if let Some(text) = clipboard_text(self.hwnd) {
                    let eff = self
                        .app
                        .doc_mut()
                        .and_then(|d| d.search.as_mut())
                        .map(|st| st.paste(&text));
                    if let Some(eff) = eff {
                        self.apply_search_effect(eff);
                    }
                }
            }
            0x46 if ctrl => {
                // Ctrl+F while open: re-arm select-all emulation.
                self.app.search_open();
                self.invalidate();
            }
            _ if ctrl => return false, // other Ctrl chords keep their global meaning
            _ if vk == VK_NEXT.0 || vk == VK_PRIOR.0 => return false, // page the doc
            _ if vk == VK_TAB.0
                || vk == VK_LEFT.0
                || vk == VK_RIGHT.0
                || vk == VK_UP.0
                || vk == VK_DOWN.0
                || vk == VK_HOME.0
                || vk == VK_END.0 => {} // swallowed: no caret model; no surprise jumps
            _ => {} // printables arrive via WM_CHAR
        }
        true
    }
```

- [ ] **Step 3: Insert the rung into on_key and add the openers**

1. In `on_key`, insert the search rung directly after the Pro form-typing
rung (i.e. after its closing `}` of `if self.app.form_focus { ... }` — in the
free build, directly after the goto-entry rung) and before the `if vk ==
VK_F1.0` line:

```rust
        // Find bar (persistent, semi-modal): position matters — a clicked
        // form field wins the keyboard (the rung above), and the bar
        // shadows the global plain-key shortcuts below (G/R/F/0/Space type
        // into the query via WM_CHAR).
        if self.search_key(vk) {
            return;
        }
```

2. In the goto-entry rung's `match vk`, add an arm (before the `_ => {}`):

```rust
                _ if vk == 0x46 && unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 => {
                    // Ctrl+F cancels the half-typed goto prompt and opens
                    // the find bar (they must not stack).
                    self.app.goto_entry_cancel();
                    self.search_open_ui();
                }
```

3. In the Pro form-typing rung's `match vk`, add an arm next to `0x56 if ctrl`:

```rust
                0x46 if ctrl => {
                    // Ctrl+F leaves the field (commit) and opens the find bar.
                    self.form_kill_focus();
                    self.search_open_ui();
                }
```

4. In the global-shortcut `match vk`, add two arms — Ctrl+F directly above
the Pro `0x46 if !ctrl` arm (both editions, NOT cfg-gated), and F3 after the
Backspace arm:

```rust
            0x46 if ctrl => self.search_open_ui(), // Ctrl+F: find in the book
            _ if vk == VK_F3.0 => {
                // F3 with the bar closed: reopen the last query and jump to
                // its first hit when it arrives. With no remembered query
                // this just opens an empty bar (no scan, no pending jump).
                if self.app.doc().is_some() {
                    self.search_open_ui();
                    if let Some(st) = self.app.doc_mut().and_then(|d| d.search.as_mut()) {
                        if st.legal() {
                            // Set AFTER search_open_ui's sync_search reset.
                            st.pending_jump = true;
                        }
                    }
                    self.invalidate();
                }
            }
```

5. F1 help stays fully modal: no change to its rung (Ctrl+F is swallowed
there by the existing catch-all). `modal_active()` is deliberately NOT
touched — the persistent bar must not block Alt+arrow/mouse-X history nav.

- [ ] **Step 4: WM_CHAR consumer**

In the `WM_CHAR` arm, after the Password-mode `if` block (and before the
final `LRESULT(0)`), insert:

```rust
                    // Find bar: the first shared-code WM_CHAR consumer. One
                    // filter (`!ctrl && c >= 0x20`) drops Enter/Tab/Esc/
                    // Backspace chars and the control BYTES Ctrl chords
                    // produce (Ctrl+F=0x06 etc.); char::from_u32 rejects
                    // unpaired surrogate halves, so input is BMP-only —
                    // the password path's existing limitation.
                    if self.app.search_active() {
                        let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                        let c32 = wparam.0 as u32;
                        if !ctrl && c32 >= 0x20 {
                            if let Some(c) = char::from_u32(c32) {
                                let eff = self
                                    .app
                                    .doc_mut()
                                    .and_then(|d| d.search.as_mut())
                                    .map(|st| st.push_char(c));
                                if let Some(eff) = eff {
                                    self.apply_search_effect(eff);
                                }
                            }
                        }
                        return LRESULT(0);
                    }
```

(`search_active()` requires Doc mode, so Password mode is excluded by
construction; the Pro form-focus block above already returned when a field
owns the keyboard.)

- [ ] **Step 5: Alt+C / Alt+W toggles in WM_SYSKEYDOWN**

(The toast below is a deliberate house-convention addition beyond the spec
— the 'F' field-indication cycle toasts its mode the same way; the spec's
own feedback is the bar's lit `Aa`/`W` indicators, which Task 8 draws.)

Extend the WM_SYSKEYDOWN handler's `if/else` chain — after the two
Alt+arrow branches, before the final `else`:

```rust
                    } else if alt && (vk == 0x43 || vk == 0x57) && self.app.search_active() {
                        // Alt+C / Alt+W: match-case / whole-word toggles.
                        let eff = self.app.doc_mut().and_then(|d| d.search.as_mut()).map(|st| {
                            if vk == 0x43 {
                                st.toggle_case()
                            } else {
                                st.toggle_word()
                            }
                        });
                        let on = self.app.doc().and_then(|d| d.search.as_ref()).map_or(
                            false,
                            |st| {
                                if vk == 0x43 {
                                    st.options.match_case
                                } else {
                                    st.options.whole_word
                                }
                            },
                        );
                        self.show_toast(
                            match (vk, on) {
                                (0x43, true) => "Match case: on",
                                (0x43, false) => "Match case: off",
                                (_, true) => "Whole words: on",
                                (_, false) => "Whole words: off",
                            }
                            .into(),
                        );
                        if let Some(eff) = eff {
                            self.apply_search_effect(eff);
                        }
                        LRESULT(0)
                    } else {
```

- [ ] **Step 6: Reply pumping and the restore-resync rule**

1. In `pump_pdf_replies`, **REPLACE the Task-1 placeholder arm**
(`PdfReply::SearchHits { .. } | PdfReply::SearchDone { .. } => {}`) with
these two arms (shared, no cfg):

```rust
                PdfReply::SearchHits { id, page, matches } => {
                    let target = self
                        .app
                        .doc_mut()
                        .and_then(|d| d.search.as_mut())
                        .and_then(|st| st.hits(id, page, matches));
                    if let Some((page, idx)) = target {
                        self.search_jump(page, idx); // a pending jump fired
                    }
                }
                PdfReply::SearchDone { id } => {
                    let target = self
                        .app
                        .doc_mut()
                        .and_then(|d| d.search.as_mut())
                        .and_then(|st| st.done(id));
                    if let Some((page, idx)) = target {
                        self.search_jump(page, idx); // frontier jump wrapped
                    }
                }
```

(The pump's trailing `sync_renders()` + `invalidate()` repaint the counter.)

2. Password-cancel re-issue — in `on_key`'s Password rung:

```rust
            if vk == VK_ESCAPE.0 {
                // Restores the stashed doc if one was open, else Empty.
                self.app.password_cancel();
                // The Load that displaced the doc killed its scan without a
                // SearchDone — re-issue so `scanning` can't spin forever.
                self.resync_open_search();
                self.sync_renders();
                self.invalidate();
            }
```

3. Failed-load re-issue — in the `PdfReply::LoadFailed` arm's non-fatal
branch, extend the mode check:

```rust
                        if matches!(self.app.mode, crate::app::Mode::Loading { .. }) {
                            self.app.mode = crate::app::Mode::Empty;
                        } else {
                            // The surviving open doc may have had its scan
                            // killed by the failed Load — re-issue it.
                            self.resync_open_search();
                        }
```

- [ ] **Step 7: Mouse — bar hit-test and click swallow**

1. In `wants_client_mouse`, inside the `if let Some(d) = self.app.doc()`
block (after the `field_at` clause):

```rust
            if d.search.is_some()
                && crate::search::bar_rect(self.app.client, self.dpi_scale()).contains(p)
            {
                return true; // clicks on the find bar must not drag the window
            }
```

2. In `WM_LBUTTONDOWN`, directly after the help-dismiss block (before
`dismiss_goto_entry`):

```rust
                    // Clicks on the find bar are a no-op (keyboard-only UI):
                    // swallow them so they don't fall through to fields,
                    // links, or a pan-drag underneath the bar.
                    {
                        let p = self.mouse_point(lparam);
                        if self.app.search_active()
                            && crate::search::bar_rect(self.app.client, self.dpi_scale())
                                .contains(p)
                        {
                            return LRESULT(0);
                        }
                    }
```

- [ ] **Step 8: Build and test both editions**

Run: `cargo build && cargo test`, then
`cargo build --features pro && cargo test --features pro`
Expected: everything green (this task adds no tests; it must break none).

- [ ] **Step 9: Commit**

```bash
git add src/window.rs
git commit -m "feat(search): Ctrl+F find bar input wiring - semi-modal rung, WM_CHAR, F3, Alt toggles, reply pumping"
```

### Task 8: d2d.rs — match highlights and the find bar

**Files:**
- Modify: `src/d2d.rs`

**Interfaces:**
- Consumes: `App::page_rects_window` (Task 4), `crate::search::{bar_rect, SearchState}` (Task 3), the existing `color`/`rectf`/`text_format`/`draw_text` helpers.
- Produces: two private draw methods; the draw order becomes
  `page bitmap → field indication (Pro) → search highlights → TOC → badge → goto pill → search bar → dirty dot (Pro) → F1 help`.

- [ ] **Step 1: Wire the two draw calls into draw_inner**

1. Inside the `Mode::Doc(d)` arm's cache-entry block, directly after the
`#[cfg(feature = "pro")] self.draw_field_indication(rt, app)?;` line
(un-gated — search is both editions; the bitmap-presence gate is this
block itself):

```rust
                            // Search-match highlights: client-side over the
                            // cached bitmap (toggling/retyping is a repaint,
                            // never a render), only when the page bitmap is
                            // present — same gating as field indication.
                            self.draw_search_highlights(rt, app)?;
```

2. At the end of the `Mode::Doc(d)` arm, after the `draw_goto` call (so the
bar draws above the TOC/badge/goto pill, below the dirty dot and F1 help):

```rust
                    if let Some(st) = &d.search {
                        self.draw_search_bar(rt, st, client)?;
                    }
```

- [ ] **Step 2: Implement the two methods**

Add after `draw_field_indication` (or after `dot_stroke` — anywhere in the
`impl Renderer` block; keep them un-gated):

```rust
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
```

- [ ] **Step 3: Build and test both editions**

Run: `cargo build && cargo test && cargo build --features pro && cargo test --features pro`
Expected: green. Then a quick smoke run against a real book:
`cargo run -- <some.pdf>` → Ctrl+F, type a common word: highlights appear
live, counter counts up, Enter steps with zoom preserved, Esc closes,
Ctrl+F restores the query, first keystroke replaces it.

- [ ] **Step 4: Commit**

```bash
git add src/d2d.rs
git commit -m "feat(search): draw match highlights and the top-right find bar"
```

---

### Task 9: Documentation — the three-surface sync

**Files:**
- Modify: `README.md`
- Modify: `src/d2d.rs` (HELP_ROWS)
- Modify: `docs/manual-tests.md`

**Interfaces:** none — pure docs, but README and HELP_ROWS must use
**byte-identical wording** (d2d.rs:15-16 promises it).

- [ ] **Step 1: README key table rows**

In `README.md`, insert after the `| Alt+→ / Shift+Backspace / mouse forward | ... |` row:

```markdown
| Ctrl+F | Find text in the book (Esc closes the bar) |
| Enter / F3 · Shift steps back | Next / previous match |
| Alt+C / Alt+W | Match case / whole words while finding |
```

- [ ] **Step 2: HELP_ROWS — same rows, plus the drift fix**

In `src/d2d.rs` `HELP_ROWS`, insert after the `("Alt+\u{2192} / Shift+Backspace / mouse forward", ...)` row:

```rust
    ("Ctrl+F", "Find text in the book (Esc closes the bar)"),
    ("Enter / F3 \u{b7} Shift steps back", "Next / previous match"),
    ("Alt+C / Alt+W", "Match case / whole words while finding"),
```

And fix the pre-existing drift in the F1 row — change
`("F1", "Show / hide this help")` to
`("F1", "Show / hide the help overlay")` so it matches README.md line 76
exactly (the HELP_ROWS comment promises verbatim mirroring; only d2d.rs
makes that claim, and it was already broken).

- [ ] **Step 3: manual-tests.md section**

In `docs/manual-tests.md`, insert after the "Page history (both editions):"
block (before "Form filling (Pro)"):

```markdown
Text search (both editions):
- [ ] Ctrl+F opens the bar top-right; typing live-highlights from the 2nd character; counter counts up with "…" while scanning, then settles
- [ ] Enter reveals the current match first, then steps; Shift+Enter / Shift+F3 step back; both directions wrap around
- [ ] Match on another page: the jump keeps the zoom level and scrolls the match into view; page badge shows; Alt+Left returns to the origin
- [ ] Wheel, page flips, clicks, and zooming all leave the bar open; PgUp/PgDn page the document while the bar has focus
- [ ] Esc closes the bar (highlights vanish); Ctrl+F reopens with the last query — the first keystroke replaces it wholesale, Backspace clears it
- [ ] F3 with the bar closed reopens the last query and jumps to its first hit as it arrives
- [ ] Alt+C / Alt+W flip the Aa / W indicators, re-run the search, and change the counts ("Dragon" vs "dragonfire" style probes)
- [ ] A term not in the document: "0 / 0" in red once the scan finishes; a 1-char query shows no counter and no highlights
- [ ] R rotation and Ctrl+wheel zoom with highlights showing: the washes track the text exactly
- [ ] 200-DIP window: the bar clamps inside; clicks ON it don't drag the window, clicks beside it still do
- [ ] Pro: the dirty dot stays visible above the bar's corner; Ctrl+F while typing in a form field leaves the field and opens the bar; clicking a field while the bar is open types into the field (bar stays); Esc leaves the field, second Esc closes the bar
- [ ] Open a password-protected PDF over a live search, Esc back to the document: the search re-runs (counter live again — never stuck on "…")
- [ ] With the bar open: Ctrl+W still quits, Ctrl+O still opens, Ctrl+N still spawns, Ctrl+S (Pro) still saves
```

- [ ] **Step 4: Verify the mirror + suites**

Run: `cargo build && cargo test && cargo build --features pro && cargo test --features pro`
Then eyeball README rows vs HELP_ROWS strings — they must be byte-identical
(the `\u{b7}` is `·`).

- [ ] **Step 5: Commit**

```bash
git add README.md src/d2d.rs docs/manual-tests.md
git commit -m "docs(search): README/help/manual-tests rows in exact-wording sync; fix F1 help drift"
```

---

### Task 10: Final verification + roadmap tick

**Files:**
- Modify: `TODO.md`

- [ ] **Step 1: Full matrix**

Run all four, in order, each must be fully green:

```bash
cargo build
cargo test
cargo build --features pro
cargo test --features pro
```

Expected: zero warnings from pppdf's own code (the two upstream
`vendor/pdfium-render` dead-code warnings on full rebuilds are expected and
exempt), all unit + integration tests pass in both editions.

- [ ] **Step 2: Check the roadmap box**

In `TODO.md`, change `- [ ] Full text search` to `- [x] Full text search`.

- [ ] **Step 3: Smoke-run the real thing**

`cargo run -- tests/fixtures/form.pdf` (Pro build too if convenient):
Ctrl+F → type `name` → highlights + counter; Enter → reveal; Esc → closed.
Then run the "Text search (both editions)" manual checklist rows that touch
input routing (the full checklist runs before release).

- [ ] **Step 4: Commit**

```bash
git add TODO.md
git commit -m "docs: check off full-text search on the roadmap"
```

---

## Plan self-review notes (already applied)

- Spec coverage: every spec section maps to a task — protocol/worker (T1/T5/T6), state machine + stepping/pending-jump lifecycle (T3), geometry/landing (T2/T4), input ladder/openers/mouse/modality (T7), highlights/bar/draw-order (T8), docs three-surface sync + F1 drift (T9), TODO tick (T10). The Pro scan-loop duties are in T5 Step 4.4; the password-stash re-issue rule is T7 Step 6.
- Type consistency: `SearchOptions`/`SearchMatch` (types.rs) are consumed by name in T3/T4/T5/T7/T8; `QueryEffect`/`Step`/`bar_rect` (T3) consumed in T7/T8; `page_rects_window`/`goto_match`/`next_search_id`/`search_open`/`search_close`/`search_active` (T4) consumed in T7/T8. `scroll_to_reveal(scroll, target, page_px, client, margin)` (T2) is called only from `App::goto_match` (T4).
- Known judgment calls an implementer must NOT "fix": the bar is deliberately keyboard-only (clicks swallowed); `modal_active()` deliberately excludes search; superseded scans deliberately send no reply; `field_rects_window` keeps its cache gate while `page_rects_window` must not have one.






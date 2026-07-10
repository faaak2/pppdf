# Full-text search — design

Date: 2026-07-10 · Status: approved
Scope: browser-style Ctrl+F find bar with live whole-document search,
match highlighting, and next/prev navigation. Both editions, shared code
only. In-memory, per window, per document — nothing persisted.

## Goal

A reader presses Ctrl+F, types "grapple", and watches matches highlight
and a "3 / 17" counter build while the document is scanned live. Enter /
Shift+Enter (and F3 / Shift+F3) step through matches — the view keeps its
zoom and scrolls just enough to reveal each one. The bar is persistent
like a browser's: scrolling, clicking, and page flips leave it open; only
Esc (or a new document) closes it.

## Decisions (settled in brainstorming)

- **Live, whole document**: find-as-you-type; the worker scans page by
  page and streams hits, so counts and highlights build up live.
  Retyping cancels and restarts the scan.
- **Minimum query length 2**: a query shorter than 2 chars is treated
  exactly like an empty one — no scan is issued, matches clear, counter
  hidden. Find-as-you-type starts on the second character.
- **Persistent bar**: no dismissal matrix. Wheel, clicks, page flips,
  zooming all leave the bar open (unlike the modal goto prompt). Esc,
  Ctrl+W, or a doc load are the only ways it goes away.
- **Keep zoom, scroll into view**: match navigation preserves the zoom
  level and scrolls minimally to reveal the match (new pure
  `scroll_to_reveal` primitive in layout.rs). At fit, nothing moves —
  the current-match highlight just advances.
- **Typing never moves the view** (deliberate deviation from browsers):
  highlights and counter update live, but only Enter/F3 jump. Auto-jump
  per keystroke would either pollute page history (jumps record, per the
  v1.5.0 contract) or need a bespoke non-recording path, and yanking the
  page mid-read is wrong for a reading app.
- **v1 extras**: Ctrl+V paste into the query; remember the last query
  per document (restored on reopen); Alt+C match-case toggle; Alt+W
  whole-word toggle. Both toggles are PDFium-native flags.
- **Top-right floating bar**: browser placement; collides with nothing
  (badge bottom-right, goto pill bottom-center, TOC left).
- **Both editions**: TODO.md lists search under "Both editions" — no
  code in src/pro/, no new `cfg(feature = "pro")` beyond one arm in the
  already-gated form-typing rung.

## Approach (decided)

Streaming scan → per-document match store. The pdf worker scans with
PDFium's native matcher (`PdfPageText::search()`, safe API — **zero new
vendor patches**) and streams per-page hits over the existing reply
channel; the UI accumulates them into a match list on `DocState` —
per-document, immune to page-cache eviction, gone on doc load, survives
the password stash. Match ordinals ("3 / 17") fall out of the sorted
list, so the raw FFI (`FPDFText_GetSchResultIndex`) is not needed.

Rejected: piggybacking match rects on `PdfReply::Rendered` like
links/fields (matches would exist only for the 6 cached pages — no total
count, no cross-page next/prev); extracting text to the UI side and
searching in Rust (mapping hits back to highlight rects needs the
char-index remap through raw FFI per hit, plus PDFium's generated-chars
quirks — most complexity for marginal gain).

## Protocol & worker scan (src/types.rs, src/pdf.rs)

New shared (un-gated) variants:

```rust
PdfCmd::Search   { id: u32, query: String, options: SearchOptions,
                   start_page: u16 }
PdfCmd::CancelSearch

PdfReply::SearchHits { id: u32, page: u16, matches: Vec<SearchMatch> }
PdfReply::SearchDone { id: u32 }

pub struct SearchOptions { pub match_case: bool, pub whole_word: bool }
pub struct SearchMatch  { pub rects: Vec<RectF> }
```

`SearchMatch.rects` follow the exact `PageLink`/`FormField` convention:
PDF points, unrotated page frame, origin bottom-left (top > bottom
numerically). Multi-rect because one hit can span line wraps; the UI
paints every rect and navigates to the union bbox.

Worker rules:

- The worker keeps `active_search: Option<ScanState { id, query,
  options, next }>`. While a scan is active the loop does not block on
  `recv`; after each command batch it scans **one page at a time**,
  polling `cmd_rx` between pages (the render loop's existing preempt
  pattern). Renders in a batch always run before the scan resumes — a
  page flip never waits on more than one page's scan chunk.
- Per page: load page → `page.text()` → `search()` with the options →
  copy each hit's rects into plain structs → send `SearchHits` iff
  non-empty (with `wake()`) → drop the handles. No text-page caching in
  v1; a retype rescans.
- **Scan order starts at the current page and wraps** (`start_page..end`,
  then `0..start_page`) so the nearest hits stream first and Enter can
  jump almost immediately even mid-scan. The UI passes the current page
  as `start_page`; `ScanState.next` walks from there.
- `SearchDone { id }` is sent when the scan has covered every page.
- Coalescing: among queued `Search` commands only the **last** survives
  (visible-render semantics). `CancelSearch`, `Load`, and `Shutdown`
  kill the active scan and any queued searches. Superseded scans send
  **no** reply — unlike renders there is no pending-set to strand: the
  UI accepts only replies whose `id` matches its current search and
  drops everything else. The scanning indicator clears on
  `SearchDone{current id}` or local reset.
- A page whose text fails to load is skipped silently (corrupt-page
  render policy) and the scan continues; `SearchDone` still arrives.

## UI state & input (src/search.rs, src/app.rs, src/window.rs)

A new pure module `src/search.rs` (the overlay.rs pattern: no OS types,
panel-geometry constants + state machine, fully unit-testable):

```rust
pub struct SearchState {
    pub query: String,
    pub options: SearchOptions,
    pub id: u32,                        // current scan id
    pub matches: Vec<PageMatches>,      // sorted by page; streamed inserts
    pub current: Option<(u16, usize)>,  // (page, index within page)
    pub scanning: bool,
    pub replace_on_type: bool,          // pseudo-select-all armed
    pub pending_jump: bool,             // jump when the first hit arrives
}
pub struct PageMatches { pub page: u16, pub matches: Vec<SearchMatch> }
```

- Lives as `DocState.search: Option<SearchState>` (Some = bar open) —
  per-document, survives the password stash, dies on doc load. Closing
  the bar stashes `(query, options)` into `DocState.last_search` for
  restore on reopen (per-document memory). The monotonic id counter
  lives on `App` (like the render generation).
- Ordinal/counter math: sum sorted `matches` up to `current`. Current-
  match selection on a fresh scan: first hit at-or-after the current
  page — which, given wrap scan order, is simply the first hit received.

Input ladder (`on_key`) gains a rung **after Pro form typing, before
plain F1 / TOC / globals**: password → help → goto → form typing →
**search** → F1 → TOC → globals. A clicked form field wins the keyboard
(first Esc kills field focus, second closes the bar); the bar shadows
global plain-key shortcuts (G, R, F, 0, Space type into the query).

Keys while the bar is open:

| Input | Action |
|---|---|
| printables (WM_CHAR) | append iff `!ctrl && c >= 0x20` (one filter drops Enter/Tab/Esc/Backspace chars and Ctrl control-bytes; BMP-only, matching the password path's limitation) |
| Enter / Shift+Enter | next / prev match (wraps around) |
| F3 / Shift+F3 | next / prev match (same) |
| Esc | close bar → `CancelSearch`, stash query |
| Backspace | delete last char (clears all while replace-armed) |
| Ctrl+V | paste (`clipboard_text` moves out of its Pro gate) |
| Alt+C / Alt+W | toggle match-case / whole-word (WM_SYSKEYDOWN arms beside Alt+arrows); toggling rescans |
| PgUp / PgDn | **fall through** — page the document (the bar's one non-modal exception) |
| Tab, arrows, Home/End | swallowed (no caret model; surprise page-jumps mid-typing are worse than dead keys) |
| other Ctrl chords | **fall through** to the global rung — Ctrl+W quits, Ctrl+O/N open, Ctrl+S (Pro) saves |

Openers and select-all emulation:

- **Ctrl+F** (global rung, `0x46 if ctrl`, Doc mode only — like goto):
  open the bar, restore `last_search` if any, arm `replace_on_type`,
  issue a scan if the restored query is legal (≥ 2 chars). With the bar
  already open, Ctrl+F re-arms `replace_on_type`.
- `replace_on_type` armed: the first typed/pasted char **replaces** the
  whole query, Backspace **clears** it; either disarms the flag.
- **F3 with the bar closed**: reopen with the last query, rescan, set
  `pending_jump` — the first arriving hit lands immediately.
  `pending_jump` also covers Enter pressed mid-scan before any hit has
  arrived.
- Ctrl+F while the goto prompt is open cancels the prompt and opens
  search. Ctrl+F while a Pro form field is focused kills field focus and
  opens search (one arm in the already-gated form rung). F1 help stays
  fully modal — Ctrl+F is swallowed there.

Mouse and modality:

- No dismissal matrix (contrast `dismiss_goto_entry`'s five call sites —
  none apply to search).
- `wants_client_mouse` gains a bar-rect clause so clicks on the bar are
  swallowed rather than dragging the window; v1 bar controls are
  keyboard-only.
- `modal_active()` does **not** include search: Alt+arrows / mouse-X
  history nav works under the open bar. (Backspace-as-history is
  shadowed while the bar is focused; Alt+arrows still work.)

Query legality: `len < 2` ⇒ treated as empty — clear matches locally,
send `CancelSearch` if a scan was running, counter hidden. The worker
never sees a short query, so pdfium-render's empty-term error path is
never exercised.

## Geometry, highlights & the bar (src/layout.rs, src/app.rs, src/d2d.rs)

- **Un-gate the rect pipeline**: the Pro-only `field_rects_window()`
  transform generalizes to a shared `App::page_rects_window(&[RectF])`
  (PDF points → `pdf_rect_to_window` → `rotate_rect` → offset by
  `current_page_rect`). Pro field indication becomes a caller; search
  highlights are the second caller.
- Highlights draw only for the currently displayed page, straight from
  `DocState.search.matches` (eviction-proof), and only when the page
  bitmap is present — same gating as field indication. Client-side over
  the cached bitmap, never baked into rendered pixels.
- Colors: all matches translucent yellow `(1.0, 0.85, 0.2, 0.28)`;
  current match stronger orange `(1.0, 0.55, 0.1, 0.45)` — Chromium's
  scheme, distinct from Pro's pale-blue field wash.
- Draw order: page bitmap → field indication (Pro) → **search
  highlights** → TOC panel → badge → goto pill → **search bar** → dirty
  dot (Pro) → F1 help. The dirty dot draws above the bar (few px in the
  corner; manual-tests gets a visual-overlap row).
- The bar reuses the goto pill's styling family (rounded fill + border +
  text) at top-right: `Find: {query}_` (static underscore caret, house
  convention), the counter, and `[Aa] [W]` indicators lit when toggled.
  Counter states: `3 / 17…` while scanning; `3 / 17` when done; `0 / 0`
  tinted red when a legal query finished with no hits; hidden while the
  query is illegal (< 2 chars). Bar geometry constants live in
  search.rs so drawing (d2d.rs) and hit-testing (window.rs) share one
  definition.
- **`layout::scroll_to_reveal(scroll, target, viewport, margin) ->
  PointF`**: minimal scroll change bringing a target rect (window space,
  post-rotation — rotation handled by construction) fully into the
  viewport with a comfort margin; clamped by `clamp_scroll`; no-op when
  already visible. Pure, unit-tested.
- **Landing path**: `App::goto` keeps its public shape; internally it
  gains a landing variant so `App::goto_match(page, union_bbox_pt)`
  shares the same history-recording path (records iff the page changes —
  the v1.5.0 contract) but preserves zoom (a fit-multiplier, so it
  carries across pages of different sizes) and computes scroll via
  `scroll_to_reveal`. Same-page steps record nothing and only scroll.

## Edge cases

- **Stale replies** die by id-check; after a doc load the fresh DocState
  has no search state, so late hits are dropped the same way. Stale
  matches can never decorate a new document or a new query.
- **Password-stash trap**: a `Load` kills the active scan without a
  `SearchDone`; if that load lands in the password prompt and the user
  cancels, the stashed DocState returns with `scanning: true` forever.
  Rule: **any path that restores a stashed DocState re-issues an open
  search with a fresh id.**
- Wrap-around stepping: next past the last match wraps to the first;
  prev symmetrically (browser behavior).
- Rotation mid-search is safe by construction (rects stored unrotated;
  `rotate_cw` lands at fit; highlights and current match persist). Zoom,
  scroll, and resize invalidate nothing — highlights are recomputed per
  draw; no re-reveal on resize.
- Scanned/image-only PDFs and text with broken ToUnicode maps yield
  `0 / 0` — PDFium limitations, accepted. Text typed into Pro form
  fields is not in the static char stream until committed — known
  limitation, not chased in v1.
- Scale: no artificial cap on matches. A 2-char query on a 500-page book
  is a few MB of rect data on DocState worst-case; drawing touches only
  the visible page's list.

## UI & docs

Three surfaces gain search rows in exact-wording sync:

- README key table: `Ctrl+F`, `Enter / Shift+Enter / F3 / Shift+F3`,
  `Alt+C / Alt+W`, `Esc` rows in the house wording style.
- `HELP_ROWS` in d2d.rs mirrors them verbatim (free rows — nothing
  Pro-gated). While in there, fix the pre-existing one-word F1 drift
  ("Show / hide this help" vs README's "Show / hide the help overlay") —
  both files claim exact mirroring.
- docs/manual-tests.md gets a "Text search (both editions):" section —
  bar UX, highlights under rotation/zoom, dirty-dot overlap, persistent
  bar across wheel/click/flip, form-field/search keyboard handoff (Pro),
  password-stash restore re-issuing the scan.

TODO.md's "Full text search" box gets checked when this ships.

## Testing

- **Unit** (pure logic, no window):
  - search.rs: typing/backspace; the 2-char legality threshold;
    replace-on-type arming (open-with-restored-query, Ctrl+F-while-open;
    first char replaces, Backspace clears, disarm after); sorted
    streaming insertion; ordinal/counter math; current-match selection
    (first hit at-or-after current page); next/prev with wraparound;
    toggles forcing a rescan; id-mismatch drops; `done()` clearing
    `scanning`.
  - layout.rs: `scroll_to_reveal` — already-visible no-op, minimal delta
    each direction, margin, clamping, rect-larger-than-viewport.
  - app.rs: `goto_match` records history iff page changes, preserves
    zoom; same-page step records nothing; stash-restore re-issue rule.
- **Integration** (real pdfium, tests/pdf_integration.rs, under
  `PDFIUM_TEST_LOCK` + `join_pdf_thread()`, both editions):
  - New generated fixture in `%TEMP%` via tests/common/mod.rs — 3 pages
    of hand-authored text ops with known strings at known positions
    ("dragon" on pages 1 and 3, "Dragon" for case tests, a term absent
    entirely).
  - Scan streams `SearchHits` with correct pages/counts and sane rects
    (PDF-point bounds); `SearchDone` arrives.
  - Wrap-order scan starting mid-document.
  - match-case and whole-word options honored.
  - A second `Search` supersedes the first (ids differ; stale hits
    droppable). `CancelSearch` mid-scan → no further hits after drain.
    `Load` mid-scan kills the scan.
  - Renders stay interleaved: a `Render` sent mid-scan gets its reply
    before scan completion.
- **Manual** (docs/manual-tests.md additions as above).
- **Both editions**: `cargo build` and `cargo test`, each with and
  without `--features pro`.

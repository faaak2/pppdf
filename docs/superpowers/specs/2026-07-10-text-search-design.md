# Full-text search — design

Date: 2026-07-10 · Status: approved (revised after adversarial review)
Scope: browser-style Ctrl+F find bar with live whole-document search,
match highlighting, and next/prev navigation. Both editions, shared code
only. In-memory, per window, per document — nothing persisted.

## Goal

A reader presses Ctrl+F, types "grapple", and watches matches highlight
and a "3 / 17" counter build while the document is scanned live. Enter /
Shift+Enter (and F3 / Shift+F3) step through matches — the view keeps its
zoom and scrolls just enough to reveal each one. The bar is persistent
like a browser's: scrolling, clicking, and page flips leave it open; only
Esc — or the document going away — closes it.

## Decisions (settled in brainstorming)

- **Live, whole document**: find-as-you-type; the worker scans page by
  page and streams hits, so counts and highlights build up live.
  Retyping cancels and restarts the scan.
- **Minimum query length 2**: a query shorter than 2 chars is treated
  exactly like an empty one — no scan is issued, matches clear, counter
  hidden. Find-as-you-type starts on the second character.
- **Persistent bar**: no dismissal matrix. Wheel, clicks, page flips,
  zooming all leave the bar open (unlike the modal goto prompt). Esc or
  a doc load are the only ways it goes away (quitting aside).
- **Keep zoom, scroll into view**: match navigation preserves the zoom
  level and scrolls minimally to reveal the match (new pure
  `scroll_to_reveal` primitive in layout.rs). At fit, nothing moves —
  the current-match highlight just advances.
- **Typing never moves the view** (deliberate deviation from browsers):
  highlights and counter update live, but only Enter/F3 jump. Auto-jump
  per keystroke would either pollute page history (jumps record, per the
  v1.5.0 contract) or need a bespoke non-recording path, and yanking the
  page mid-read is wrong for a reading app.
- **v1 extras**: Ctrl+V paste into the query; remember the last query —
  restored when the bar is reopened while the same document is still
  open (per-document, in-memory, dies with the doc load like the rest of
  `DocState`); Alt+C match-case toggle; Alt+W whole-word toggle. Both
  toggles are PDFium-native flags.
- **Top-right floating bar**: browser placement; collides with nothing
  (badge bottom-right, goto pill bottom-center, TOC left).
- **Both editions**: TODO.md lists search under "Both editions" — no
  code in src/pro/, no new `cfg(feature = "pro")` beyond one arm in the
  already-gated form-typing rung and the Pro scan-loop duties below.

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
- **Pro scan-loop duties**: in the Pro build, `wait_cmd`'s timeout arm is
  the only place form timers fire and throttle-deferred unsolicited
  renders flush. Between scan pages the loop must perform the same
  duties (fire due form timers, `maybe_render_unsolicited`) — otherwise
  a focused form field's caret freezes for the duration of a long scan.
- Per page: load page → `page.text()` → `search()` with the options →
  copy each hit's rects into plain structs → send `SearchHits` iff
  non-empty → drop the handles. **Every search reply — Hits and Done —
  is sent with `wake()`.** No text-page caching in v1; a retype rescans.
- **Scan order starts at the current page and wraps** (`start_page..end`,
  then `0..start_page`) so the nearest hits stream first and Enter can
  jump almost immediately even mid-scan. The UI passes the current page
  as `start_page`; `ScanState.next` walks from there.
- `SearchDone { id }` is sent when the scan has covered every page.
- Coalescing is **order-sensitive, like `Load`**: a `CancelSearch` kills
  the active scan and any `Search` queued *before* it in the batch; a
  `Search` that follows a `CancelSearch` in the same batch survives.
  Among surviving `Search` commands only the **last** runs
  (visible-render semantics). `Load` and `Shutdown` kill the active scan
  and every queued search regardless of order. Superseded scans send
  **no** reply — unlike renders there is no pending-set to strand: the
  UI accepts only replies whose `id` matches its current search and
  drops everything else (in-flight stragglers). The scanning indicator
  clears on `SearchDone{current id}` or local reset.
- A page whose text fails to load is skipped silently (corrupt-page
  render policy) and the scan continues; `SearchDone` still arrives.

## UI state & input (src/search.rs, src/app.rs, src/window.rs)

A new pure module `src/search.rs` (the overlay.rs pattern: no OS types,
bar-geometry constants + state machine, fully unit-testable):

```rust
pub struct SearchState {
    pub query: String,
    pub options: SearchOptions,
    pub id: u32,                        // current scan id
    pub matches: Vec<PageMatches>,      // sorted by page; streamed inserts
    pub current: Option<(u16, usize)>,  // (page, index within page)
    pub scanning: bool,
    pub landed: bool,                   // navigated at least once this scan
    pub replace_on_type: bool,          // pseudo-select-all armed
    pub pending_jump: bool,             // jump fires on the next arriving hit
}
pub struct PageMatches { pub page: u16, pub matches: Vec<SearchMatch> }
```

- Lives as `DocState.search: Option<SearchState>` (Some = bar open) —
  per-document, survives the password stash, dies on doc load. Closing
  the bar stashes `(query, options)` into `DocState.last_search` for
  restore when the bar reopens in the same still-open document. The
  monotonic id counter lives on `App` (like the render generation).
- **New-scan reset (the invariant)**: issuing a new `Search` — every
  retype keystroke, paste, Alt+C/Alt+W toggle, or stash-restore re-issue
  — atomically bumps `id`, clears `matches`, sets `current = None`,
  `landed = false`, `pending_jump = false`, `scanning = true`. The
  id-check on replies exists only for in-flight stragglers; local reset
  is what keeps a new query's results clean. (The F3-reopen path sets
  `pending_jump` *after* this reset.)
- Current-match selection: `current` auto-selects to **the first hit
  received for this scan id** — given wrap scan order, the nearest hit
  at-or-after `start_page`. Page flips during the scan don't re-anchor
  it (accepted). Auto-selection never navigates; it feeds the counter
  and the orange highlight only.
- Ordinal/counter math: sum sorted `matches` up to `current`.

Input ladder (`on_key`) gains a rung **after Pro form typing, before
plain F1 / TOC / globals**: password → help → goto → form typing →
**search** → F1 → TOC → globals. A clicked form field wins the keyboard
(first Esc kills field focus, second closes the bar); the bar shadows
global plain-key shortcuts (G, R, F, 0, Space type into the query).

Keys while the bar is open:

| Input | Action |
|---|---|
| printables (WM_CHAR) | append iff `!ctrl && c >= 0x20` (one filter drops Enter/Tab/Esc/Backspace chars and Ctrl control-bytes; BMP-only, matching the password path's limitation) |
| Enter / Shift+Enter | first press this scan reveals the current match; afterwards next / prev (see stepping rules) |
| F3 / Shift+F3 | same as Enter / Shift+Enter |
| Esc | close bar → `CancelSearch`, stash query |
| Backspace | delete last char (clears all while replace-armed) |
| Ctrl+V | paste (`clipboard_text` moves out of its Pro gate) |
| Alt+C / Alt+W | toggle match-case / whole-word (WM_SYSKEYDOWN arms beside Alt+arrows); toggling rescans |
| PgUp / PgDn | fall through — page the document (the only *unmodified* keys that fall through) |
| Tab, arrows, Home/End | swallowed (no caret model; surprise page-jumps mid-typing are worse than dead keys) |
| other Ctrl chords | fall through to the global rung — Ctrl+W quits, Ctrl+O/N open, Ctrl+S (Pro) saves |

Stepping rules (`landed` / `pending_jump` lifecycle):

- **First activation**: Enter (or F3, or their Shift variants) with
  `landed == false` jumps to `current` if it exists, setting `landed`.
  If no hit has arrived yet, it sets `pending_jump` instead — the first
  arriving hit auto-selects, jumps, and sets `landed`. Both paths land
  on the same match; the outcome no longer depends on reply timing.
- **Advancing**: with `landed == true`, Enter/F3 step forward,
  Shift+Enter/Shift+F3 step backward from `current`.
- **Streaming frontier**: while scanning, stepping forward from the last
  *received* match sets `pending_jump` — it fires on the next arriving
  hit past `current`; if `SearchDone` arrives with it still set, wrap to
  the first match and clear it. Stepping backward from the first match
  wraps to the last received match (mid-scan: the frontier). After
  `SearchDone`, both directions wrap normally.
- **`pending_jump` reset set**: cleared when its jump fires, on
  `SearchDone` (wrapping if the frontier case applies), and by the
  new-scan reset (any query edit, toggle, or local reset) — so a
  keystroke can never inherit a stale armed jump and auto-navigate,
  which would violate "typing never moves the view". Only Enter/F3 (and
  the F3-reopen path) set it.

Openers and select-all emulation:

- **Ctrl+F** (global rung, `0x46 if ctrl`, Doc mode only — like goto):
  open the bar, restore `last_search` if any, and issue a scan if the
  restored query is legal (≥ 2 chars). With the bar already open, Ctrl+F
  re-arms `replace_on_type`. Ctrl+F with the TOC panel open **closes the
  panel** first (the same courtesy as cancelling the goto prompt).
- **Any open that restores a non-empty query arms `replace_on_type`**
  (Ctrl+F and F3 alike): while armed, the first typed/pasted char
  **replaces** the whole query, Backspace **clears** it; either disarms.
- **F3 with the bar closed**: reopen with the last query, rescan, set
  `pending_jump` — the first arriving hit lands immediately. F3 with no
  remembered query opens an empty bar (no scan, no `pending_jump`).
- Ctrl+F while the goto prompt is open cancels the prompt and opens
  search. Ctrl+F while a Pro form field is focused kills field focus and
  opens search (one arm in the already-gated form rung). F1 help stays
  fully modal — Ctrl+F is swallowed there.
- TOC keyboard ownership: while the bar is open it holds Enter/arrows/
  Tab, so a TOC panel opened by mouse (context menu) is **mouse-only**
  until the bar closes; Tab-to-open is swallowed. Ctrl+F closing the
  panel (above) keeps the common case from arising.

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
never exercised. Query length is capped at 256 chars (bounds pasted
text; the password path's no-cap precedent doesn't survive contact with
Ctrl+V).

## Geometry, highlights & the bar (src/layout.rs, src/app.rs, src/d2d.rs)

- **Un-gate the rect pipeline**: the Pro-only `field_rects_window()`
  transform generalizes to a shared `App::page_rects_window(&[RectF])`
  (PDF points → `pdf_rect_to_window` → `rotate_rect` → offset by
  `current_page_rect`). Pro field indication becomes a caller; search
  highlights are the second caller. **The shared helper is pure geometry
  with no cache-presence gate** (its progenitor bails without a cached
  bitmap; the generalization must not) — `goto_match` needs window-space
  bboxes for pages that have never rendered, and `current_page_rect` is
  pure. The bitmap gate applies only at the highlight-draw call site.
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
  query is illegal (< 2 chars). **Bar width clamps to the client width
  minus margins** (the goto pill precedent — the 200-DIP minimum window
  is narrower than the natural bar) and the query display elides its
  head so the tail stays visible. Bar geometry constants and the
  clamped rect live in search.rs so drawing (d2d.rs) and the
  `wants_client_mouse` hit-test (window.rs) share one definition — the
  hit-test rect **is** the clamped drawn rect.
- **`layout::scroll_to_reveal(scroll, target, page_px, client, margin)
  -> PointF`**: minimal scroll change bringing a target rect (window
  space under the input scroll, post-rotation — rotation handled by
  construction) fully into the viewport with a comfort margin; the
  result passes through `clamp_scroll(page_px, client, ·)` (hence the
  `page_px` parameter); no-op when already visible. Pure, unit-tested.
- **Landing path**: `App::goto` keeps its public shape; internally it
  gains a landing variant so `App::goto_match(page, union_bbox_pt)`
  shares the same history-recording path (records iff the page changes —
  the v1.5.0 contract) but preserves zoom and computes scroll via
  `scroll_to_reveal`. Precisely:
  - The carried zoom (a fit-multiplier) is **clamped to the target
    page's `max_zoom`** on landing — an unclamped carry would let
    highlight geometry (`fit_scale × zoom`, uncapped) drift from the
    rendered bitmap (`render_size`, capped) by the cap ratio. The
    highlight/reveal transform must use the same px basis as
    `render_size`.
  - **Cross-page baseline**: on a page change the landing variant sets
    `scroll = (0,0)` (clamped), computes the union bbox's window rect
    under that baseline, then applies `scroll_to_reveal` from it — never
    from the old page's scroll, which is meaningless in the new page's
    overhang range. Same-page steps use the live scroll.

## Edge cases

- **Stale results cannot survive** — by two cooperating mechanisms: the
  new-scan reset clears accumulated matches locally the moment a new
  query/toggle/re-issue happens, and the id-check drops in-flight
  stragglers from superseded scans. After a doc load the fresh DocState
  has no search state at all.
- **Password-stash trap**: a `Load` kills the active scan without a
  `SearchDone`; if that load lands in the password prompt and the user
  cancels, the stashed DocState returns with `scanning: true` forever.
  Rule: **any path that restores a stashed DocState re-issues an open
  search with a fresh id** (the new-scan reset applies).
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
  Pro-gated). While in there: d2d.rs's HELP_ROWS comment claims exact
  mirroring of the README key map, but its F1 row has drifted ("Show /
  hide this help" vs README's "Show / hide the help overlay") — fix the
  drift.
- docs/manual-tests.md gets a "Text search (both editions):" section —
  bar UX, highlights under rotation/zoom, dirty-dot overlap, persistent
  bar across wheel/click/flip, form-field/search keyboard handoff (Pro),
  password-stash restore re-issuing the scan, bar clamping at the
  200-DIP minimum window.

TODO.md's "Full text search" box gets checked when this ships.

## Testing

- **Unit** (pure logic, no window):
  - search.rs: typing/backspace; the 2-char legality threshold; the
    256-char cap; **new-scan reset clears prior matches/current before
    the first hit arrives**; replace-on-type arming (any restored-query
    open, Ctrl+F-while-open; first char replaces, Backspace clears,
    disarm after); sorted streaming insertion; ordinal/counter math;
    current-match auto-selection (first hit received, no navigation);
    **first activation reveals `current`, second press advances**;
    **frontier stepping: forward arms `pending_jump`, `SearchDone`
    wraps; backward wraps to last received**; **`pending_jump` reset:
    zero-hit scan then retype does not auto-jump**; wraparound when
    done; toggles forcing a rescan (through the reset); id-mismatch
    drops; `done()` clearing `scanning`; F3-reopen with no remembered
    query opens an empty bar.
  - layout.rs: `scroll_to_reveal` — already-visible no-op, minimal delta
    each direction, margin, clamping via `clamp_scroll`,
    rect-larger-than-viewport.
  - app.rs: `goto_match` records history iff page changes; preserves
    zoom **clamped to the target page's max_zoom**; **cross-page landing
    computes reveal from the zero-scroll baseline**; **works with an
    empty page cache** (no bitmap needed for the bbox transform);
    same-page step records nothing; stash-restore re-issue rule.
- **Integration** (real pdfium, tests/pdf_integration.rs, under
  `PDFIUM_TEST_LOCK` + `join_pdf_thread()`, both editions):
  - New generated fixture in `%TEMP%` via tests/common/mod.rs — 3 pages
    of hand-authored text ops with known strings at known positions
    ("dragon" on pages 1 and 3, "Dragon" for case tests, a term absent
    entirely).
  - Scan streams `SearchHits` with correct pages/counts and sane rects
    (PDF-point bounds); `SearchDone` arrives (with a wake per reply).
  - Wrap-order scan starting mid-document.
  - match-case and whole-word options honored.
  - A second `Search` supersedes the first (ids differ; stale hits
    droppable). `CancelSearch` mid-scan → no further hits after drain.
    **`CancelSearch` immediately followed by `Search` in one burst →
    the later id still gets hits and `SearchDone`** (order-sensitive
    coalescing). `Load` mid-scan kills the scan.
  - Renders stay interleaved: a `Render` sent mid-scan gets its reply
    before scan completion.
- **Manual** (docs/manual-tests.md additions as above).
- **Both editions**: `cargo build` and `cargo test`, each with and
  without `--features pro`.

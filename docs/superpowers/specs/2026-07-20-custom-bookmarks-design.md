# Custom bookmarks (Pro) — design

Date: 2026-07-20. Status: approved design, pre-implementation.

Pro users toggle a named bookmark on the current page with **Ctrl+B**. Bookmarks
are stored *inside the PDF file* and appear in the TOC panel above the
document's own outline, sorted by name. The free edition displays them
read-only.

## Decisions taken (with the user)

- **Persistence: in the PDF file itself** — not a sidecar, not session-only.
- **Mechanism: an embedded attachment** (`pppdf.bookmarks`) written through
  PDFium's attachment API. PDFium cannot write `/Outlines` (all seven
  `FPDFBookmark_*` functions are read-only in every binding generation,
  `vendor/pdfium-render/src/bindings.rs:5792-5920`), and hand-rolled outline
  splicing was rejected: it needs a private PDF writer, breaks on encrypted
  documents, and risks corrupting user files. Consequence, accepted: bookmarks
  are visible only in pppdf; other viewers merely list the attachment.
- **Auto-save on toggle, bookmarks only.** Ctrl+B writes the file immediately,
  but must never persist pending form edits — those remain Ctrl+S's job.
- **TOC layout: "Bookmarks" section on top**, document outline below.
- **Sort: by name, natural order** — digit runs compare numerically
  ("1 - x" above "10 - x").

## Data model

- `types.rs`: `Bookmark { page: u16, name: String }` — unconditional shared
  type (the `FormField` precedent: free needs it to display).
- `DocState.bookmarks: Vec<Bookmark>`, kept **sorted by page** (page is the
  toggle identity; display order is computed separately). Per-document,
  survives the password stash, rebuilt from the file on every load.
- **One bookmark per page** — Ctrl+B toggles it.
- Names: ≤ 64 chars; the WM_CHAR intake filter (`!ctrl && c >= 0x20`) already
  excludes control chars, tabs and newlines, which keeps the file format safe.
  BMP-only input is an accepted, pre-existing limitation.

## File format (attachment `pppdf.bookmarks`)

UTF-8 text, hand-rolled in the house style (no serde; the `assoc_values`
pure-format-plus-tests pattern):

```
pppdf-bookmarks v1
12	Fireball table
70	Chapter 4 notes
```

- Header line, then one `page<TAB>name` line per bookmark.
- Page numbers are **1-based in the file** (human-readable if extracted);
  0-based everywhere in code.
- Lenient parse: malformed lines skipped; out-of-range pages **dropped** at
  load against the real page count (clamping could collide with an existing
  bookmark on the last page); unknown header version → read as empty.
- Removing the last bookmark **deletes the attachment** rather than leaving an
  empty blob.

## Sort order (TOC display)

Natural, case-insensitive comparison, hand-rolled pure function:

- Split names into alternating runs of ASCII digits and non-digits.
- Digit runs compare **numerically**: leading zeros stripped, then by length of
  significant digits, then lexically ("7" == "007" numerically; "2" < "10").
  Only ASCII digits are numeric; Unicode digits compare as text.
- Non-digit runs compare case-insensitively (`char::to_lowercase`).
- Full ties fall back to page number, then raw name (total order, stable).

Applied when building the panel list; `DocState.bookmarks` itself stays
page-sorted.

## Protocol & threading

- `PdfReply::Loaded` gains `bookmarks: Vec<Bookmark>` — **unconditional**;
  `describe()` (pdf.rs) reads and parses the attachment in both editions.
- New, Pro-gated (`#[cfg(feature = "pro")]`):
  - `PdfCmd::SaveBookmarks { bookmarks: Vec<Bookmark> }` — always the full
    set, never deltas (idempotent, self-healing).
  - `PdfReply::BookmarksSaved { result: Result<(), String> }`.

  Protocol invariants (the "every pending entry clears" rule applies —
  `saves_inflight` is incremented per command sent, so a dropped command
  would block window destruction forever):
  - **Exactly one `BookmarksSaved` reply per `SaveBookmarks` command.**
    Never coalesced, never silently dropped — each is processed FIFO
    (a save is one shadow load+write, far faster than a human can toggle).
  - **Ordering vs `Load`:** a `SaveBookmarks` queued before a `Load` must be
    executed against the *old* document/path — or, if the batch handling
    can't guarantee that, answered with `BookmarksSaved { Err }` — never
    written against the newly loaded document. (The rare error toast landing
    after a doc switch is accepted noise.)

**The double-write** (PDF thread, on `SaveBookmarks`):

1. Serialize the set; update/replace the `pppdf.bookmarks` attachment on the
   **live document** in memory — so any later Ctrl+S form save carries current
   bookmarks and can never wipe them.
2. **Shadow-load the on-disk bytes** (`fs::read(current_path)` →
   `load_pdf_from_byte_vec`), update the attachment there, save the shadow via
   the existing temp-file + atomic-rename + antivirus-retry idiom. The disk
   bytes lack uncommitted form edits by definition — this is what enforces
   "save only bookmarks".
3. Drop the shadow; reply `BookmarksSaved`.

Supporting changes:

- The PDF thread retains the load **password** alongside `current_path`
  (pro-gated thread-local) so shadow-loading encrypted documents works.
  PDFium handles decrypt/re-encrypt transparently on load/save.
- Bookmark saves increment the existing `saves_inflight` counter (window
  destruction must not kill the thread mid-write), but `BookmarksSaved`
  neither toasts "Saved" nor touches `form_dirty`. On error it restores
  nothing — see Failure handling.

## Input UX

**Pro — Ctrl+B**, new arm in `on_key`'s global rung (beside Ctrl+W/S/N/O,
window.rs:1309-1321). The find bar's `_ if ctrl => return false` fall-through
(window.rs:2097) makes the chord work while the bar is open, with zero extra
code. Gated on Doc mode with pages > 0.

- Page already bookmarked → remove, `SaveBookmarks`, toast *"Bookmark
  removed"* — the same bottom-right badge pill the goto-page navigation
  shows (`show_toast` → `draw_badge`, hold-then-fade timing, no new UI).
- Not bookmarked → open the **name prompt**: `DocState.bookmark_entry:
  Option<String>` (pro-gated), modeled exactly on the goto-entry pattern
  (start/active/char/backspace/cancel/commit on `App`, own modal rung beside
  the goto rung, everything else swallowed). Opening it kills form focus and
  closes the TOC panel. Text arrives via a new consuming WM_CHAR branch (the
  find bar's filter), 64-char cap.
- Enter commits; **empty buffer commits the default name "Page N"** (1-based,
  shown as placeholder in the prompt). Esc cancels — no bookmark. Ctrl+F
  inside the prompt cancels it and opens find (the goto precedent).
- Commit → insert page-sorted, `SaveBookmarks`, toast *"Bookmarked: {name}"*.
- Drawn as a bottom-center pill like `draw_goto` (`draw_bookmark_entry`,
  pro-gated).

**Free — Ctrl+B** toasts *"Custom bookmarks are a pppdf Pro feature"* —
**every press**, not one-shot (a deliberate chord deserves feedback each
time, unlike the incidental form-field click hint). `#[cfg(not(feature =
"pro"))]` arm; no new state.

## TOC integration (shared code)

`App::toggle_overlay` builds the panel list as:

```
[ "Bookmarks" header row ]          (only when bookmarks exist; depth 0, page None)
[ bookmark rows, depth 1, name-sorted ]
[ document outline, unchanged ]
```

- `OutlineItem` stays untouched (it crosses the thread boundary). The header
  is marked UI-side: `OutlineState` records the header row index so d2d can
  style it and activation can special-case it.
- **Activating a `page: None` row toggles collapse instead of closing the
  panel** — fixing the current close-unconditionally behavior at both call
  sites (window.rs:1433-1442 keyboard, 1683-1690 mouse). Bookmark rows
  navigate through `App::goto` — history recording for free.
- Header auto-expands (depth-0 rule); chevron collapse works unchanged.
- Empty-state message "No table of contents" only when outline *and*
  bookmarks are both empty.
- Free edition renders bookmarks read-only with no extra code.

## Failure handling

- Save failure (read-only file, AV-lock retries exhausted, shadow-load
  failure): the bookmark **stays in `DocState`** — the TOC keeps working all
  session — plus an error toast ("Couldn't save bookmarks"). The next
  successful toggle rewrites the full set, so the disk self-heals.
- Two pppdf windows on the same file: last-writer-wins, accepted (one process
  per window is the architecture; concurrent writers are already the norm).
  A stale window's later Ctrl+S can regress the other window's bookmarks —
  same class of race, same acceptance.
- Zero-page documents: Ctrl+B is a no-op (the flip/goto guard pattern).

## Out of scope (YAGNI)

Rename (remove + re-add covers it), panel-side deletion, nested bookmarks,
capturing zoom/scroll (a bookmark is a page, like History entries), exporting
as real `/Outlines` for other viewers, sidecar fallback.

## Testing

- `app.rs` `pro_tests`: toggle add/remove, entry flow (char/cap/commit/cancel,
  default name), doc_loaded clears, password-stash survival, zero-page guard,
  bookmark-activate records history.
- Natural-sort comparator: dedicated unit tests ("2" < "10", "007" == "7"
  numerically, case-insensitivity, tie → page order).
- Format: serialize/parse round-trip, lenient-parse cases (malformed lines,
  unknown version, out-of-range pages).
- `overlay.rs`: section building, header activate-toggles, bookmark-row
  activate returns page, both-empty message condition.
- Integration (`tests/pdf_integration.rs`, Pro, under `PDFIUM_TEST_LOCK` with
  `join_pdf_thread()`): save → reload → `Loaded` carries bookmarks;
  **bookmarks-only save excludes an unsaved form edit** (fill a field, don't
  Ctrl+S, toggle a bookmark, reload: bookmark present, field empty); Ctrl+S
  after toggling preserves both; remove-all deletes the attachment.
- Both editions must build and test green (`cargo build`/`cargo test` ±
  `--features pro`).

## Documentation touch-list

README keybinding table (`Ctrl+B — Add / remove bookmark (Pro)`), byte-identical
`PRO_HELP_ROWS` entry (minus the "(Pro)" suffix), `docs/manual-tests.md`
(Pro section rows: toggle, prompt, TOC section, persistence across reopen,
free-edition toast; add Ctrl+B to the find-bar-open chord check), TODO.md
(tick "Set custom bookmarks").

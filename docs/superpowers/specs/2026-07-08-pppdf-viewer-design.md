# pppdf — Borderless Native PDF Viewer for Tabletop RPG Sessions

**Date:** 2026-07-08
**Status:** Approved design, pending implementation plan
**Platform:** Windows 10/11 x64 only

## Purpose

A minimal, fully native PDF viewer for people playing pen & paper RPGs at their
computer. The core idea: the PDF page *is* the window. No chrome, no toolbars,
no borders — you snap a rulebook to half the screen next to your VTT or notes
and it looks like a page lying on the desktop.

## Requirements (as agreed)

- Full PDF rendering (text, vector art, images, embedded fonts) via PDFium
- Clickable in-document links (internal GoTo destinations and URI links)
- Table of contents = the PDF's embedded outline ("bookmarks" in Adobe
  terminology). No user-created bookmarks, no persistence layer.
- **Borderless above all**: zero pixels of window chrome, ever
- Pages render fit-to-window by default, always crisp (rendered at exact
  physical pixel size, never bitmap-scaled except transiently)
- Native Win32 — no web technologies, no UI framework, no runtime dependency
- As lightweight as possible: target ~1.5 MB exe + ~6 MB pdfium.dll
- Single page displayed at a time, snap page navigation (no continuous scroll)
- One window per PDF; multiple books = multiple instances
- Zoom: Ctrl+wheel at cursor, drag-pan while zoomed, reset to fit

Out of scope (YAGNI): text selection, search, printing, annotations, form
filling, tabs, session persistence, settings/config files, installers.

## Stack

| Piece | Choice | Notes |
|---|---|---|
| Language | Rust (stable, 2021 edition) | |
| Windowing / input | `windows` crate (windows-rs), raw Win32 | `WS_POPUP` borderless window |
| Drawing | Direct2D + DirectWrite via `windows` crate | page blit + translucent overlay + text |
| PDF engine | `pdfium-render` crate binding `pdfium.dll` | prebuilt BSD binary from bblanchon/pdfium-binaries, shipped next to the exe |
| Build | `cargo build --release` | no build-time codegen beyond crates |

No other significant dependencies. Anything tempted in (logging frameworks,
config crates, UI toolkits) is rejected by default.

## Architecture

Two threads, message-passing, no shared mutable state:

```
UI thread (main)                     PDF thread
──────────────────                   ─────────────────────────────
Win32 window + message loop          owns Pdfium, PdfDocument, all
Direct2D device & drawing            PDFium objects (not thread-safe)
input → Command ──── mpsc channel ──→ handles LoadDoc / Goto /
                                      Resized / QueryOutline …
bitmap cache (UI-side LRU)  ←── mpsc + PostMessage(WM_APP_*) wake ──
                                      replies: RenderedPage { page#,
                                      pixels, size }, OutlineTree,
                                      LinkRects, DocInfo, Error
```

- **UI thread** never calls PDFium. It draws whatever bitmap it has, handles
  all input, and owns the page-bitmap LRU cache (~4 entries at current size).
- **PDF thread** owns the `Pdfium` instance and document. Processes commands
  in order but drains the channel first: a newer `Resized`/`Goto` supersedes
  queued stale render requests (renders are cancelled by obsolescence, not
  interrupted mid-render).
- After delivering the visible page, the PDF thread prefetches pages ±1 at the
  same size so page flips feel instant.
- Wake mechanism: PDF thread calls `PostMessage(hwnd, WM_APP_RENDER_DONE, …)`;
  payload travels through the mpsc channel, the message is only a wake signal.

### Modules

| Module | Responsibility | Key interface |
|---|---|---|
| `main.rs` | entry, CLI arg (PDF path), spawn threads, run loop | — |
| `window.rs` | window class, WndProc, borderless tricks, input routing | translates WM_* → `AppEvent` |
| `app.rs` | application state machine: current page, zoom, overlay state, cache | `handle(AppEvent) → [Command / redraw]` |
| `d2d.rs` | Direct2D/DirectWrite setup, draw page bitmap, overlay panel, page badge | `draw(&Scene)` |
| `pdf.rs` | PDF thread: pdfium lifecycle, render, outline & link extraction | `Command` in, `Reply` out |
| `overlay.rs` | outline tree model: flatten, expand/collapse, scroll, hit-test, key nav | pure logic, unit-testable |
| `layout.rs` | fit math, zoom/pan transform, pan clamping, hit zones | pure functions, unit-testable |

`app.rs` + `layout.rs` + `overlay.rs` are pure logic (no OS calls) so the
interesting behavior is unit-testable without a window.

## Borderless Window (the flagship feature)

- Style `WS_POPUP | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX`;
  `WM_NCCALCSIZE` returns the full window rect as client area → **zero
  visible chrome** while keeping native resize, Aero Snap, Win+Arrow keys,
  minimize animation, and taskbar behavior.
- `WM_NCHITTEST` implements the invisible controls:
  - outer 8 px band → `HTLEFT/HTRIGHT/HTTOP/…` (native resize grips)
  - cursor inside a link rect (at fit zoom, overlay closed) → `HTCLIENT`
  - overlay panel open and cursor over it → `HTCLIENT`
  - zoomed in past fit → `HTCLIENT` (drag pans; Ctrl+drag moves window via
    programmatic `HTCAPTION`)
  - otherwise → `HTCAPTION` (drag anywhere to move, native snap included)
- `DWMWA_WINDOW_CORNER_PREFERENCE = DONTROUND` — square corners so the page
  is a perfect rectangle (Win11 rounds by default). *(If the rounded look is
  preferred later it's a one-line change.)*
- Right-click anywhere → native `TrackPopupMenu`: **Open…** (Ctrl+O),
  **Table of Contents** (Tab), **Fit to Window** (0), **Minimize**, **Close**
  (Esc).
- Per-Monitor V2 DPI awareness declared in the application manifest;
  `WM_DPICHANGED` re-renders at the new scale.
- Min window size clamped (e.g. 200×200) via `WM_GETMINMAXINFO`.

### Input map

| Input | Action |
|---|---|
| PgDn / → / Space / wheel down (at fit) | next page |
| PgUp / ← / Shift+Space / wheel up (at fit) | previous page |
| wheel (zoomed) | scroll page vertically; extra notch at page edge flips page |
| Home / End | first / last page |
| Ctrl+wheel | zoom at cursor (fit … 500%) |
| drag (zoomed) | pan (clamped to page bounds) |
| double-click or `0` | reset to fit-to-window (link hit wins over double-click if the first click lands on a link) |
| Tab | toggle TOC overlay |
| ↑↓←→ / Enter (overlay open) | navigate tree / jump |
| Esc | close overlay → else quit |
| Ctrl+O | open file dialog (replaces current doc) |
| Ctrl+W | quit |
| F11 | toggle fill-work-area (borderless "maximize") |
| drop .pdf file | open it (replaces current doc) |
| left-click on link | follow (internal → jump; URI → default browser) |

When zoomed, wheel scrolls the page vertically; at the page edge a further
wheel notch flips the page and resets scroll — matching snap-page semantics.

## Rendering

- **Fit computation** (`layout.rs`): aspect-fit the page's point size into the
  client rect in physical pixels; PDFium renders directly at that pixel size.
  The displayed bitmap is therefore always 1:1 — crisp at every DPI.
- Zoom multiplies the fit size; render happens at the zoomed pixel size (page
  bitmaps at 500% of a huge art page can be large — cap render dimension at
  ~16k px per side, beyond that clamp zoom).
- **Live resize**: draw the current cached bitmap scaled (cheap, slightly
  soft), debounce ~150 ms after the last `WM_SIZE`, then request a crisp
  re-render at the settled size.
- **Cache**: UI-side LRU keyed by (page, pixel size), ~4 entries. Resize and
  zoom invalidate by size mismatch naturally (stale entries age out).
- **Zoom interaction**: on Ctrl+wheel, immediately draw the cached bitmap
  scaled around the cursor point (soft preview), request crisp render; swap
  when it arrives. Pan uses the already-rendered zoomed bitmap.
- Direct2D `ID2D1HwndRenderTarget` (simple, sufficient; no swap-chain
  composition needed). Background around the page: near-black `#1a1a1a`
  letterbox bars when aspect ratios differ.
- Links: with each rendered page the PDF thread sends that page's link rects
  (in page space) + destinations. `layout.rs` transforms them to window space
  for hit-testing (hover cursor: hand) at any zoom.

## TOC Overlay

- Tab / menu opens a panel over the left edge: ~320 DIP wide, full height,
  ~85% opaque dark background, DirectWrite text (Segoe UI Variable, system
  fallback Segoe UI).
- Content: the PDF outline tree. Nodes expand/collapse (click the chevron or
  ←/→ keys); top-level nodes start expanded, deeper levels collapsed.
- Mouse: wheel scrolls, click a title jumps to its page and closes the panel,
  click a chevron toggles expansion without jumping.
- Keyboard: ↑/↓ move selection, →/← expand/collapse, Enter jumps + closes,
  Esc closes without jumping.
- PDF with no outline: panel shows a single dim line "No table of contents".
- **Page badge**: on every page change a small "42 / 320" pill fades in at the
  bottom-right corner and fades out after ~1 s. It is the only transient UI.

## Error Handling

| Failure | Behavior |
|---|---|
| pdfium.dll missing | message box explaining the dll must sit next to the exe; exit |
| file missing/unreadable/not a PDF | native message box with the reason; keep current doc if one is open, else stay open with a dark empty window + hint text "Right-click or drop a PDF" |
| password-protected PDF | minimal native password prompt (small dialog), retry on submit, cancel = treat as unopenable |
| corrupt page content | PDFium renders what it can (typically partial/blank page) — accept silently |
| render request for out-of-range page | clamped in `app.rs`, cannot reach PDF thread |
| PDF thread panic | message box + clean exit (no zombie window) |
| URI link | open via `ShellExecuteW` with the system default handler; non-http(s) schemes ignored |

## Testing

- **Unit tests** (pure logic, no window): fit/zoom/pan math incl. clamping,
  NCHITTEST zone resolution, LRU eviction, outline tree flatten/expand/
  key-navigation, link rect transform, wheel-at-edge page-flip rule.
- **Integration smoke test** (`cargo test`, requires pdfium.dll in target
  dir): load a small fixture PDF (checked into `tests/fixtures/`, built with
  outline + internal link + URI link), assert page count, non-blank rendered
  pixels, outline titles, link destinations.
- **Manual checklist** (documented in `docs/manual-tests.md`): Aero snap all
  edges, Win+arrows, drag between monitors of different DPI, resize from all
  8 grips, taskbar minimize/restore, F11 on multi-monitor, drop file, open
  via file association, two instances side by side.

## Distribution

`cargo build --release` produces `pppdf.exe`. A small `fetch-pdfium.ps1`
script downloads the matching `pdfium.dll` release from
bblanchon/pdfium-binaries into `target/release/`. "Install" = copy the two
files anywhere; optional "Open with" association done by the user in Explorer.

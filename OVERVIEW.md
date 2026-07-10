# pppdf — Codebase Overview

An orientation document for developers new to the codebase: the commands you
need, the architecture that spans multiple files, and the invariants that are
easy to break without knowing why they exist. The README covers user-facing
behavior; CLAUDE.md covers edition/publishing rules; this document covers how
the code actually works.

pppdf is a borderless native Win32 PDF viewer for tabletop RPG sessions,
written in Rust against Direct2D and PDFium. The page *is* the window: no
chrome, one document per window, a single self-contained exe.

## Commands

```powershell
.\fetch-pdfium.ps1                       # REQUIRED once per clone: downloads pdfium.dll
                                         # into target\debug, target\release, and assets\
cargo build --release                    # free edition
cargo build --release --features pro     # pppdf Pro (same exe path — build one at a time)

cargo test                               # full suite, free edition
cargo test --features pro                # full suite, Pro edition (adds 6 integration
                                         # form tests + 4 form unit tests in app.rs)
cargo test --test pdf_integration load_render_outline_links            # single test
cargo test --features pro --test pdf_integration fill_save_reload_roundtrip
```

A fresh clone does **not compile** until `fetch-pdfium.ps1` has run:
`src/pdf.rs` embeds the DLL with `include_bytes!("../assets/pdfium.dll")`, a
compile-time dependency. Every change must build and test green in **both**
editions.

Runtime subcommands and diagnostics:

```powershell
pppdf.exe --register                     # per-user (HKCU) PDF-handler registration; --unregister removes it
$env:PPPDF_LOG = '1'                     # trace log to %TEMP%\pppdf-debug.log (any other value = log file path)
```

The exe is `#![windows_subsystem = "windows"]` — there is no console, so
`println!` debugging is invisible. Use the `dlog!` macro (`src/dbglog.rs`)
with `PPPDF_LOG` set; errors surface via `MessageBoxW`, and a global panic
hook catches any thread's panic (including the PDF thread's) in a message box.

## Architecture

### Two threads, one process per window

Every window is its own **process**. `Ctrl+N` / extra command-line paths
spawn a fresh copy of the exe rather than a second HWND, because the
pdfium-render binding is a process-global singleton (`src/pdf.rs::pdfium()`,
a `OnceLock` + `Box::leak`) — and a hung PDF can then never stall another
window.

Inside a process there are exactly two threads:

```
UI thread                                   "pdf" thread
─────────────────────────────               ─────────────────────────────
main.rs    entry, panic hook, CLI dispatch  pdf.rs   owns ALL PDFium state:
window.rs  Win32 shell: WndProc, input               binding, document,
           routing, timers, menus                    (Pro) FormEngine
app.rs     state machine + page cache
d2d.rs     Direct2D/DirectWrite drawing
              │                                          ▲
              │  mpsc Sender<PdfCmd> ────────────────────┘
              ▼
        PdfCmd::{Load, Render, Form*, SaveDoc*, Shutdown}     (* = Pro)
        PdfReply::{Loaded, Rendered, RenderFailed, NeedsPassword,
                   LoadFailed, FormDirty*, Saved*}
              ▲
              └── mpsc reply channel; the PDF thread calls a `wake` closure
                  that posts WM_APP_PDF_REPLY so the UI thread drains replies
                  on its own message loop (the message is only a wake signal —
                  payload rides the channel)
```

The full protocol lives in `src/types.rs`. PDFium is not thread-safe;
single-thread ownership by the PDF thread is the real safety mechanism
(the crate's `thread_safe` feature is belt-and-braces).

### Layering: pure logic vs OS shims

The pure-logic modules hold all interesting behavior so it is unit-testable
without a window; the OS shims stay thin:

| Module | Role | OS types? |
|---|---|---|
| `app.rs` | State machine (`Mode::{Empty, Loading, Password, Doc}`), page cache, render scheduling, hit-testing, page history | none |
| `layout.rs` | All geometry: fit/zoom/scroll math, rotation transforms, PDF-point↔window-px mapping | none (enforced by file header) |
| `overlay.rs` | TOC panel state machine (selection, expand/collapse, hit-test) in panel-local DIPs | none |
| `window.rs` | Win32 shell: window class, WndProc, input routing, timers, drag/resize, menus, PDF-thread wiring | all of them |
| `d2d.rs` | The only place anything is drawn: D2D render target, page-bitmap upload, text, overlays | Direct2D/DWrite |
| `pdf.rs` | The PDF worker thread: DLL binding, load/render, links/outline/field extraction | PDFium FFI |
| `types.rs` | The UI↔PDF-thread message protocol and shared data types | none |
| `register.rs`, `edition.rs`, `dbglog.rs` | CLI registration, edition branding, opt-in trace log | registry/COM (register.rs only) |
| `pro/` (Pro only) | Form-filling: drives PDFium's form engine (see below) | PDFium FFI |

`lib.rs` is a 13-line facade exposing everything `pub` so integration tests
(and `main.rs`, which links the crate as a library) reach internals. It also
contains the **single** Pro gate: `#[cfg(feature = "pro")] pub mod pro;`.

`app.rs` never draws or posts messages — its mutators return `bool`
("something changed, repaint") and `wanted_renders()` produces render
requests; `window.rs` interprets both. Replies feed back through
`App::doc_loaded()` / `rendered()` / `render_failed()`.

### The render pipeline

1. `App::wanted_renders()` asks for the current page at current zoom
   (`visible: true`) plus prev/next neighbors at fit (`visible: false`),
   skipping anything cached, pending, or previously failed.
2. `window.rs::sync_renders()` marks them pending and sends `PdfCmd::Render`.
3. The PDF thread renders with `set_fixed_size` — the bitmap is **exactly**
   the requested pixel size — as tightly-packed BGRA bytes, and ships them in
   `PdfReply::Rendered` along with that page's link and form-field rects.
4. `App::rendered()` stores the pixels in the `PageCache` stamped with a
   monotonic generation; `d2d.rs` uploads them to a GPU bitmap memoized by
   `(page, px, rot, gen)` and blits on `WM_PAINT`.

Load-bearing details, each protecting against a specific failure loop:

- **Size tolerance, not exact match.** The cache accepts replies within
  ±2 px (`SIZE_TOLERANCE`) and pending is cleared *by page*, because PDFium
  may round dimensions differently than `layout::render_size`. Exact-match
  logic anywhere in this path re-requests forever (render livelock) — the
  same reason `render_page` must never switch to `set_target_size`.
- **Every pending entry is guaranteed to clear.** The PDF thread coalesces
  its queue (a `Load` supersedes queued renders; only the last visible
  render survives), and every dropped render either gets a
  `RenderFailed{"superseded"}` reply or shares a page with a surviving
  request whose reply clears pending by page (per-page-deduped prefetches
  are dropped silently — safe only because of that by-page clear).
  Genuinely failed pages go into a permanent `failed` set (corrupt pages are
  accepted silently, per spec).
- **Cache identity is `(page, px, rot)` + generation.** A 180° render has
  the same pixel size as upright (without `rot`, stale sideways pixels), and
  Pro form filling re-renders the *same* key with new pixels (without `gen`,
  typed text never appears).
- **Eviction protection.** `PageCache` (6 entries, 384 MB byte budget) never
  evicts the current page ± 1 — exactly the set `wanted_renders()` would
  re-request; evicting one causes an infinite render/evict loop.
- `d2d.rs::clear_page_cache()` runs on document load, dropping the old
  document's device bitmap. (Historically this was what stopped the memo
  serving the old document's same-sized page 0; the `gen` stamp now
  independently rules that out, so today the call is defense-in-depth that
  also frees the old GPU bitmap.)

### Borderless window mechanics

The window keeps the full `WS_POPUP | WS_THICKFRAME | WS_CAPTION |
WS_SYSMENU | WS_MAXIMIZEBOX | WS_MINIMIZEBOX` style set — every flag is
load-bearing for native Aero Snap and animations — but:

- `WM_NCCALCSIZE` returns 0 for **both** `wparam` values, claiming the whole
  rect as client area (no title bar ever paints). When maximized it clamps to
  the monitor work area and leaves 2 px insets for auto-hide taskbars.
- `WM_NCHITTEST` hand-rolls the zones: an 8-DIP band at the edges returns
  resize grips; interactive content (links, form fields, overlays, a zoomed
  page) returns `HTCLIENT`; everything else returns `HTCAPTION` — which is
  what makes "drag anywhere moves the window" work via the OS move loop.
- DWM: square corners (`DWMWCP_DONOTROUND`) and a 1 px
  `DwmExtendFrameIntoClientArea` margin to restore the drop shadow.

The `HTCAPTION` trick has consequences elsewhere: clicks on the unzoomed page
body never arrive as client mouse messages. That is why mouse X-buttons
(back/forward) are handled via `WM_APPCOMMAND` rather than `WM_XBUTTONDOWN`,
and why Pro must kill form focus in `WM_NCLBUTTONDOWN`.

### Coordinate systems

Three spaces meet in this codebase; conversions live in `layout.rs` and
`app.rs`:

- **Window space:** physical pixels, origin top-left, y down. The D2D render
  target is pinned to 96 DPI so 1 DIP == 1 px; all DPI scaling is done
  manually via `scale()` (per-monitor DPI aware, PerMonitorV2).
- **PDF space:** points (1/72"), origin **bottom-left, y up** — a PDF rect
  numerically has `top > bottom`. All link/field rects cross the thread
  boundary in this space, and *unrotated*: hit-testing first un-rotates the
  cursor (`unrotate_point`), drawing rotates rects forward (`rotate_rect`).
  `page_pt()` (unrotated, for link/field rects) vs `page_pt_rot()` (axes
  swapped at 90°/270°, for all fit/zoom geometry) must not be mixed up.
- **Panel-local DIPs:** the TOC overlay works purely in DIPs; `window.rs`
  divides physical px by the DPI scale before hit-testing.

Zoom is a multiplier over the fit scale — `1.0` always means fit-to-window,
and every page landing (`DocState::land`) resets zoom/scroll to fit.

### Input routing

`on_key` in `window.rs` is a strict modal ladder — earlier layers swallow
keys from later ones:

1. Password prompt (Esc only; text via `WM_CHAR`)
2. F1 help (fully modal — every key is swallowed; F1/Esc or any click
   dismisses)
3. Go-to-page entry (`G`, digits, Enter/Esc)
4. Pro form typing (field owns the keyboard; Tab/Esc/Ctrl-chords excepted)
5. TOC overlay keys — deliberately **non-modal**: unhandled keys
   (PgUp/PgDn, history nav) still act on the document underneath
6. Global shortcuts

Page **history** (v1.5.0): a pure `History` struct (back/forward stacks,
capped at 100) inside `DocState`. Only `App::goto` records (links, TOC, `G`,
Home/End); `App::flip` (adjacent paging, wheel) never does. Browser
semantics: a new jump truncates the forward stack. Input arrives via three
routes (Alt+arrows in `WM_SYSKEYDOWN`, mouse X-buttons in `WM_APPCOMMAND`,
Backspace in `on_key`) that all funnel into `nav_back`/`nav_forward`. New
jump-style navigation features should call `goto`, never a bespoke path —
recording inside `goto` was chosen so future sources record by default.

## Free vs Pro edition

One codebase, two builds, split by the `pro` Cargo feature (`pro = []`, a
pure compile-time switch). The rules (from CLAUDE.md, enforced in review):

- Pro code lives in `src/pro/`, gated **once** at the module boundary in
  `lib.rs`. Pro calls into shared code, never the reverse; the free exe
  contains no Pro code (not even strings — e.g. the Pro F1 help rows are a
  separate `cfg`'d const).
- `src/edition.rs` is the single source of truth for branding: `IS_PRO`,
  `NAME`, `NAME_W`. The name literal exists in **three** places — `NAME`,
  `NAME_W` (the `w!` macro needs a literal), and `build.rs` (build scripts
  can't see the crate; feeds the exe's version resources). A unit test guards
  the first two against drift; `build.rs` is by convention only.
- Small runtime branches (labels, menu entries) may use `edition::IS_PRO`;
  whole features must be module-gated.

Exception worth knowing: `app.rs` and the seams in `window.rs`/`pdf.rs`/
`d2d.rs`/`types.rs` carry small `cfg(feature = "pro")`-gated fields and
match arms (in `d2d.rs`: the Pro help rows, the dotted field-frame stroke
style, and the field-indication drawing) — accepted because `App` is the
state root and the channel protocol is shared.
Read-only form geometry (`has_forms`, field rects) is *unconditional* shared
code: the free build uses it for the one-shot "Form filling is a pppdf Pro
feature" hint and cursor hit-testing.

### Pro form filling

The UI never calls PDFium: `window.rs` translates Win32 input into
`FormEvent` values (in PDF page points) sent as `PdfCmd::Form`; on the PDF
thread, `src/pro/forms/mod.rs::FormEngine` forwards them to PDFium's raw
`FORM_On*` APIs and installs four `FPDF_FORMFILLINFO` callbacks (invalidate,
on-change, set/kill timer) whose state lives in thread-locals. pppdf builds
plumbing, not widgets — PDFium itself handles the caret, selection, radio
logic, and dropdown popups.

Hard-won invariants (each one was a bug; several have regression tests):

- **Render through the engine's page handle.** `FPDF_LoadPage` returns a
  distinct object per call, and `FPDF_FFLDraw` paints the live editor overlay
  (caret, in-edit text, combo popups) only into the page view of the handle
  it's given. `render_page_pro` therefore rasters through the FormEngine's
  own handle; a fresh handle makes typed text invisible until focus loss.
- **MouseMove before every MouseDown.** PDFium builds a widget's interactive
  editor only on mouse-enter or a clean focus grab, not on a bare
  focus-moving click (touchpad taps arrive with no prior move).
- **Tab cycles via `FORM_OnFocus`, never synthesized clicks** (a click would
  toggle checkboxes it passes over); double-clicks on button-like widgets are
  downgraded to plain `MouseDown` (PDFium ignores DblClk on them, and the OS
  turns every second rapid click into one).
- **Timer flood defenses.** PDFium once requested a ~0 ms timer that drove
  454 full-page renders/s: `ffi_set_timer` floors intervals at 100 ms, and
  unsolicited re-renders are throttled to one per 33 ms. The engine's
  `invalidated` flag is consumed only when a render is actually dispatched —
  throttled paths must leave it set or the final repaint is lost.
- **Dirty tracking is deliberately pessimistic.** PDFium reports text-field
  changes only on focus-loss commit, so the UI optimistically marks
  `form_dirty` per keystroke and paste, and per click on button-like widgets
  (text-field clicks are focus-only and deliberately don't mark) —
  "over-marking is safe; under-marking loses data". It is cleared at save *dispatch* (edits made
  while a save is in flight re-mark it) and restored on save failure.
- **Saves gate window destruction.** `saves_inflight` is a counter (saves
  can overlap); the PDF thread is detached, and process exit would kill it
  mid-write. Ctrl+S saves in place via temp-file + atomic rename (with
  antivirus-lock retries) — possible only because documents are loaded from
  *bytes*, so PDFium holds no file handle on the original. `FormEngine` owns
  a raw page handle and must be torn down before its document drops.

### The vendored pdfium-render

`Cargo.toml` pins pdfium-render **exactly 0.9.2** and `[patch.crates-io]`
redirects it to `vendor/pdfium-render`: a verbatim copy carrying exactly
five accessor patches that expose the raw document/form/page handles Pro
needs. `vendor/PATCHES.md` is the authoritative inventory — in-source, only
the two *added* methods are marked `PATCH(pppdf)`; the three
`pub(crate)`→`pub` visibility changes are not.
Nothing else in `vendor/` may be edited; bumping the version means
re-applying the five patches by hand. Path-vendoring disables `--cap-lints`,
so two upstream dead-code warnings are expected on full rebuilds — the
zero-warnings bar applies only to pppdf's own code. Both editions compile
against the vendored copy; the free build just never links the pro tree.

Dependency austerity is a standing rule: `windows` and `pdfium-render` are
the only significant runtime deps, and new ones are rejected by default.

## The pdfium.dll story

End-to-end path of the rendering engine:

1. `fetch-pdfium.ps1` downloads the **latest** bblanchon/pdfium-binaries
   release (the DLL version is *not pinned* — re-running can silently change
   the embedded PDFium) into `target\debug`, `target\release`, and `assets\`.
2. `src/pdf.rs` embeds `assets/pdfium.dll` via `include_bytes!`.
3. At runtime, `bind_pdfium` tries in order: (a) `pdfium.dll` beside the exe
   or up to two ancestor dirs (covers test exes in `target\debug\deps`, and
   doubles as a user override — a stray DLL beside the exe silently wins);
   (b) the embedded copy, unpacked to
   `%LOCALAPPDATA%\pppdf\pdfium-{version}.dll` — stamped with *pppdf's* crate
   version (e.g. `pdfium-1.5.0.dll`), not the PDFium release; an existing
   file of the right byte length is reused. This is the shipped single-file
   exe's path — written to a unique temp file and atomically renamed, with
   antivirus-lock retries and self-healing re-extraction; (c) the system
   search path.
4. If nothing binds, the PDF thread reports `LoadFailed` with an empty path,
   which `window.rs` recognizes as "the library itself failed" → error box →
   quit.

`build.rs` handles the rest of packaging: manifest embedding (PerMonitorV2
DPI, common-controls v6, UTF-8, longPathAware) via embed-manifest — do not
also set a manifest through winresource, two manifests are a linker error —
plus the icon and edition-branded version resources. The release profile
optimizes for size (`opt-level = "s"`, LTO, strip) for the one-click itch.io
download.

## Testing

Automated coverage is the **PDF-thread protocol**, end-to-end against a real
`pdfium.dll` with no mocks (`tests/pdf_integration.rs`): load/render/
outline/links/rotation in both editions, plus six Pro form tests (dirty
tracking, fill-save-reload roundtrip, and regression pins for the render
flood and cannot-type bugs). Pure-logic modules (`app.rs`, `layout.rs`,
`overlay.rs`, `register.rs`) carry extensive in-file unit tests.

Rules for writing integration tests:

- Every test that touches PDFium must hold `PDFIUM_TEST_LOCK` (a static
  mutex in the test file) for its whole body — the binding is a process-wide
  singleton and PDFium is not thread-safe. The lock is the mechanism; do not
  rely on `--test-threads=1`.
- Call `join_pdf_thread()` (drains the reply channel to disconnection)
  before releasing the lock or touching PDFium from the test thread —
  shutdown is asynchronous.
- Fixtures: `tests/fixtures/form.pdf` is the only checked-in file — a real
  AcroForm whose widget count (exactly 9) and geometric layout several tests
  pin: specific widgets are found by rect heuristics (topmost text field =
  FullName; the unpaired near-square widget = Married), deliberately not by
  `/Annots` index (its ResetButton wipes the form; never blanket-click
  "Other" widgets).
  The viewer and rotation fixtures are generated into `%TEMP%` by
  `tests/common/mod.rs`; the page rectangle is pure red because one pixel
  check then proves BGRA-vs-RGBA byte order.
- One Pro test intentionally pins *current PDFium behavior* (editor built
  only on mouse-enter) and is designed to fail when a future DLL fixes it —
  that failure means "retire the leading MouseMove", not a product bug.

Everything needing a real HWND, GPU, or DPI change — borderless chrome,
snap, zoom anchoring, overlay UX, the in-window form experience — is covered
only by the checklist in `docs/manual-tests.md`.

## Documentation map

- `README.md` — user-facing behavior contract; its keybinding table and the
  F1 help overlay in `d2d.rs` are kept in **exact-wording sync**, and
  `docs/manual-tests.md` covers the same inputs — any input change touches
  all three.
- `docs/superpowers/specs/` + `plans/` — date-prefixed design-spec /
  implementation-plan pairs per feature (viewer 2026-07-08, Pro forms
  2026-07-09, page history 2026-07-10). These are *snapshots*, not current
  truth — the earliest ones declare constraints since relaxed; the Pro spec's
  "Implementation notes" appendix overrides its own body in nine places.
  Read README + code for current behavior.
- `TODO.md` — roadmap: full-text search (both editions, open); user-placed
  input fields/markers and custom bookmarks (Pro, planned).
- `CLAUDE.md` — edition rules and the itch.io/GitHub publishing workflow
  (butler push, free-exe-only guard).

# Pro form filling — design

Date: 2026-07-09 · Status: approved
Scope: fill existing AcroForm fields in the Pro edition. User-created
labels/inputs are explicitly out of scope for this iteration.

## Goal

A Pro user opens a form PDF (e.g. a character sheet), clicks a field, types,
toggles checkboxes/radios, picks from dropdowns, presses Ctrl+S, and the
filled PDF is saved in place. The free edition renders forms passively and
shows a one-time hint that filling is a Pro feature.

Test document: `tests/fixtures/form.pdf` (moved from repo root `Form.pdf`) —
3 text fields (FullName, ID, Notes), checkbox (Married), radio group
(Gender), 2 dropdowns (City, Language), and a ResetButton.

## Approach (decided)

Drive **PDFium's own form engine** — the same machinery Chrome uses.
pdfium-render already initializes the document's single form-fill
environment at load and its renderer already composites widget appearance
through it (`FPDF_FFLDraw`). We forward input events into that same
environment via the raw bindings (`FORM_OnLButtonDown/Up`, `FORM_OnChar`,
`FORM_OnKeyDown`, …); PDFium handles caret, selection, radio-group logic,
dropdown popups, field formatting, and reset actions. We build plumbing,
not widgets.

Rejected: a second form environment (PDFium supports one per document);
custom D2D widgets (reimplements text editing + appearance-stream
regeneration risks); floating Win32 EDIT control (positioning bugs on a
borderless zoomable DPI-scaled window, two write paths).

## Vendored dependency patch

`PdfForm::handle()` and related handles are `pub(crate)` in pdfium-render 0.9.2, so the form
handles are unreachable from outside the crate. Decision:

- Vendor pdfium-render 0.9.2 source into `vendor/pdfium-render/`.
- Wire with `[patch.crates-io]` in Cargo.toml. Version stays exactly 0.9.2.
- Patch (documented in `vendor/PATCHES.md`):
  1. `src/pdf/document.rs` — `PdfDocument::handle()` pub(crate) → pub
  2. `src/pdf/document.rs` — added `PdfDocument::form_mut()`
  3. `src/pdf/document/form.rs` — `PdfForm::handle()` pub(crate) → pub
  4. `src/pdf/document/form.rs` — added `PdfForm::form_fill_info_mut()` → `&mut FPDF_FORMFILLINFO`, so the pro module can install callbacks after init (PDFium reads callback pointers at call time, so post-init installation is valid)
  5. `src/pdf/document/page.rs` — `PdfPage::page_handle()` pub(crate) → pub

## Module structure & gating

- All interactive-form logic lives in `src/pro/forms/` (part of the
  existing `#[cfg(feature = "pro")] pub mod pro;` tree): FORMFILLINFO
  callback installation, timer registry, event translation, save, dirty
  tracking.
- Shared modules get small, hook-shaped `#[cfg(feature = "pro")]` touch
  points only at integration seams (pdf.rs command handling and loop
  timeout, window.rs input routing / Ctrl+S / close prompt, types.rs
  cfg-gated enum variants).
- Shared (both editions, unconditional): `has_forms` flag on
  `PdfReply::Loaded`; `fields: Vec<RectF>` (page points, like `links`) on
  `PdfReply::Rendered`, extracted read-only from widget annotations;
  byte-based document loading.

## Threading & data flow

Everything touching the document stays on the PDF thread.

- New cfg(pro) commands: `PdfCmd::Form(FormEvent)` where `FormEvent` is
  `MouseDown/MouseUp/MouseMove { page_pt, modifiers }`, `Char(u16)`,
  `KeyDown(vk, modifiers)`, `KillFocus`; plus `PdfCmd::SaveDoc`.
- New cfg(pro) replies: `FormFocus(bool)`, `FormDirty`, `Saved(Result)`.
- The PDF thread remembers the last visible render request (page, px, rot).
  After processing form events (or a fired timer, or `FFI_Invalidate`), it
  re-renders that page unconditionally and sends a normal `Rendered` reply.
  No dirty-rect optimization in v1.
- Caret blink: install `FFI_SetTimer`/`FFI_KillTimer`; timers live in a
  registry on the PDF thread; the command loop uses
  `recv_timeout(next deadline)` while timers exist, plain `recv()`
  otherwise (free build: loop unchanged).
- `FFI_OnFocusChange` → `FormFocus(bool)` reply keeps the UI's typing mode
  synced to PDFium's actual focus. `FFI_OnChange` → `FormDirty`.

## Input routing (UI thread)

Hit-test order for clicks: **field → link → window drag** (fields come from
the `Rendered` reply; client→page transform already exists in layout.rs).

Typing mode (Pro; entered by clicking a field, tracked via `FormFocus`):

- All keys forward to the form: printable chars (Space types a space, G/R/0
  type letters/digits), arrows, Del, Backspace, Home/End. Tab = next field
  via PDFium tab navigation (fallback if the pinned PDFium ignores VK_TAB:
  cycle our own field-rect list).
- Clipboard: Ctrl+C/X via `FORM_GetSelectedText` (+ `FORM_ReplaceSelection`
  with empty string for cut), Ctrl+V via `FORM_ReplaceSelection` with
  clipboard text.
- App keeps only **Esc** (kill focus, commit — document stays open; next
  Esc behaves as today) and **F1**.
- Wheel or drag-on-empty-area kills focus first, then acts normally.
- Cursor: I-beam over text fields, hand over clickable widgets (PDFium's
  `FFI_SetCursor` hint if convenient, else by field type).

Outside typing mode, and everywhere in the free edition, input behavior is
unchanged from today.

## Save & dirty state

- Loading switches to bytes (`load_pdf_from_byte_vec` equivalent): required
  so the original file isn't held open by PDFium (in-place save would
  otherwise hit a sharing violation), and the viewed file stops being
  locked on disk. Memory cost = file size; same trade-off Chrome makes.
- Dirty indicator: small low-alpha dot, top-right corner, persistent while
  unsaved changes exist.
- **Ctrl+S**: `FORM_ForceToKillFocus` (commits the in-progress edit) →
  save via pdfium-render's save API to a temp file in the same directory →
  atomic rename over the original → "Saved" badge; dot clears. On failure:
  error box, original untouched.
- Close (Esc at top level, Ctrl+W, WM_CLOSE) while dirty: message box
  **Save / Discard / Cancel**.
- ResetButton works through PDFium's action handling; it marks dirty like
  any other change.

## Free edition

- Fields render with whatever values the file contains (unchanged).
- First click on a field rect per run: badge toast "Form filling is a
  pppdf Pro feature". No cursor change, no other behavior.
- No form-engine code compiles into the free exe.

## Error handling

- Save failure: error box with OS error, dirty state retained.
- Form env unavailable (no form in doc): commands are no-ops; UI never
  enters typing mode because no field rects exist.
- Renderer/PDF-thread contract unchanged: superseded-render replies and
  preemption behavior stay as today; form re-renders go through the same
  `Rendered` path so pending-set accounting is untouched.

## Testing

Integration (headless — the form env works without a window; callbacks
optional), fixture `tests/fixtures/form.pdf`, run in BOTH feature
configurations in CI/local practice (`cargo test`,
`cargo test --features pro`):

- `has_forms` true for form.pdf, false for the existing non-form fixture.
- Rendered reply carries the expected widget rects (8 on page 1).
- Pro: click-center + `FORM_OnChar` sequence into FullName →
  `field_values()` shows the text; toggle Married; select a Gender radio;
  save to temp → reopen → values persisted.
- Free configuration: rects + flag still produced; form commands don't
  exist (compile-time guarantee).

Manual (docs/manual-tests.md additions): caret blink, dropdown popup
open/pick, tab order, typing while zoomed/rotated/HiDPI, Esc semantics,
wheel-kills-focus, dirty dot, Ctrl+S badge, close prompt, free-edition
hint appears exactly once per run.

## Risks / spike first

Milestone 1 of the implementation plan is an interactive spike proving, in
the real window: (1) dropdown popups render and respond inside the page
bitmap; (2) the caret appears and blinks with our timer callbacks. These
two behaviors are the load-bearing assumptions of approach A; everything
else builds on the same mechanism they exercise.

## Out of scope

User-created labels/inputs (next iteration), signature fields, file-select
fields, JavaScript form logic (no JS platform is provided to PDFium), XFA
forms, dirty-rect render optimization, undo/redo inside fields beyond what
PDFium provides natively.

# Pro Form Filling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Pro-edition users fill AcroForm fields (text, checkbox, radio, dropdown) in-place and save with Ctrl+S; the free edition shows a one-time hint.

**Architecture:** Drive PDFium's own form engine (the one pdfium-render already initializes at document load) by forwarding input events through the raw `FORM_On*` bindings on the PDF thread. Rendering of widgets/caret/popups comes free — the existing render path already composites the form layer via `FPDF_FFLDraw`. A 5-line vendored patch exposes the needed handles.

**Tech Stack:** Rust (edition 2021, rust 1.82+), pdfium-render 0.9.2 (vendored), windows 0.62.2, Win32/Direct2D.

**Spec:** `docs/superpowers/specs/2026-07-09-pro-form-filling-design.md` — read it first.

## Global Constraints

- Shell is PowerShell 7. `cargo` may not be on PATH; prefix every cargo command with: `$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"; `
- EVERY task must end with both configurations green: `cargo test` AND `cargo test --features pro` (also `cargo build` both ways when only build is affected).
- pdfium-render stays pinned at exactly **0.9.2**. The vendored copy IS 0.9.2 plus the 5 documented patches — nothing else may be edited in `vendor/`.
- Pro-only interactive-form logic lives in `src/pro/`; shared files get only small `#[cfg(feature = "pro")]` hook points at integration seams. Read-only geometry (field rects, `has_forms`) is shared code compiled into both editions.
- Product copy, exact strings: window/dialog branding via `crate::edition::NAME` / `NAME_W`; free-edition hint text is exactly `Form filling is a pppdf Pro feature`.
- Tests requiring pdfium need `pdfium.dll` in `target\debug` (already present; if missing run `.\fetch-pdfium.ps1`).
- Commit after every task. Commit messages end with:
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`
- The fixture `tests/fixtures/form.pdf` (moved from repo-root `Form.pdf` in Task 1) contains on page 0: text fields `FullName`, `ID`, `Notes`; checkbox `Married`; radio group `Gender`; dropdowns `City`, `Language`; push button `ResetButton`.

---

### Task 1: Vendor pdfium-render 0.9.2 with the 5-line accessor patch

**Files:**
- Create: `vendor/pdfium-render/` (copied crate source)
- Create: `vendor/PATCHES.md`
- Modify: `Cargo.toml` (add `[patch.crates-io]`)
- Modify: `vendor/pdfium-render/src/pdf/document.rs`, `vendor/pdfium-render/src/pdf/document/form.rs`, `vendor/pdfium-render/src/pdf/document/page.rs`
- Move: `Form.pdf` → `tests/fixtures/form.pdf`
- Modify: `docs/superpowers/specs/2026-07-09-pro-form-filling-design.md` (patch list: 2 accessors → 5 changes)

**Interfaces:**
- Consumes: nothing (first task).
- Produces (used by Tasks 2–4): `PdfDocument::handle() -> FPDF_DOCUMENT` (pub), `PdfDocument::form_mut() -> Option<&mut PdfForm>` (pub), `PdfForm::handle() -> FPDF_FORMHANDLE` (pub), `PdfForm::form_fill_info_mut() -> &mut FPDF_FORMFILLINFO` (pub), `PdfPage::page_handle() -> FPDF_PAGE` (pub). Fixture at `tests/fixtures/form.pdf`.

- [ ] **Step 1: Copy the crate source into vendor/**

```powershell
New-Item -ItemType Directory -Force vendor | Out-Null
$src = Get-ChildItem "$env:USERPROFILE\.cargo\registry\src\*\pdfium-render-0.9.2" | Select-Object -First 1 -ExpandProperty FullName
Copy-Item -Recurse $src vendor\pdfium-render
Remove-Item vendor\pdfium-render\.cargo-checksum.json -ErrorAction SilentlyContinue
Remove-Item vendor\pdfium-render\.cargo_vcs_info.json -ErrorAction SilentlyContinue
```

- [ ] **Step 2: Wire the path patch in Cargo.toml**

Add at the END of `Cargo.toml`:

```toml
# Vendored copy of the exact pinned version, carrying only the accessor
# patches documented in vendor/PATCHES.md (form handles for Pro form filling).
[patch.crates-io]
pdfium-render = { path = "vendor/pdfium-render" }
```

- [ ] **Step 3: Apply the 5 patches**

In `vendor/pdfium-render/src/pdf/document.rs` — find `pub(crate) fn handle(&self) -> FPDF_DOCUMENT {` and change to `pub fn handle(&self) -> FPDF_DOCUMENT {`. Then find the existing method `pub fn form(&self)` (near line 260, body `self.form.as_ref()`) and add directly below it:

```rust
    /// PATCH(pppdf): mutable access to the embedded form, so an embedder can
    /// install FPDF_FORMFILLINFO callbacks post-init (PDFium reads the
    /// callback pointers at call time, so this is valid).
    pub fn form_mut(&mut self) -> Option<&mut PdfForm<'a>> {
        self.form.as_mut()
    }
```

In `vendor/pdfium-render/src/pdf/document/form.rs` — change `pub(crate) fn handle(&self) -> FPDF_FORMHANDLE {` to `pub fn handle(&self) -> FPDF_FORMHANDLE {`, and add below it:

```rust
    /// PATCH(pppdf): mutable access to the FPDF_FORMFILLINFO this form was
    /// initialized with, for installing interactive callbacks after init.
    pub fn form_fill_info_mut(&mut self) -> &mut FPDF_FORMFILLINFO {
        self.form_fill_info.deref_mut()
    }
```

In `vendor/pdfium-render/src/pdf/document/page.rs` — change `pub(crate) fn page_handle(&self) -> FPDF_PAGE {` to `pub fn page_handle(&self) -> FPDF_PAGE {`.

- [ ] **Step 4: Write vendor/PATCHES.md**

```markdown
# Vendored pdfium-render 0.9.2 — local patches

Source: crates.io pdfium-render 0.9.2, vendored verbatim except for the
changes below. Wired via `[patch.crates-io]` in the workspace Cargo.toml.
Reason: interactive form filling (Pro) drives PDFium's form engine through
the raw bindings; the handles it needs are `pub(crate)` upstream.

1. `src/pdf/document.rs`  — `PdfDocument::handle()`  pub(crate) → pub
2. `src/pdf/document.rs`  — added `PdfDocument::form_mut()`
3. `src/pdf/document/form.rs` — `PdfForm::handle()`  pub(crate) → pub
4. `src/pdf/document/form.rs` — added `PdfForm::form_fill_info_mut()`
5. `src/pdf/document/page.rs` — `PdfPage::page_handle()` pub(crate) → pub

All patched lines are commented `PATCH(pppdf)` where new code was added.
When bumping the pdfium-render version, re-apply these five changes.
```

- [ ] **Step 5: Move the fixture**

```powershell
New-Item -ItemType Directory -Force tests\fixtures | Out-Null
Move-Item Form.pdf tests\fixtures\form.pdf
```

- [ ] **Step 6: Update the spec's patch list**

In `docs/superpowers/specs/2026-07-09-pro-form-filling-design.md`, section "Vendored dependency patch", replace the two-item patch list with the five items from PATCHES.md (same wording is fine). Implementation revealed three more `pub(crate)` walls (document handle, page handle, form_mut); the approach is unchanged.

- [ ] **Step 7: Verify both configurations build and tests pass**

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"; cargo build; cargo build --features pro; cargo test; cargo test --features pro
```
Expected: all green. Cargo.lock will note pdfium-render is patched — commit that too.

- [ ] **Step 8: Commit**

```powershell
git add vendor Cargo.toml Cargo.lock tests/fixtures/form.pdf docs/superpowers/specs/2026-07-09-pro-form-filling-design.md
git commit -m @'
build: vendor pdfium-render 0.9.2 with form-handle accessor patches

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```
(Note: `Form.pdf` was untracked, so the move needs no `git rm`.)

---

### Task 2: Byte loading, has_forms flag, shared form-field extraction

**Files:**
- Modify: `src/types.rs` (FieldKind, FormField; extend Loaded/Rendered)
- Modify: `src/pdf.rs` (byte loading, extract_form_fields, has_forms)
- Modify: `src/app.rs` (CachedPage.fields, DocState.has_forms, page_hit_frame refactor, field_at/field_hit/pdf_point_at)
- Modify: `src/window.rs` (pump: pass new fields through)
- Test: `tests/pdf_integration.rs` (new test `form_fixture_reports_fields`), `src/app.rs` unit tests

**Interfaces:**
- Consumes: patched accessors from Task 1 (`doc.form()`, `form.handle()`, `page.page_handle()`, `doc.bindings()`).
- Produces (used by Tasks 3–6):
  - `types::FieldKind { Text, Other }`, `types::FormField { rect_pt: RectF, kind: FieldKind }`
  - `PdfReply::Loaded { .., has_forms: bool }`, `PdfReply::Rendered { .., fields: Vec<FormField> }`
  - `App::field_at(&self, p: PointF) -> Option<FieldKind>`
  - `App::field_hit(&self, p: PointF) -> Option<(usize, FieldKind)>` (index into the current cache entry's `fields`)
  - `App::pdf_point_at(&self, p: PointF) -> Option<PointF>` — PDF points, origin bottom-left, y-up
  - `App::doc_loaded(.., has_forms: bool)`

- [ ] **Step 1: Write the failing integration test**

Append to `tests/pdf_integration.rs`:

```rust
/// The real form fixture must report has_forms and per-render field rects;
/// the generated (form-free) fixture must not.
#[test]
fn form_fixture_reports_fields() {
    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    let form_pdf = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/form.pdf");
    cmd_tx.send(PdfCmd::Load { path: form_pdf, password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { has_forms, .. } => assert!(has_forms, "form.pdf must set has_forms"),
        other => panic!("expected Loaded, got {other:?}"),
    }
    cmd_tx.send(PdfCmd::Render { page: 0, px: SizeU { w: 612, h: 792 }, rot: 0, visible: true }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Rendered { fields, .. } => {
            // 8 named fields; radio groups may contribute extra widgets.
            assert!(fields.len() >= 8, "expected >= 8 widgets, got {}", fields.len());
            assert!(
                fields.iter().filter(|f| f.kind == pppdf::types::FieldKind::Text).count() >= 3,
                "FullName, ID, Notes must be Text kind"
            );
            for f in &fields {
                assert!(f.rect_pt.right > f.rect_pt.left && f.rect_pt.top > f.rect_pt.bottom);
            }
        }
        other => panic!("expected Rendered, got {other:?}"),
    }
    // The link-test fixture has no form.
    cmd_tx.send(PdfCmd::Load { path: common::fixture_path(), password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { has_forms, .. } => assert!(!has_forms),
        other => panic!("expected Loaded, got {other:?}"),
    }
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
}
```

Add `use pppdf::types::FieldKind;` is NOT needed (fully qualified above).

- [ ] **Step 2: Run it — expect compile failure** (`has_forms`/`fields` don't exist):

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"; cargo test --test pdf_integration
```

- [ ] **Step 3: Extend types.rs**

In `src/types.rs`, after the `PageLink` definition add:

```rust
/// Coarse widget classification: enough for cursor choice and hit behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Other,
}

/// One form-widget annotation on a page. `rect_pt` uses the same convention
/// as `PageLink::rect_pt` (PDF points, origin bottom-left, top > bottom).
#[derive(Clone, Debug, PartialEq)]
pub struct FormField {
    pub rect_pt: RectF,
    pub kind: FieldKind,
}
```

Extend the reply enum: `Loaded` gains `has_forms: bool` (after `outline`), `Rendered` gains `fields: Vec<FormField>` (after `links`).

- [ ] **Step 4: pdf.rs — byte loading, has_forms, field extraction**

In `run()`, replace the `pdfium.load_pdf_from_file(&path, password.as_deref())` load with byte loading (releases the on-disk file — required for in-place save later, and the file is no longer locked while viewed):

```rust
            // Load from bytes: pdfium then holds no file handle, so the
            // original stays writable (in-place save) and un-locked on disk.
            let loaded = std::fs::read(&path)
                .map_err(|e| PdfiumError::IoError(e))
                .and_then(|bytes| pdfium.load_pdf_from_byte_vec(bytes, password.as_deref()));
            match loaded {
```
…keeping the existing `Ok`/`PasswordError`/`Err` arms exactly as they are. (Check `PdfiumError::IoError` exists in 0.9.2 — if the variant name differs, map the read error into the generic `LoadFailed` reply directly instead:)

```rust
            let bytes = match std::fs::read(&path) {
                Ok(b) => Some(b),
                Err(e) => {
                    let _ = reply_tx.send(PdfReply::LoadFailed { path: path.clone(), reason: e.to_string() });
                    wake();
                    None
                }
            };
            if let Some(bytes) = bytes {
                match pdfium.load_pdf_from_byte_vec(bytes, password.as_deref()) {
                    // ... existing three arms unchanged ...
                }
                wake();
            }
```

In `describe()`, add `has_forms: doc.form().is_some()` to the `Loaded` reply.

Add the extraction function and the two ABI constants near `extract_links`:

```rust
/// Stable PDFium ABI constants not re-exported by pdfium-render's prelude.
const ANNOT_SUBTYPE_WIDGET: i32 = 20; // FPDF_ANNOT_WIDGET
const FORMFIELD_TEXT: i32 = 6; // FPDF_FORMFIELD_TEXTFIELD

/// Widget-annotation rects (PDF points, bottom-left origin) + coarse kind.
/// Read-only and shared by both editions: the free build needs the rects for
/// its "Pro feature" hint, the pro build for hit-testing and cursors.
fn extract_form_fields(doc: &PdfDocument, page: &PdfPage) -> Vec<FormField> {
    let Some(form) = doc.form() else { return Vec::new() };
    let b = doc.bindings();
    let (form_h, page_h) = (form.handle(), page.page_handle());
    let mut out = Vec::new();
    unsafe {
        for i in 0..b.FPDFPage_GetAnnotCount(page_h) {
            let annot = b.FPDFPage_GetAnnot(page_h, i);
            if annot.is_null() {
                continue;
            }
            if b.FPDFAnnot_GetSubtype(annot) == ANNOT_SUBTYPE_WIDGET {
                let mut r = FS_RECTF { left: 0.0, top: 0.0, right: 0.0, bottom: 0.0 };
                if b.FPDFAnnot_GetRect(annot, &mut r) != 0 {
                    let kind = if b.FPDFAnnot_GetFormFieldType(form_h, annot) == FORMFIELD_TEXT {
                        FieldKind::Text
                    } else {
                        FieldKind::Other
                    };
                    out.push(FormField {
                        rect_pt: RectF { left: r.left, top: r.top, right: r.right, bottom: r.bottom },
                        kind,
                    });
                }
            }
            b.FPDFPage_CloseAnnot(annot);
        }
    }
    out
}
```

Adjust imports (`use crate::types::{FieldKind, FormField, ...}`; `FS_RECTF` comes via `pdfium_render::prelude::*` which is already imported). In `render_page`, add `fields: extract_form_fields(doc, &page)` to the `Rendered` reply (the function already receives `doc`).

- [ ] **Step 5: app.rs — carry fields, refactor hit-frame, add field/point queries**

`CachedPage` gains `pub fields: Vec<FormField>` (import `FormField`, `FieldKind` from types). `DocState` gains `pub has_forms: bool`. `doc_loaded` gains a `has_forms: bool` parameter and stores it.

Refactor the shared projection out of `link_at` (DRY — `field_at` needs the identical transform). Add:

```rust
    /// Project a window-space point into the current page's unrotated bitmap
    /// frame. Returns (point in unrotated bitmap px, px-per-pt scale, page
    /// size in points). None when no doc/render or the point is off-page.
    fn page_hit_frame(&self, p: PointF) -> Option<(PointF, f32, (f32, f32))> {
        let d = self.doc()?;
        // Cache presence gate kept from link_at: hit-testing against a page
        // that has never rendered would use stale geometry.
        self.cache.get_any(d.current, d.rot)?;
        let pr = self.current_page_rect()?;
        if !pr.contains(p) {
            return None;
        }
        let page_pt = d.page_pt();
        let px_per_pt = fit_scale(d.page_pt_rot(), self.client) * d.zoom;
        let q = unrotate_point(
            PointF { x: p.x - pr.left, y: p.y - pr.top },
            pr.width(),
            pr.height(),
            d.rot,
        );
        let q = PointF {
            x: q.x.min(page_pt.0 * px_per_pt - 1e-3),
            y: q.y.min(page_pt.1 * px_per_pt - 1e-3),
        };
        Some((q, px_per_pt, page_pt))
    }
```

Rewrite `link_at` to use it (behavior identical — keep the existing doc comments about edge nudging on `page_hit_frame`):

```rust
    pub fn link_at(&self, p: PointF) -> Option<LinkTarget> {
        let d = self.doc()?;
        let entry = self.cache.get_any(d.current, d.rot)?;
        let (q, px_per_pt, page_pt) = self.page_hit_frame(p)?;
        let unrot =
            RectF { left: 0.0, top: 0.0, right: page_pt.0 * px_per_pt, bottom: page_pt.1 * px_per_pt };
        for link in &entry.links {
            let wr = pdf_rect_to_window(link.rect_pt, page_pt.1, px_per_pt, unrot);
            if wr.contains(q) {
                return Some(link.target.clone());
            }
        }
        None
    }
```

Add the new queries below it:

```rust
    /// Form field under a window-space point: (index into the cache entry's
    /// `fields`, kind). Same projection as link_at.
    pub fn field_hit(&self, p: PointF) -> Option<(usize, FieldKind)> {
        let d = self.doc()?;
        let entry = self.cache.get_any(d.current, d.rot)?;
        let (q, px_per_pt, page_pt) = self.page_hit_frame(p)?;
        let unrot =
            RectF { left: 0.0, top: 0.0, right: page_pt.0 * px_per_pt, bottom: page_pt.1 * px_per_pt };
        for (i, f) in entry.fields.iter().enumerate() {
            let wr = pdf_rect_to_window(f.rect_pt, page_pt.1, px_per_pt, unrot);
            if wr.contains(q) {
                return Some((i, f.kind));
            }
        }
        None
    }

    pub fn field_at(&self, p: PointF) -> Option<FieldKind> {
        self.field_hit(p).map(|(_, k)| k)
    }

    /// Window point -> PDF page coordinates (points, origin bottom-left,
    /// y up) — the space FORM_On* events and field rects live in.
    pub fn pdf_point_at(&self, p: PointF) -> Option<PointF> {
        let (q, px_per_pt, page_pt) = self.page_hit_frame(p)?;
        Some(PointF { x: q.x / px_per_pt, y: page_pt.1 - q.y / px_per_pt })
    }
```

Update the test helpers in app.rs: `cached()` gains `fields: vec![]`; `app_with_doc` passes `false` for has_forms. Add unit tests:

```rust
    #[test]
    fn field_hit_transforms_like_links() {
        let mut a = app_with_doc(1);
        let fit_px = render_size(LETTER, a.client, 1.0);
        let field = crate::types::FormField {
            rect_pt: RectF { left: 0.0, top: 792.0, right: 306.0, bottom: 396.0 },
            kind: crate::types::FieldKind::Text,
        };
        a.rendered(CachedPage {
            page: 0, px: fit_px, rot: 0, bgra: vec![0; 4], links: vec![], fields: vec![field],
        });
        let pr = a.current_page_rect().unwrap();
        let p = PointF { x: pr.left + 5.0, y: pr.top + 5.0 };
        assert_eq!(a.field_hit(p), Some((0, crate::types::FieldKind::Text)));
        let q = PointF { x: pr.left + 5.0, y: pr.bottom - 5.0 };
        assert_eq!(a.field_at(q), None);
    }

    #[test]
    fn pdf_point_at_round_trips_pdf_space() {
        let mut a = app_with_doc(1);
        let fit_px = render_size(LETTER, a.client, 1.0);
        a.rendered(CachedPage {
            page: 0, px: fit_px, rot: 0, bgra: vec![0; 4], links: vec![], fields: vec![],
        });
        let pr = a.current_page_rect().unwrap();
        // Window point 10% in from the page's top-left corner.
        let p = PointF { x: pr.left + pr.width() * 0.1, y: pr.top + pr.height() * 0.1 };
        let pt = a.pdf_point_at(p).unwrap();
        assert!((pt.x - 61.2).abs() < 2.0, "x: {}", pt.x);
        assert!((pt.y - (792.0 - 79.2)).abs() < 2.0, "y-up flip: {}", pt.y);
    }
```

- [ ] **Step 6: window.rs — thread the new reply fields through**

In `pump_pdf_replies`: `Loaded { path, pages, page_sizes, outline, has_forms }` → `self.app.doc_loaded(path, pages, page_sizes, outline, has_forms);`. `Rendered { page, px, rot, bgra, links, fields }` → include `fields` in the `CachedPage`.

- [ ] **Step 7: Run all tests, both configurations**

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"; cargo test; cargo test --features pro
```
Expected: PASS, including the new `form_fixture_reports_fields`. After the first successful run, tighten the `fields.len() >= 8` assertion to the exact observed count with a comment naming it.

- [ ] **Step 8: Commit**

```powershell
git add -A
git commit -m @'
feat: byte loading, has_forms flag, shared form-field extraction

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

### Task 3: Pro form engine — callbacks, timers, event forwarding, auto re-render

**Files:**
- Modify: `src/types.rs` (cfg(pro): FormEvent, PdfCmd::Form/SaveDoc, PdfReply::FormDirty/Saved)
- Create: `src/pro/forms/mod.rs` (moved from `src/pro/mod.rs` content? No — see step 2)
- Modify: `src/pro/mod.rs` (declare `pub mod forms;`)
- Modify: `src/pdf.rs` (engine lifecycle, event handling, timer-aware wait, auto re-render, last_visible memo)
- Test: `tests/pdf_integration.rs` (`#[cfg(feature = "pro")]` test `form_events_mark_dirty_and_rerender`)

**Interfaces:**
- Consumes: Task 1 accessors; Task 2 `FormField` rects (test uses them to find click targets).
- Produces (used by Tasks 4–6):
  - `types::FormEvent` (cfg pro): `MouseDown/MouseUp/MouseMove { x: f32, y: f32, modifiers: i32 }`, `Char { c: u16, modifiers: i32 }`, `KeyDown { vk: u16, modifiers: i32 }`, `KillFocus`
  - `PdfCmd::Form { page: u16, ev: FormEvent }`, `PdfCmd::SaveDoc` (cfg pro)
  - `PdfReply::FormDirty`, `PdfReply::Saved { result: Result<(), String> }` (cfg pro)
  - `pro::forms::FormEngine` with: `new(doc: &mut PdfDocument<'static>, reply_tx: Sender<PdfReply>) -> Option<FormEngine>`, `handle_event(&mut self, b: &dyn PdfiumLibraryBindings, page: u16, ev: FormEvent)`, `save(&mut self, b: &dyn PdfiumLibraryBindings, doc: &PdfDocument, path: &Path) -> Result<(), String>` (stub until Task 4), `take_invalidated(&mut self) -> bool`, `next_deadline(&self) -> Option<std::time::Instant>`, `fire_due_timers(&mut self)`, `teardown(&mut self, b: &dyn PdfiumLibraryBindings)`
- Note: `PdfReply::FormFocus` from the spec is **dropped** — the UI initiates every focus change itself (click / tab / Esc), so it owns typing-mode state directly. Record this deviation in Task 7's spec-conformance step.

- [ ] **Step 1: types.rs — cfg(pro) protocol extensions**

```rust
/// One interactive form input event, in PDF page coordinates (points,
/// origin bottom-left — what App::pdf_point_at produces).
#[cfg(feature = "pro")]
#[derive(Clone, Copy, Debug)]
pub enum FormEvent {
    MouseDown { x: f32, y: f32, modifiers: i32 },
    MouseUp { x: f32, y: f32, modifiers: i32 },
    MouseMove { x: f32, y: f32, modifiers: i32 },
    Char { c: u16, modifiers: i32 },
    KeyDown { vk: u16, modifiers: i32 },
    KillFocus,
}
```

`PdfCmd` gains (cfg pro): `Form { page: u16, ev: FormEvent }` and `SaveDoc`. `PdfReply` gains (cfg pro): `FormDirty` and `Saved { result: Result<(), String> }`.

- [ ] **Step 2: src/pro/forms/mod.rs — the engine**

Convert `src/pro/mod.rs` to declare a submodule: keep its doc comment, body becomes `pub mod forms;`. Create `src/pro/forms/mod.rs`:

```rust
//! Interactive AcroForm engine: drives PDFium's form-fill machinery (the
//! environment pdfium-render already initialized at load) by forwarding
//! events through the raw FORM_On* bindings. Everything here runs on the
//! PDF thread — PDFium invokes the FPDF_FORMFILLINFO callbacks synchronously
//! on the calling thread, so plain thread_locals hold the callback state.

use crate::types::{FormEvent, PdfReply};
use pdfium_render::prelude::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::os::raw::c_int;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

struct Timer {
    interval: Duration,
    next_fire: Instant,
    func: unsafe extern "C" fn(c_int),
}

#[derive(Default)]
struct CallbackState {
    invalidated: bool,
    reply_tx: Option<Sender<PdfReply>>,
    timers: BTreeMap<i32, Timer>,
    next_timer_id: i32,
}

thread_local! {
    static CB: RefCell<CallbackState> = RefCell::new(CallbackState::default());
}

unsafe extern "C" fn ffi_invalidate(
    _this: *mut FPDF_FORMFILLINFO,
    _page: FPDF_PAGE,
    _l: f64,
    _t: f64,
    _r: f64,
    _b: f64,
) {
    CB.with(|s| s.borrow_mut().invalidated = true);
}

unsafe extern "C" fn ffi_on_change(_this: *mut FPDF_FORMFILLINFO) {
    CB.with(|s| {
        let mut s = s.borrow_mut();
        s.invalidated = true; // a change always needs fresh pixels
        if let Some(tx) = &s.reply_tx {
            let _ = tx.send(PdfReply::FormDirty);
        }
    });
}

unsafe extern "C" fn ffi_set_timer(
    _this: *mut FPDF_FORMFILLINFO,
    elapse_ms: c_int,
    func: Option<unsafe extern "C" fn(c_int)>,
) -> c_int {
    let Some(func) = func else { return 0 };
    let interval = Duration::from_millis(elapse_ms.max(1) as u64);
    CB.with(|s| {
        let mut s = s.borrow_mut();
        s.next_timer_id += 1;
        let id = s.next_timer_id;
        s.timers.insert(id, Timer { interval, next_fire: Instant::now() + interval, func });
        id
    })
}

unsafe extern "C" fn ffi_kill_timer(_this: *mut FPDF_FORMFILLINFO, id: c_int) {
    CB.with(|s| {
        s.borrow_mut().timers.remove(&id);
    });
}

/// Owns the raw page handle interactive events target. The FORM handle and
/// document handle belong to pdfium-render's PdfDocument/PdfForm — we only
/// borrow their values; teardown() must run before the document is dropped
/// or replaced.
pub struct FormEngine {
    form: FPDF_FORMHANDLE,
    doc: FPDF_DOCUMENT,
    /// (page index, raw page handle) FORM events currently target.
    page: Option<(u16, FPDF_PAGE)>,
}

impl FormEngine {
    /// None when the document has no form. Installs the interactive
    /// callbacks into the FPDF_FORMFILLINFO pdfium-render registered at
    /// init — PDFium reads these function pointers at call time, so
    /// post-init installation is valid.
    pub fn new(doc: &mut PdfDocument<'static>, reply_tx: Sender<PdfReply>) -> Option<Self> {
        let doc_h = doc.handle();
        let form = doc.form_mut()?;
        let form_h = form.handle();
        let info = form.form_fill_info_mut();
        info.FFI_Invalidate = Some(ffi_invalidate);
        info.FFI_OnChange = Some(ffi_on_change);
        info.FFI_SetTimer = Some(ffi_set_timer);
        info.FFI_KillTimer = Some(ffi_kill_timer);
        CB.with(|s| {
            *s.borrow_mut() =
                CallbackState { reply_tx: Some(reply_tx), ..CallbackState::default() };
        });
        Some(Self { form: form_h, doc: doc_h, page: None })
    }

    /// Raw page handle for `idx`, switching the form's page context if
    /// needed. PDFium refcounts pages per index, so this handle aliases the
    /// one the render path opens — focus and caret state drawn by FFLDraw
    /// during rendering belong to the same page view we feed events to.
    fn ensure_page(&mut self, b: &dyn PdfiumLibraryBindings, idx: u16) -> Option<FPDF_PAGE> {
        if let Some((cur, h)) = self.page {
            if cur == idx {
                return Some(h);
            }
            unsafe {
                b.FORM_OnBeforeClosePage(h, self.form);
                b.FPDF_ClosePage(h);
            }
            self.page = None;
        }
        let h = unsafe { b.FPDF_LoadPage(self.doc, idx as c_int) };
        if h.is_null() {
            return None;
        }
        unsafe {
            b.FORM_OnAfterLoadPage(h, self.form);
        }
        self.page = Some((idx, h));
        Some(h)
    }

    pub fn handle_event(&mut self, b: &dyn PdfiumLibraryBindings, page: u16, ev: FormEvent) {
        let Some(h) = self.ensure_page(b, page) else { return };
        unsafe {
            match ev {
                FormEvent::MouseDown { x, y, modifiers } => {
                    b.FORM_OnLButtonDown(self.form, h, modifiers, x as f64, y as f64);
                }
                FormEvent::MouseUp { x, y, modifiers } => {
                    b.FORM_OnLButtonUp(self.form, h, modifiers, x as f64, y as f64);
                }
                FormEvent::MouseMove { x, y, modifiers } => {
                    b.FORM_OnMouseMove(self.form, h, modifiers, x as f64, y as f64);
                }
                FormEvent::Char { c, modifiers } => {
                    b.FORM_OnChar(self.form, h, c as c_int, modifiers);
                }
                FormEvent::KeyDown { vk, modifiers } => {
                    b.FORM_OnKeyDown(self.form, h, vk as c_int, modifiers);
                }
                FormEvent::KillFocus => {
                    b.FORM_ForceToKillFocus(self.form);
                }
            }
        }
    }

    /// Placeholder — implemented in the save task.
    pub fn save(
        &mut self,
        _b: &dyn PdfiumLibraryBindings,
        _doc: &PdfDocument,
        _path: &std::path::Path,
    ) -> Result<(), String> {
        Err("save not implemented yet".into())
    }

    pub fn take_invalidated(&mut self) -> bool {
        CB.with(|s| std::mem::take(&mut s.borrow_mut().invalidated))
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        CB.with(|s| s.borrow().timers.values().map(|t| t.next_fire).min())
    }

    /// Fire every due timer callback (caret blink etc.); PDFium reacts by
    /// invoking FFI_Invalidate, which take_invalidated() then reports.
    pub fn fire_due_timers(&mut self) {
        let now = Instant::now();
        // Collect first: the callback may call ffi_set_timer/ffi_kill_timer,
        // which re-borrows CB.
        let due: Vec<(i32, unsafe extern "C" fn(c_int))> = CB.with(|s| {
            let mut s = s.borrow_mut();
            let mut due = Vec::new();
            for (id, t) in s.timers.iter_mut() {
                if t.next_fire <= now {
                    t.next_fire = now + t.interval;
                    due.push((*id, t.func));
                }
            }
            due
        });
        for (id, func) in due {
            unsafe { func(id) };
        }
    }

    /// Must run before the PdfDocument is dropped or replaced.
    pub fn teardown(&mut self, b: &dyn PdfiumLibraryBindings) {
        if let Some((_, h)) = self.page.take() {
            unsafe {
                b.FORM_OnBeforeClosePage(h, self.form);
                b.FPDF_ClosePage(h);
            }
        }
        CB.with(|s| *s.borrow_mut() = CallbackState::default());
    }
}
```

- [ ] **Step 3: pdf.rs — engine lifecycle and event pump**

All additions below are `#[cfg(feature = "pro")]` unless noted. In `run()` after `let mut carry`:

```rust
    // Both editions: the last visible render request, re-issued when the
    // form layer invalidates (pro) — cheap to track unconditionally.
    let mut last_visible: Option<(u16, SizeU, u8)> = None;
    let mut current_path: Option<PathBuf> = None;
    #[cfg(feature = "pro")]
    let mut engine: Option<crate::pro::forms::FormEngine> = None;
```

Replace the plain `cmd_rx.recv()` arm of `first` with a timer-aware wait (free build keeps `recv()`):

```rust
        let first = match carry.take() {
            Some(c) => c,
            None => {
                #[cfg(feature = "pro")]
                {
                    match wait_cmd(&cmd_rx, &mut engine, &doc, last_visible, &reply_tx, &wake) {
                        Some(c) => c,
                        None => break,
                    }
                }
                #[cfg(not(feature = "pro"))]
                {
                    match cmd_rx.recv() {
                        Ok(c) => c,
                        Err(_) => break,
                    }
                }
            }
        };
```

Add the helper at module level:

```rust
/// Block for the next command; while form timers exist, wake at their
/// deadlines to blink the caret (fire timers -> pdfium invalidates -> the
/// visible page re-renders). Returns None when the UI hung up.
#[cfg(feature = "pro")]
fn wait_cmd(
    cmd_rx: &Receiver<PdfCmd>,
    engine: &mut Option<crate::pro::forms::FormEngine>,
    doc: &Option<PdfDocument<'static>>,
    last_visible: Option<(u16, SizeU, u8)>,
    reply_tx: &Sender<PdfReply>,
    wake: &impl Fn(),
) -> Option<PdfCmd> {
    loop {
        let deadline = engine.as_ref().and_then(|e| e.next_deadline());
        let Some(deadline) = deadline else {
            return cmd_rx.recv().ok();
        };
        let timeout = deadline.saturating_duration_since(std::time::Instant::now());
        match cmd_rx.recv_timeout(timeout) {
            Ok(c) => return Some(c),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some(eng) = engine.as_mut() {
                    eng.fire_due_timers();
                    if eng.take_invalidated() {
                        if let (Some(d), Some((page, px, rot))) = (doc, last_visible) {
                            let _ = reply_tx.send(render_page(d, page, px, rot));
                            wake();
                        }
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    }
}
```

In the coalesce loop, collect the new commands (declare `let mut form_events: Vec<(u16, FormEvent)> = Vec::new(); let mut save_requested = false;` beside `load`/`visible` — cfg(pro) on both):

```rust
                #[cfg(feature = "pro")]
                PdfCmd::Form { page, ev } => form_events.push((page, ev)),
                #[cfg(feature = "pro")]
                PdfCmd::SaveDoc => save_requested = true,
```

Track the visible memo (non-cfg — in the `Render { visible: true }` arm the coalescer already keeps `visible`; after the coalesce loop add):

```rust
        if let Some(v) = visible {
            last_visible = Some(v);
        }
```

In the load block: teardown the engine BEFORE loading, rebuild AFTER a successful load; remember the path; reset `last_visible` on a load attempt:

```rust
        if let Some((path, password)) = load {
            #[cfg(feature = "pro")]
            if let Some(mut eng) = engine.take() {
                eng.teardown(pdfium.bindings());
            }
            last_visible = None;
            // ... existing byte-read + load match; inside the Ok(d) arm,
            // after `doc = Some(d);` add:
            //     current_path = Some(path.clone());   (path was moved into the reply — clone before)
            //     #[cfg(feature = "pro")]
            //     { engine = doc.as_mut().and_then(|d| crate::pro::forms::FormEngine::new(d, reply_tx.clone())); }
        }
```

(Adjust the existing arm so `path` is still available: `let _ = reply_tx.send(describe(&d, &path)); current_path = Some(path); doc = Some(d);` — `describe` takes `&Path`, so send the reply first, then move.)

After the load block and before the render work loop, process form events and auto re-render:

```rust
        #[cfg(feature = "pro")]
        {
            if !form_events.is_empty() || save_requested {
                if let Some(eng) = engine.as_mut() {
                    for (page, ev) in form_events.drain(..) {
                        eng.handle_event(pdfium.bindings(), page, ev);
                    }
                    if save_requested {
                        let result = match (&doc, &current_path) {
                            (Some(d), Some(p)) => eng.save(pdfium.bindings(), d, p),
                            _ => Err("no document open".into()),
                        };
                        let _ = reply_tx.send(PdfReply::Saved { result });
                        wake();
                    }
                }
            }
            // Events (or fired timers) dirtied the widgets: refresh the
            // visible page — unless this very batch already renders it.
            if visible.is_none() {
                if let Some(eng) = engine.as_mut() {
                    if eng.take_invalidated() {
                        if let (Some(d), Some((page, px, rot))) = (&doc, last_visible) {
                            let _ = reply_tx.send(render_page(d, page, px, rot));
                            wake();
                        }
                    }
                }
            }
        }
```

NOTE on the borrow of `visible`: the existing code does `let visible_page = visible.map(...)` then consumes `visible` into `work`. Do the form-event block BEFORE `let work = visible.into_iter()...` and use `visible.is_none()` there — `Option<(u16, SizeU, u8)>` is `Copy`, so this is fine. On `Shutdown` (`break 'outer`) add engine teardown right before the loop breaks... simplest: after the `'outer` loop ends, add:

```rust
    #[cfg(feature = "pro")]
    if let Some(mut eng) = engine.take() {
        eng.teardown(pdfium.bindings());
    }
```

- [ ] **Step 4: Write the failing pro integration test**

Append to `tests/pdf_integration.rs`:

```rust
/// Pro: clicking a checkbox and typing into a text field must mark the
/// document dirty (FFI_OnChange -> FormDirty) and trigger an unsolicited
/// re-render of the visible page (FFI_Invalidate -> Rendered).
#[cfg(feature = "pro")]
#[test]
fn form_events_mark_dirty_and_rerender() {
    use pppdf::types::{FieldKind, FormEvent, FormField};

    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    let form_pdf = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/form.pdf");
    cmd_tx.send(PdfCmd::Load { path: form_pdf, password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { has_forms, .. } => assert!(has_forms),
        other => panic!("expected Loaded, got {other:?}"),
    }
    // Establish the visible-render memo + get field rects.
    cmd_tx.send(PdfCmd::Render { page: 0, px: SizeU { w: 612, h: 792 }, rot: 0, visible: true }).unwrap();
    let fields: Vec<FormField> = match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Rendered { fields, .. } => fields,
        other => panic!("expected Rendered, got {other:?}"),
    };
    let text = fields.iter().find(|f| f.kind == FieldKind::Text).expect("a text field");
    let center = |f: &FormField| {
        ((f.rect_pt.left + f.rect_pt.right) / 2.0, (f.rect_pt.top + f.rect_pt.bottom) / 2.0)
    };
    let (cx, cy) = center(text);
    // Click into the text field, then type 'A'.
    cmd_tx.send(PdfCmd::Form { page: 0, ev: FormEvent::MouseDown { x: cx, y: cy, modifiers: 0 } }).unwrap();
    cmd_tx.send(PdfCmd::Form { page: 0, ev: FormEvent::MouseUp { x: cx, y: cy, modifiers: 0 } }).unwrap();
    cmd_tx.send(PdfCmd::Form { page: 0, ev: FormEvent::Char { c: 'A' as u16, modifiers: 0 } }).unwrap();
    // Expect FormDirty and at least one unsolicited Rendered within the batch replies.
    let (mut dirty, mut rerendered) = (false, false);
    let deadline = std::time::Instant::now() + WAIT;
    while std::time::Instant::now() < deadline && !(dirty && rerendered) {
        match reply_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(PdfReply::FormDirty) => dirty = true,
            Ok(PdfReply::Rendered { .. }) => rerendered = true,
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(dirty, "typing must produce FormDirty");
    assert!(rerendered, "typing must trigger an unsolicited visible re-render");
    cmd_tx.send(PdfCmd::Form { page: 0, ev: FormEvent::KillFocus }).unwrap();
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
}
```

- [ ] **Step 5: Run — expect failure, then make it pass**

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"; cargo test --features pro --test pdf_integration
```
First run fails to compile until Steps 1–3 are complete; iterate until PASS. Debugging note: if `FormDirty` never arrives, the likely cause is the checkbox/text field click missing its rect (print the field rects and the click point); if `Rendered` never arrives, `take_invalidated`/`last_visible` wiring is wrong.

- [ ] **Step 6: Full suites, both configurations**

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"; cargo test; cargo test --features pro
```
The free build must compile with ZERO warnings about unused pro plumbing (everything new in pdf.rs is cfg-gated; `last_visible`/`current_path` are used... `current_path` is only read under cfg(pro) — mark it `#[cfg_attr(not(feature = "pro"), allow(unused))]` or cfg-gate it too; prefer cfg-gating both `current_path` writes/decl for cleanliness).

- [ ] **Step 7: Commit**

```powershell
git add -A
git commit -m @'
feat(pro): interactive form engine on the PDF thread

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

### Task 4: Save — atomic in-place write + end-to-end fill/save/reload proof

**Files:**
- Modify: `src/pro/forms/mod.rs` (real `save`, `read_field_values` helper)
- Modify: `src/pdf.rs` (make `pdfium()` reachable: `pub(crate) fn pdfium()`)
- Test: `tests/pdf_integration.rs` (`#[cfg(feature = "pro")]` test `fill_save_reload_roundtrip`)

**Interfaces:**
- Consumes: Task 3 engine + protocol; Task 2 field rects.
- Produces (used by Task 6): working `PdfCmd::SaveDoc` → `PdfReply::Saved { result }`; `pro::forms::read_field_values(path: &Path) -> Result<std::collections::HashMap<String, Option<String>>, String>`.

- [ ] **Step 1: Write the failing test**

```rust
/// The full pro loop: click + type into FullName, toggle the Married
/// checkbox, save in place, reload from disk, and read the values back.
#[cfg(feature = "pro")]
#[test]
fn fill_save_reload_roundtrip() {
    use pppdf::types::{FieldKind, FormEvent, FormField};

    // Work on a copy — the test saves in place.
    let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/form.pdf");
    let work = std::env::temp_dir().join(format!("pppdf-roundtrip-{}.pdf", std::process::id()));
    std::fs::copy(&src, &work).unwrap();

    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    cmd_tx.send(PdfCmd::Load { path: work.clone(), password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { .. } => {}
        other => panic!("expected Loaded, got {other:?}"),
    }
    cmd_tx.send(PdfCmd::Render { page: 0, px: SizeU { w: 612, h: 792 }, rot: 0, visible: true }).unwrap();
    let fields: Vec<FormField> = match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Rendered { fields, .. } => fields,
        other => panic!("expected Rendered, got {other:?}"),
    };
    let center = |f: &FormField| {
        ((f.rect_pt.left + f.rect_pt.right) / 2.0, (f.rect_pt.top + f.rect_pt.bottom) / 2.0)
    };
    // Highest text field on the page = FullName (fields are page-ordered;
    // rank by rect top descending to be layout-robust).
    let mut texts: Vec<&FormField> = fields.iter().filter(|f| f.kind == FieldKind::Text).collect();
    texts.sort_by(|a, b| b.rect_pt.top.total_cmp(&a.rect_pt.top));
    let fullname = texts[0];
    let (cx, cy) = center(fullname);
    for ev in [
        FormEvent::MouseDown { x: cx, y: cy, modifiers: 0 },
        FormEvent::MouseUp { x: cx, y: cy, modifiers: 0 },
        FormEvent::Char { c: 'A' as u16, modifiers: 0 },
        FormEvent::Char { c: 'b' as u16, modifiers: 0 },
    ] {
        cmd_tx.send(PdfCmd::Form { page: 0, ev }).unwrap();
    }
    // Toggle the Married checkbox: the smallest Other-kind widget that is
    // roughly square (radio buttons are square too, so click ALL near-square
    // Other widgets — checkbox + radios; the assertions below only rely on
    // Married and FullName).
    // Simpler and deterministic: click every Other widget once, top-down.
    // ResetButton is /T ResetButton with a reset action — clicking it resets
    // the form INCLUDING FullName. So click Other widgets FIRST, then type.
    // => Reorder: do the Other-clicks BEFORE the FullName typing above when
    //    writing the final test. Then save.
    cmd_tx.send(PdfCmd::SaveDoc).unwrap();
    let deadline = std::time::Instant::now() + WAIT;
    let mut saved = false;
    while std::time::Instant::now() < deadline && !saved {
        match reply_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(PdfReply::Saved { result }) => {
                result.expect("save must succeed");
                saved = true;
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(saved, "no Saved reply");
    cmd_tx.send(PdfCmd::Shutdown).unwrap();

    let values = pppdf::pro::forms::read_field_values(&work).unwrap();
    assert_eq!(values.get("FullName"), Some(&Some("Ab".to_string())));
    std::fs::remove_file(&work).ok();
}
```

IMPORTANT test-authoring note (bake this in when writing the real test): the fixture's `ResetButton` resets the form when clicked, so do NOT blanket-click every `Other` widget after typing. Robust order: (1) click+type FullName, (2) click ONLY the checkbox — identify `Married` as the near-square `Other` widget whose width/height ratio is within 0.5–2.0 AND whose area is the smallest such widget, excluding nothing else — then assert `values.get("Married") == Some(&Some("true".to_string()))`. If the first run shows the checkbox identification is wrong (assertion failure), print all field rects+kinds, identify Married by its actual rect, and pin the selection with a comment. This is fixture-empirical work; the assertions themselves must not be weakened.

- [ ] **Step 2: Run — expect failure** (`read_field_values` missing, save stubbed):

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"; cargo test --features pro --test pdf_integration fill_save_reload_roundtrip
```

- [ ] **Step 3: Implement save + read_field_values**

`src/pdf.rs`: change `fn pdfium()` to `pub(crate) fn pdfium()`.

`src/pro/forms/mod.rs` — replace the `save` stub:

```rust
    /// Commit the in-progress edit, then write the document atomically over
    /// `path`: full save to a temp file in the same directory, then rename.
    /// The original is untouched on any failure. Rename is retried a few
    /// times — antivirus briefly locks freshly written files (same idiom as
    /// pdf.rs's pdfium extraction).
    pub fn save(
        &mut self,
        b: &dyn PdfiumLibraryBindings,
        doc: &PdfDocument,
        path: &std::path::Path,
    ) -> Result<(), String> {
        unsafe {
            b.FORM_ForceToKillFocus(self.form);
        }
        CB.with(|s| s.borrow_mut().invalidated = true); // caret/focus ring gone
        let bytes = doc.save_to_bytes().map_err(|e| format!("{e:?}"))?;
        let dir = path.parent().ok_or("path has no parent directory")?;
        let tmp = dir.join(format!(".pppdf-save-{}.tmp", std::process::id()));
        std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
        let mut last = None;
        for attempt in 0..5u64 {
            match std::fs::rename(&tmp, path) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(20 * (attempt + 1)));
                }
            }
        }
        let _ = std::fs::remove_file(&tmp);
        Err(last.map(|e| e.to_string()).unwrap_or_else(|| "rename failed".into()))
    }
```

(Windows `rename` over an existing file succeeds since Rust 1.x uses MoveFileEx REPLACE_EXISTING; the loaded document holds no lock because loading is from bytes — Task 2.)

Add at the bottom of `src/pro/forms/mod.rs`:

```rust
/// Read all form field values from a PDF on disk. Test/diagnostic helper —
/// uses the process-wide pdfium binding.
pub fn read_field_values(
    path: &std::path::Path,
) -> Result<std::collections::HashMap<String, Option<String>>, String> {
    let pdfium = crate::pdf::pdfium()?;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let doc = pdfium.load_pdf_from_byte_vec(bytes, None).map_err(|e| format!("{e:?}"))?;
    let form = doc.form().ok_or("document has no form")?;
    Ok(form.field_values(doc.pages()))
}
```

(`pdfium()` returns `Result<&'static Pdfium, String>` — the `?` fits. Verify `field_values(doc.pages())` — signature is `field_values(&self, pages: &PdfPages)`; `doc.pages()` returns `&PdfPages` in 0.9.2. If borrow lifetimes fight, bind `let pages = doc.pages();` first.)

- [ ] **Step 4: Iterate until the roundtrip passes, then run everything**

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"; cargo test --features pro; cargo test
```

- [ ] **Step 5: Commit**

```powershell
git add -A
git commit -m @'
feat(pro): atomic in-place save with fill/save/reload roundtrip test

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

### Task 5: UI routing — typing mode, mouse forwarding, cursors, free hint. INTERACTIVE CHECKPOINT

**Files:**
- Modify: `src/app.rs` (cfg(pro): `form_focus`, `form_tab_idx`, `form_tab_target`)
- Modify: `src/window.rs` (mouse/key routing, cursor, toast refactor, free hint)
- Modify: `src/d2d.rs` (`draw_badge` width scales with text)
- Test: app.rs unit tests (tab order); manual interactive checkpoint

**Interfaces:**
- Consumes: `App::field_hit/field_at/pdf_point_at` (Task 2), `PdfCmd::Form`/`FormEvent` (Task 3).
- Produces (used by Task 6): `App::form_focus` (cfg pro), `Ui::show_toast(text: String)`, `Ui::send_form(ev: FormEvent)` (cfg pro).

- [ ] **Step 1: app.rs — typing-mode state + tab cycling (TDD: test first)**

Add to the app.rs test module (they compile only under `--features pro`; wrap the whole block in `#[cfg(feature = "pro")] mod pro_tests { use super::*; ... }` inside `mod tests`):

```rust
    #[cfg(feature = "pro")]
    mod pro_tests {
        use super::*;
        use crate::types::{FieldKind, FormField};

        fn field(l: f32, b: f32, r: f32, t: f32) -> FormField {
            FormField { rect_pt: RectF { left: l, top: t, right: r, bottom: b }, kind: FieldKind::Text }
        }

        #[test]
        fn tab_cycles_fields_in_reading_order_and_wraps() {
            let mut a = app_with_doc(1);
            let fit_px = render_size(LETTER, a.client, 1.0);
            // Three fields: top (y 700..720), middle (500..520), bottom (300..320).
            a.rendered(CachedPage {
                page: 0, px: fit_px, rot: 0, bgra: vec![0; 4], links: vec![],
                fields: vec![field(72.0, 500.0, 300.0, 520.0),   // middle
                             field(72.0, 700.0, 300.0, 720.0),   // top
                             field(72.0, 300.0, 300.0, 320.0)],  // bottom
            });
            // From nothing selected, Tab goes to the TOP field (max top).
            let p1 = a.form_tab_target(false).unwrap();
            assert!((p1.y - 710.0).abs() < 1.0, "first tab -> top field, got y {}", p1.y);
            let p2 = a.form_tab_target(false).unwrap();
            assert!((p2.y - 510.0).abs() < 1.0, "then middle");
            let p3 = a.form_tab_target(false).unwrap();
            assert!((p3.y - 310.0).abs() < 1.0, "then bottom");
            let p4 = a.form_tab_target(false).unwrap();
            assert!((p4.y - 710.0).abs() < 1.0, "wraps to top");
            let p5 = a.form_tab_target(true).unwrap();
            assert!((p5.y - 310.0).abs() < 1.0, "shift-tab goes back (wraps to bottom)");
        }
    }
```

Implement on `App` (fields + method, all `#[cfg(feature = "pro")]`):

```rust
    /// Pro: a form field currently has keyboard focus (typing mode). The UI
    /// owns this state — it initiates every focus change (click, Tab, Esc).
    #[cfg(feature = "pro")]
    pub form_focus: bool,
    /// Pro: index (into the current cache entry's fields) last focused via
    /// click or Tab — the anchor for Tab cycling.
    #[cfg(feature = "pro")]
    pub form_tab_idx: Option<usize>,
```

(init both in `App::new()`: `form_focus: false, form_tab_idx: None` with matching cfg attributes on the struct-literal fields; reset both at the top of `doc_loaded`.)

```rust
    /// Advance Tab focus through the current page's fields in reading order
    /// (top-to-bottom, then left-to-right; PDF y is up, so top DESCENDING).
    /// Returns the next field's center in PDF points for a synthesized click.
    #[cfg(feature = "pro")]
    pub fn form_tab_target(&mut self, backwards: bool) -> Option<PointF> {
        let d = self.doc()?;
        let entry = self.cache.get_any(d.current, d.rot)?;
        if entry.fields.is_empty() {
            return None;
        }
        let mut order: Vec<usize> = (0..entry.fields.len()).collect();
        order.sort_by(|&a, &b| {
            let (ra, rb) = (&entry.fields[a].rect_pt, &entry.fields[b].rect_pt);
            rb.top.total_cmp(&ra.top).then(ra.left.total_cmp(&rb.left))
        });
        let pos = self.form_tab_idx.and_then(|cur| order.iter().position(|&i| i == cur));
        let next_pos = match (pos, backwards) {
            (Some(p), false) => (p + 1) % order.len(),
            (Some(p), true) => (p + order.len() - 1) % order.len(),
            (None, false) => 0,
            (None, true) => order.len() - 1,
        };
        let idx = order[next_pos];
        self.form_tab_idx = Some(idx);
        let r = entry.fields[idx].rect_pt;
        Some(PointF { x: (r.left + r.right) / 2.0, y: (r.top + r.bottom) / 2.0 })
    }
```

Run: `cargo test --features pro form_tab` → PASS (and `cargo test` still green — the pro test is cfg'd out).

- [ ] **Step 2: d2d.rs — badge width follows text**

In `draw_badge`, replace `let (bw, bh, m) = (110.0 * s, 36.0 * s, 24.0 * s);` with:

```rust
        // Width tracks the text so longer toasts (save confirmation, the
        // free-edition Pro hint) fit; the page badge keeps its old size.
        let bw = ((text.chars().count() as f32 * 8.0 + 44.0) * s)
            .max(110.0 * s)
            .min(client.w as f32 - 16.0 * s);
        let (bh, m) = (36.0 * s, 24.0 * s);
```

- [ ] **Step 3: window.rs — toast refactor + all routing**

(a) Toast refactor:

```rust
    /// Transient bottom-right pill with arbitrary text (badge family).
    fn show_toast(&mut self, text: String) {
        self.badge = Some((text, 0));
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_BADGE, 33, None);
        }
        self.invalidate();
    }

    fn show_badge(&mut self) {
        if let Some(d) = self.app.doc() {
            let text = format!("{} / {}", d.current + 1, d.pages);
            self.show_toast(text);
        }
    }
```

(b) `Ui` struct gains (with matching init in the `Box::new(Ui { ... })` literal — `#[cfg]` on struct-literal fields is valid):

```rust
    /// Pro: left button went down inside a form field; route move/up to the
    /// form until release.
    #[cfg(feature = "pro")]
    pub form_mouse: bool,
    /// Free: the one-per-run "Pro feature" hint has been shown.
    #[cfg(not(feature = "pro"))]
    pub hint_shown: bool,
```

(c) Helpers (near `send_pdf`):

```rust
    /// FWL_EVENTFLAG modifier bits for FORM_On* (fpdf_fwlevent.h: Shift=1,
    /// Control=2, Alt=4).
    #[cfg(feature = "pro")]
    fn form_modifiers() -> i32 {
        let mut m = 0;
        unsafe {
            if GetKeyState(VK_SHIFT.0 as i32) < 0 {
                m |= 1;
            }
            if GetKeyState(VK_CONTROL.0 as i32) < 0 {
                m |= 2;
            }
            if GetKeyState(VK_MENU.0 as i32) < 0 {
                m |= 4;
            }
        }
        m
    }

    #[cfg(feature = "pro")]
    fn send_form(&self, ev: crate::types::FormEvent) {
        if let Some(d) = self.app.doc() {
            let page = d.current;
            self.send_pdf(PdfCmd::Form { page, ev });
        }
    }

    /// Leave typing mode, committing the in-progress edit.
    #[cfg(feature = "pro")]
    fn form_kill_focus(&mut self) {
        if self.app.form_focus {
            self.app.form_focus = false;
            self.send_form(crate::types::FormEvent::KillFocus);
        }
    }

    /// Synthesize a click at a PDF-point to move focus (Tab cycling).
    #[cfg(feature = "pro")]
    fn form_tab(&mut self, backwards: bool) {
        if let Some(pt) = self.app.form_tab_target(backwards) {
            let m = Self::form_modifiers() & !1; // strip Shift: it selects text
            self.send_form(crate::types::FormEvent::MouseDown { x: pt.x, y: pt.y, modifiers: m });
            self.send_form(crate::types::FormEvent::MouseUp { x: pt.x, y: pt.y, modifiers: m });
            self.app.form_focus = true;
        }
    }
```

`VK_MENU` needs importing in the KeyboardAndMouse use list (also `VK_DELETE` for step (f)).

(d) `WM_LBUTTONDOWN` — after the help-dismiss block and `self.dismiss_goto_entry();`, before `self.suppress_click = false;` insert:

```rust
                    // Form fields win over links and window dragging.
                    #[cfg(feature = "pro")]
                    {
                        let p = self.mouse_point(lparam);
                        if let (Some((idx, _)), Some(pt)) =
                            (self.app.field_hit(p), self.app.pdf_point_at(p))
                        {
                            self.app.form_tab_idx = Some(idx);
                            self.app.form_focus = true;
                            self.form_mouse = true;
                            let m = Self::form_modifiers();
                            self.send_form(crate::types::FormEvent::MouseDown {
                                x: pt.x,
                                y: pt.y,
                                modifiers: m,
                            });
                            return LRESULT(0);
                        }
                        // Clicking off-field leaves typing mode (commit), then
                        // the click acts normally (drag/pan/link).
                        self.form_kill_focus();
                    }
                    #[cfg(not(feature = "pro"))]
                    {
                        let p = self.mouse_point(lparam);
                        if self.app.field_at(p).is_some() {
                            if !self.hint_shown {
                                self.hint_shown = true;
                                self.show_toast("Form filling is a pppdf Pro feature".into());
                            }
                            return LRESULT(0);
                        }
                    }
```

(The existing `let p = self.mouse_point(lparam);` a few lines further down stays; shadowing inside the cfg blocks is fine.)

(e) `WM_LBUTTONUP` — at the very top of the arm insert:

```rust
                    #[cfg(feature = "pro")]
                    if std::mem::take(&mut self.form_mouse) {
                        let p = self.mouse_point(lparam);
                        if let Some(pt) = self.app.pdf_point_at(p) {
                            self.send_form(crate::types::FormEvent::MouseUp {
                                x: pt.x,
                                y: pt.y,
                                modifiers: Self::form_modifiers(),
                            });
                        }
                        return LRESULT(0);
                    }
```

`WM_MOUSEMOVE` — at the top of the arm:

```rust
                    #[cfg(feature = "pro")]
                    if self.form_mouse && (wparam.0 as u32 & 0x0001) != 0 {
                        // MK_LBUTTON: drag inside a field = text selection.
                        let p = self.mouse_point(lparam);
                        if let Some(pt) = self.app.pdf_point_at(p) {
                            self.send_form(crate::types::FormEvent::MouseMove {
                                x: pt.x,
                                y: pt.y,
                                modifiers: Self::form_modifiers(),
                            });
                        }
                        return LRESULT(0);
                    }
```

(f) `on_key` — insert AFTER the goto_entry block, BEFORE `if vk == VK_F1.0`:

```rust
        // Typing mode: the focused field owns the keyboard. Only Esc
        // (commit + leave) and F1 stay app shortcuts; Space/G/R/0 type text.
        #[cfg(feature = "pro")]
        if self.app.form_focus {
            let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            match vk {
                _ if vk == VK_ESCAPE.0 => self.form_kill_focus(),
                _ if vk == VK_F1.0 => {
                    self.app.toggle_help();
                    self.invalidate();
                }
                _ if vk == VK_TAB.0 => self.form_tab(shift),
                _ if vk == VK_LEFT.0
                    || vk == VK_RIGHT.0
                    || vk == VK_UP.0
                    || vk == VK_DOWN.0
                    || vk == VK_DELETE.0
                    || vk == VK_HOME.0
                    || vk == VK_END.0 =>
                {
                    self.send_form(crate::types::FormEvent::KeyDown {
                        vk,
                        modifiers: Self::form_modifiers(),
                    });
                }
                0x56 if ctrl => self.form_paste(), // Ctrl+V (copy/cut deferred, see below)
                _ => {} // printable keys arrive via WM_CHAR
            }
            return;
        }
```

Clipboard, v1 decision: **paste only.** Copy/cut need the selection read back synchronously from the PDF thread (request/reply round-trip) — deferred; recorded as a deviation in Task 7. Paste carries the text in the event:

Add to `types::FormEvent` (Task 3's enum — extend now): `ReplaceSelection { text: String }` — and because `FormEvent` derives `Copy`, CHANGE the derive to `#[derive(Clone, Debug)]` when adding it (update any use sites that relied on Copy; the pdf.rs drain loop moves values, fine). Copy/cut require reading the selection back synchronously — defer BOTH to a follow-up; v1 ships **paste only**:

```rust
    /// Ctrl+V: paste clipboard text into the focused field.
    #[cfg(feature = "pro")]
    fn form_paste(&mut self) {
        if let Some(text) = clipboard_text(self.hwnd) {
            self.send_form(crate::types::FormEvent::ReplaceSelection { text });
        }
    }
```

with a module-level Win32 clipboard reader in window.rs:

```rust
/// Plain-text clipboard contents (CF_UNICODETEXT), if any.
#[cfg(feature = "pro")]
fn clipboard_text(hwnd: HWND) -> Option<String> {
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock, HGLOBAL};
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let out = GetClipboardData(13 /* CF_UNICODETEXT */).ok().and_then(|h| {
            let hg = HGLOBAL(h.0 as *mut core::ffi::c_void);
            let ptr = GlobalLock(hg) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            let _ = GlobalUnlock(hg);
            Some(s)
        });
        let _ = CloseClipboard();
        out
    }
}
```

This needs `Win32_System_DataExchange` and `Win32_System_Memory` added to the `windows` crate features in Cargo.toml (annotate: `# Ctrl+V paste into form fields (pro)`). Engine side (`src/pro/forms/mod.rs`, `handle_event`): add

```rust
                FormEvent::ReplaceSelection { ref text } => {
                    let mut wide: Vec<u16> = text.encode_utf16().collect();
                    wide.push(0);
                    b.FORM_ReplaceSelection(self.form, h, wide.as_ptr());
                }
```

(`FORM_ReplaceSelection` takes `FPDF_WIDESTRING` = `*const u16`; check the trait's exact parameter type when implementing — some pdfium-render binding methods take `&str` and convert internally; if the trait method signature is `FORM_ReplaceSelection(&self, form, page, text: &str)` use it directly.)

(g) `WM_CHAR` — extend the arm (it currently only handles Password mode):

```rust
                WM_CHAR => {
                    #[cfg(feature = "pro")]
                    if self.app.form_focus
                        && !matches!(self.app.mode, crate::app::Mode::Password { .. })
                    {
                        let c = wparam.0 as u32;
                        // Tab/Esc handled in WM_KEYDOWN; Ctrl chords (incl.
                        // Ctrl+S=0x13, Ctrl+V=0x16) must not type control
                        // bytes — \r (Enter) and \x08 (Backspace) pass.
                        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
                        if c != 0x09 && c != 0x1B && !ctrl {
                            self.send_form(crate::types::FormEvent::Char {
                                c: c as u16,
                                modifiers: Self::form_modifiers(),
                            });
                        }
                        return LRESULT(0);
                    }
                    // ... existing Password-mode body unchanged ...
```

(h) Wheel and page flips leave typing mode. `WM_MOUSEWHEEL`, first line of the arm: `#[cfg(feature = "pro")] self.form_kill_focus();`. Same single line at the top of `fn flip(&mut self, ...)` and in the `WM_LBUTTONDBLCLK` arm.

(i) Cursor — in `WM_SETCURSOR`, after the `link_at` check add (pro only; free keeps the arrow per spec):

```rust
                        #[cfg(feature = "pro")]
                        if !over_overlay {
                            if let Some(kind) = self.app.field_at(p) {
                                let cursor = match kind {
                                    crate::types::FieldKind::Text => IDC_IBEAM,
                                    crate::types::FieldKind::Other => IDC_HAND,
                                };
                                if let Ok(c) = LoadCursorW(None, cursor) {
                                    let _ = SetCursor(Some(c));
                                }
                                return LRESULT(1);
                            }
                        }
```

(j) `wants_client_mouse` — both editions must receive field clicks (free: for the hint). Add before the final `false`:

```rust
            if self.app.field_at(p).is_some() {
                return true; // form fields: pro types into them, free shows the hint
            }
```

(inside the existing `if let Some(d) = self.app.doc()` block).

- [ ] **Step 4: Build + full suites, both configurations**

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"; cargo test; cargo test --features pro
```

- [ ] **Step 5: INTERACTIVE CHECKPOINT — the spec's load-bearing spike**

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"; cargo run --features pro -- tests\fixtures\form.pdf
```

Verify by hand, in order (STOP and report if any of the first three fail — they are the approach's load-bearing assumptions):
1. Click the FullName field → focus ring appears, **caret appears and blinks**.
2. Type `Hello World` → text appears; Space typed a space (no page flip). G/R/0 type letters.
3. Click the City dropdown → **the popup list renders inside the page**; click an option → it's selected.
4. Married checkbox toggles on click; Gender radios are mutually exclusive.
5. Tab cycles fields top-to-bottom; Shift+Tab reverses.
6. Esc leaves the field (caret gone); second Esc quits (no save prompt yet — Task 6).
7. Ctrl+wheel zoom in, click a field while zoomed → caret lands where clicked; R rotate, type again — still correct.
8. Wheel over the page in typing mode → leaves the field, flips the page.
9. Free edition check: `cargo run -- tests\fixtures\form.pdf` → clicking a field shows the toast "Form filling is a pppdf Pro feature" exactly once per run; fields never focus.

Known fallbacks if a load-bearing check fails:
- **Caret doesn't blink:** confirm `ffi_set_timer` is reached (temporary `eprintln!` — it runs on the pdf thread; run with `cargo run` from a console to see stderr). If PDFium never calls it, check that the callbacks were installed on the SAME FPDF_FORMFILLINFO the env was inited with (`form_fill_info_mut` — not a copy).
- **Dropdown popup doesn't render:** the FFLDraw flags may exclude popups at 0.9.2's defaults — try adding `.render_form_data(true)` to the `PdfRenderConfig` in `render_page` (available in 0.9.2 as `set_render_form_data` / check `PdfRenderConfig` methods; the render path FFLDraws whenever the doc has a form, so this is likely already fine).
- **Wrong caret position when zoomed/rotated:** `pdf_point_at` mapping bug — write a focused unit test against the failing zoom/rot combination before touching the transform.

- [ ] **Step 6: Update docs/manual-tests.md**

Add a "Form filling (Pro)" section listing exactly the 9 checkpoint items above as repeatable manual tests.

- [ ] **Step 7: Commit**

```powershell
git add -A
git commit -m @'
feat(pro): form typing mode, mouse routing, cursors, free-edition hint

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

### Task 6: Save UX — Ctrl+S, dirty dot, close prompt

**Files:**
- Modify: `src/app.rs` (cfg(pro) `form_dirty`, shared `dirty_dot_visible()`)
- Modify: `src/window.rs` (Ctrl+S, Saved/FormDirty replies, quit() prompt, WM_CLOSE, pending_close)
- Modify: `src/d2d.rs` (dirty dot)
- Test: app.rs unit tests (dirty lifecycle); manual close-prompt checks

**Interfaces:**
- Consumes: `PdfCmd::SaveDoc`/`PdfReply::Saved`/`FormDirty` (Tasks 3–4), `show_toast` (Task 5).
- Produces: complete feature.

- [ ] **Step 1: app.rs — dirty state (test first)**

In the pro_tests module:

```rust
        #[test]
        fn dirty_flag_lifecycle() {
            let mut a = app_with_doc(1);
            assert!(!a.dirty_dot_visible());
            a.form_dirty = true;
            assert!(a.dirty_dot_visible());
            // A fresh document clears it.
            a.doc_loaded(PathBuf::from("y.pdf"), 1, vec![LETTER], vec![], false);
            assert!(!a.dirty_dot_visible());
        }
```

Implement: `#[cfg(feature = "pro")] pub form_dirty: bool` on App (init false; reset in `doc_loaded` beside form_focus), plus the shared accessor:

```rust
    /// Unsaved form changes exist (always false in the free edition).
    pub fn dirty_dot_visible(&self) -> bool {
        #[cfg(feature = "pro")]
        {
            self.form_dirty
        }
        #[cfg(not(feature = "pro"))]
        {
            false
        }
    }
```

Run `cargo test --features pro dirty_flag` → PASS.

- [ ] **Step 2: d2d.rs — the dot**

In `draw_inner`, directly before the F1-help block (`if app.help_visible`):

```rust
            // Unsaved-changes dot (pro): small, top-right, deliberately quiet.
            if app.dirty_dot_visible() {
                let s = self.scale();
                let (radius, m) = (4.0 * s, 10.0 * s);
                let ell = D2D1_ELLIPSE {
                    point: D2D_POINT_2F { x: client.w as f32 - m - radius, y: m + radius },
                    radiusX: radius,
                    radiusY: radius,
                };
                unsafe {
                    let b = rt.CreateSolidColorBrush(&color(1.0, 0.72, 0.2, 0.55), None)?;
                    rt.FillEllipse(&ell, &b);
                }
            }
```

Imports: `D2D1_ELLIPSE` from `windows::Win32::Graphics::Direct2D`, `D2D_POINT_2F` from `windows::Win32::Graphics::Direct2D::Common` (check the existing use lists — Common types may already be imported).

- [ ] **Step 3: window.rs — Ctrl+S, replies, close prompt**

(a) `Ui` gains `#[cfg(feature = "pro")] pub pending_close: bool` (init false).

(b) Save trigger:

```rust
    /// Ctrl+S: save the (form-bearing) document in place.
    #[cfg(feature = "pro")]
    fn save_doc(&mut self) {
        let has_forms = self.app.doc().map_or(false, |d| d.has_forms);
        if !has_forms {
            return;
        }
        self.app.form_focus = false; // the save commits + kills focus PDF-side
        self.send_pdf(PdfCmd::SaveDoc);
    }
```

Wire it twice in `on_key`: inside the typing-mode block (`0x53 if ctrl => self.save_doc(),`) and in the normal shortcut section next to Ctrl+W (`0x53 if ctrl => self.save_doc(), // Ctrl+S`).

(c) Reply pump — new arms:

```rust
                #[cfg(feature = "pro")]
                PdfReply::FormDirty => {
                    self.app.form_dirty = true;
                }
                #[cfg(feature = "pro")]
                PdfReply::Saved { result } => match result {
                    Ok(()) => {
                        self.app.form_dirty = false;
                        if self.pending_close {
                            unsafe {
                                let _ = DestroyWindow(self.hwnd);
                            }
                        } else {
                            self.show_toast("Saved".into());
                        }
                    }
                    Err(e) => {
                        self.pending_close = false;
                        self.error_box(&format!("Could not save:\n{e}"));
                    }
                },
```

(d) Close prompt — `quit()` becomes `&mut self` (call sites already have `&mut self`; the `WM_NCRBUTTONUP`→menu→`on_command` chain too):

```rust
    fn quit(&mut self) {
        #[cfg(feature = "pro")]
        if self.app.form_dirty && self.app.doc().is_some() {
            let choice = unsafe {
                MessageBoxW(
                    Some(self.hwnd),
                    w!("Save changes to this PDF before closing?"),
                    crate::edition::NAME_W,
                    MB_YESNOCANCEL | MB_ICONWARNING,
                )
            };
            match choice {
                IDYES => {
                    self.pending_close = true;
                    self.save_doc();
                }
                IDNO => unsafe {
                    let _ = DestroyWindow(self.hwnd);
                },
                _ => {} // IDCANCEL: stay open, keep edits
            }
            return;
        }
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
```

(`IDYES`/`IDNO`, `MB_YESNOCANCEL`, `MB_ICONWARNING` come from `WindowsAndMessaging` — already glob-imported.)

(e) Alt+F4 / external close goes through the same gate — add an arm:

```rust
                WM_CLOSE => {
                    self.quit();
                    LRESULT(0)
                }
```

- [ ] **Step 4: Full suites + manual verification**

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"; cargo test; cargo test --features pro
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"; cargo run --features pro -- tests\fixtures\form.pdf
```
Manual: type into a field → dot appears top-right; Ctrl+S → "Saved" toast, dot gone; type again, press Esc then Esc (quit) → Save/Discard/Cancel box; Cancel stays with edits; Esc-Esc again, Discard → closes without writing; reopen, type, Alt+F4 → same prompt; Yes → saves then closes (verify with `pro::forms::read_field_values` or by reopening). Add these to docs/manual-tests.md's pro section.

- [ ] **Step 5: Commit**

```powershell
git add -A
git commit -m @'
feat(pro): Ctrl+S in-place save, dirty indicator, close prompt

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

### Task 7: Docs, spec conformance, final sweep

**Files:**
- Modify: `README.md`, `CLAUDE.md`, `docs/manual-tests.md` (if anything from Tasks 5–6 is still missing)
- Modify: `docs/superpowers/specs/2026-07-09-pro-form-filling-design.md` (conformance notes)

**Interfaces:** consumes everything; produces the release-ready state.

- [ ] **Step 1: README**

In the "Pro edition" section: replace "(form filling, user-created labels/inputs — in progress)" with a shipped-feature description: form filling (text, checkboxes, radios, dropdowns), Ctrl+S saves in place, unsaved-changes dot, close prompt. Add `Ctrl+S | Save filled form (Pro)` to the key table.

- [ ] **Step 2: CLAUDE.md**

Under the edition rules, update the "Pro scope (planned)" line: form filling has shipped; user-created labels/inputs remain planned. Add one line: "Interactive form code: `src/pro/forms/` drives PDFium's form engine via vendored accessor patches (`vendor/PATCHES.md`)."

- [ ] **Step 3: Spec conformance pass**

Read the spec top to bottom against the implementation; append a short "## Implementation notes (2026-07-09)" section recording the deliberate deviations:
1. `FormFocus` reply dropped — UI owns typing-mode state (it initiates all focus changes).
2. Tab cycling is app-side (synthesized clicks in reading order), not PDFium `VK_TAB`.
3. Clipboard v1 = paste only (`FORM_ReplaceSelection`); copy/cut deferred.
4. Vendored patch grew from 2 accessors to 5 (documented in vendor/PATCHES.md).
Anything ELSE that diverged during execution gets recorded here too — check each spec section explicitly.

- [ ] **Step 4: Final verification, both editions**

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"; cargo build --release; cargo build --release --features pro; cargo test; cargo test --features pro
```
Expected: all green, zero warnings in both configurations. Then one last interactive smoke of each edition (pro: fill/save/reopen; free: hint toast once, everything else unchanged).

- [ ] **Step 5: Commit**

```powershell
git add -A
git commit -m @'
docs: pro form filling — README, manual tests, spec conformance notes

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

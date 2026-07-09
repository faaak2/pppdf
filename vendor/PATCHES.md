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

## Known upstream warnings

Vendoring as a path dependency disables Cargo's `--cap-lints allow`, so two
pre-existing upstream dead-code warnings surface on full rebuilds
(`BYTES_PER_GRAYSCALE_PIXEL`, `aligned_grayscale_to_unaligned`). They are
NOT ours and are not silenced — the patch policy is "5 documented changes,
nothing else". "Zero warnings" gates apply to pppdf's own code.

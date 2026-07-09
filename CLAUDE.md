# pppdf

Borderless native Win32 PDF viewer (Rust + Direct2D + PDFium).

## Free vs Pro edition

One codebase, two builds, split by the `pro` Cargo feature — there is no
separate branch or repo, and no license-key system (itch.io's paywall is the
gate; pro is sold as a separate download):

```
cargo build --release                  # free edition
cargo build --release --features pro   # pppdf Pro
```

Rules:

- **Pro code lives in `src/pro/`**, gated once at the module boundary in
  `lib.rs` (`#[cfg(feature = "pro")] pub mod pro;`). The free exe must not
  contain pro code. Don't scatter `cfg(feature = "pro")` through shared
  modules — pro calls into shared code, never the reverse.
- **`src/edition.rs` is the single source of truth for branding**
  (`IS_PRO`, `NAME`, `NAME_W`). Anything user-visible that differs between
  editions goes through it. `build.rs` repeats the name literals for the
  exe version resources (build scripts can't see the crate) — keep in sync.
- **Verify both editions**: any change must pass `cargo build` and
  `cargo test` both with and without `--features pro`.
- Pro scope (planned): form-fillable elements, user-created labels and
  input fields.

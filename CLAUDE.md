# pppdf

Borderless native Win32 PDF viewer (Rust + Direct2D + PDFium).

## Free vs Pro edition

One codebase, two builds, split by the `pro` Cargo feature — there is no
separate branch or repo, and no license-key system. Pro is still in
development; once it ships it will be sold as a separate paid download on the
*same* itch.io page as the free build, gated by itch.io's paywall (see
Publishing below):

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
- Pro scope: form filling has shipped (fill/save AcroForm fields);
  user-created labels and input fields remain planned.
- Interactive form code: `src/pro/forms/` drives PDFium's form engine via
  vendored accessor patches (`vendor/PATCHES.md`).

## Publishing

Two distribution channels, both carrying the same version:

- **GitHub release** (private repo) — carries *both* free and Pro binaries.
  Keeping the repo private is what keeps the paid Pro binary off the public
  internet; never make it public without pulling the Pro assets first.
- **itch.io** (https://sekadd.itch.io/pppdf, tagged Free) — currently the
  **free** build only, published with **butler** (itch.io's CLI). Pro is still
  in development; once ready it will be sold on this *same* page as a separate
  paid download, gated by itch.io's paywall.

### Shipping the free build to itch.io with butler

butler comes bundled with the installed itch desktop app (no separate install):
`%APPDATA%\itch\broth\butler\versions\{version}\butler.exe` (active version in
`...\broth\butler\.chosen-version`). It's already logged in as SeKaDD, so
`butler login` only validates.

```
butler push <folder-with-pppdf.exe> sekadd/pppdf:windows --userversion X.Y.Z
butler status sekadd/pppdf:windows      # verify; build "processes" a few seconds first
```

Rules:

- **Push the free exe, never Pro.** Building Pro overwrites
  `target/release/pppdf.exe` with the Pro exe. Before pushing, confirm the
  binary's `ProductName` is `pppdf` (not `pppdf Pro`) — e.g. build free last, or
  push from a kept-aside `pppdf-free-<ver>.exe`.
- Target is `sekadd/pppdf`, channel `windows` (the name contains "windows" so
  itch auto-tags it as a Windows build). Push a folder containing only
  `pppdf.exe` — a single-file build preserves the one-click download-and-run UX.
- `--dry-run` first to confirm exactly one file goes up; publishing is public and
  effectively immediate.

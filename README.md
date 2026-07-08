# pppdf

Borderless native PDF viewer for tabletop RPG sessions. Windows x64.

## Build

1. Install Rust (stable, 1.82+): https://rustup.rs
2. `.\fetch-pdfium.ps1`  (downloads pdfium.dll next to the build outputs)
3. `cargo build --release`

Run: `.\target\release\pppdf.exe path\to\book.pdf`

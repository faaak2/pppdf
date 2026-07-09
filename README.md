# pppdf

A borderless, native PDF viewer for pen & paper RPG sessions on Windows.
The page *is* the window: no title bar, no toolbars, no borders — snap a
rulebook next to your VTT and it looks like a page lying on the desktop.

- Full PDF rendering (PDFium — the same engine Chrome uses, as a plain DLL;
  no browser, no web runtime)
- Clickable in-document links; table of contents from the PDF outline
- Single page, always crisp, fit-to-window; Ctrl+wheel zoom for maps
- One window per book — run several instances side by side
- A single self-contained ~8 MB exe, no installation, nothing persisted
  (PDFium is bundled inside and unpacked to `%LOCALAPPDATA%\pppdf` on first run)

## Build

1. Install Rust (stable, 1.82+): https://rustup.rs
2. `.\fetch-pdfium.ps1` (downloads pdfium.dll into `target\` and `assets\`,
   where the release build embeds it into the exe)
3. `cargo build --release`

"Install": copy `target\release\pppdf.exe` anywhere and run it — PDFium is
baked in, so there is nothing else to keep alongside it. Optionally
right-click a PDF → Open with → choose pppdf.exe.

## Use

`pppdf.exe book.pdf` — or start it empty and right-click / drop a PDF on it.
Each window is one document; open more with Ctrl+N (empty) or Ctrl+Shift+O
(pick a file), each in its own window. The right-click menu also has an
**Always on Top** toggle to pin a window above everything else.

| Input | Action |
|---|---|
| Drag anywhere | Move the window (Aero Snap works) |
| Drag near an edge (8 px) | Resize |
| PgDn / → / Space / wheel | Next page |
| PgUp / ← / Shift+Space / wheel up | Previous page |
| Home / End | First / last page |
| Tab (or right-click menu) | Table of contents overlay |
| ↑ ↓ → ← / Enter / Esc | Navigate the open overlay |
| Click a link | Follow it (web links open in your browser) |
| Ctrl+wheel | Zoom at the cursor |
| Drag (zoomed) | Pan · Ctrl+drag moves the window |
| Double-click or 0 | Back to fit |
| Wheel (zoomed) | Scroll; one more notch at the edge flips the page |
| Ctrl+O | Open file · drop a PDF works too |
| Ctrl+N | Open a new empty window |
| Ctrl+Shift+O | Open a file in a new window |
| F11 | Fill the screen (work area) / restore |
| F1 | Show / hide the help overlay |
| Esc / Ctrl+W | Quit (Esc closes the overlay first) |

## Licenses

pppdf's PDF rendering uses PDFium (BSD-3-Clause / Apache-2.0, © The PDFium
Authors) via prebuilt binaries from
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) (MIT).

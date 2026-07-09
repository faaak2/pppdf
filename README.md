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

### Pro edition

The paid edition adds interactive form filling — click a field, type,
toggle checkboxes/radios, pick from dropdowns, press Ctrl+S to save the
filled PDF in place, with an unsaved-changes dot and a close prompt
(Save/Discard/Cancel) protecting anything unsaved. Fillable fields are
marked with a pale blue highlight; `F` cycles highlight → dotted frames
→ off. (User-created
labels/inputs are planned for a future release.) It's the same codebase
behind a Cargo feature; its code lives in `src/pro/` and is not compiled
into the free exe:

```
cargo build --release --features pro   # pppdf Pro
cargo build --release                  # free edition
```

Both land as `target\release\pppdf.exe` — build them one at a time and copy
the exe aside. CI/tests should run both: `cargo test` and
`cargo test --features pro`.

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
| G, then a number, Enter | Go to a page |
| Tab (or right-click menu) | Table of contents overlay |
| ↑ ↓ → ← / Enter / Esc | Navigate the open overlay |
| Click a link | Follow it (web links open in your browser) |
| Ctrl+wheel | Zoom at the cursor |
| Drag (zoomed) | Pan · Ctrl+drag moves the window |
| Double-click or 0 | Back to fit |
| R | Rotate 90° clockwise |
| Wheel (zoomed) | Scroll; one more notch at the edge flips the page |
| Ctrl+O | Open file · drop a PDF works too |
| Ctrl+N | Open a new empty window |
| Ctrl+Shift+O | Open a file in a new window |
| F11 | Fill the screen (work area) / restore |
| F1 | Show / hide the help overlay |
| Esc / Ctrl+W | Quit (Esc closes the overlay first) |
| Click a form field | Fill it · Tab/Shift+Tab next/previous · Esc leaves (Pro) |
| F | Mark form fields: highlight / frames / off (Pro) |
| Ctrl+S | Save filled form in place (Pro) |

## Make it a PDF handler (optional)

pppdf can register itself in Windows' **Open with** menu and **Default apps**
page — no installer, no admin:

```
pppdf.exe --register      # add pppdf as a PDF handler (for your user)
pppdf.exe --unregister    # remove it again
```

`--register` points the association at wherever `pppdf.exe` currently sits, so
put the exe in a permanent folder first (moved it later? just run `--register`
again). A normal launch never touches the registry — registration only happens
when you pass `--register`.

Windows won't let any app silently *become* the default PDF viewer, so the last
step is yours: right-click a PDF → **Open with → pppdf**, or set it in
**Settings → Apps → Default apps**. Registering just makes pppdf show up there.

## Licenses

pppdf's PDF rendering uses PDFium (BSD-3-Clause / Apache-2.0, © The PDFium
Authors) via prebuilt binaries from
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) (MIT).

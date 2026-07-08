# pppdf Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A fully borderless, native Win32 PDF viewer for tabletop RPG players — single crisp fit-to-window page, clickable links, PDF-outline TOC overlay, zoom — shipped as `pppdf.exe` + `pdfium.dll`.

**Architecture:** Two threads with message passing: the UI thread owns the Win32 window, input, Direct2D drawing, and a page-bitmap LRU cache; a dedicated PDF thread owns all PDFium state (PDFium is not thread-safe) and answers `PdfCmd` messages with `PdfReply` messages, waking the UI loop via `PostMessage`. Pure-logic modules (`layout`, `overlay`, `app`) contain all interesting behavior and have no OS dependencies, so they are unit-tested; OS-heavy modules (`window`, `d2d`, `pdf`) are thin and verified by an integration smoke test plus a manual checklist.

**Tech Stack:** Rust (stable, edition 2021), `windows` crate (raw Win32 + Direct2D + DirectWrite), `pdfium-render` crate dynamically binding `pdfium.dll` (prebuilt from bblanchon/pdfium-binaries).

**Spec:** `docs/superpowers/specs/2026-07-08-pppdf-viewer-design.md` — the authoritative requirements document. Read it before starting.

## Global Constraints

- Windows 10/11 x64 only. Shell for all commands: PowerShell 7 (`pwsh`).
- Dependencies: exactly two runtime crates — `windows` and `pdfium-render`. No logging, config, UI, or async crates. Anything else is rejected by default.
- Zero pixels of window chrome, ever. No title bar, no visible border, no toolbars.
- Pages always render at exact physical pixel size (bitmap-scaling only as a transient preview during live resize/zoom).
- Single page at a time; snap navigation; fit-to-window is the default and reset state.
- One window per PDF. Opening a file in an existing window replaces its document.
- No persistence of any kind: no config files, no registry writes, no recent-files list.
- Out of scope (do NOT implement, even if tempting): text selection, search, printing, annotations, form filling, tabs, installers, session restore.
- All user-visible UI text is English; the app name everywhere is `pppdf`.
- Commit after every task (and at every step that says Commit) with the exact message given.
- Repo root = `C:\Users\faaak\Desktop\Workspace\pppdf`. All paths below are relative to it.

## File Structure (final state)

```
Cargo.toml
build.rs                  (embeds the Per-Monitor-V2 DPI manifest)
fetch-pdfium.ps1          (downloads pdfium.dll into target dirs)
README.md
docs/manual-tests.md
src/main.rs               (entry: DPI call, panic hook, window::run)
src/lib.rs                (library facade so integration tests reach the modules)
src/layout.rs             (pure: fit/zoom/pan math, hit zones, rect transforms)
src/overlay.rs            (pure: outline tree state, visibility, key nav, hit-test)
src/app.rs                (pure: app state machine, page cache, wheel semantics)
src/types.rs              (shared types: PdfCmd, PdfReply, OutlineItem, PageLink…)
src/pdf.rs                (PDF thread: pdfium binding, render, outline/links)
src/d2d.rs                (Direct2D/DirectWrite device + all drawing)
src/window.rs             (Win32 window, WndProc, borderless tricks, input)
tests/common/mod.rs       (fixture PDF builder — raw PDF bytes, computed xref)
tests/pdf_integration.rs  (smoke test through real pdfium.dll)
```

---

### Task 1: Project scaffold — crate, dependencies, manifest, pdfium fetch script

**Files:**
- Create: `Cargo.toml`, `build.rs`, `.gitignore`, `fetch-pdfium.ps1`, `README.md`, `src/main.rs`

**Interfaces:**
- Consumes: nothing
- Produces: a compiling binary crate named `pppdf`; `pdfium.dll` present in `target\debug` and `target\release`; Per-Monitor-V2 DPI manifest embedded at link time. Every later task builds on this.

**Version facts (verified against crates.io / docs.rs / GitHub on 2026-07-08 — do not "upgrade" these without checking the APIs):**
- `windows` 0.62.2 (latest stable; MSRV Rust 1.82), `windows-numerics` 0.3.1
- `pdfium-render` 0.9.2 — NOTE: 0.9.0 was a breaking release (`PdfPageIndex` is now `i32`, `PdfRect` fields are private, `Send`/`Sync` behind default `thread_safe` feature). Code in later tasks targets the 0.9.x API.
- `embed-manifest` 1.5.0 (build dependency; its default manifest already declares PerMonitorV2 DPI awareness)
- pdfium.dll from bblanchon/pdfium-binaries; crate feature `pdfium_latest` (= `pdfium_7763` in 0.9.2) requires a DLL build ≥ 7763 — the fetch script downloads the latest release (≥ chromium/7934), which satisfies this. A too-old DLL fails cleanly at startup with `PdfiumError::LoadLibraryError`.

- [ ] **Step 1: Create the crate files**

`Cargo.toml`:

```toml
[package]
name = "pppdf"
version = "0.1.0"
edition = "2021"
rust-version = "1.82"
description = "Borderless native PDF viewer for tabletop RPG sessions"

[dependencies]
pdfium-render = { version = "0.9.2", default-features = false, features = ["pdfium_latest", "thread_safe"] }
windows-numerics = "0.3.1"

[dependencies.windows]
version = "0.62.2"
features = [
    "Win32_Foundation",                # HWND/WPARAM/LPARAM/LRESULT/RECT, D2DERR_RECREATE_TARGET
    "Win32_System_LibraryLoader",      # GetModuleHandleW
    "Win32_UI_WindowsAndMessaging",    # window class, message loop, menus, timers, cursors
    "Win32_Graphics_Gdi",              # HBRUSH (WNDCLASSEXW), BeginPaint, MonitorFromWindow
    "Win32_Graphics_Direct2D",
    "Win32_Graphics_Direct2D_Common",
    "Win32_Graphics_Dxgi_Common",      # DXGI_FORMAT_B8G8R8A8_UNORM
    "Win32_Graphics_DirectWrite",
    "Win32_Graphics_Dwm",              # DwmSetWindowAttribute, corner preference
    "Win32_UI_HiDpi",                  # SetProcessDpiAwarenessContext, GetDpiForWindow
    "Win32_UI_Shell",                  # DragAcceptFiles/DragQueryFileW, ShellExecuteW
    "Win32_UI_Controls_Dialogs",       # GetOpenFileNameW
    "Win32_UI_Input_KeyboardAndMouse", # VK_*, GetKeyState
    "Win32_System_SystemServices",     # MK_CONTROL (mouse-message modifier flags)
]

[build-dependencies]
embed-manifest = "1.5.0"

[profile.release]
opt-level = "s"
lto = true
codegen-units = 1
strip = true
```

`build.rs` (embeds a PerMonitorV2-aware manifest; the runtime `SetProcessDpiAwarenessContext` call added in Task 6 is belt-and-suspenders):

```rust
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        // new_manifest() defaults already include PerMonitorV2 DPI awareness,
        // common-controls v6, UTF-8 codepage, and longPathAware.
        embed_manifest(new_manifest("Pppdf.Viewer")).expect("unable to embed manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
```

`.gitignore`:

```
/target
```

`src/main.rs` (placeholder — replaced step by step in later tasks):

```rust
fn main() {
    println!("pppdf scaffold");
}
```

`fetch-pdfium.ps1` (downloads the latest prebuilt pdfium.dll into both target dirs; Windows 11 built-ins only — `Invoke-RestMethod` + `tar`):

```powershell
# Downloads the latest bblanchon/pdfium-binaries pdfium.dll (win-x64) into
# target\debug and target\release so `cargo run` / `cargo test` find it.
$ErrorActionPreference = 'Stop'

$release = Invoke-RestMethod `
    -Uri 'https://api.github.com/repos/bblanchon/pdfium-binaries/releases/latest' `
    -Headers @{ Accept = 'application/vnd.github+json'; 'User-Agent' = 'pppdf-fetch' }
$asset = $release.assets | Where-Object name -EQ 'pdfium-win-x64.tgz'
if (-not $asset) { throw "pdfium-win-x64.tgz not found in release $($release.tag_name)" }

$work = Join-Path ([IO.Path]::GetTempPath()) ("pdfium-" + ($release.tag_name -replace '/', '-'))
New-Item -ItemType Directory -Force $work | Out-Null
$tgz = Join-Path $work 'pdfium-win-x64.tgz'
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $tgz
tar -xzf $tgz -C $work
if ($LASTEXITCODE -ne 0) { throw 'tar extraction failed' }

foreach ($dir in @("$PSScriptRoot\target\debug", "$PSScriptRoot\target\release")) {
    New-Item -ItemType Directory -Force $dir | Out-Null
    Copy-Item (Join-Path $work 'bin\pdfium.dll') $dir -Force
}
Write-Host "pdfium.dll $($release.tag_name) -> target\debug, target\release"
```

`README.md` (stub — completed in Task 11):

```markdown
# pppdf

Borderless native PDF viewer for tabletop RPG sessions. Windows x64.

## Build

1. Install Rust (stable, 1.82+): https://rustup.rs
2. `.\fetch-pdfium.ps1`  (downloads pdfium.dll next to the build outputs)
3. `cargo build --release`

Run: `.\target\release\pppdf.exe path\to\book.pdf`
```

- [ ] **Step 2: Verify the build and the dll fetch**

Run: `cargo build`
Expected: compiles cleanly (warnings OK), produces `target\debug\pppdf.exe`.

Run: `.\fetch-pdfium.ps1`
Expected: prints `pdfium.dll chromium/<build> -> target\debug, target\release`; `target\debug\pdfium.dll` exists and is ~5–7 MB.

Run: `cargo run`
Expected output: `pppdf scaffold`

- [ ] **Step 3: Commit**

```pwsh
git add Cargo.toml Cargo.lock build.rs .gitignore fetch-pdfium.ps1 README.md src/main.rs
git commit -m "chore: scaffold pppdf crate - deps, DPI manifest, pdfium fetch script"
```

---

### Task 2: `layout.rs` — fit / zoom / pan math and hit zones (pure logic, TDD)

**Files:**
- Create: `src/layout.rs`
- Modify: `src/main.rs` (add `mod layout;`)

**Interfaces:**
- Consumes: nothing (pure module, `std` only)
- Produces (used by `app.rs`, `window.rs`, `d2d.rs`):
  - `struct SizeU { w: u32, h: u32 }`, `struct PointF { x: f32, y: f32 }`, `struct RectF { left, top, right, bottom: f32 }` (all `Clone, Copy, PartialEq, Debug, Default`)
  - `RectF::contains(PointF) -> bool`, `RectF::width() -> f32`, `RectF::height() -> f32`
  - `fn fit_scale(page_pt: (f32, f32), client: SizeU) -> f32`
  - `fn render_size(page_pt: (f32, f32), client: SizeU, zoom: f32) -> SizeU`
  - `fn max_zoom(page_pt: (f32, f32), client: SizeU) -> f32`
  - `fn clamp_scroll(page_px: SizeU, client: SizeU, scroll: PointF) -> PointF`
  - `fn page_rect(page_px: SizeU, client: SizeU, scroll: PointF) -> RectF`
  - `fn zoom_scroll(cursor: PointF, old_rect: RectF, zoom_ratio: f32, new_page_px: SizeU, client: SizeU) -> PointF`
  - `fn pdf_rect_to_window(rect_pt: RectF, page_h_pt: f32, px_per_pt: f32, page_rect: RectF) -> RectF`
  - `enum Zone { TopLeft, Top, TopRight, Left, Interior, Right, BottomLeft, Bottom, BottomRight }` + `fn hit_zone(p: PointF, client: SizeU, grip: f32) -> Zone`
  - Constants: `MAX_RENDER_DIM: u32 = 16_384`, `MAX_ZOOM: f32 = 5.0`, `SCROLL_STEP: f32 = 120.0`

**Coordinate conventions (memorize — used everywhere):**
- Window space: physical pixels, origin top-left, y down.
- PDF space: points (1/72"), origin bottom-left, y up. PDF rects carry `top > bottom` numerically.
- `zoom` is a multiplier over the fit scale; `1.0` = fit-to-window.
- `scroll` is how far the page content has been scrolled right/down past the client's top-left, in px; `{0,0}` when the page fits.

- [ ] **Step 1: Write the failing tests**

Create `src/layout.rs` containing ONLY the test module for now:

```rust
//! Pure geometry: fit, zoom, pan, hit zones. No OS types allowed in this file.

#[cfg(test)]
mod tests {
    use super::*;

    const LETTER: (f32, f32) = (612.0, 792.0); // US Letter, points

    fn sz(w: u32, h: u32) -> SizeU {
        SizeU { w, h }
    }
    fn pt(x: f32, y: f32) -> PointF {
        PointF { x, y }
    }

    // --- fit_scale / render_size ---

    #[test]
    fn fit_portrait_page_in_wide_client_is_height_bound() {
        // client 2000x1000: height binds: 1000/792
        let s = fit_scale(LETTER, sz(2000, 1000));
        assert!((s - 1000.0 / 792.0).abs() < 1e-5);
    }

    #[test]
    fn fit_portrait_page_in_tall_client_is_width_bound() {
        let s = fit_scale(LETTER, sz(500, 5000));
        assert!((s - 500.0 / 612.0).abs() < 1e-5);
    }

    #[test]
    fn fit_degenerate_inputs_yield_scale_one() {
        assert_eq!(fit_scale((0.0, 792.0), sz(100, 100)), 1.0);
        assert_eq!(fit_scale(LETTER, sz(0, 100)), 1.0);
    }

    #[test]
    fn render_size_at_fit_matches_client_on_binding_axis() {
        let px = render_size(LETTER, sz(2000, 1000), 1.0);
        assert_eq!(px.h, 1000); // height-bound
        assert!((px.w as f32 - 612.0 * (1000.0 / 792.0)).abs() <= 1.0);
    }

    #[test]
    fn render_size_never_zero() {
        let px = render_size(LETTER, sz(1, 1), 1.0);
        assert!(px.w >= 1 && px.h >= 1);
    }

    #[test]
    fn render_size_capped_at_max_dim() {
        let px = render_size(LETTER, sz(16_000, 16_000), 5.0);
        assert!(px.w <= MAX_RENDER_DIM && px.h <= MAX_RENDER_DIM);
    }

    #[test]
    fn max_zoom_capped_by_render_dim() {
        let mz = max_zoom(LETTER, sz(16_000, 16_000));
        let px = render_size(LETTER, sz(16_000, 16_000), mz);
        assert!(px.w <= MAX_RENDER_DIM && px.h <= MAX_RENDER_DIM);
        assert!(mz >= 1.0 && mz <= MAX_ZOOM);
    }

    // --- clamp_scroll / page_rect ---

    #[test]
    fn scroll_clamps_to_zero_when_page_fits() {
        let s = clamp_scroll(sz(500, 500), sz(1000, 1000), pt(50.0, -50.0));
        assert_eq!(s, pt(0.0, 0.0));
    }

    #[test]
    fn scroll_clamps_to_overhang() {
        let s = clamp_scroll(sz(1500, 2000), sz(1000, 1000), pt(9999.0, -5.0));
        assert_eq!(s, pt(500.0, 0.0));
    }

    #[test]
    fn page_rect_centers_when_smaller() {
        let r = page_rect(sz(500, 800), sz(1000, 1000), pt(0.0, 0.0));
        assert_eq!((r.left, r.top), (250.0, 100.0));
        assert_eq!((r.width(), r.height()), (500.0, 800.0));
    }

    #[test]
    fn page_rect_applies_scroll_when_larger() {
        let r = page_rect(sz(2000, 3000), sz(1000, 1000), pt(300.0, 700.0));
        assert_eq!((r.left, r.top), (-300.0, -700.0));
    }

    // --- zoom_scroll: the page point under the cursor stays put ---

    #[test]
    fn zoom_keeps_cursor_point_stationary() {
        let client = sz(1000, 1000);
        let old_px = render_size(LETTER, client, 1.0);
        let old_rect = page_rect(old_px, client, pt(0.0, 0.0));
        let cursor = pt(500.0, 500.0);
        let ratio = 2.0;
        let new_px = render_size(LETTER, client, 2.0);
        let scroll = zoom_scroll(cursor, old_rect, ratio, new_px, client);
        let new_rect = page_rect(new_px, client, scroll);
        // page-space position under cursor before == after (in new-zoom px, so *ratio)
        let before = ((cursor.x - old_rect.left) * ratio, (cursor.y - old_rect.top) * ratio);
        let after = (cursor.x - new_rect.left, cursor.y - new_rect.top);
        assert!((before.0 - after.0).abs() < 1.5, "x drifted: {before:?} vs {after:?}");
        assert!((before.1 - after.1).abs() < 1.5, "y drifted: {before:?} vs {after:?}");
    }

    // --- pdf_rect_to_window: y flips ---

    #[test]
    fn pdf_rect_transform_flips_y() {
        // A rect at the TOP of a 792pt page (top=792, bottom=772) must land at
        // the TOP of the window-space page rect.
        let link = RectF { left: 72.0, top: 792.0, right: 200.0, bottom: 772.0 };
        let pr = RectF { left: 0.0, top: 0.0, right: 612.0, bottom: 792.0 }; // 1 px/pt
        let w = pdf_rect_to_window(link, 792.0, 1.0, pr);
        assert_eq!((w.left, w.top, w.right, w.bottom), (72.0, 0.0, 200.0, 20.0));
    }

    #[test]
    fn pdf_rect_transform_scales_and_offsets() {
        let link = RectF { left: 100.0, top: 400.0, right: 200.0, bottom: 300.0 };
        let pr = RectF { left: 50.0, top: 10.0, right: 50.0 + 1224.0, bottom: 10.0 + 1584.0 }; // 2 px/pt
        let w = pdf_rect_to_window(link, 792.0, 2.0, pr);
        assert_eq!(w.left, 50.0 + 200.0);
        assert_eq!(w.top, 10.0 + (792.0 - 400.0) * 2.0);
    }

    // --- hit_zone: corners take precedence over edges ---

    #[test]
    fn hit_zone_corners_beat_edges() {
        let c = sz(1000, 1000);
        assert_eq!(hit_zone(pt(3.0, 3.0), c, 8.0), Zone::TopLeft);
        assert_eq!(hit_zone(pt(996.0, 3.0), c, 8.0), Zone::TopRight);
        assert_eq!(hit_zone(pt(3.0, 996.0), c, 8.0), Zone::BottomLeft);
        assert_eq!(hit_zone(pt(996.0, 996.0), c, 8.0), Zone::BottomRight);
    }

    #[test]
    fn hit_zone_edges_and_interior() {
        let c = sz(1000, 1000);
        assert_eq!(hit_zone(pt(500.0, 3.0), c, 8.0), Zone::Top);
        assert_eq!(hit_zone(pt(500.0, 996.0), c, 8.0), Zone::Bottom);
        assert_eq!(hit_zone(pt(3.0, 500.0), c, 8.0), Zone::Left);
        assert_eq!(hit_zone(pt(996.0, 500.0), c, 8.0), Zone::Right);
        assert_eq!(hit_zone(pt(500.0, 500.0), c, 8.0), Zone::Interior);
    }

    #[test]
    fn rectf_contains_is_half_open() {
        let r = RectF { left: 0.0, top: 0.0, right: 10.0, bottom: 10.0 };
        assert!(r.contains(pt(0.0, 0.0)));
        assert!(!r.contains(pt(10.0, 5.0)));
    }
}
```

Add `mod layout;` at the top of `src/main.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test layout`
Expected: compile FAILURE — `SizeU`, `fit_scale`, etc. not found.

- [ ] **Step 3: Implement the module**

Prepend the implementation above the `#[cfg(test)]` module in `src/layout.rs`:

```rust
pub const MAX_RENDER_DIM: u32 = 16_384;
pub const MAX_ZOOM: f32 = 5.0;
/// Pixels one wheel notch scrolls a zoomed page.
pub const SCROLL_STEP: f32 = 120.0;

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct SizeU {
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct PointF {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct RectF {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl RectF {
    pub fn contains(&self, p: PointF) -> bool {
        p.x >= self.left && p.x < self.right && p.y >= self.top && p.y < self.bottom
    }
    pub fn width(&self) -> f32 {
        self.right - self.left
    }
    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }
}

/// Scale that aspect-fits a page (in points) into the client (in px): px per pt.
pub fn fit_scale(page_pt: (f32, f32), client: SizeU) -> f32 {
    if page_pt.0 <= 0.0 || page_pt.1 <= 0.0 || client.w == 0 || client.h == 0 {
        return 1.0;
    }
    (client.w as f32 / page_pt.0).min(client.h as f32 / page_pt.1)
}

/// Pixel size to render the page at for the given zoom (1.0 = fit), capped at MAX_RENDER_DIM.
pub fn render_size(page_pt: (f32, f32), client: SizeU, zoom: f32) -> SizeU {
    let s = fit_scale(page_pt, client) * zoom;
    let mut w = (page_pt.0 * s).round().max(1.0);
    let mut h = (page_pt.1 * s).round().max(1.0);
    let cap = MAX_RENDER_DIM as f32;
    if w > cap || h > cap {
        let k = (cap / w).min(cap / h);
        w = (w * k).floor().max(1.0);
        h = (h * k).floor().max(1.0);
    }
    SizeU { w: w as u32, h: h as u32 }
}

/// Largest usable zoom for this page/client (<= MAX_ZOOM, honors MAX_RENDER_DIM).
pub fn max_zoom(page_pt: (f32, f32), client: SizeU) -> f32 {
    let s = fit_scale(page_pt, client);
    if s <= 0.0 {
        return 1.0;
    }
    let cap_w = MAX_RENDER_DIM as f32 / (page_pt.0 * s);
    let cap_h = MAX_RENDER_DIM as f32 / (page_pt.1 * s);
    MAX_ZOOM.min(cap_w).min(cap_h).max(1.0)
}

/// Clamp scroll to the page's overhang; axes where the page fits snap to 0.
pub fn clamp_scroll(page_px: SizeU, client: SizeU, scroll: PointF) -> PointF {
    let mx = (page_px.w as f32 - client.w as f32).max(0.0);
    let my = (page_px.h as f32 - client.h as f32).max(0.0);
    PointF { x: scroll.x.clamp(0.0, mx), y: scroll.y.clamp(0.0, my) }
}

/// Where the page bitmap lands in window coords (centered per axis when smaller).
pub fn page_rect(page_px: SizeU, client: SizeU, scroll: PointF) -> RectF {
    let x = if page_px.w <= client.w {
        ((client.w - page_px.w) as f32) / 2.0
    } else {
        -scroll.x
    };
    let y = if page_px.h <= client.h {
        ((client.h - page_px.h) as f32) / 2.0
    } else {
        -scroll.y
    };
    RectF { left: x, top: y, right: x + page_px.w as f32, bottom: y + page_px.h as f32 }
}

/// New scroll such that the page point under `cursor` stays under it after zooming
/// by `zoom_ratio` (new_zoom / old_zoom). Result is clamped.
pub fn zoom_scroll(
    cursor: PointF,
    old_rect: RectF,
    zoom_ratio: f32,
    new_page_px: SizeU,
    client: SizeU,
) -> PointF {
    let want_x = (cursor.x - old_rect.left) * zoom_ratio - cursor.x;
    let want_y = (cursor.y - old_rect.top) * zoom_ratio - cursor.y;
    clamp_scroll(new_page_px, client, PointF { x: want_x, y: want_y })
}

/// PDF-point rect (origin bottom-left, y up, top > bottom) -> window px rect.
/// `page_rect` is where the page bitmap sits in the window (from `page_rect()`).
pub fn pdf_rect_to_window(rect_pt: RectF, page_h_pt: f32, px_per_pt: f32, page_rect: RectF) -> RectF {
    RectF {
        left: page_rect.left + rect_pt.left * px_per_pt,
        top: page_rect.top + (page_h_pt - rect_pt.top) * px_per_pt,
        right: page_rect.left + rect_pt.right * px_per_pt,
        bottom: page_rect.top + (page_h_pt - rect_pt.bottom) * px_per_pt,
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Zone {
    TopLeft,
    Top,
    TopRight,
    Left,
    Interior,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

/// Resolve a client-space point to a resize-grip zone (corners win over edges).
pub fn hit_zone(p: PointF, client: SizeU, grip: f32) -> Zone {
    let l = p.x < grip;
    let r = p.x >= client.w as f32 - grip;
    let t = p.y < grip;
    let b = p.y >= client.h as f32 - grip;
    match (l, r, t, b) {
        (true, _, true, _) => Zone::TopLeft,
        (_, true, true, _) => Zone::TopRight,
        (true, _, _, true) => Zone::BottomLeft,
        (_, true, _, true) => Zone::BottomRight,
        (true, ..) => Zone::Left,
        (_, true, ..) => Zone::Right,
        (_, _, true, _) => Zone::Top,
        (_, _, _, true) => Zone::Bottom,
        _ => Zone::Interior,
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test layout`
Expected: `test result: ok. 17 passed`

- [ ] **Step 5: Commit**

```pwsh
git add src/layout.rs src/main.rs
git commit -m "feat: layout math - fit/zoom/pan, PDF rect transform, hit zones"
```

---

### Task 3: `types.rs` (OutlineItem) + `overlay.rs` — TOC panel state (pure logic, TDD)

**Files:**
- Create: `src/types.rs` (only `OutlineItem` for now; Task 4 extends this file)
- Create: `src/overlay.rs`
- Modify: `src/main.rs` (add `mod types;` and `mod overlay;`)

**Interfaces:**
- Consumes: `layout::PointF`
- Produces (used by `pdf.rs`, `app.rs`, `d2d.rs`, `window.rs`):
  - `types::OutlineItem { title: String, page: Option<u16>, depth: u8, has_children: bool }` (`Clone, Debug, PartialEq`)
  - `overlay::OutlineState::new(items: Vec<OutlineItem>) -> OutlineState` — top-level rows start expanded, deeper collapsed
  - `OutlineState::visible(&self) -> Vec<usize>` — indices into `items`, in display order
  - `OutlineState::move_selection(&mut self, delta: i32, panel_h: f32)`
  - `OutlineState::expand_selected(&mut self)` / `collapse_selected(&mut self)` (collapse on an already-collapsed row jumps to its parent)
  - `OutlineState::toggle(&mut self, item_idx: usize)`
  - `OutlineState::activate(&self) -> Option<u16>` — page of the selected row
  - `OutlineState::hit_test(&self, p: PointF, panel_h: f32) -> OverlayHit` where `enum OverlayHit { None, Chevron(usize), Title(usize) }` (indices into `items`; `p` is panel-local DIPs)
  - `OutlineState::select(&mut self, item_idx: usize)`
  - `OutlineState::is_expanded(&self, item_idx: usize) -> bool` (drawer uses it for ▸/▾ chevrons)
  - `OutlineState::scroll_by(&mut self, dy: f32, panel_h: f32)`
  - `OutlineState::row_top(&self, visible_pos: usize) -> f32` — DIP y of a row given its position in `visible()`
  - `OutlineState` fields readable by the drawer: `items`, `selected` (index into `items`), `scroll`
  - Constants: `ROW_H: f32 = 28.0`, `PANEL_W: f32 = 320.0`, `INDENT: f32 = 16.0`, `CHEVRON_W: f32 = 22.0`, `PANEL_PAD: f32 = 8.0` (all DIPs; `d2d.rs` multiplies by dpi/96)

- [ ] **Step 1: Create `src/types.rs`**

```rust
//! Types shared between the UI thread and the PDF thread.

/// One entry of the PDF outline, pre-order flattened.
#[derive(Clone, Debug, PartialEq)]
pub struct OutlineItem {
    pub title: String,
    /// 0-based target page, if the entry resolves to one.
    pub page: Option<u16>,
    /// 0 = top level.
    pub depth: u8,
    pub has_children: bool,
}
```

Add `mod types;` and `mod overlay;` to `src/main.rs`.

- [ ] **Step 2: Write the failing tests**

Create `src/overlay.rs` with only the test module:

```rust
//! TOC overlay state: visibility, selection, expansion, scrolling, hit-testing.
//! Pure logic — all coordinates are panel-local DIPs. No OS types allowed.

use crate::layout::PointF;
use crate::types::OutlineItem;

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, page: u16, depth: u8, has_children: bool) -> OutlineItem {
        OutlineItem { title: title.into(), page: Some(page), depth, has_children }
    }

    /// A(0)+ -> A1(1)+ -> A1a(2), A2(1); B(0)   ("+" = has children)
    fn tree() -> Vec<OutlineItem> {
        vec![
            item("A", 0, 0, true),
            item("A1", 2, 1, true),
            item("A1a", 4, 2, false),
            item("A2", 6, 1, false),
            item("B", 8, 0, false),
        ]
    }

    #[test]
    fn default_visibility_top_level_expanded_deeper_collapsed() {
        let s = OutlineState::new(tree());
        assert_eq!(s.visible(), vec![0, 1, 3, 4]); // A, A1, A2, B — A1a hidden
    }

    #[test]
    fn expanding_a1_reveals_grandchild() {
        let mut s = OutlineState::new(tree());
        s.select(1);
        s.expand_selected();
        assert_eq!(s.visible(), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn collapsing_root_hides_subtree() {
        let mut s = OutlineState::new(tree());
        s.select(0);
        s.collapse_selected();
        assert_eq!(s.visible(), vec![0, 4]); // A, B
    }

    #[test]
    fn collapse_on_leaf_jumps_to_parent() {
        let mut s = OutlineState::new(tree());
        s.select(3); // A2, no children
        s.collapse_selected();
        assert_eq!(s.selected, 0); // parent A
    }

    #[test]
    fn selection_moves_over_visible_rows_and_clamps() {
        let mut s = OutlineState::new(tree());
        assert_eq!(s.selected, 0);
        s.move_selection(1, 1000.0);
        assert_eq!(s.selected, 1); // A1
        s.move_selection(1, 1000.0);
        assert_eq!(s.selected, 3); // A2 (skips hidden A1a)
        s.move_selection(10, 1000.0);
        assert_eq!(s.selected, 4); // clamped at B
        s.move_selection(-10, 1000.0);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn toggling_ancestor_never_leaves_selection_on_hidden_row() {
        let mut s = OutlineState::new(tree());
        s.select(3); // A2 (child of A) is selected
        s.toggle(0); // collapse A -> A1, A2 disappear
        assert_eq!(s.selected, 0, "selection must jump to the collapsed ancestor");
        assert!(s.visible().contains(&s.selected));
    }

    #[test]
    fn activate_returns_selected_page() {
        let mut s = OutlineState::new(tree());
        s.move_selection(1, 1000.0);
        assert_eq!(s.activate(), Some(2)); // A1 -> page 2
    }

    #[test]
    fn hit_test_resolves_chevron_vs_title_vs_none() {
        let s = OutlineState::new(tree());
        // Row 1 in visible order is A1 (depth 1). Its chevron zone starts at
        // INDENT*1 = 16 and is CHEVRON_W wide.
        let row1_y = PANEL_PAD + ROW_H * 1.0 + ROW_H / 2.0;
        match s.hit_test(PointF { x: 20.0, y: row1_y }, 1000.0) {
            OverlayHit::Chevron(i) => assert_eq!(i, 1),
            other => panic!("expected Chevron, got {other:?}"),
        }
        match s.hit_test(PointF { x: 150.0, y: row1_y }, 1000.0) {
            OverlayHit::Title(i) => assert_eq!(i, 1),
            other => panic!("expected Title, got {other:?}"),
        }
        // Leaf rows have no chevron: x inside chevron zone still yields Title.
        let row3_y = PANEL_PAD + ROW_H * 3.0 + ROW_H / 2.0; // B, depth 0
        match s.hit_test(PointF { x: 4.0, y: row3_y }, 1000.0) {
            OverlayHit::Title(i) => assert_eq!(i, 4),
            other => panic!("expected Title, got {other:?}"),
        }
        // Below the last row: None.
        assert!(matches!(
            s.hit_test(PointF { x: 50.0, y: PANEL_PAD + ROW_H * 10.0 }, 1000.0),
            OverlayHit::None
        ));
    }

    #[test]
    fn scroll_clamps_to_content() {
        let mut s = OutlineState::new(tree()); // 4 visible rows
        let panel_h = 100.0; // content = 2*PANEL_PAD + 4*28 = 128
        s.scroll_by(9999.0, panel_h);
        assert!((s.scroll - 28.0).abs() < 0.01, "scroll = {}", s.scroll);
        s.scroll_by(-9999.0, panel_h);
        assert_eq!(s.scroll, 0.0);
        // taller panel than content: no scrolling at all
        let mut s2 = OutlineState::new(tree());
        s2.scroll_by(50.0, 1000.0);
        assert_eq!(s2.scroll, 0.0);
    }

    #[test]
    fn ensure_visible_scrolls_selection_into_view() {
        let mut s = OutlineState::new(tree());
        let panel_h = 2.0 * PANEL_PAD + ROW_H * 2.0; // fits 2 rows
        s.move_selection(3, panel_h); // to B, visible pos 3
        // row_top of B must now be inside [PANEL_PAD, panel_h - PANEL_PAD - ROW_H]
        let pos = s.visible().iter().position(|&i| i == s.selected).unwrap();
        let top = s.row_top(pos);
        assert!(top >= 0.0 && top + ROW_H <= panel_h, "top = {top}");
    }

    #[test]
    fn empty_outline_is_harmless() {
        let mut s = OutlineState::new(vec![]);
        assert!(s.visible().is_empty());
        s.move_selection(1, 100.0);
        assert_eq!(s.activate(), None);
        assert!(matches!(s.hit_test(PointF { x: 5.0, y: 5.0 }, 100.0), OverlayHit::None));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test overlay`
Expected: compile FAILURE — `OutlineState` not found.

- [ ] **Step 4: Implement the module**

Insert between the `use` lines and the test module in `src/overlay.rs`:

```rust
pub const ROW_H: f32 = 28.0;
pub const PANEL_W: f32 = 320.0;
pub const INDENT: f32 = 16.0;
pub const CHEVRON_W: f32 = 22.0;
pub const PANEL_PAD: f32 = 8.0;

#[derive(Debug)]
pub enum OverlayHit {
    None,
    Chevron(usize),
    Title(usize),
}

pub struct OutlineState {
    pub items: Vec<OutlineItem>,
    expanded: Vec<bool>,
    /// Index into `items`; always a visible row (invariant kept by all methods).
    pub selected: usize,
    /// DIP scroll offset into the row list, >= 0.
    pub scroll: f32,
}

impl OutlineState {
    pub fn new(items: Vec<OutlineItem>) -> Self {
        let expanded = items.iter().map(|it| it.depth == 0).collect();
        Self { items, expanded, selected: 0, scroll: 0.0 }
    }

    /// Indices into `items` of the rows currently shown, in display order.
    pub fn visible(&self) -> Vec<usize> {
        let mut out = Vec::new();
        let mut hide_deeper_than: Option<u8> = None;
        for (i, it) in self.items.iter().enumerate() {
            if let Some(d) = hide_deeper_than {
                if it.depth > d {
                    continue;
                }
                hide_deeper_than = None;
            }
            out.push(i);
            if it.has_children && !self.expanded[i] {
                hide_deeper_than = Some(it.depth);
            }
        }
        out
    }

    pub fn select(&mut self, item_idx: usize) {
        if item_idx < self.items.len() {
            self.selected = item_idx;
        }
    }

    pub fn is_expanded(&self, item_idx: usize) -> bool {
        self.expanded.get(item_idx).copied().unwrap_or(false)
    }

    pub fn move_selection(&mut self, delta: i32, panel_h: f32) {
        let vis = self.visible();
        if vis.is_empty() {
            return;
        }
        let pos = vis.iter().position(|&i| i == self.selected).unwrap_or(0) as i32;
        let new = (pos + delta).clamp(0, vis.len() as i32 - 1) as usize;
        self.selected = vis[new];
        self.ensure_visible(panel_h);
    }

    pub fn expand_selected(&mut self) {
        if self.items.get(self.selected).map_or(false, |it| it.has_children) {
            self.expanded[self.selected] = true;
        }
    }

    /// Collapse the selected row; if it has no children or is already
    /// collapsed, move selection to its parent instead.
    pub fn collapse_selected(&mut self) {
        let Some(it) = self.items.get(self.selected) else { return };
        if it.has_children && self.expanded[self.selected] {
            self.expanded[self.selected] = false;
        } else if let Some(parent) = self.parent_of(self.selected) {
            self.selected = parent;
        }
    }

    pub fn toggle(&mut self, item_idx: usize) {
        if self.items.get(item_idx).map_or(false, |it| it.has_children) {
            self.expanded[item_idx] = !self.expanded[item_idx];
            // Selection must never rest on a row that just got hidden.
            let vis = self.visible();
            if !vis.contains(&self.selected) {
                self.selected = item_idx;
            }
        }
    }

    pub fn activate(&self) -> Option<u16> {
        self.items.get(self.selected).and_then(|it| it.page)
    }

    fn parent_of(&self, item_idx: usize) -> Option<usize> {
        let depth = self.items.get(item_idx)?.depth;
        if depth == 0 {
            return None;
        }
        (0..item_idx).rev().find(|&i| self.items[i].depth == depth - 1)
    }

    /// DIP y of the top of the row at position `visible_pos` in `visible()`.
    pub fn row_top(&self, visible_pos: usize) -> f32 {
        PANEL_PAD + visible_pos as f32 * ROW_H - self.scroll
    }

    pub fn hit_test(&self, p: PointF, panel_h: f32) -> OverlayHit {
        if p.y < 0.0 || p.y > panel_h {
            return OverlayHit::None;
        }
        let vis = self.visible();
        let row_f = (p.y + self.scroll - PANEL_PAD) / ROW_H;
        if row_f < 0.0 {
            return OverlayHit::None;
        }
        let pos = row_f as usize;
        let Some(&idx) = vis.get(pos) else { return OverlayHit::None };
        let it = &self.items[idx];
        let chevron_x = it.depth as f32 * INDENT;
        if it.has_children && p.x >= chevron_x && p.x < chevron_x + CHEVRON_W {
            OverlayHit::Chevron(idx)
        } else {
            OverlayHit::Title(idx)
        }
    }

    fn max_scroll(&self, panel_h: f32) -> f32 {
        let content = 2.0 * PANEL_PAD + self.visible().len() as f32 * ROW_H;
        (content - panel_h).max(0.0)
    }

    pub fn scroll_by(&mut self, dy: f32, panel_h: f32) {
        self.scroll = (self.scroll + dy).clamp(0.0, self.max_scroll(panel_h));
    }

    /// Scroll just enough that the selected row is fully inside the panel.
    fn ensure_visible(&mut self, panel_h: f32) {
        let vis = self.visible();
        let Some(pos) = vis.iter().position(|&i| i == self.selected) else { return };
        let top = self.row_top(pos);
        if top < 0.0 {
            self.scroll += top;
        } else if top + ROW_H > panel_h {
            self.scroll += top + ROW_H - panel_h;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll(panel_h));
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test overlay`
Expected: `test result: ok. 11 passed`

Also run the full suite to make sure nothing else broke: `cargo test`
Expected: all green.

- [ ] **Step 6: Commit**

```pwsh
git add src/types.rs src/overlay.rs src/main.rs
git commit -m "feat: outline overlay state - visibility, selection, expansion, hit-test"
```

---

### Task 4: PDF thread — protocol, pdfium binding, render, outline, links (integration-tested)

**Files:**
- Modify: `src/types.rs` (add protocol types)
- Create: `src/pdf.rs`
- Create: `tests/common/mod.rs` (fixture PDF builder)
- Create: `tests/pdf_integration.rs`
- Modify: `src/main.rs` (add `mod pdf;`)

**Interfaces:**
- Consumes: `types::OutlineItem` (Task 3), `layout::{RectF, SizeU}` (Task 2), `pdfium-render` crate (Task 1)
- Produces (used by `app.rs`, `main.rs`):
  - `types::LinkTarget { Page(u16), Uri(String) }` (`Clone, Debug, PartialEq`)
  - `types::PageLink { rect_pt: RectF, target: LinkTarget }` (`Clone, Debug, PartialEq`) — `rect_pt` in PDF points, bottom-left origin, `top > bottom`
  - `types::PdfCmd { Load { path: PathBuf, password: Option<String> }, Render { page: u16, px: SizeU, visible: bool }, Shutdown }`
  - `types::PdfReply { Loaded { path, pages: u16, page_sizes: Vec<(f32, f32)>, outline: Vec<OutlineItem> }, NeedsPassword { path }, LoadFailed { path, reason: String }, Rendered { page: u16, px: SizeU, bgra: Vec<u8>, links: Vec<PageLink> }, RenderFailed { page: u16, reason: String } }`
  - `pdf::spawn(reply_tx: mpsc::Sender<PdfReply>, wake: impl Fn() + Send + 'static) -> mpsc::Sender<PdfCmd>` — spawns the PDF thread; `wake()` is called after every reply is queued (the UI passes a `PostMessage` closure; tests pass a no-op)
- Threading rule this task enforces: ALL pdfium calls happen on the spawned thread. `Pdfium`, `PdfDocument` etc. never cross the channel — only plain data (`Vec<u8>`, `String`, sizes).

**Command coalescing rule (implement exactly):** after blocking on the first command, drain everything else queued with `try_recv()`. From the drained batch keep: the LAST `Load` (a newer document supersedes everything), the LAST visible `Render`, and all non-superseded prefetch `Render`s (drop prefetches for a different size of a page that also has a newer request, and drop ALL renders older than a kept `Load`). `Shutdown` anywhere in the batch wins immediately.

- [ ] **Step 1: Extend `src/types.rs` with the protocol**

Append to `src/types.rs`:

```rust
use crate::layout::{RectF, SizeU};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub enum LinkTarget {
    /// 0-based page index inside this document.
    Page(u16),
    Uri(String),
}

/// A clickable region on a page. `rect_pt` is in PDF points
/// (origin bottom-left, y up, so `top > bottom` numerically).
#[derive(Clone, Debug, PartialEq)]
pub struct PageLink {
    pub rect_pt: RectF,
    pub target: LinkTarget,
}

/// UI thread -> PDF thread.
#[derive(Debug)]
pub enum PdfCmd {
    Load { path: PathBuf, password: Option<String> },
    Render { page: u16, px: SizeU, visible: bool },
    Shutdown,
}

/// PDF thread -> UI thread.
#[derive(Debug)]
pub enum PdfReply {
    Loaded {
        path: PathBuf,
        pages: u16,
        page_sizes: Vec<(f32, f32)>,
        outline: Vec<OutlineItem>,
    },
    NeedsPassword {
        path: PathBuf,
    },
    LoadFailed {
        path: PathBuf,
        reason: String,
    },
    Rendered {
        page: u16,
        px: SizeU,
        bgra: Vec<u8>,
        links: Vec<PageLink>,
    },
    RenderFailed {
        page: u16,
        reason: String,
    },
}
```

- [ ] **Step 2: Write the fixture PDF builder**

Create `tests/common/mod.rs`. This builds a complete, valid 3-page PDF **in memory with a correctly computed xref table** (offsets are computed at runtime, so nothing is hand-counted). Each page draws a filled black rectangle (font-independent, so "non-blank pixels" is a robust assertion). Page 1 carries two link annotations (an internal GoTo to page 3 and a URI); the outline has two top-level chapters and one nested section.

```rust
//! Builds a small fixture PDF entirely in memory: 3 US-Letter pages, each
//! with a filled black rectangle; page 1 has a GoTo link and a URI link;
//! outline: "Chapter One" -> p2, "Chapter Two" -> p3 with child "Section 2.1".
//! Object numbers are fixed; byte offsets for the xref are computed, never
//! hand-counted.

pub fn build_fixture_pdf() -> Vec<u8> {
    fn stream_obj(content: &str) -> String {
        format!("<< /Length {} >>\nstream\n{}\nendstream", content.len(), content)
    }

    let objs: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 9 0 R >>".into()),
        (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>".into()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R \
             /Annots [13 0 R 14 0 R] >>"
                .into(),
        ),
        (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R >>".into()),
        (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 8 0 R >>".into()),
        (6, stream_obj("0 0 0 rg 100 600 200 100 re f")),
        (7, stream_obj("0 0 0 rg 100 500 300 150 re f")),
        (8, stream_obj("0 0 0 rg 200 300 150 300 re f")),
        (9, "<< /Type /Outlines /First 10 0 R /Last 11 0 R /Count 3 >>".into()),
        (
            10,
            "<< /Title (Chapter One) /Parent 9 0 R /Next 11 0 R \
             /Dest [4 0 R /XYZ null null null] >>"
                .into(),
        ),
        (
            11,
            "<< /Title (Chapter Two) /Parent 9 0 R /Prev 10 0 R /First 12 0 R \
             /Last 12 0 R /Count 1 /Dest [5 0 R /XYZ null null null] >>"
                .into(),
        ),
        (
            12,
            "<< /Title (Section 2.1) /Parent 11 0 R /Dest [5 0 R /XYZ null null null] >>".into(),
        ),
        (
            13,
            "<< /Type /Annot /Subtype /Link /Rect [72 700 200 720] /Border [0 0 0] \
             /A << /S /GoTo /D [5 0 R /XYZ null null null] >> >>"
                .into(),
        ),
        (
            14,
            "<< /Type /Annot /Subtype /Link /Rect [300 650 500 670] /Border [0 0 0] \
             /A << /S /URI /URI (https://example.com/) >> >>"
                .into(),
        ),
    ];

    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![0usize; objs.len() + 1];
    for (id, body) in &objs {
        offsets[*id as usize] = out.len();
        out.extend_from_slice(format!("{id} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_pos = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objs.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for id in 1..=objs.len() {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[id]).as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objs.len() + 1,
            xref_pos
        )
        .as_bytes(),
    );
    out
}

/// Write the fixture to a temp file and return its path.
pub fn fixture_path() -> std::path::PathBuf {
    let path = std::env::temp_dir().join("pppdf_fixture.pdf");
    std::fs::write(&path, build_fixture_pdf()).expect("write fixture pdf");
    path
}
```

- [ ] **Step 3: Write the failing integration test**

Create `tests/pdf_integration.rs`:

```rust
//! End-to-end smoke test through a real pdfium.dll. Requires pdfium.dll in
//! target\debug (run .\fetch-pdfium.ps1 once first — see README).

mod common;

use pppdf::layout::SizeU;
use pppdf::pdf;
use pppdf::types::{LinkTarget, PdfCmd, PdfReply};
use std::sync::mpsc;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(20);

#[test]
fn load_render_outline_links() {
    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});

    // --- load ---
    cmd_tx
        .send(PdfCmd::Load { path: common::fixture_path(), password: None })
        .unwrap();
    let (pages, page_sizes, outline) = match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { pages, page_sizes, outline, .. } => (pages, page_sizes, outline),
        other => panic!("expected Loaded, got {other:?}"),
    };
    assert_eq!(pages, 3);
    assert_eq!(page_sizes.len(), 3);
    assert!((page_sizes[0].0 - 612.0).abs() < 0.5);
    assert!((page_sizes[0].1 - 792.0).abs() < 0.5);

    // --- outline: titles, depths, targets ---
    let titles: Vec<_> = outline.iter().map(|o| o.title.as_str()).collect();
    assert_eq!(titles, vec!["Chapter One", "Chapter Two", "Section 2.1"]);
    let depths: Vec<_> = outline.iter().map(|o| o.depth).collect();
    assert_eq!(depths, vec![0, 0, 1]);
    assert_eq!(outline[0].page, Some(1)); // Chapter One -> page 2 (0-based 1)
    assert_eq!(outline[2].page, Some(2));
    assert!(outline[1].has_children);
    assert!(!outline[0].has_children);

    // --- render page 1 (index 0) at exact pixel size ---
    let px = SizeU { w: 612, h: 792 };
    cmd_tx.send(PdfCmd::Render { page: 0, px, visible: true }).unwrap();
    let (rpage, rpx, bgra, links) = match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Rendered { page, px, bgra, links } => (page, px, bgra, links),
        other => panic!("expected Rendered, got {other:?}"),
    };
    assert_eq!(rpage, 0);
    assert_eq!(rpx, px);
    assert_eq!(bgra.len(), (px.w * px.h * 4) as usize);
    // The content stream fills a black rect: some pixel must be dark.
    let dark = bgra.chunks_exact(4).any(|p| p[0] < 60 && p[1] < 60 && p[2] < 60);
    assert!(dark, "rendered page has no dark pixels — rendering failed?");

    // --- links on page 1 ---
    assert_eq!(links.len(), 2);
    let goto = links.iter().find(|l| matches!(l.target, LinkTarget::Page(_))).unwrap();
    assert_eq!(goto.target, LinkTarget::Page(2));
    // PDF rect [72 700 200 720]: left/bottom/right/top.
    assert!((goto.rect_pt.left - 72.0).abs() < 1.0);
    assert!((goto.rect_pt.bottom - 700.0).abs() < 1.0);
    assert!((goto.rect_pt.top - 720.0).abs() < 1.0);
    let uri = links.iter().find(|l| matches!(l.target, LinkTarget::Uri(_))).unwrap();
    assert_eq!(uri.target, LinkTarget::Uri("https://example.com/".into()));

    // --- shutdown is clean ---
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
}

#[test]
fn load_failure_reports_reason() {
    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    let bogus = std::env::temp_dir().join("pppdf_not_a_pdf.pdf");
    std::fs::write(&bogus, b"this is not a pdf").unwrap();
    cmd_tx.send(PdfCmd::Load { path: bogus, password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::LoadFailed { reason, .. } => assert!(!reason.is_empty()),
        other => panic!("expected LoadFailed, got {other:?}"),
    }
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
}
```

Note: for integration tests to `use pppdf::...`, the crate needs a library target. Add `src/lib.rs`:

```rust
//! Library facade so integration tests can reach the internals.
pub mod layout;
pub mod overlay;
pub mod pdf;
pub mod types;
```

and slim `src/main.rs` down to modules-free form (from this task on, ALL modules live in the lib; later tasks add `pub mod app;` / `pub mod d2d;` / `pub mod window;` to `src/lib.rs` as those files appear):

```rust
fn main() {
    println!("pppdf scaffold");
}
```

- [ ] **Step 4: Run the test to verify it fails**

Run: `cargo test --test pdf_integration`
Expected: compile FAILURE — `pdf::spawn` not found.

- [ ] **Step 5: Implement `src/pdf.rs`**

API notes (verified against pdfium-render **0.9.2** docs — do not use 0.8.x idioms):
`PdfPageIndex` is `i32`; `Pixels` is `i32`; `PdfRect` fields are private (use `.left().value` etc.); `PdfRenderConfig` defaults `set_reverse_byte_order(true)` which produces RGBA — we explicitly disable it to get true BGRA for Direct2D; `as_raw_bytes()` returns the whole buffer (`stride * height`) so compute `stride = len / height` and repack if padded; bindings are a process-global singleton (`PdfiumLibraryBindingsAlreadyInitialized` on second init) so `Pdfium` is created once and leaked to get `'static` document lifetimes; a wrong/missing password is `PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError)`.

```rust
//! The PDF thread: owns ALL pdfium state; speaks PdfCmd in, PdfReply out.

use crate::layout::{RectF, SizeU, MAX_RENDER_DIM};
use crate::types::{LinkTarget, OutlineItem, PageLink, PdfCmd, PdfReply};
use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::OnceLock;

pub fn spawn(reply_tx: Sender<PdfReply>, wake: impl Fn() + Send + 'static) -> Sender<PdfCmd> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("pdf".into())
        .spawn(move || run(cmd_rx, reply_tx, wake))
        .expect("spawn pdf thread");
    cmd_tx
}

/// Locate pdfium.dll: the exe's directory first, then up to two ancestors
/// (covers `cargo test`, where the test exe is in target\debug\deps but the
/// dll sits in target\debug), then the system search path.
fn bind_pdfium() -> Result<Pdfium, PdfiumError> {
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(Path::to_path_buf);
        for _ in 0..3 {
            let Some(d) = dir else { break };
            let candidate = Pdfium::pdfium_platform_library_name_at_path(&d);
            if candidate.exists() {
                return Ok(Pdfium::new(Pdfium::bind_to_library(&candidate)?));
            }
            dir = d.parent().map(Path::to_path_buf);
        }
    }
    Ok(Pdfium::new(Pdfium::bind_to_system_library()?))
}

/// Process-wide binding. pdfium-render 0.9.x bindings are a global singleton
/// (a second bind returns PdfiumLibraryBindingsAlreadyInitialized), so bind
/// exactly once and share the leaked instance — this also lets multiple test
/// functions in one test process each spawn a PDF thread safely.
fn pdfium() -> Result<&'static Pdfium, String> {
    static PDFIUM: OnceLock<Result<&'static Pdfium, String>> = OnceLock::new();
    PDFIUM
        .get_or_init(|| {
            bind_pdfium()
                .map(|p| &*Box::leak(Box::new(p)))
                .map_err(|e| format!("{e:?}"))
        })
        .clone()
}

fn run(cmd_rx: Receiver<PdfCmd>, reply_tx: Sender<PdfReply>, wake: impl Fn()) {
    let pdfium: &'static Pdfium = match pdfium() {
        Ok(p) => p,
        Err(e) => {
            let _ = reply_tx.send(PdfReply::LoadFailed {
                path: PathBuf::new(),
                reason: format!("pdfium.dll could not be loaded: {e}"),
            });
            wake();
            return;
        }
    };
    let mut doc: Option<PdfDocument<'static>> = None;
    // A command consumed by the preemption check below, carried into the next iteration.
    let mut carry: Option<PdfCmd> = None;

    'outer: loop {
        let first = match carry.take() {
            Some(c) => c,
            None => match cmd_rx.recv() {
                Ok(c) => c,
                Err(_) => break, // UI hung up
            },
        };
        let mut batch = vec![first];
        loop {
            match cmd_rx.try_recv() {
                Ok(c) => batch.push(c),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'outer,
            }
        }

        // Coalesce. Shutdown wins outright. A Load supersedes everything queued
        // before it. Only the LAST visible render is kept. EVERY dropped render
        // gets a RenderFailed("superseded") so the UI can clear its pending set
        // — including renders dropped by a Load: if that load then fails (bad
        // file, password), no Loaded reply arrives to reset the UI, and without
        // the supersede replies those requests would be stranded as pending
        // forever. When the load succeeds, the extra replies are harmless
        // (Loaded clears everything anyway).
        let mut load: Option<(PathBuf, Option<String>)> = None;
        let mut visible: Option<(u16, SizeU)> = None;
        let mut prefetch: Vec<(u16, SizeU)> = Vec::new();
        let mut superseded: Vec<u16> = Vec::new();
        for cmd in batch {
            match cmd {
                PdfCmd::Shutdown => break 'outer,
                PdfCmd::Load { path, password } => {
                    if let Some((old, _)) = visible.take() {
                        superseded.push(old);
                    }
                    superseded.extend(prefetch.drain(..).map(|(page, _)| page));
                    load = Some((path, password));
                }
                PdfCmd::Render { page, px, visible: true } => {
                    if let Some((old, _)) = visible.replace((page, px)) {
                        superseded.push(old);
                    }
                }
                PdfCmd::Render { page, px, visible: false } => prefetch.push((page, px)),
            }
        }
        for page in superseded {
            let _ = reply_tx.send(PdfReply::RenderFailed { page, reason: "superseded".into() });
        }

        if let Some((path, password)) = load {
            // Keep the old document until the new one opens successfully, so a
            // failed open leaves the current book fully usable.
            match pdfium.load_pdf_from_file(&path, password.as_deref()) {
                Ok(d) => {
                    let _ = reply_tx.send(describe(&d, &path));
                    doc = Some(d);
                }
                Err(PdfiumError::PdfiumLibraryInternalError(
                    PdfiumInternalError::PasswordError,
                )) => {
                    let _ = reply_tx.send(PdfReply::NeedsPassword { path });
                }
                Err(e) => {
                    let _ = reply_tx.send(PdfReply::LoadFailed { path, reason: format!("{e:?}") });
                }
            }
            wake();
        }

        let work: Vec<(u16, SizeU)> = visible.into_iter().chain(prefetch).collect();
        let mut dropped_after: Option<usize> = None;
        for (i, (page, px)) in work.iter().enumerate() {
            let Some(d) = &doc else { break };
            let _ = reply_tx.send(render_page(d, *page, *px));
            wake();
            // Preempt: if new commands arrived mid-batch (e.g. a rapid page
            // flip), stop rendering stale work and loop around.
            match cmd_rx.try_recv() {
                Ok(c) => {
                    carry = Some(c);
                    dropped_after = Some(i + 1);
                    break;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => break 'outer,
            }
        }
        if let Some(from) = dropped_after {
            for (page, _) in &work[from..] {
                let _ = reply_tx
                    .send(PdfReply::RenderFailed { page: *page, reason: "superseded".into() });
            }
            wake();
        }
    }
}

/// Page count, all page sizes in points, and the outline flattened pre-order.
fn describe(doc: &PdfDocument, path: &Path) -> PdfReply {
    let pages = doc.pages().len().clamp(0, u16::MAX as i32) as u16;
    let page_sizes: Vec<(f32, f32)> = match doc.pages().page_sizes() {
        Ok(sizes) => sizes.iter().map(|r| (r.width().value, r.height().value)).collect(),
        Err(_) => vec![(612.0, 792.0); pages as usize],
    };
    let mut outline = Vec::new();
    // root() is the FIRST top-level bookmark; walk it and its sibling chain.
    let mut current = doc.bookmarks().root();
    while let Some(b) = current {
        let next = b.next_sibling();
        walk_bookmark(&b, 0, &mut outline);
        current = next;
    }
    PdfReply::Loaded { path: path.to_path_buf(), pages, page_sizes, outline }
}

fn walk_bookmark(b: &PdfBookmark, depth: u8, out: &mut Vec<OutlineItem>) {
    out.push(OutlineItem {
        title: b.title().unwrap_or_default(),
        page: resolve_bookmark_page(b),
        depth,
        has_children: b.first_child().is_some(),
    });
    for child in b.iter_direct_children() {
        walk_bookmark(&child, depth.saturating_add(1), out);
    }
}

/// Direct /Dest first, then a GoTo action's destination.
fn resolve_bookmark_page(b: &PdfBookmark) -> Option<u16> {
    if let Some(dest) = b.destination() {
        return dest.page_index().ok().map(to_u16);
    }
    if let Some(action) = b.action() {
        if let Some(local) = action.as_local_destination_action() {
            return local.destination().ok().and_then(|d| d.page_index().ok()).map(to_u16);
        }
    }
    None
}

fn to_u16(i: PdfPageIndex) -> u16 {
    i.clamp(0, u16::MAX as i32) as u16
}

fn render_page(doc: &PdfDocument, page_idx: u16, px: SizeU) -> PdfReply {
    let page = match doc.pages().get(page_idx as PdfPageIndex) {
        Ok(p) => p,
        Err(e) => return PdfReply::RenderFailed { page: page_idx, reason: format!("{e:?}") },
    };
    let config = PdfRenderConfig::new()
        .set_target_size(px.w as Pixels, px.h as Pixels)
        .set_maximum_width(MAX_RENDER_DIM as Pixels)
        .set_maximum_height(MAX_RENDER_DIM as Pixels)
        .set_format(PdfBitmapFormat::BGRA)
        // Default is true, which writes RGBA; Direct2D wants real BGRA.
        .set_reverse_byte_order(false);
    let bitmap = match page.render_with_config(&config) {
        Ok(b) => b,
        Err(e) => return PdfReply::RenderFailed { page: page_idx, reason: format!("{e:?}") },
    };
    let (w, h) = (bitmap.width() as usize, bitmap.height() as usize);
    if w == 0 || h == 0 {
        return PdfReply::RenderFailed { page: page_idx, reason: "empty bitmap".into() };
    }
    let raw = bitmap.as_raw_bytes();
    let stride = raw.len() / h;
    // Direct2D wants tight rows; repack only if pdfium padded the stride.
    let bgra = if stride == w * 4 {
        raw
    } else {
        let mut tight = Vec::with_capacity(w * h * 4);
        for row in 0..h {
            let start = row * stride;
            tight.extend_from_slice(&raw[start..start + w * 4]);
        }
        tight
    };
    PdfReply::Rendered {
        page: page_idx,
        px: SizeU { w: w as u32, h: h as u32 },
        bgra,
        links: extract_links(&page),
    }
}

/// Link rects (PDF points, bottom-left origin) plus resolved targets.
fn extract_links(page: &PdfPage) -> Vec<PageLink> {
    let mut out = Vec::new();
    for link in page.links().iter() {
        let Ok(rect) = link.rect() else { continue };
        let target = if let Some(dest) = link.destination() {
            dest.page_index().ok().map(|i| LinkTarget::Page(to_u16(i)))
        } else if let Some(action) = link.action() {
            match action {
                PdfAction::LocalDestination(local) => local
                    .destination()
                    .ok()
                    .and_then(|d| d.page_index().ok())
                    .map(|i| LinkTarget::Page(to_u16(i))),
                PdfAction::Uri(uri) => uri.uri().ok().map(LinkTarget::Uri),
                _ => None, // Launch / RemoteDestination / Embedded: ignored per spec
            }
        } else {
            None
        };
        if let Some(target) = target {
            out.push(PageLink {
                rect_pt: RectF {
                    left: rect.left().value,
                    top: rect.top().value,
                    right: rect.right().value,
                    bottom: rect.bottom().value,
                },
                target,
            });
        }
    }
    out
}
```

If `action.as_local_destination_action()` does not exist under this exact name, match on the `PdfAction` enum variants instead (as `extract_links` does) — the enum form is verified for 0.9.2.

- [ ] **Step 6: Run the integration test to verify it passes**

First make sure the dll is in place (idempotent): `.\fetch-pdfium.ps1`
Run: `cargo test --test pdf_integration`
Expected: `test result: ok. 2 passed`

Run everything: `cargo test`
Expected: all green.

- [ ] **Step 7: Commit**

```pwsh
git add src/types.rs src/pdf.rs src/lib.rs src/main.rs tests/
git commit -m "feat: PDF thread - pdfium binding, render, outline and link extraction"
```

---

### Task 5: `app.rs` — state machine, page cache, wheel/zoom semantics (pure logic, TDD)

**Files:**
- Create: `src/app.rs`
- Modify: `src/lib.rs` (add `pub mod app;` — the lib facade exists since Task 4)

**Interfaces:**
- Consumes:
  - `layout::{SizeU, PointF, RectF, fit_scale, render_size, max_zoom, clamp_scroll, page_rect, zoom_scroll, pdf_rect_to_window, SCROLL_STEP}` (Task 2)
  - `overlay::OutlineState` (Task 3)
  - `types::{OutlineItem, PageLink, LinkTarget}` (Tasks 3–4)
- Produces (used by `main.rs`, `window.rs`, `d2d.rs`):
  - `enum Mode { Empty, Loading { path: PathBuf }, Password { path: PathBuf, buf: String, prev: Option<Box<DocState>> }, Doc(DocState) }` — `prev` stashes the currently open document while the password prompt shows, so cancel restores it (spec: "keep current doc if one is open")
  - `struct DocState { path: PathBuf, pages: u16, page_sizes: Vec<(f32, f32)>, outline: Vec<OutlineItem>, current: u16, zoom: f32, scroll: PointF, overlay: Option<OutlineState> }`
  - `struct App { mode: Mode, cache: PageCache, pending: HashSet<(u16, u32, u32)>, failed: HashSet<u16>, client: SizeU, dpi: u32 }` with `App::new() -> App` (mode Empty, cache cap 6, dpi 96) — `failed` holds pages whose render genuinely failed, so they are never hot-loop re-requested (spec: corrupt pages are accepted silently); cleared on document load
  - `struct CachedPage { page: u16, px: SizeU, bgra: Vec<u8>, links: Vec<PageLink> }`
  - `struct PageCache` — `new(cap)`, `get_any(&self, page) -> Option<&CachedPage>` (freshest entry for the page), `get_best(&self, page, want: SizeU) -> Option<&CachedPage>` (prefers a size within ±2 px, else freshest), `has_close(&self, page, px) -> bool` (any entry within ±2 px — tolerance absorbs renderer rounding so requests never loop), `len(&self)`, `put(&mut, CachedPage)`, `clear(&mut)`
  - `struct RenderReq { page: u16, px: SizeU, visible: bool }`
  - `enum WheelOutcome { None, Redraw, Flipped }`
  - `App` methods: `set_client(SizeU)`, `doc_loaded(path, pages, page_sizes, outline)`, `flip(i32) -> bool`, `goto(u16) -> bool`, `wheel(i32) -> WheelOutcome`, `zoom_by(PointF, f32) -> bool`, `reset_zoom() -> bool`, `toggle_overlay()`, `wanted_renders() -> Vec<RenderReq>`, `mark_pending(&RenderReq)`, `rendered(CachedPage)`, `render_failed(page: u16, superseded: bool)` (clears that page's pending entries; a non-superseded failure also marks the page `failed` so it is not re-requested), `current_px() -> Option<SizeU>`, `current_page_rect() -> Option<RectF>`, `link_at(PointF) -> Option<LinkTarget>`, `doc(&self) -> Option<&DocState>`, `doc_mut(&mut self) -> Option<&mut DocState>`, `password_char(char)`, `password_backspace()`, `password_take() -> Option<(PathBuf, String)>` (restores the stashed doc — or Loading — and hands back the typed password), `password_cancel()` (restores the stashed doc, or Empty)

**Pending/supersede protocol (matches the PDF thread's coalescing in Task 4):** the UI marks a request pending when it sends it. The PDF thread answers every request eventually — either `Rendered` or `RenderFailed` (reason `"superseded"` for requests it coalesced away). Both replies clear pending **by page** (not by exact size, so renderer size-rounding can never strand a pending entry).

**Behavioral rules encoded here (from the spec):**
- Page change always resets zoom to fit (1.0) and scroll to 0.
- Wheel at fit flips pages. Wheel while zoomed scrolls vertically by `SCROLL_STEP` px per notch; a notch arriving while ALREADY at the page edge flips (so one notch scrolls to the edge and stops, the next flips).
- Zoom is clamped to `[1.0, layout::max_zoom(...)]`; zooming keeps the page point under the cursor stationary; reaching 1.0 zeroes scroll.
- `wanted_renders()` = crisp render of the current page at the current zoom (if not cached and not pending), then prefetch of pages current±1 at THEIR fit size (zoom 1.0), next before previous. Nothing when no document.

- [ ] **Step 1: Write the failing tests**

Create `src/app.rs` with only imports and the test module:

```rust
//! Application state machine + page cache. Pure logic — no OS types.

use crate::layout::{
    self, clamp_scroll, fit_scale, max_zoom, page_rect, pdf_rect_to_window, render_size,
    zoom_scroll, PointF, RectF, SizeU, SCROLL_STEP,
};
use crate::overlay::OutlineState;
use crate::types::{LinkTarget, OutlineItem, PageLink};
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::PathBuf;

#[cfg(test)]
mod tests {
    use super::*;

    const LETTER: (f32, f32) = (612.0, 792.0);

    fn app_with_doc(pages: u16) -> App {
        let mut a = App::new();
        a.set_client(SizeU { w: 1000, h: 1000 });
        a.doc_loaded(
            PathBuf::from("x.pdf"),
            pages,
            vec![LETTER; pages as usize],
            vec![],
        );
        a
    }

    fn cached(page: u16, px: SizeU) -> CachedPage {
        CachedPage { page, px, bgra: vec![0; 4], links: vec![] }
    }

    // --- paging ---

    #[test]
    fn flip_clamps_at_both_ends() {
        let mut a = app_with_doc(3);
        assert!(!a.flip(-1), "already at first page");
        assert!(a.flip(1));
        assert_eq!(a.doc().unwrap().current, 1);
        assert!(a.flip(5)); // clamps to last
        assert_eq!(a.doc().unwrap().current, 2);
        assert!(!a.flip(1));
    }

    #[test]
    fn flip_resets_zoom_and_scroll() {
        let mut a = app_with_doc(3);
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 2.0);
        assert!(a.doc().unwrap().zoom > 1.0);
        a.flip(1);
        let d = a.doc().unwrap();
        assert_eq!(d.zoom, 1.0);
        assert_eq!(d.scroll, PointF { x: 0.0, y: 0.0 });
    }

    #[test]
    fn goto_clamps_out_of_range() {
        let mut a = app_with_doc(3);
        a.goto(99);
        assert_eq!(a.doc().unwrap().current, 2);
    }

    // --- wheel ---

    #[test]
    fn wheel_at_fit_flips() {
        let mut a = app_with_doc(3);
        assert_eq!(a.wheel(1), WheelOutcome::Flipped);
        assert_eq!(a.doc().unwrap().current, 1);
        assert_eq!(a.wheel(-1), WheelOutcome::Flipped);
        assert_eq!(a.doc().unwrap().current, 0);
    }

    #[test]
    fn wheel_zoomed_scrolls_then_flips_only_from_edge() {
        let mut a = app_with_doc(3);
        // Zoom deep so the page is much taller than the client.
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 4.0);
        let max_y = {
            let d = a.doc().unwrap();
            let px = render_size(LETTER, a.client, d.zoom);
            px.h as f32 - a.client.h as f32
        };
        assert!(max_y > SCROLL_STEP, "test premise: page overflows by more than one step");
        // Scroll to the bottom edge with many notches: must NOT flip.
        let notches = (max_y / SCROLL_STEP).ceil() as i32 + 1;
        for _ in 0..notches {
            if a.doc().unwrap().scroll.y >= max_y {
                break;
            }
            assert_eq!(a.wheel(1), WheelOutcome::Redraw);
        }
        assert!((a.doc().unwrap().scroll.y - max_y).abs() < 0.5);
        assert_eq!(a.doc().unwrap().current, 0, "scrolling to the edge must not flip");
        // The NEXT notch, already at the edge, flips and resets to fit.
        assert_eq!(a.wheel(1), WheelOutcome::Flipped);
        let d = a.doc().unwrap();
        assert_eq!(d.current, 1);
        assert_eq!(d.zoom, 1.0);
    }

    #[test]
    fn wheel_zoomed_up_at_top_flips_back() {
        let mut a = app_with_doc(3);
        a.flip(1);
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 3.0);
        // scroll.y is clamped >= 0; possibly already 0 at top depending on cursor. Force top:
        a.doc_mut().unwrap().scroll.y = 0.0;
        assert_eq!(a.wheel(-1), WheelOutcome::Flipped);
        assert_eq!(a.doc().unwrap().current, 0);
    }

    #[test]
    fn wheel_without_doc_is_none() {
        let mut a = App::new();
        assert_eq!(a.wheel(1), WheelOutcome::None);
    }

    // --- zoom ---

    #[test]
    fn zoom_clamps_to_one_and_max() {
        let mut a = app_with_doc(1);
        assert!(!a.zoom_by(PointF { x: 0.0, y: 0.0 }, 0.5), "already at fit");
        assert_eq!(a.doc().unwrap().zoom, 1.0);
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 100.0);
        let d = a.doc().unwrap();
        let mz = max_zoom(LETTER, a.client);
        assert!((d.zoom - mz).abs() < 1e-4);
    }

    #[test]
    fn zoom_back_to_fit_zeroes_scroll() {
        let mut a = app_with_doc(1);
        a.zoom_by(PointF { x: 700.0, y: 700.0 }, 3.0);
        a.doc_mut().unwrap().scroll = PointF { x: 50.0, y: 400.0 };
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 0.01); // clamps to 1.0
        let d = a.doc().unwrap();
        assert_eq!(d.zoom, 1.0);
        assert_eq!(d.scroll, PointF { x: 0.0, y: 0.0 });
    }

    #[test]
    fn reset_zoom_only_reports_change() {
        let mut a = app_with_doc(1);
        assert!(!a.reset_zoom());
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 2.0);
        assert!(a.reset_zoom());
        assert_eq!(a.doc().unwrap().zoom, 1.0);
    }

    // --- wanted_renders / pending / cache ---

    #[test]
    fn wanted_renders_current_then_next_then_prev() {
        let mut a = app_with_doc(5);
        a.flip(2); // current = 2
        let reqs = a.wanted_renders();
        assert_eq!(reqs.len(), 3);
        assert_eq!((reqs[0].page, reqs[0].visible), (2, true));
        assert_eq!((reqs[1].page, reqs[1].visible), (3, false));
        assert_eq!((reqs[2].page, reqs[2].visible), (1, false));
        let fit_px = render_size(LETTER, a.client, 1.0);
        assert_eq!(reqs[0].px, fit_px);
    }

    #[test]
    fn wanted_renders_skips_cached_and_pending_and_ends() {
        let mut a = app_with_doc(2); // pages 0, 1; current 0 -> no prev
        let fit_px = render_size(LETTER, a.client, 1.0);
        a.rendered(cached(0, fit_px)); // current cached
        let reqs = a.wanted_renders();
        assert_eq!(reqs.len(), 1, "only the next-page prefetch remains");
        assert_eq!(reqs[0].page, 1);
        a.mark_pending(&reqs[0]);
        assert!(a.wanted_renders().is_empty());
        // Rendered reply clears pending.
        a.rendered(cached(1, fit_px));
        assert!(a.wanted_renders().is_empty()); // now cached instead of pending
        assert!(a.pending.is_empty());
    }

    #[test]
    fn wanted_renders_empty_without_doc() {
        let a = App::new();
        assert!(a.wanted_renders().is_empty());
    }

    #[test]
    fn cache_evicts_oldest_beyond_cap() {
        let mut c = PageCache::new(2);
        let px = SizeU { w: 10, h: 10 };
        c.put(cached(0, px));
        c.put(cached(1, px));
        c.put(cached(2, px));
        assert!(c.get_any(0).is_none(), "0 was evicted");
        assert!(c.get_any(1).is_some());
        assert!(c.get_any(2).is_some());
    }

    #[test]
    fn cache_replaces_same_key_instead_of_duplicating() {
        let mut c = PageCache::new(4);
        c.put(cached(0, SizeU { w: 10, h: 10 }));
        c.put(cached(0, SizeU { w: 10, h: 10 }));
        assert_eq!(c.len(), 1);
        c.put(cached(0, SizeU { w: 20, h: 20 })); // different size = separate entry
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn cache_has_close_tolerates_render_rounding() {
        let mut c = PageCache::new(4);
        c.put(cached(0, SizeU { w: 800, h: 1000 }));
        assert!(c.has_close(0, SizeU { w: 801, h: 999 }));
        assert!(!c.has_close(0, SizeU { w: 850, h: 1000 }));
        assert!(!c.has_close(1, SizeU { w: 800, h: 1000 }));
    }

    #[test]
    fn cache_get_best_prefers_size_match_then_freshest() {
        let mut c = PageCache::new(4);
        c.put(cached(0, SizeU { w: 100, h: 100 }));
        c.put(cached(0, SizeU { w: 200, h: 200 })); // freshest
        let best = c.get_best(0, SizeU { w: 101, h: 100 }).unwrap();
        assert_eq!(best.px, SizeU { w: 100, h: 100 }, "size match wins over freshness");
        let fallback = c.get_best(0, SizeU { w: 999, h: 999 }).unwrap();
        assert_eq!(fallback.px, SizeU { w: 200, h: 200 }, "no size match -> freshest");
    }

    #[test]
    fn superseded_render_failure_allows_rerequest() {
        let mut a = app_with_doc(2);
        let reqs = a.wanted_renders();
        for r in &reqs {
            a.mark_pending(r);
        }
        assert!(a.wanted_renders().is_empty());
        a.render_failed(0, true); // superseded by the PDF thread: retry is fine
        let again = a.wanted_renders();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].page, 0);
    }

    #[test]
    fn real_render_failure_is_not_rerequested() {
        let mut a = app_with_doc(2);
        for r in a.wanted_renders() {
            a.mark_pending(&r);
        }
        a.render_failed(1, false); // genuine pdfium failure on the prefetch page
        assert!(a.wanted_renders().iter().all(|r| r.page != 1), "failed page must not hot-loop");
        // A fresh document clears the failed set.
        a.doc_loaded(PathBuf::from("y.pdf"), 2, vec![LETTER; 2], vec![]);
        assert!(a.wanted_renders().iter().any(|r| r.page == 1));
    }

    // --- links ---

    #[test]
    fn link_at_transforms_pdf_coords() {
        let mut a = app_with_doc(1);
        let fit_px = render_size(LETTER, a.client, 1.0);
        // Link occupying the top-left quarter of the page (PDF coords: y up).
        let link = PageLink {
            rect_pt: RectF { left: 0.0, top: 792.0, right: 306.0, bottom: 396.0 },
            target: LinkTarget::Page(0),
        };
        a.rendered(CachedPage { page: 0, px: fit_px, bgra: vec![0; 4], links: vec![link] });
        let pr = a.current_page_rect().unwrap();
        // A window point just inside the page's top-left corner is on the link.
        let p = PointF { x: pr.left + 5.0, y: pr.top + 5.0 };
        assert_eq!(a.link_at(p), Some(LinkTarget::Page(0)));
        // Bottom-left corner of the page is NOT on the link.
        let q = PointF { x: pr.left + 5.0, y: pr.bottom - 5.0 };
        assert_eq!(a.link_at(q), None);
    }

    // --- password mode ---

    #[test]
    fn password_mode_buffer_ops() {
        let mut a = App::new();
        a.mode = Mode::Password { path: PathBuf::from("x.pdf"), buf: String::new(), prev: None };
        a.password_char('h');
        a.password_char('i');
        a.password_backspace();
        match &a.mode {
            Mode::Password { buf, .. } => assert_eq!(buf, "h"),
            _ => panic!(),
        }
        let (path, pw) = a.password_take().unwrap();
        assert_eq!(path, PathBuf::from("x.pdf"));
        assert_eq!(pw, "h");
        assert!(matches!(a.mode, Mode::Loading { .. }), "no stashed doc -> Loading screen");
    }

    #[test]
    fn password_cancel_and_take_restore_stashed_doc() {
        // A doc is open when a password-protected file is opened on top of it:
        // the prompt stashes the DocState; cancel restores it (spec: keep the
        // current doc), and Enter/take ALSO restores it (it keeps showing
        // while the retry loads).
        let mut a = app_with_doc(3);
        a.flip(1);
        let prev = match std::mem::replace(&mut a.mode, Mode::Empty) {
            Mode::Doc(d) => Some(Box::new(d)),
            _ => None,
        };
        a.mode = Mode::Password { path: PathBuf::from("locked.pdf"), buf: String::new(), prev };
        a.password_cancel();
        assert_eq!(a.doc().unwrap().current, 1, "cancel restores the doc at its page");
        // Without a stashed doc, cancel falls back to Empty.
        let mut b = App::new();
        b.mode = Mode::Password { path: PathBuf::from("locked.pdf"), buf: String::new(), prev: None };
        b.password_cancel();
        assert!(matches!(b.mode, Mode::Empty));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test app`
Expected: compile FAILURE — `App`, `PageCache`, etc. not found.

- [ ] **Step 3: Implement the module**

Insert between the imports and test module in `src/app.rs`:

```rust
/// One rendered page: BGRA pixels + that page's links (PDF coords).
pub struct CachedPage {
    pub page: u16,
    pub px: SizeU,
    pub bgra: Vec<u8>,
    pub links: Vec<PageLink>,
}

/// How far (px per axis) a cached size may differ from a requested size and
/// still count as the same render — absorbs rounding differences between our
/// fit math and pdfium's output so requests can never loop.
const SIZE_TOLERANCE: i32 = 2;

fn close(a: SizeU, b: SizeU) -> bool {
    (a.w as i32 - b.w as i32).abs() <= SIZE_TOLERANCE
        && (a.h as i32 - b.h as i32).abs() <= SIZE_TOLERANCE
}

/// Tiny insertion-ordered cache keyed by (page, pixel size). Front = newest.
pub struct PageCache {
    entries: VecDeque<CachedPage>,
    cap: usize,
}

impl PageCache {
    pub fn new(cap: usize) -> Self {
        Self { entries: VecDeque::new(), cap }
    }

    /// Freshest cached render of this page at any size (soft preview).
    pub fn get_any(&self, page: u16) -> Option<&CachedPage> {
        self.entries.iter().find(|e| e.page == page)
    }

    /// Entry within SIZE_TOLERANCE of `want` if one exists, else the freshest.
    pub fn get_best(&self, page: u16, want: SizeU) -> Option<&CachedPage> {
        self.entries
            .iter()
            .find(|e| e.page == page && close(e.px, want))
            .or_else(|| self.get_any(page))
    }

    pub fn has_close(&self, page: u16, px: SizeU) -> bool {
        self.entries.iter().any(|e| e.page == page && close(e.px, px))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn put(&mut self, entry: CachedPage) {
        self.entries.retain(|e| !(e.page == entry.page && e.px == entry.px));
        self.entries.push_front(entry);
        while self.entries.len() > self.cap {
            self.entries.pop_back();
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

pub struct DocState {
    pub path: PathBuf,
    pub pages: u16,
    pub page_sizes: Vec<(f32, f32)>,
    pub outline: Vec<OutlineItem>,
    pub current: u16,
    /// 1.0 = fit-to-window.
    pub zoom: f32,
    pub scroll: PointF,
    /// Some = TOC overlay is open.
    pub overlay: Option<OutlineState>,
}

impl DocState {
    fn page_pt(&self) -> (f32, f32) {
        self.page_sizes
            .get(self.current as usize)
            .copied()
            .unwrap_or((612.0, 792.0))
    }
}

pub enum Mode {
    Empty,
    Loading {
        path: PathBuf,
    },
    /// Typing a password into the borderless window. `prev` stashes the doc
    /// that was open when the prompt appeared, so cancel restores it.
    Password {
        path: PathBuf,
        buf: String,
        prev: Option<Box<DocState>>,
    },
    Doc(DocState),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderReq {
    pub page: u16,
    pub px: SizeU,
    pub visible: bool,
}

#[derive(Debug, PartialEq)]
pub enum WheelOutcome {
    None,
    Redraw,
    Flipped,
}

pub struct App {
    pub mode: Mode,
    pub cache: PageCache,
    /// Render requests sent to the PDF thread but not yet answered.
    pub pending: HashSet<(u16, u32, u32)>,
    /// Pages whose render genuinely failed — never re-requested (avoids a
    /// request/fail hot loop on malformed pages). Cleared on document load.
    pub failed: HashSet<u16>,
    pub client: SizeU,
    pub dpi: u32,
}

impl App {
    pub fn new() -> Self {
        Self {
            mode: Mode::Empty,
            cache: PageCache::new(6),
            pending: HashSet::new(),
            failed: HashSet::new(),
            client: SizeU { w: 0, h: 0 },
            dpi: 96,
        }
    }

    pub fn doc(&self) -> Option<&DocState> {
        match &self.mode {
            Mode::Doc(d) => Some(d),
            _ => None,
        }
    }

    pub fn doc_mut(&mut self) -> Option<&mut DocState> {
        match &mut self.mode {
            Mode::Doc(d) => Some(d),
            _ => None,
        }
    }

    pub fn set_client(&mut self, client: SizeU) {
        self.client = client;
    }

    pub fn doc_loaded(
        &mut self,
        path: PathBuf,
        pages: u16,
        page_sizes: Vec<(f32, f32)>,
        outline: Vec<OutlineItem>,
    ) {
        self.cache.clear();
        self.pending.clear();
        self.failed.clear();
        self.mode = Mode::Doc(DocState {
            path,
            pages,
            page_sizes,
            outline,
            current: 0,
            zoom: 1.0,
            scroll: PointF::default(),
            overlay: None,
        });
    }

    /// Change page by delta; returns true if the page changed. Resets zoom/scroll.
    pub fn flip(&mut self, delta: i32) -> bool {
        let Some(d) = self.doc_mut() else { return false };
        let target = (d.current as i32 + delta).clamp(0, d.pages as i32 - 1) as u16;
        if target == d.current {
            return false;
        }
        d.current = target;
        d.zoom = 1.0;
        d.scroll = PointF::default();
        true
    }

    pub fn goto(&mut self, page: u16) -> bool {
        let Some(d) = self.doc() else { return false };
        let delta = page.min(d.pages.saturating_sub(1)) as i32 - d.current as i32;
        self.flip(delta)
    }

    /// notches > 0 = wheel down (toward the next page).
    pub fn wheel(&mut self, notches: i32) -> WheelOutcome {
        let client = self.client;
        let Some(d) = self.doc_mut() else { return WheelOutcome::None };
        if notches == 0 {
            return WheelOutcome::None;
        }
        if d.zoom <= 1.0 + 1e-4 {
            return if self.flip(notches.signum()) {
                WheelOutcome::Flipped
            } else {
                WheelOutcome::None
            };
        }
        let px = render_size(d.page_pt(), client, d.zoom);
        let max_y = (px.h as f32 - client.h as f32).max(0.0);
        let at_bottom = d.scroll.y >= max_y - 0.5;
        let at_top = d.scroll.y <= 0.5;
        if (notches > 0 && at_bottom) || (notches < 0 && at_top) {
            return if self.flip(notches.signum()) {
                WheelOutcome::Flipped
            } else {
                WheelOutcome::None
            };
        }
        d.scroll.y += notches as f32 * SCROLL_STEP;
        d.scroll = clamp_scroll(px, client, d.scroll);
        WheelOutcome::Redraw
    }

    /// Multiply zoom by `factor`, keeping the page point under `cursor` fixed.
    /// Returns true if anything changed.
    pub fn zoom_by(&mut self, cursor: PointF, factor: f32) -> bool {
        let client = self.client;
        let Some(d) = self.doc_mut() else { return false };
        let page_pt = d.page_pt();
        let old_zoom = d.zoom;
        let new_zoom = (old_zoom * factor).clamp(1.0, max_zoom(page_pt, client));
        if (new_zoom - old_zoom).abs() < 1e-4 {
            return false;
        }
        if new_zoom <= 1.0 + 1e-4 {
            d.zoom = 1.0;
            d.scroll = PointF::default();
            return true;
        }
        let old_px = render_size(page_pt, client, old_zoom);
        let old_rect = page_rect(old_px, client, d.scroll);
        let new_px = render_size(page_pt, client, new_zoom);
        d.scroll = zoom_scroll(cursor, old_rect, new_zoom / old_zoom, new_px, client);
        d.zoom = new_zoom;
        true
    }

    pub fn reset_zoom(&mut self) -> bool {
        let Some(d) = self.doc_mut() else { return false };
        if d.zoom == 1.0 {
            return false;
        }
        d.zoom = 1.0;
        d.scroll = PointF::default();
        true
    }

    pub fn toggle_overlay(&mut self) {
        let Some(d) = self.doc_mut() else { return };
        d.overlay = match d.overlay.take() {
            Some(_) => None,
            None => Some(OutlineState::new(d.outline.clone())),
        };
    }

    /// Pixel size the current page renders at (current zoom).
    pub fn current_px(&self) -> Option<SizeU> {
        let d = self.doc()?;
        Some(render_size(d.page_pt(), self.client, d.zoom))
    }

    /// Window-space rect the current page occupies.
    pub fn current_page_rect(&self) -> Option<RectF> {
        let d = self.doc()?;
        Some(page_rect(self.current_px()?, self.client, d.scroll))
    }

    /// Crisp render of the current page first, then prefetch next/prev at fit.
    pub fn wanted_renders(&self) -> Vec<RenderReq> {
        let Some(d) = self.doc() else { return Vec::new() };
        let mut out = Vec::new();
        let mut push_if_needed = |page: u16, px: SizeU, visible: bool| {
            if !self.cache.has_close(page, px)
                && !self.pending.contains(&(page, px.w, px.h))
                && !self.failed.contains(&page)
            {
                out.push(RenderReq { page, px, visible });
            }
        };
        push_if_needed(d.current, render_size(d.page_pt(), self.client, d.zoom), true);
        for neighbor in [d.current as i32 + 1, d.current as i32 - 1] {
            if neighbor < 0 || neighbor >= d.pages as i32 {
                continue;
            }
            let page = neighbor as u16;
            let pt = d.page_sizes.get(page as usize).copied().unwrap_or((612.0, 792.0));
            push_if_needed(page, render_size(pt, self.client, 1.0), false);
        }
        drop(push_if_needed);
        out
    }

    pub fn mark_pending(&mut self, req: &RenderReq) {
        self.pending.insert((req.page, req.px.w, req.px.h));
    }

    /// A render reply arrived: clear the page's pending entries (by page, not
    /// exact size — pdfium may round differently than layout::render_size).
    pub fn rendered(&mut self, page: CachedPage) {
        self.pending.retain(|k| k.0 != page.page);
        self.cache.put(page);
    }

    /// RenderFailed reply. Superseded requests may be re-issued; a genuine
    /// pdfium failure marks the page so it is never hot-loop re-requested
    /// (the spec accepts corrupt pages silently).
    pub fn render_failed(&mut self, page: u16, superseded: bool) {
        self.pending.retain(|k| k.0 != page);
        if !superseded {
            self.failed.insert(page);
        }
    }

    /// Link under a window-space point on the current page, if any.
    pub fn link_at(&self, p: PointF) -> Option<LinkTarget> {
        let d = self.doc()?;
        let entry = self.cache.get_any(d.current)?;
        let pr = self.current_page_rect()?;
        let page_pt = d.page_pt();
        let px_per_pt = fit_scale(page_pt, self.client) * d.zoom;
        for link in &entry.links {
            let wr = pdf_rect_to_window(link.rect_pt, page_pt.1, px_per_pt, pr);
            if wr.contains(p) {
                return Some(link.target.clone());
            }
        }
        None
    }

    // --- password entry (typed straight into the borderless window) ---

    pub fn password_char(&mut self, c: char) {
        if let Mode::Password { buf, .. } = &mut self.mode {
            if !c.is_control() {
                buf.push(c);
            }
        }
    }

    pub fn password_backspace(&mut self) {
        if let Mode::Password { buf, .. } = &mut self.mode {
            buf.pop();
        }
    }

    /// Take the typed password (caller sends PdfCmd::Load with it). The
    /// stashed doc — if any — keeps showing while the retry loads; otherwise
    /// show the Loading screen.
    pub fn password_take(&mut self) -> Option<(PathBuf, String)> {
        if let Mode::Password { path, buf, prev } = &mut self.mode {
            let (path, pw) = (path.clone(), buf.clone());
            self.mode = match prev.take() {
                Some(d) => Mode::Doc(*d),
                None => Mode::Loading { path: path.clone() },
            };
            return Some((path, pw));
        }
        None
    }

    /// Cancel the password prompt: restore the stashed doc, or Empty.
    pub fn password_cancel(&mut self) {
        if let Mode::Password { prev, .. } = &mut self.mode {
            self.mode = match prev.take() {
                Some(d) => Mode::Doc(*d),
                None => Mode::Empty,
            };
        }
    }
}
```

Borrow-checker note: the `push_if_needed` closure borrows `self` immutably and `out` mutably; if your compiler version rejects it, inline the two checks at each call site — behavior must stay identical to the tests.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test app::`
(Note the `::` — a bare `app` filter would also match layout tests whose names contain "app", like `page_rect_applies_scroll_when_larger`.)
Expected: `test result: ok. 22 passed`

Run the whole suite: `cargo test`
Expected: all green (layout 17, overlay 11, app 22).

- [ ] **Step 5: Commit**

```pwsh
git add src/app.rs src/lib.rs
git commit -m "feat: app state machine - paging, wheel semantics, zoom, page cache"
```

---

### Task 6: `window.rs` — the borderless window shell (manual verification)

**Files:**
- Create: `src/window.rs`
- Modify: `src/lib.rs` (add `pub mod window;`)
- Modify: `src/main.rs` (replace entirely)

**Interfaces:**
- Consumes: `app::App`, `layout::{hit_zone, PointF, SizeU, Zone}`, `windows` crate
- Produces:
  - `window::run(app: App) -> windows::core::Result<()>` — registers the class, creates the borderless window, runs the message loop (Task 7 extends the signature with the initial PDF path)
  - `struct Ui { hwnd: HWND, app: App }` with `Ui::handle(msg, wparam, lparam) -> LRESULT` — later tasks add fields and match arms; the struct is heap-allocated and pointed to by `GWLP_USERDATA`
  - Constants: `WM_APP_PDF_REPLY = WM_APP + 1`, menu ids `CMD_OPEN=1, CMD_TOC=2, CMD_FIT=3, CMD_MINIMIZE=4, CMD_CLOSE=5`

**The borderless recipe (verified against melak47/BorderlessWindow, Chromium, Windows Terminal — keep these exact):**
- Style `WS_POPUP | WS_THICKFRAME | WS_CAPTION | WS_SYSMENU | WS_MAXIMIZEBOX | WS_MINIMIZEBOX`. Each flag is load-bearing: THICKFRAME = resize + Aero Snap; CAPTION = minimize animation; MAXIMIZEBOX = Win+Up / drag-to-top; SYSMENU = taskbar menu. The caption never shows because of the next point.
- `WM_NCCALCSIZE` with `wparam == TRUE`: return `0` leaving `rgrc[0]` untouched → client area == window rect → zero-pixel chrome. EXCEPT when maximized: a maximized window intentionally overhangs the monitor by its (now-removed) frame, so clamp `rgrc[0]` to the monitor work area (`MONITOR_DEFAULTTONEAREST` — NULL fails during restore-from-minimized), then inset 2 px on any edge with an auto-hide taskbar so the taskbar can still fly out.
- `WM_NCHITTEST` receives SCREEN coordinates: resize grips on the outer 8 px (DPI-scaled, disabled while maximized), interactive content → `HTCLIENT`, everything else → `HTCAPTION` (native drag-to-move + snap).
- `WM_NCLBUTTONDBLCLK` on `HTCAPTION` must be swallowed (return 0) or DefWindowProc toggles maximize.
- `DWMWA_WINDOW_CORNER_PREFERENCE = DWMWCP_DONOTROUND` (spec: the page is a perfect rectangle) + `DwmExtendFrameIntoClientArea` with 1 px margins (invisible; restores the DWM drop shadow).
- `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` first thing in `main()` (the manifest from Task 1 already declares it; the call is harness for `cargo run` scenarios and fails harmlessly with access-denied when the manifest won).

- [ ] **Step 1: Write `src/window.rs`**

```rust
//! The borderless Win32 window: class, WndProc, chromeless tricks, input.

use crate::app::App;
use crate::layout::{hit_zone, PointF, SizeU, Zone};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND, DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, GetMonitorInfoW,
    MonitorFromWindow, HBRUSH, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_ESCAPE};
use windows::Win32::UI::Shell::{
    DragAcceptFiles, SHAppBarMessage, ABM_GETAUTOHIDEBAREX, ABM_GETSTATE, ABS_AUTOHIDE,
    APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_APP_PDF_REPLY: u32 = WM_APP + 1;

pub const CMD_OPEN: usize = 1;
pub const CMD_TOC: usize = 2;
pub const CMD_FIT: usize = 3;
pub const CMD_MINIMIZE: usize = 4;
pub const CMD_CLOSE: usize = 5;

const GRIP_DIP: f32 = 8.0;
const MIN_SIZE_DIP: i32 = 200;

pub struct Ui {
    pub hwnd: HWND,
    pub app: App,
}

pub fn run(app: App) -> Result<()> {
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class_name = w!("pppdf_window");

        let wc = WNDCLASSEXW {
            cbSize: core::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH::default(), // we paint everything ourselves
            lpszClassName: class_name,
            ..Default::default()
        };
        // NEVER put this call inside debug_assert! — the assertion (and the
        // call!) would vanish in release builds and window creation would fail.
        let atom = RegisterClassExW(&wc);
        debug_assert!(atom != 0);

        let mut ui = Box::new(Ui { hwnd: HWND::default(), app });

        // Every flag here is load-bearing for native snap/animations; the
        // caption never shows because WM_NCCALCSIZE claims the whole rect.
        let style =
            WS_POPUP | WS_THICKFRAME | WS_CAPTION | WS_SYSMENU | WS_MAXIMIZEBOX | WS_MINIMIZEBOX;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("pppdf"),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            850,
            1100,
            None,
            None,
            Some(instance),
            Some(&mut *ui as *mut Ui as *const core::ffi::c_void),
        )?;

        // Spec: square corners — the page is a perfect rectangle.
        let pref = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const core::ffi::c_void,
            core::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );
        // Invisible 1px frame extension: restores the DWM drop shadow.
        let margins =
            MARGINS { cxLeftWidth: 1, cxRightWidth: 1, cyTopHeight: 1, cyBottomHeight: 1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);

        DragAcceptFiles(hwnd, true);
        let _ = ShowWindow(hwnd, SW_SHOW);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let cs = lparam.0 as *const CREATESTRUCTW;
            let ui = (*cs).lpCreateParams as *mut Ui;
            (*ui).hwnd = hwnd;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ui as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let ui = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ui;
        if ui.is_null() {
            // e.g. WM_GETMINMAXINFO arrives before WM_NCCREATE
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        (*ui).handle(msg, wparam, lparam)
    }
}

impl Ui {
    fn dpi_scale(&self) -> f32 {
        unsafe { GetDpiForWindow(self.hwnd) as f32 / 96.0 }
    }

    fn client_size(&self) -> SizeU {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        SizeU { w: (rc.right - rc.left).max(0) as u32, h: (rc.bottom - rc.top).max(0) as u32 }
    }

    fn maximized(&self) -> bool {
        let mut wp = WINDOWPLACEMENT {
            length: core::mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        unsafe { GetWindowPlacement(self.hwnd, &mut wp).is_ok() }
            && wp.showCmd == SW_SHOWMAXIMIZED.0 as u32
    }

    pub fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        unsafe {
            match msg {
                // === chromeless core ===
                WM_NCCALCSIZE if wparam.0 != 0 => {
                    // Claim the whole window rect as client area (zero chrome).
                    // When maximized, clamp to the work area so client pixels
                    // don't hang off-monitor, and leave 2px for an auto-hide
                    // taskbar to fly out.
                    if self.maximized() {
                        let params = &mut *(lparam.0 as *mut NCCALCSIZE_PARAMS);
                        let monitor = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
                        let mut mi = MONITORINFO {
                            cbSize: core::mem::size_of::<MONITORINFO>() as u32,
                            ..Default::default()
                        };
                        if GetMonitorInfoW(monitor, &mut mi).as_bool() {
                            let mut rect = mi.rcWork;
                            apply_autohide_taskbar_insets(&mut rect, &mi);
                            params.rgrc[0] = rect;
                        }
                    }
                    LRESULT(0)
                }
                WM_NCHITTEST => {
                    let sx = (lparam.0 & 0xFFFF) as i16 as i32; // screen coords
                    let sy = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                    LRESULT(self.hit_test(sx, sy) as isize)
                }
                WM_NCLBUTTONDBLCLK => {
                    // DefWindowProc would toggle maximize on caption dbl-click.
                    // Swallow it; Task 9 wires this to reset-zoom.
                    if wparam.0 as u32 == HTCAPTION {
                        return LRESULT(0);
                    }
                    DefWindowProcW(self.hwnd, msg, wparam, lparam)
                }
                WM_GETMINMAXINFO => {
                    let mmi = lparam.0 as *mut MINMAXINFO;
                    let dpi = GetDpiForWindow(self.hwnd) as i32;
                    (*mmi).ptMinTrackSize.x = MIN_SIZE_DIP * dpi / 96;
                    (*mmi).ptMinTrackSize.y = MIN_SIZE_DIP * dpi / 96;
                    LRESULT(0)
                }

                // === right-click menu ===
                WM_NCRBUTTONUP => {
                    let sx = (lparam.0 & 0xFFFF) as i16 as i32;
                    let sy = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                    self.show_menu(sx, sy);
                    LRESULT(0)
                }
                WM_RBUTTONUP => {
                    let mut pt = windows::Win32::Foundation::POINT {
                        x: (lparam.0 & 0xFFFF) as i16 as i32,
                        y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
                    };
                    let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut pt);
                    self.show_menu(pt.x, pt.y);
                    LRESULT(0)
                }
                WM_CONTEXTMENU => LRESULT(0), // we run our own menu above

                // === lifecycle / painting ===
                WM_SIZE => {
                    self.app.set_client(self.client_size());
                    LRESULT(0)
                }
                WM_DPICHANGED => {
                    self.app.dpi = (wparam.0 & 0xFFFF) as u32;
                    let suggested = &*(lparam.0 as *const RECT);
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        suggested.left,
                        suggested.top,
                        suggested.right - suggested.left,
                        suggested.bottom - suggested.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                    LRESULT(0)
                }
                WM_ERASEBKGND => LRESULT(1),
                WM_PAINT => {
                    // Temporary GDI fill; Task 7 replaces this with Direct2D.
                    let mut ps = PAINTSTRUCT::default();
                    let hdc = BeginPaint(self.hwnd, &mut ps);
                    let brush = CreateSolidBrush(COLORREF(0x001A1A1A));
                    FillRect(hdc, &ps.rcPaint, brush);
                    let _ = DeleteObject(brush.into());
                    let _ = EndPaint(self.hwnd, &ps);
                    LRESULT(0)
                }
                WM_KEYDOWN => {
                    self.on_key(wparam.0 as u16);
                    LRESULT(0)
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    LRESULT(0)
                }
                _ => DefWindowProcW(self.hwnd, msg, wparam, lparam),
            }
        }
    }

    /// Screen point -> HT* code. Grips on the outer band, HTCLIENT over
    /// interactive content (extended by Tasks 8/9), HTCAPTION elsewhere.
    fn hit_test(&self, sx: i32, sy: i32) -> u32 {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rc);
        }
        let size =
            SizeU { w: (rc.right - rc.left).max(0) as u32, h: (rc.bottom - rc.top).max(0) as u32 };
        let p = PointF { x: (sx - rc.left) as f32, y: (sy - rc.top) as f32 };

        if !self.maximized() {
            let grip = GRIP_DIP * self.dpi_scale();
            match hit_zone(p, size, grip) {
                Zone::TopLeft => return HTTOPLEFT,
                Zone::Top => return HTTOP,
                Zone::TopRight => return HTTOPRIGHT,
                Zone::Left => return HTLEFT,
                Zone::Right => return HTRIGHT,
                Zone::BottomLeft => return HTBOTTOMLEFT,
                Zone::Bottom => return HTBOTTOM,
                Zone::BottomRight => return HTBOTTOMRIGHT,
                Zone::Interior => {}
            }
        }
        if self.wants_client_mouse(p) {
            return HTCLIENT;
        }
        HTCAPTION
    }

    /// Should this client-space point get normal mouse handling instead of
    /// dragging the window? Extended in Task 8 (links, overlay) and Task 9
    /// (zoomed state). For now: no.
    fn wants_client_mouse(&self, _p: PointF) -> bool {
        false
    }

    fn on_key(&mut self, vk: u16) {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        match vk {
            _ if vk == VK_ESCAPE.0 => self.quit(),
            0x57 if ctrl => self.quit(), // Ctrl+W
            _ => {}
        }
    }

    fn quit(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    fn show_menu(&mut self, sx: i32, sy: i32) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, w!("Open…\tCtrl+O"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_TOC, w!("Table of Contents\tTab"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_FIT, w!("Fit to Window\t0"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, CMD_MINIMIZE, w!("Minimize"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_CLOSE, w!("Close\tEsc"));
            let _ = SetForegroundWindow(self.hwnd);
            let cmd =
                TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, sx, sy, None, self.hwnd, None);
            let _ = DestroyMenu(menu);
            self.on_command(cmd.0 as usize);
        }
    }

    fn on_command(&mut self, cmd: usize) {
        match cmd {
            CMD_MINIMIZE => unsafe {
                let _ = ShowWindow(self.hwnd, SW_MINIMIZE);
            },
            CMD_CLOSE => self.quit(),
            // CMD_OPEN (Task 10), CMD_TOC (Task 8), CMD_FIT (Task 9)
            _ => {}
        }
    }
}

/// Inset the maximized client rect by 2px on any monitor edge hosting an
/// auto-hide taskbar, so the bar can still fly out (Windows Terminal GH#1438).
fn apply_autohide_taskbar_insets(rect: &mut RECT, mi: &MONITORINFO) {
    unsafe {
        let mut state_data = APPBARDATA {
            cbSize: core::mem::size_of::<APPBARDATA>() as u32,
            ..Default::default()
        };
        let state = SHAppBarMessage(ABM_GETSTATE, &mut state_data) as u32;
        if state & ABS_AUTOHIDE == 0 {
            return;
        }
        let has_bar = |edge: u32| -> bool {
            let mut data = APPBARDATA {
                cbSize: core::mem::size_of::<APPBARDATA>() as u32,
                uEdge: edge,
                rc: mi.rcMonitor,
                ..Default::default()
            };
            SHAppBarMessage(ABM_GETAUTOHIDEBAREX, &mut data) != 0
        };
        // ABE_LEFT=0, ABE_TOP=1, ABE_RIGHT=2, ABE_BOTTOM=3
        if has_bar(1) {
            rect.top += 2;
        }
        if has_bar(3) {
            rect.bottom -= 2;
        }
        if has_bar(0) {
            rect.left += 2;
        }
        if has_bar(2) {
            rect.right -= 2;
        }
    }
}
```

Add `pub mod window;` to `src/lib.rs`.

API-drift notes for the implementer (windows 0.62.2 verified idioms — if a call fails to compile, check these first, do not "fix" by downgrading): `BOOL` lives in `windows::core`; `GetMessageW(...).into()` for the loop condition; nullable params are `Option<T>` (`SetWindowPos(hwnd, None, ...)`, `TrackPopupMenu(..., None, hwnd, None)`); `CreateWindowExW` returns `Result<HWND>`; `GetModuleHandleW(None)?.into()` converts `HMODULE→HINSTANCE`; `DeleteObject` takes `HGDIOBJ` (`brush.into()`); `SHAppBarMessage(msg: u32, pdata: *mut APPBARDATA) -> usize` — if the `ABM_*`/`ABS_*` constants are typed wrappers in this version, add `.0` as needed; `GetWindowPlacement` may return `Result` or `BOOL` — adapt the `maximized()` helper accordingly; `ClientToScreen` is in `Win32::Graphics::Gdi`.

- [ ] **Step 2: Replace `src/main.rs`**

```rust
//! pppdf — borderless native PDF viewer.
#![windows_subsystem = "windows"]

use pppdf::app::App;
use pppdf::window;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

fn main() {
    // Belt and suspenders with the embedded manifest; must precede any HWND.
    // Access-denied here just means the manifest already set it.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    if let Err(e) = window::run(App::new()) {
        // No console in windows subsystem — surface startup failures visibly.
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
            let text: Vec<u16> =
                format!("pppdf failed to start:\n{e}\0").encode_utf16().collect();
            MessageBoxW(
                None,
                windows::core::PCWSTR(text.as_ptr()),
                windows::core::w!("pppdf"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}
```

- [ ] **Step 3: Build and verify the borderless behavior manually**

Run: `cargo test`
Expected: all existing tests still pass (window.rs adds no test-visible logic).

Run: `cargo run`
Verify each of these by hand (this is the flagship feature — do not skip):
1. A dark window with NO title bar, NO border, square corners, subtle drop shadow.
2. Drag anywhere on the window body → window moves; drag to the top edge of the screen → Aero Snap maximize preview works; snap left/right works.
3. Win+←/→/↑/↓ snap, maximize, minimize, restore all work with animations.
4. When maximized: window fills the work area exactly (taskbar visible, nothing hangs off-screen); dragging it down un-maximizes.
5. All 8 edge/corner grips resize (cursor changes within ~8 px of each edge); interior drag never resizes. While maximized, edges do NOT resize.
6. Double-click on the body does NOT maximize (swallowed).
7. Right-click anywhere → menu appears; Minimize minimizes; Close exits. Esc and Ctrl+W also exit.
8. Minimize to taskbar and restore → animation plays, window returns.

- [ ] **Step 4: Commit**

```pwsh
git add src/window.rs src/lib.rs src/main.rs
git commit -m "feat: chromeless borderless window - native snap, grips, menu, DPI"
```

---

### Task 7: `d2d.rs` + wiring — a usable viewer (open, see pages, flip, resize)

**Files:**
- Create: `src/d2d.rs`
- Modify: `src/lib.rs` (add `pub mod d2d;`)
- Modify: `src/window.rs` (Ui fields, WM_CREATE, WM_APP_PDF_REPLY, paging keys, wheel, resize debounce, D2D paint)
- Modify: `src/main.rs` (pass the CLI argument through)

**Interfaces:**
- Consumes: everything from Tasks 2–6; `windows` crate D2D/DWrite APIs
- Produces:
  - `d2d::Renderer::new() -> windows::core::Result<Renderer>` (factories only; the render target is created lazily per-HWND)
  - `Renderer::draw(&mut self, hwnd: HWND, app: &App, badge: Option<(&str, f32)>) -> windows::core::Result<()>` — draws the whole scene; handles `D2DERR_RECREATE_TARGET` internally by dropping device resources (caller just calls again next frame)
  - `Renderer::resize(&mut self, w: u32, h: u32)` — forwards to the HWND render target if one exists
  - `Renderer::set_dpi(&mut self, dpi: u32)` — drops cached text formats so they recreate at the new scale; MUST also be called once at WM_CREATE with the window's initial DPI
  - `Renderer::clear_page_cache(&mut self)` — drops the memoized page bitmap; called whenever a `Loaded` reply replaces the document
  - `window::run(app: App, initial_pdf: Option<PathBuf>) -> Result<()>` — extended signature
  - `Ui::sync_renders(&mut self)` — sends `app.wanted_renders()` to the PDF thread, marking pending; called after every state change
- Timer ids on `Ui`: `TIMER_RESIZE: usize = 1` (150 ms debounce), `TIMER_BADGE: usize = 2` (Task 8)

**Drawing rules (from the spec):**
- Clear to `#1a1a1a`; that is also the letterbox color.
- Current page: `cache.get_best(current, current_px())`. If the bitmap size equals the destination rect size (crisp case) draw with NEAREST_NEIGHBOR 1:1; otherwise draw scaled with LINEAR (soft preview during live resize/zoom, replaced when the crisp render lands).
- `Mode::Empty` → centered dim hint `Right-click or drop a PDF`. `Mode::Loading` → dim `Loading…`. `Mode::Password` → centered `Password required` + one `•` per typed char + `Enter to open — Esc to cancel` (drawn, not a dialog: the window stays chromeless).
- D2D bitmaps are device resources: the renderer memoizes ONE `(page, px) -> ID2D1Bitmap` pair and recreates it when the key changes or the target is recreated. Text formats are cached per DPI. The render target runs at 96 DPI so all D2D coordinates are physical pixels (font sizes are multiplied by dpi/96 manually).

- [ ] **Step 1: Write `src/d2d.rs`**

```rust
//! Direct2D/DirectWrite: device management and ALL drawing.

use crate::app::{App, Mode};
use crate::layout::{page_rect, PointF, RectF, SizeU};
use windows::core::*;
use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

const BG: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.102, g: 0.102, b: 0.102, a: 1.0 }; // #1a1a1a

fn color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

fn rectf(r: RectF) -> D2D_RECT_F {
    D2D_RECT_F { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

pub struct Renderer {
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    rt: Option<ID2D1HwndRenderTarget>,
    /// Memo of the last uploaded page bitmap: (page, px) -> device bitmap.
    page_bitmap: Option<(u16, SizeU, ID2D1Bitmap)>,
    dpi: u32,
    /// ((size_dip*10) as u32, centered) -> format; rebuilt when DPI changes.
    formats: Vec<((u32, bool), IDWriteTextFormat)>,
}

impl Renderer {
    pub fn new() -> Result<Renderer> {
        unsafe {
            let factory: ID2D1Factory =
                D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Renderer {
                factory,
                dwrite,
                rt: None,
                page_bitmap: None,
                dpi: 96,
                formats: Vec::new(),
            })
        }
    }

    pub fn set_dpi(&mut self, dpi: u32) {
        self.dpi = dpi;
        self.formats.clear();
    }

    /// Forget the memoized device bitmap. MUST be called when a new document
    /// loads — a same-sized page 0 of the new document would otherwise show
    /// the old document's pixels.
    pub fn clear_page_cache(&mut self) {
        self.page_bitmap = None;
    }

    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if let Some(rt) = &self.rt {
            unsafe {
                let _ = rt.Resize(&D2D_SIZE_U { width: w.max(1), height: h.max(1) });
            }
        }
    }

    fn ensure_rt(&mut self, hwnd: HWND) -> Result<ID2D1HwndRenderTarget> {
        if let Some(rt) = &self.rt {
            return Ok(rt.clone());
        }
        unsafe {
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                // 96 dpi = 1 DIP == 1 physical px; we do our own DPI scaling.
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd,
                pixelSize: D2D_SIZE_U {
                    width: (rc.right - rc.left).max(1) as u32,
                    height: (rc.bottom - rc.top).max(1) as u32,
                },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let rt = self.factory.CreateHwndRenderTarget(&props, &hwnd_props)?;
            self.rt = Some(rt.clone());
            Ok(rt)
        }
    }

    fn text_format(&mut self, size_dip: f32, centered: bool) -> Result<IDWriteTextFormat> {
        let key = ((size_dip * 10.0) as u32, centered);
        if let Some((_, f)) = self.formats.iter().find(|(k, _)| *k == key) {
            return Ok(f.clone());
        }
        unsafe {
            let make = |family: PCWSTR| {
                self.dwrite.CreateTextFormat(
                    family,
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size_dip * self.scale(),
                    w!("en-us"),
                )
            };
            // Spec: Segoe UI Variable, with plain Segoe UI as the fallback.
            let f = make(w!("Segoe UI Variable")).or_else(|_| make(w!("Segoe UI")))?;
            if centered {
                f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            }
            self.formats.push((key, f.clone()));
            Ok(f)
        }
    }

    fn draw_text(
        &self,
        rt: &ID2D1HwndRenderTarget,
        text: &str,
        fmt: &IDWriteTextFormat,
        rect: RectF,
        col: D2D1_COLOR_F,
    ) -> Result<()> {
        unsafe {
            let brush = rt.CreateSolidColorBrush(&col, None)?;
            let utf16: Vec<u16> = text.encode_utf16().collect();
            rt.DrawText(
                &utf16,
                fmt,
                &rectf(rect),
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
        Ok(())
    }

    fn page_bitmap(
        &mut self,
        rt: &ID2D1HwndRenderTarget,
        page: u16,
        px: SizeU,
        bgra: &[u8],
    ) -> Result<ID2D1Bitmap> {
        if let Some((p, s, bmp)) = &self.page_bitmap {
            if *p == page && *s == px {
                return Ok(bmp.clone());
            }
        }
        unsafe {
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            let bmp = rt.CreateBitmap(
                D2D_SIZE_U { width: px.w, height: px.h },
                Some(bgra.as_ptr() as *const core::ffi::c_void),
                px.w * 4,
                &props,
            )?;
            self.page_bitmap = Some((page, px, bmp.clone()));
            Ok(bmp)
        }
    }

    /// Draw the whole scene. On device loss, drops device resources and
    /// returns Ok — the next WM_PAINT recreates everything.
    pub fn draw(&mut self, hwnd: HWND, app: &App, badge: Option<(&str, f32)>) -> Result<()> {
        let rt = self.ensure_rt(hwnd)?;
        let result = self.draw_inner(&rt, app, badge);
        match result {
            Err(e) if e.code() == D2DERR_RECREATE_TARGET => {
                self.rt = None;
                self.page_bitmap = None;
                Ok(())
            }
            other => other,
        }
    }

    fn draw_inner(
        &mut self,
        rt: &ID2D1HwndRenderTarget,
        app: &App,
        badge: Option<(&str, f32)>,
    ) -> Result<()> {
        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&BG));
            let client = app.client;
            let full = RectF { left: 0.0, top: 0.0, right: client.w as f32, bottom: client.h as f32 };

            match &app.mode {
                Mode::Empty => {
                    let fmt = self.text_format(16.0, true)?;
                    self.draw_text(rt, "Right-click or drop a PDF", &fmt, full, color(1.0, 1.0, 1.0, 0.35))?;
                }
                Mode::Loading { .. } => {
                    let fmt = self.text_format(16.0, true)?;
                    self.draw_text(rt, "Loading…", &fmt, full, color(1.0, 1.0, 1.0, 0.35))?;
                }
                Mode::Password { buf, .. } => {
                    let fmt = self.text_format(16.0, true)?;
                    let dots = "•".repeat(buf.chars().count().max(0));
                    let msg =
                        format!("Password required\n\n{dots}_\n\nEnter to open — Esc to cancel");
                    self.draw_text(rt, &msg, &fmt, full, color(1.0, 1.0, 1.0, 0.8))?;
                }
                Mode::Doc(d) => {
                    if let (Some(px), Some(dest)) = (app.current_px(), app.current_page_rect()) {
                        if let Some(entry) = app.cache.get_best(d.current, px) {
                            let bmp = self.page_bitmap(rt, entry.page, entry.px, &entry.bgra)?;
                            let crisp = entry.px.w as f32 == dest.width().round()
                                && entry.px.h as f32 == dest.height().round();
                            let mode = if crisp {
                                D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR
                            } else {
                                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR
                            };
                            rt.DrawBitmap(&bmp, Some(&rectf(dest)), 1.0, mode, None);
                        }
                    }
                    // Overlay + badge drawing lands in Task 8.
                    let _ = badge;
                }
            }
            rt.EndDraw(None, None)
        }
    }
}
```

Note for Task 8: `draw_inner` gets the overlay/badge code then; the `let _ = badge;` line is removed at that point. If `RectF` lacks a needed helper, add it in `layout.rs` (with a test) rather than inlining math here.

- [ ] **Step 2: Wire the PDF thread and paging into `window.rs`**

Apply ALL of the following edits.

2a. Extend imports and the `Ui` struct:

```rust
use crate::d2d::Renderer;
use crate::types::{PdfCmd, PdfReply};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
```

```rust
pub const TIMER_RESIZE: usize = 1;
pub const TIMER_BADGE: usize = 2;

pub struct Ui {
    pub hwnd: HWND,
    pub app: App,
    pub renderer: Option<Renderer>,
    pub pdf_tx: Option<Sender<PdfCmd>>,
    pub reply_rx: Option<Receiver<PdfReply>>,
    pub initial_pdf: Option<PathBuf>,
}
```

2b. `run` signature becomes `pub fn run(app: App, initial_pdf: Option<PathBuf>) -> Result<()>` and the Ui construction becomes:

```rust
let mut ui = Box::new(Ui {
    hwnd: HWND::default(),
    app,
    renderer: None,
    pdf_tx: None,
    reply_rx: None,
    initial_pdf,
});
```

2c. Add a `WM_CREATE` arm to `handle` (before `WM_SIZE`) — spawns the PDF thread with a `PostMessage` wake (HWND is !Send, so smuggle the pointer as `isize`), creates the renderer, and loads the CLI document:

```rust
WM_CREATE => {
    let hwnd_raw = self.hwnd.0 as isize;
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let wake = move || {
        let _ = PostMessageW(
            Some(HWND(hwnd_raw as *mut core::ffi::c_void)),
            WM_APP_PDF_REPLY,
            WPARAM(0),
            LPARAM(0),
        );
    };
    self.pdf_tx = Some(crate::pdf::spawn(reply_tx, wake));
    self.reply_rx = Some(reply_rx);
    self.renderer = Renderer::new().ok();
    self.app.set_client(self.client_size());
    self.app.dpi = GetDpiForWindow(self.hwnd);
    // CRITICAL: forward the window's actual DPI to the renderer NOW.
    // WM_DPICHANGED never fires on a single monitor, and without this the
    // overlay/text draw at 96-DPI scale while hit-testing uses the real DPI
    // (misaligned clicks on every >100% display).
    if let Some(r) = &mut self.renderer {
        r.set_dpi(self.app.dpi);
    }
    if let Some(path) = self.initial_pdf.take() {
        self.open_document(path);
    }
    LRESULT(0)
}
```

2d. Add the reply pump, document opening, and render syncing as `Ui` methods:

```rust
impl Ui {
    pub fn open_document(&mut self, path: PathBuf) {
        self.app.mode = crate::app::Mode::Loading { path: path.clone() };
        self.send_pdf(PdfCmd::Load { path, password: None });
        self.invalidate();
    }

    fn send_pdf(&self, cmd: PdfCmd) {
        if let Some(tx) = &self.pdf_tx {
            let _ = tx.send(cmd);
        }
    }

    /// Ask the PDF thread for whatever the current state needs rendered.
    pub fn sync_renders(&mut self) {
        for req in self.app.wanted_renders() {
            self.app.mark_pending(&req);
            self.send_pdf(PdfCmd::Render { page: req.page, px: req.px, visible: req.visible });
        }
    }

    fn invalidate(&self) {
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn pump_pdf_replies(&mut self) {
        let Some(rx) = self.reply_rx.take() else { return };
        let mut page_changed = false;
        while let Ok(reply) = rx.try_recv() {
            match reply {
                PdfReply::Loaded { path, pages, page_sizes, outline } => {
                    self.app.doc_loaded(path, pages, page_sizes, outline);
                    // The renderer memoizes a device bitmap keyed (page, px);
                    // a new same-sized document would otherwise show the OLD
                    // document's page. Drop the memo with the document.
                    if let Some(r) = &mut self.renderer {
                        r.clear_page_cache();
                    }
                    page_changed = true;
                }
                PdfReply::Rendered { page, px, bgra, links } => {
                    self.app.rendered(crate::app::CachedPage { page, px, bgra, links });
                }
                PdfReply::RenderFailed { page, reason } => {
                    self.app.render_failed(page, reason == "superseded");
                }
                PdfReply::NeedsPassword { path } => {
                    // Task 10 shows the in-window password prompt.
                    let _ = path;
                }
                PdfReply::LoadFailed { path, reason } => {
                    // Task 10 shows the error box; until then keep the state sane.
                    let _ = (path, reason);
                    self.app.mode = crate::app::Mode::Empty;
                }
            }
        }
        self.reply_rx = Some(rx);
        let _ = page_changed;
        self.sync_renders();
        self.invalidate();
    }

    fn flip(&mut self, delta: i32) {
        if self.app.flip(delta) {
            self.sync_renders();
            self.invalidate();
            // Task 8 shows the page badge here.
        }
    }
}
```

2e. New match arms in `handle` (keep the existing ones):

```rust
WM_APP_PDF_REPLY => {
    self.pump_pdf_replies();
    LRESULT(0)
}
WM_MOUSEWHEEL => {
    let notches = -(((wparam.0 >> 16) & 0xFFFF) as i16 as i32) / 120; // wheel down = +1
    if notches != 0 && self.app.wheel(notches) != crate::app::WheelOutcome::None {
        self.sync_renders();
        self.invalidate();
    }
    LRESULT(0)
}
WM_TIMER if wparam.0 == TIMER_RESIZE => {
    unsafe {
        let _ = KillTimer(Some(self.hwnd), TIMER_RESIZE);
    }
    self.sync_renders();
    self.invalidate();
    LRESULT(0)
}
WM_EXITSIZEMOVE => {
    unsafe {
        let _ = KillTimer(Some(self.hwnd), TIMER_RESIZE);
    }
    self.sync_renders();
    self.invalidate();
    LRESULT(0)
}
```

2f. Replace the `WM_SIZE` arm — resize the target, draw the scaled preview immediately, debounce the crisp re-render:

```rust
WM_SIZE => {
    let size = self.client_size();
    self.app.set_client(size);
    // Re-clamp a zoomed pan against the new client size, or the page draws
    // off-center with a blank band until the next wheel/drag.
    if let Some(px) = self.app.current_px() {
        let client = self.app.client;
        if let Some(d) = self.app.doc_mut() {
            d.scroll = crate::layout::clamp_scroll(px, client, d.scroll);
        }
    }
    if let Some(r) = &mut self.renderer {
        r.resize(size.w, size.h);
    }
    unsafe {
        SetTimer(Some(self.hwnd), TIMER_RESIZE, 150, None);
    }
    self.invalidate();
    LRESULT(0)
}
```

2g. Replace the `WM_PAINT` arm (GDI fill goes away):

```rust
WM_PAINT => {
    let mut ps = PAINTSTRUCT::default();
    let _ = BeginPaint(self.hwnd, &mut ps);
    if let Some(mut r) = self.renderer.take() {
        let _ = r.draw(self.hwnd, &self.app, None);
        self.renderer = Some(r);
    }
    let _ = unsafe { EndPaint(self.hwnd, &ps) };
    LRESULT(0)
}
```

(Remove the now-unused `CreateSolidBrush`/`FillRect`/`DeleteObject`/`COLORREF` imports.)

2h. Extend `on_key` with paging (BEFORE the existing Esc/Ctrl+W arms is fine; keep those):

```rust
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_END, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RIGHT, VK_SHIFT, VK_SPACE,
};
```

```rust
fn on_key(&mut self, vk: u16) {
    let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    match vk {
        _ if vk == VK_ESCAPE.0 => self.quit(),
        0x57 if ctrl => self.quit(), // Ctrl+W
        _ if vk == VK_NEXT.0 || vk == VK_RIGHT.0 => self.flip(1),
        _ if vk == VK_PRIOR.0 || vk == VK_LEFT.0 => self.flip(-1),
        _ if vk == VK_SPACE.0 => self.flip(if shift { -1 } else { 1 }),
        _ if vk == VK_HOME.0 => {
            let changed = self.app.goto(0);
            if changed {
                self.sync_renders();
                self.invalidate();
            }
        }
        _ if vk == VK_END.0 => {
            let last = self.app.doc().map(|d| d.pages.saturating_sub(1)).unwrap_or(0);
            if self.app.goto(last) {
                self.sync_renders();
                self.invalidate();
            }
        }
        _ => {}
    }
}
```

Also extend `WM_DPICHANGED` to forward the DPI to the renderer (add inside the existing arm, before `SetWindowPos`):

```rust
if let Some(r) = &mut self.renderer {
    r.set_dpi((wparam.0 & 0xFFFF) as u32);
}
```

- [ ] **Step 3: Update `src/main.rs` to pass the CLI argument**

Replace the `window::run(App::new())` call:

```rust
    let initial = std::env::args().nth(1).map(std::path::PathBuf::from);
    if let Err(e) = window::run(App::new(), initial) {
```

- [ ] **Step 4: Verify — the app now views PDFs**

Run: `cargo test`
Expected: all green (nothing pure changed).

Generate the fixture PDF (the integration test writes it to `%TEMP%`), then open it in the app:

```pwsh
cargo test --test pdf_integration load_render_outline_links
cargo run -- $env:TEMP\pppdf_fixture.pdf
```

Verify manually:
1. The fixture opens: page 1 shows a black rectangle on white, aspect-fitted, letterboxed with `#1a1a1a`.
2. PgDn/→/Space/wheel-down flip to pages 2 and 3 and stop at the end; PgUp/←/Shift+Space/wheel-up go back; Home/End jump. Flips between cached pages are instant.
3. Resizing the window live: page follows (slightly soft while dragging), snaps crisp ~150 ms after you stop.
4. Also open a real rulebook PDF (any large one) and confirm smooth flipping and that input stays responsive while heavy pages render.
5. Launching with no argument shows the dark empty window with the hint text.

- [ ] **Step 5: Commit**

```pwsh
git add src/d2d.rs src/window.rs src/lib.rs src/main.rs
git commit -m "feat: Direct2D rendering pipeline - crisp fit-to-window pages, prefetch, resize debounce"
```

---

### Task 8: Clickable links, TOC overlay UI, page badge

**Files:**
- Modify: `src/window.rs` (link clicks + cursor, overlay input, badge state/timer, Tab/Enter keys, CMD_TOC)
- Modify: `src/d2d.rs` (overlay panel + badge drawing, row-friendly text format)

**Interfaces:**
- Consumes: `overlay::{OutlineState, OverlayHit, PANEL_W, ROW_H, INDENT, CHEVRON_W}`, `app::{App, Mode}`, `types::LinkTarget`, `layout::PointF`
- Produces: `Ui` gains `badge: Option<(String, u32)>` (text, ticks at 33 ms), `Ui::show_badge()`, `Ui::follow_link(LinkTarget)`, `Ui::overlay_key(u16) -> bool`; `Renderer::draw` badge parameter becomes live
- Behavior contract: overlay consumes ↑↓←→/Enter/Esc while open; Esc closes the overlay first and only quits when it's already closed; clicking a title jumps AND closes; clicking a chevron only toggles; URI links open only `http://`/`https://` targets

- [ ] **Step 1: Badge state + timer in `window.rs`**

1a. Add to `Ui` struct and construction site (in `run`): field `pub badge: Option<(String, u32)>`, initialized `None`.

1b. Add constants and methods:

```rust
const BADGE_HOLD_TICKS: u32 = 21; // ~700 ms at 33 ms/tick
const BADGE_FADE_TICKS: u32 = 10; // ~330 ms fade

impl Ui {
    fn show_badge(&mut self) {
        if let Some(d) = self.app.doc() {
            self.badge = Some((format!("{} / {}", d.current + 1, d.pages), 0));
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_BADGE, 33, None);
            }
            self.invalidate();
        }
    }

    fn badge_display(&self) -> Option<(&str, f32)> {
        self.badge.as_ref().map(|(text, ticks)| {
            let alpha = if *ticks <= BADGE_HOLD_TICKS {
                1.0
            } else {
                1.0 - (*ticks - BADGE_HOLD_TICKS) as f32 / BADGE_FADE_TICKS as f32
            };
            (text.as_str(), alpha.clamp(0.0, 1.0))
        })
    }
}
```

1c. New `WM_TIMER` arm for the badge (alongside the TIMER_RESIZE arm):

```rust
WM_TIMER if wparam.0 == TIMER_BADGE => {
    let done = match &mut self.badge {
        Some((_, ticks)) => {
            *ticks += 1;
            *ticks > BADGE_HOLD_TICKS + BADGE_FADE_TICKS
        }
        None => true,
    };
    if done {
        self.badge = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_BADGE);
        }
    }
    self.invalidate();
    LRESULT(0)
}
```

1d. Call `self.show_badge()` after every successful page change (spec: the badge appears on EVERY page change): inside `Ui::flip` after a successful flip (replace the `// Task 8 shows the page badge here.` comment), in `pump_pdf_replies` after `doc_loaded` (a fresh document announces "1 / N"), and in the `VK_HOME`/`VK_END` arms of `on_key` right after their successful `goto` (next to the existing `sync_renders`/`invalidate` calls).

1e. Change the `WM_PAINT` arm to pass the badge:

```rust
if let Some(mut r) = self.renderer.take() {
    let _ = r.draw(self.hwnd, &self.app, self.badge_display());
    self.renderer = Some(r);
}
```

- [ ] **Step 2: Link following + hand cursor in `window.rs`**

2a. Imports:

```rust
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{SetCursor, IDC_HAND, SW_SHOWNORMAL};
use crate::types::LinkTarget;
```

(`SW_SHOWNORMAL` for `ShellExecuteW` is a `SHOW_WINDOW_CMD`; if the import collides with the existing `ShowWindow` usage just reference it fully qualified.)

2b. Methods:

```rust
impl Ui {
    fn follow_link(&mut self, target: LinkTarget) {
        match target {
            LinkTarget::Page(p) => {
                if self.app.goto(p) {
                    self.sync_renders();
                    self.show_badge();
                    self.invalidate();
                }
            }
            LinkTarget::Uri(uri) => {
                // Spec: default browser for web links; other schemes ignored.
                if uri.starts_with("http://") || uri.starts_with("https://") {
                    unsafe {
                        ShellExecuteW(
                            None,
                            w!("open"),
                            &HSTRING::from(uri.as_str()),
                            None,
                            None,
                            SW_SHOWNORMAL,
                        );
                    }
                }
            }
        }
    }

    fn mouse_point(&self, lparam: LPARAM) -> PointF {
        PointF {
            x: (lparam.0 & 0xFFFF) as i16 as f32,
            y: ((lparam.0 >> 16) & 0xFFFF) as i16 as f32,
        }
    }
}
```

2c. Fill in `wants_client_mouse` (replace its `false` body — this makes links clickable and the open overlay interactive; the zoom clause is inert until Task 9):

```rust
    fn wants_client_mouse(&self, p: PointF) -> bool {
        if let Some(d) = self.app.doc() {
            if d.overlay.is_some() && p.x < crate::overlay::PANEL_W * self.dpi_scale() {
                return true;
            }
            if d.zoom > 1.0 + 1e-4 {
                return true; // Task 9: dragging pans instead of moving the window
            }
            if self.app.link_at(p).is_some() {
                return true;
            }
        }
        false
    }
```

2d. New match arms — clicks and cursor:

```rust
WM_LBUTTONUP => {
    let p = self.mouse_point(lparam);
    self.on_click(p);
    LRESULT(0)
}
WM_SETCURSOR => {
    if (lparam.0 & 0xFFFF) as u32 == HTCLIENT {
        let mut pt = windows::Win32::Foundation::POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(self.hwnd, &mut pt);
        }
        let p = PointF { x: pt.x as f32, y: pt.y as f32 };
        let over_overlay = self
            .app
            .doc()
            .map_or(false, |d| d.overlay.is_some() && p.x < crate::overlay::PANEL_W * self.dpi_scale());
        if !over_overlay && self.app.link_at(p).is_some() {
            unsafe {
                if let Ok(hand) = LoadCursorW(None, IDC_HAND) {
                    let _ = SetCursor(Some(hand));
                }
            }
            return LRESULT(1);
        }
    }
    unsafe { DefWindowProcW(self.hwnd, msg, wparam, lparam) }
}
```

(`GetCursorPos` is in `Win32::UI::WindowsAndMessaging`. If `SetCursor` takes `HCURSOR` directly instead of `Option`, drop the `Some`.)

2e. The click dispatcher (overlay first — it covers the page):

```rust
impl Ui {
    fn on_click(&mut self, p: PointF) {
        let s = self.dpi_scale();
        // 1) Overlay panel
        let overlay_hit = self.app.doc().and_then(|d| d.overlay.as_ref()).and_then(|ov| {
            if p.x < crate::overlay::PANEL_W * s {
                let local = PointF { x: p.x / s, y: p.y / s };
                let panel_h = self.app.client.h as f32 / s;
                Some(match ov.hit_test(local, panel_h) {
                    crate::overlay::OverlayHit::Chevron(i) => (true, i),
                    crate::overlay::OverlayHit::Title(i) => (false, i),
                    crate::overlay::OverlayHit::None => return None,
                })
            } else {
                None
            }
        });
        if let Some((is_chevron, idx)) = overlay_hit {
            let d = self.app.doc_mut().unwrap();
            let ov = d.overlay.as_mut().unwrap();
            if is_chevron {
                ov.toggle(idx);
            } else {
                ov.select(idx);
                let page = ov.activate();
                d.overlay = None; // jump closes the panel
                if let Some(page) = page {
                    if self.app.goto(page) {
                        self.sync_renders();
                        self.show_badge();
                    }
                }
            }
            self.invalidate();
            return;
        }
        // Don't treat page clicks as links while the overlay covers that spot.
        let overlay_open_here = self
            .app
            .doc()
            .map_or(false, |d| d.overlay.is_some() && p.x < crate::overlay::PANEL_W * s);
        if overlay_open_here {
            return;
        }
        // 2) Page links
        if let Some(target) = self.app.link_at(p) {
            self.follow_link(target);
        }
    }
}
```

- [ ] **Step 3: Overlay keyboard + wheel routing in `window.rs`**

3a. Import `VK_TAB, VK_RETURN, VK_UP, VK_DOWN` (KeyboardAndMouse).

3b. At the TOP of `on_key`, route to the overlay when it is open:

```rust
    fn on_key(&mut self, vk: u16) {
        if self.overlay_key(vk) {
            return;
        }
        // ... existing arms unchanged, PLUS a Tab arm:
        //   _ if vk == VK_TAB.0 => { self.app.toggle_overlay(); self.invalidate(); }
    }

    /// Handle a key inside the open overlay. Returns true if consumed.
    fn overlay_key(&mut self, vk: u16) -> bool {
        let s = self.dpi_scale();
        let panel_h = self.app.client.h as f32 / s;
        let Some(d) = self.app.doc_mut() else { return false };
        let Some(ov) = d.overlay.as_mut() else { return false };
        match vk {
            _ if vk == VK_DOWN.0 => ov.move_selection(1, panel_h),
            _ if vk == VK_UP.0 => ov.move_selection(-1, panel_h),
            _ if vk == VK_RIGHT.0 => ov.expand_selected(),
            _ if vk == VK_LEFT.0 => ov.collapse_selected(),
            _ if vk == VK_RETURN.0 => {
                let page = ov.activate();
                d.overlay = None;
                if let Some(page) = page {
                    if self.app.goto(page) {
                        self.sync_renders();
                        self.show_badge();
                    }
                }
            }
            _ if vk == VK_ESCAPE.0 => d.overlay = None,
            _ if vk == VK_TAB.0 => d.overlay = None,
            _ => return false, // PgUp/PgDn etc. still page the document
        }
        self.invalidate();
        true
    }
```

3c. Wire `CMD_TOC` in `on_command`:

```rust
CMD_TOC => {
    self.app.toggle_overlay();
    self.invalidate();
}
```

3d. Wheel over the open panel scrolls it instead of flipping pages — extend `WM_MOUSEWHEEL` (its lparam is SCREEN coordinates):

```rust
WM_MOUSEWHEEL => {
    let notches = -(((wparam.0 >> 16) & 0xFFFF) as i16 as i32) / 120;
    let mut pt = windows::Win32::Foundation::POINT {
        x: (lparam.0 & 0xFFFF) as i16 as i32,
        y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
    };
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::ScreenToClient(self.hwnd, &mut pt);
    }
    let s = self.dpi_scale();
    let over_panel = self
        .app
        .doc()
        .map_or(false, |d| d.overlay.is_some() && (pt.x as f32) < crate::overlay::PANEL_W * s);
    if over_panel {
        let panel_h = self.app.client.h as f32 / s;
        if let Some(ov) = self.app.doc_mut().and_then(|d| d.overlay.as_mut()) {
            ov.scroll_by(notches as f32 * 2.0 * crate::overlay::ROW_H, panel_h);
        }
        self.invalidate();
    } else if notches != 0 {
        match self.app.wheel(notches) {
            crate::app::WheelOutcome::Flipped => {
                self.sync_renders();
                self.show_badge();
                self.invalidate();
            }
            crate::app::WheelOutcome::Redraw => {
                self.sync_renders();
                self.invalidate();
            }
            crate::app::WheelOutcome::None => {}
        }
    }
    LRESULT(0)
}
```

(The badge resets only on an actual flip; a zoomed-scroll `Redraw` repaints without touching it.)

- [ ] **Step 4: Draw the overlay + badge in `d2d.rs`**

4a. Make the body text format row-friendly — in `text_format`, for the non-centered format add after creation:

```rust
            if !centered {
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?; // vertical centering in row rects
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            }
```

4b. In `draw_inner`'s `Mode::Doc` arm, replace `// Overlay + badge drawing lands in Task 8.` and `let _ = badge;` with:

```rust
                    if let Some(ov) = &d.overlay {
                        self.draw_overlay(rt, ov, client)?;
                    }
                    if let Some((text, alpha)) = badge {
                        self.draw_badge(rt, text, alpha, client)?;
                    }
```

4c. Add the two drawing methods to `impl Renderer`:

```rust
    fn draw_overlay(
        &mut self,
        rt: &ID2D1HwndRenderTarget,
        ov: &crate::overlay::OutlineState,
        client: SizeU,
    ) -> Result<()> {
        use crate::overlay::{CHEVRON_W, INDENT, PANEL_W, ROW_H};
        let s = self.scale();
        let panel =
            RectF { left: 0.0, top: 0.0, right: PANEL_W * s, bottom: client.h as f32 };
        unsafe {
            let bg = rt.CreateSolidColorBrush(&color(0.055, 0.055, 0.055, 0.85), None)?;
            rt.FillRectangle(&rectf(panel), &bg);
        }
        if ov.items.is_empty() {
            let fmt = self.text_format(14.0, true)?;
            return self.draw_text(rt, "No table of contents", &fmt, panel, color(1.0, 1.0, 1.0, 0.4));
        }
        let fmt = self.text_format(14.0, false)?;
        let panel_h_dip = client.h as f32 / s;
        for (pos, &idx) in ov.visible().iter().enumerate() {
            let top_dip = ov.row_top(pos);
            if top_dip + ROW_H < 0.0 {
                continue;
            }
            if top_dip > panel_h_dip {
                break;
            }
            let it = &ov.items[idx];
            let y = top_dip * s;
            let row = RectF { left: 0.0, top: y, right: PANEL_W * s, bottom: y + ROW_H * s };
            if idx == ov.selected {
                unsafe {
                    let sel = rt.CreateSolidColorBrush(&color(1.0, 1.0, 1.0, 0.12), None)?;
                    rt.FillRectangle(&rectf(row), &sel);
                }
            }
            let indent = it.depth as f32 * INDENT * s;
            if it.has_children {
                let chev_rect = RectF {
                    left: indent + 4.0 * s,
                    top: row.top,
                    right: indent + CHEVRON_W * s,
                    bottom: row.bottom,
                };
                let glyph = if ov.is_expanded(idx) { "▾" } else { "▸" };
                self.draw_text(rt, glyph, &fmt, chev_rect, color(1.0, 1.0, 1.0, 0.55))?;
            }
            let title_rect = RectF {
                left: indent + CHEVRON_W * s + 2.0 * s,
                top: row.top,
                right: PANEL_W * s - 8.0 * s,
                bottom: row.bottom,
            };
            self.draw_text(rt, &it.title, &fmt, title_rect, color(1.0, 1.0, 1.0, 0.9))?;
        }
        Ok(())
    }

    fn draw_badge(
        &mut self,
        rt: &ID2D1HwndRenderTarget,
        text: &str,
        alpha: f32,
        client: SizeU,
    ) -> Result<()> {
        if alpha <= 0.0 {
            return Ok(());
        }
        let s = self.scale();
        let (bw, bh, m) = (110.0 * s, 36.0 * s, 24.0 * s);
        let r = RectF {
            left: client.w as f32 - bw - m,
            top: client.h as f32 - bh - m,
            right: client.w as f32 - m,
            bottom: client.h as f32 - m,
        };
        unsafe {
            let rr = D2D1_ROUNDED_RECT { rect: rectf(r), radiusX: bh / 2.0, radiusY: bh / 2.0 };
            let bg = rt.CreateSolidColorBrush(&color(0.0, 0.0, 0.0, 0.7 * alpha), None)?;
            rt.FillRoundedRectangle(&rr, &bg);
        }
        let fmt = self.text_format(14.0, true)?;
        self.draw_text(rt, text, &fmt, r, color(1.0, 1.0, 1.0, 0.9 * alpha))
    }
```

- [ ] **Step 5: Verify**

Run: `cargo test`
Expected: all green.

```pwsh
cargo run -- $env:TEMP\pppdf_fixture.pdf
```

Verify manually:
1. Page flips show a "2 / 3"-style pill bottom-right that fades out after ~1 s.
2. Tab opens a translucent left panel listing Chapter One / Chapter Two; Chapter Two has a ▸ chevron; clicking it reveals Section 2.1 (indented); clicking again collapses.
3. ↑/↓ move the selection highlight (skipping collapsed children), → expands, ← collapses/jumps to parent, Enter jumps to the chapter's page AND closes the panel, Esc/Tab close without jumping. Esc with the panel closed still quits.
4. Clicking a title jumps + closes. Mouse wheel over the panel scrolls it (visible once expanded content exceeds the window height — shrink the window to test); wheel right of the panel still flips pages.
5. On fixture page 1: hovering the top-left area (the GoTo link) shows a hand cursor and LEFT-DRAGGING there does NOT move the window; clicking jumps to page 3 (badge "3 / 3"). Hovering the URI link area and clicking opens https://example.com/ in the browser. Dragging anywhere else still moves the window.
6. Open a real rulebook: its TOC tree renders with working navigation; in-text cross-reference links jump correctly.
7. A PDF without an outline (make one: print any webpage to PDF) shows "No table of contents" in the panel.

- [ ] **Step 6: Commit**

```pwsh
git add src/window.rs src/d2d.rs
git commit -m "feat: clickable links, TOC overlay with tree navigation, page badge"
```

---

### Task 9: Zoom and pan

**Files:**
- Modify: `src/window.rs` (Ctrl+wheel zoom, drag-pan, Ctrl+drag window move, double-click / `0` reset, CMD_FIT)

**Interfaces:**
- Consumes: `app::{zoom_by, reset_zoom, wheel}` (Task 5), `layout::PointF`
- Produces: `Ui` gains `drag: Option<Drag>` where `struct Drag { start: PointF, start_scroll: PointF, moved: bool }`
- Behavior contract (spec): Ctrl+wheel zooms at the cursor (fit…500%); dragging pans while zoomed (clamped); Ctrl+drag moves the window while zoomed; double-click or `0` resets to fit; page flips already reset zoom (Task 5). At fit, dragging the page still moves the window (unchanged from Task 6).

- [ ] **Step 1: Drag state**

Add to `window.rs`:

```rust
pub struct Drag {
    start: PointF,
    start_scroll: PointF,
    moved: bool,
}
```

Add field `pub drag: Option<Drag>` to `Ui` (init `None` in `run`).

Imports needed in this task:

```rust
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_F11, VK_NUMPAD0};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXDRAG, SM_CYDRAG};
```

(`MK_CONTROL` lives at `windows::Win32::System::SystemServices::MK_CONTROL` — a `MODIFIERKEYS_FLAGS(u32)` — and its cargo feature `Win32_System_SystemServices` is already enabled in Task 1's Cargo.toml. If the import fails, check the FEATURE LIST first, not the path. `VK_F11` is used by Task 10; importing now is fine.)

- [ ] **Step 2: Ctrl+wheel zooms — extend `WM_MOUSEWHEEL`**

At the top of the existing arm (after computing `notches` and the client-space `pt`), insert:

```rust
    let ctrl = (wparam.0 & 0xFFFF) as u32 & MK_CONTROL.0 != 0;
    if ctrl {
        let cursor = PointF { x: pt.x as f32, y: pt.y as f32 };
        // 1.1 per notch; wheel up (negative notches here) zooms in.
        let factor = 1.1_f32.powi(-notches);
        if self.app.zoom_by(cursor, factor) {
            self.sync_renders(); // crisp render at the new size; soft preview meanwhile
            self.invalidate();
        }
        return LRESULT(0);
    }
```

- [ ] **Step 3: Drag-pan + click discrimination**

New match arms:

```rust
WM_LBUTTONDOWN => {
    let p = self.mouse_point(lparam);
    let over_panel = self
        .app
        .doc()
        .map_or(false, |d| d.overlay.is_some() && p.x < crate::overlay::PANEL_W * self.dpi_scale());
    let zoomed = self.app.doc().map_or(false, |d| d.zoom > 1.0 + 1e-4);
    let ctrl = (wparam.0 & 0xFFFF) as u32 & MK_CONTROL.0 != 0;
    if zoomed && ctrl && !over_panel {
        // Move the borderless window from a zoomed state: hand the drag to
        // the OS as a caption drag (winit ships this exact pattern).
        unsafe {
            let _ = ReleaseCapture();
            let _ = PostMessageW(
                Some(self.hwnd),
                WM_NCLBUTTONDOWN,
                WPARAM(HTCAPTION as usize),
                LPARAM(0),
            );
        }
    } else if zoomed && !over_panel {
        unsafe {
            SetCapture(self.hwnd);
        }
        let scroll = self.app.doc().map(|d| d.scroll).unwrap_or_default();
        self.drag = Some(Drag { start: p, start_scroll: scroll, moved: false });
    }
    LRESULT(0)
}
WM_MOUSEMOVE => {
    if let Some(drag) = &mut self.drag {
        let p = self.mouse_point(lparam);
        let (dx, dy) = (p.x - drag.start.x, p.y - drag.start.y);
        let (tx, ty) = unsafe { (GetSystemMetrics(SM_CXDRAG) as f32, GetSystemMetrics(SM_CYDRAG) as f32) };
        if dx.abs() > tx || dy.abs() > ty {
            drag.moved = true;
        }
        if drag.moved {
            let want =
                PointF { x: drag.start_scroll.x - dx, y: drag.start_scroll.y - dy };
            if let (Some(px), Some(_)) = (self.app.current_px(), self.app.doc()) {
                let client = self.app.client;
                if let Some(d) = self.app.doc_mut() {
                    d.scroll = crate::layout::clamp_scroll(px, client, want);
                }
            }
            self.invalidate();
        }
    }
    LRESULT(0)
}
WM_CAPTURECHANGED => {
    self.drag = None;
    LRESULT(0)
}
WM_LBUTTONDBLCLK => {
    // Client-space double click (only reachable when zoomed / over links).
    if self.app.reset_zoom() {
        self.sync_renders();
        self.invalidate();
    }
    LRESULT(0)
}
```

Replace the `WM_LBUTTONUP` arm from Task 8 with the drag-aware version:

```rust
WM_LBUTTONUP => {
    // ORDER MATTERS: take the drag state BEFORE ReleaseCapture().
    // ReleaseCapture delivers WM_CAPTURECHANGED synchronously (reentering
    // this WndProc), and that arm clears self.drag — taking it afterwards
    // would always see None and every pan release would count as a click.
    let was_pan = self.drag.take().map_or(false, |d| d.moved);
    unsafe {
        let _ = ReleaseCapture();
    }
    if !was_pan {
        let p = self.mouse_point(lparam);
        self.on_click(p);
    }
    LRESULT(0)
}
```

Note on borrow shape in `WM_MOUSEMOVE`: `drag` is `&mut` out of `self` while we also call `self.app...`. If the borrow checker objects, copy the needed drag fields out first (`let (start, start_scroll, moved) = ...`), compute, then write back — keep the behavior identical.

- [ ] **Step 4: Keyboard/menu resets and the fit double-click**

4a. In `on_key`, add arms (keep existing ones):

```rust
    _ if vk == 0x30 || vk == VK_NUMPAD0.0 => {
        // '0': reset to fit
        if self.app.reset_zoom() {
            self.sync_renders();
            self.invalidate();
        }
    }
```

4b. In `on_command`, wire `CMD_FIT` with the same body.

4c. In the `WM_NCLBUTTONDBLCLK` arm (Task 6 swallows it), also reset zoom before returning — double-click on the caption-draggable page body resets to fit:

```rust
WM_NCLBUTTONDBLCLK => {
    if wparam.0 as u32 == HTCAPTION {
        if self.app.reset_zoom() {
            self.sync_renders();
            self.invalidate();
        }
        return LRESULT(0); // never let DefWindowProc maximize
    }
    DefWindowProcW(self.hwnd, msg, wparam, lparam)
}
```

- [ ] **Step 5: Verify**

Run: `cargo test`
Expected: all green.

```pwsh
cargo run -- $env:TEMP\pppdf_fixture.pdf
```

Verify manually (use a real, image-heavy PDF for the quality checks):
1. Ctrl+wheel-up zooms in around the cursor position (the point under the cursor stays put); zoom caps at 500%; Ctrl+wheel-down back to fit stops at fit.
2. While zoomed: page looks soft for a moment then snaps crisp (the re-render). Dragging pans, clamped at page edges; the window does NOT move. Ctrl+drag DOES move the window.
3. While zoomed: plain wheel scrolls vertically; at the bottom edge one further notch flips to the next page at fit. Wheel-up at the top edge flips back.
4. Double-click resets to fit (both zoomed and at fit — no maximize ever). `0` and right-click → Fit to Window do the same.
5. Page flip while zoomed lands on the next page at fit.
6. Links still click at fit; at zoom, clicking (without dragging) a link still follows it — small drags do not count as clicks.
7. Window resize while zoomed keeps the zoom factor and re-clamps the pan.

- [ ] **Step 6: Commit**

```pwsh
git add src/window.rs
git commit -m "feat: cursor-anchored zoom, drag pan, ctrl-drag window move, fit resets"
```

---

### Task 10: Open dialog, drag-and-drop, password entry, error handling, F11

**Files:**
- Modify: `src/window.rs` (open dialog, WM_DROPFILES, WM_CHAR password entry, LoadFailed/NeedsPassword handling, F11 fill toggle)
- Modify: `src/main.rs` (panic hook)

**Interfaces:**
- Consumes: `app::Mode::{Password, Loading, Empty}` + password buffer methods (Task 5), `types::{PdfCmd, PdfReply}`
- Produces: `Ui` gains `saved_rect: Option<RECT>` (F11 restore state); `Ui::open_dialog()`, `Ui::error_box(&str)`, `Ui::toggle_fill()`
- Behavior contract (spec's error table):
  - pdfium.dll missing → explanatory box, app exits
  - unopenable file → box with the reason; if a document is already open it STAYS open and usable (the PDF thread keeps the old document on failed loads — Task 4)
  - password-protected → chromeless in-window prompt; Enter retries; wrong password re-prompts; Esc cancels back to the document that was open before (spec: "keep current doc"), or the empty state if none was
  - URI links: http/https only (already enforced in Task 8)

- [ ] **Step 1: Open dialog + Ctrl+O + drag-and-drop**

1a. Imports:

```rust
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows::Win32::UI::Shell::{DragFinish, DragQueryFileW, HDROP};
```

1b. Methods:

```rust
impl Ui {
    fn open_dialog(&mut self) {
        let mut file_buf = [0u16; 1024];
        let mut ofn = OPENFILENAMEW {
            lStructSize: core::mem::size_of::<OPENFILENAMEW>() as u32,
            hwndOwner: self.hwnd,
            // w!() const-encodes the embedded NULs of the classic filter format.
            lpstrFilter: w!("PDF documents (*.pdf)\0*.pdf\0All files (*.*)\0*.*\0"),
            nFilterIndex: 1,
            lpstrFile: PWSTR(file_buf.as_mut_ptr()),
            nMaxFile: file_buf.len() as u32,
            lpstrTitle: w!("Open PDF"),
            Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST,
            ..Default::default()
        };
        if unsafe { GetOpenFileNameW(&mut ofn) }.as_bool() {
            let len = file_buf.iter().position(|&c| c == 0).unwrap_or(file_buf.len());
            self.open_document(PathBuf::from(String::from_utf16_lossy(&file_buf[..len])));
        }
    }

    fn error_box(&self, text: &str) {
        let wide: Vec<u16> = format!("{text}\0").encode_utf16().collect();
        unsafe {
            MessageBoxW(Some(self.hwnd), PCWSTR(wide.as_ptr()), w!("pppdf"), MB_OK | MB_ICONERROR);
        }
    }
}
```

(`PWSTR` comes from `windows::core`; `MessageBoxW`/`MB_*` from WindowsAndMessaging. If `MessageBoxW`'s hwnd is not `Option` in this version, pass `self.hwnd` bare.)

1c. Update `open_document` (Task 7 version) so an already-open document keeps showing until the replacement actually loads:

```rust
    pub fn open_document(&mut self, path: PathBuf) {
        if self.app.doc().is_none() {
            self.app.mode = crate::app::Mode::Loading { path: path.clone() };
        }
        self.send_pdf(PdfCmd::Load { path, password: None });
        self.invalidate();
    }
```

1d. Wire `CMD_OPEN => self.open_dialog(),` in `on_command`, and add to `on_key`: `0x4F if ctrl => self.open_dialog(), // Ctrl+O`.

1e. New match arm:

```rust
WM_DROPFILES => {
    let hdrop = HDROP(wparam.0 as *mut core::ffi::c_void);
    let mut buf = [0u16; 1024];
    let n = unsafe { DragQueryFileW(hdrop, 0, Some(&mut buf)) };
    unsafe { DragFinish(hdrop) };
    if n > 0 {
        self.open_document(PathBuf::from(String::from_utf16_lossy(&buf[..n as usize])));
    }
    LRESULT(0)
}
```

- [ ] **Step 2: Password prompt (chromeless, in-window) + load errors**

2a. Replace the `NeedsPassword`/`LoadFailed` handling inside `pump_pdf_replies`:

```rust
                PdfReply::NeedsPassword { path } => {
                    // Stash a currently open document so Esc can restore it
                    // (the PDF thread kept it loaded, too — Task 4).
                    let prev = match std::mem::replace(&mut self.app.mode, crate::app::Mode::Empty)
                    {
                        crate::app::Mode::Doc(d) => Some(Box::new(d)),
                        _ => None,
                    };
                    self.app.mode =
                        crate::app::Mode::Password { path, buf: String::new(), prev };
                }
                PdfReply::LoadFailed { path, reason } => {
                    if path.as_os_str().is_empty() {
                        // pdfium.dll itself failed to load — nothing can work.
                        self.error_box(
                            "pdfium.dll could not be loaded.\n\nIt must sit in the same \
                             folder as pppdf.exe. Run fetch-pdfium.ps1 or re-copy the dll, \
                             then start pppdf again.",
                        );
                        self.quit();
                    } else {
                        self.error_box(&format!(
                            "Could not open\n{}\n\n{reason}",
                            path.display()
                        ));
                        // A previously open document is untouched (the PDF thread
                        // kept it); only fall back to Empty if nothing is open.
                        if matches!(self.app.mode, crate::app::Mode::Loading { .. }) {
                            self.app.mode = crate::app::Mode::Empty;
                        }
                    }
                }
```

2b. Password keyboard routing. At the very top of `on_key`:

```rust
        if matches!(self.app.mode, crate::app::Mode::Password { .. }) {
            if vk == VK_ESCAPE.0 {
                // Restores the stashed doc if one was open, else Empty.
                self.app.password_cancel();
                self.sync_renders();
                self.invalidate();
            }
            return; // printable input + Enter/Backspace arrive via WM_CHAR
        }
```

2c. New `WM_CHAR` arm:

```rust
WM_CHAR => {
    if matches!(self.app.mode, crate::app::Mode::Password { .. }) {
        let c = char::from_u32(wparam.0 as u32).unwrap_or('\0');
        match c {
            '\r' => {
                if let Some((path, pw)) = self.app.password_take() {
                    self.send_pdf(PdfCmd::Load { path, password: Some(pw) });
                }
            }
            '\u{8}' => self.app.password_backspace(),
            c if !c.is_control() => self.app.password_char(c),
            _ => {}
        }
        self.invalidate();
    }
    LRESULT(0)
}
```

(A wrong password makes pdfium fail with the same password error → `NeedsPassword` arrives again → the prompt simply reappears empty. That is the intended retry loop.)

- [ ] **Step 3: F11 — fill the work area**

3a. Add field `pub saved_rect: Option<RECT>` to `Ui` (init `None`), and the method:

```rust
    fn toggle_fill(&mut self) {
        unsafe {
            if let Some(saved) = self.saved_rect.take() {
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    saved.left,
                    saved.top,
                    saved.right - saved.left,
                    saved.bottom - saved.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                return;
            }
            let mut rc = RECT::default();
            let _ = GetWindowRect(self.hwnd, &mut rc);
            let monitor = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: core::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(monitor, &mut mi).as_bool() {
                self.saved_rect = Some(rc);
                let w = mi.rcWork;
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    w.left,
                    w.top,
                    w.right - w.left,
                    w.bottom - w.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }
```

3b. `on_key` arm: `_ if vk == VK_F11.0 => self.toggle_fill(),`

- [ ] **Step 4: Panic hook in `main.rs`**

Insert at the top of `main()` (before the DPI call):

```rust
    // Any thread's panic (incl. the PDF thread) surfaces visibly, then exits —
    // no zombie chromeless window.
    std::panic::set_hook(Box::new(|info| {
        let text: Vec<u16> = format!("pppdf crashed:\n{info}\0").encode_utf16().collect();
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
            MessageBoxW(
                None,
                windows::core::PCWSTR(text.as_ptr()),
                windows::core::w!("pppdf"),
                MB_OK | MB_ICONERROR,
            );
        }
        std::process::exit(1);
    }));
```

- [ ] **Step 5: Verify**

Run: `cargo test`
Expected: all green.

Manual checks:
1. `cargo run` → Ctrl+O and right-click → Open… both show the file dialog filtered to PDFs; opening works. Cancel does nothing.
2. Drop a .pdf from Explorer onto the window → it opens (replacing the current one). Drop while a doc is open → replaced.
3. Open a bogus file (`Set-Content junk.pdf 'not a pdf'`, then open it while a real doc is displayed) → error box appears, the real doc is still shown and fully working afterward.
4. Rename `target\debug\pdfium.dll` away and `cargo run` → explanatory message box, app exits. Rename it back.
5. Password flow: create an encrypted PDF (e.g. in any tool that sets a user password, or grab one) → opening it shows the in-window "Password required" prompt with • per keystroke; wrong password + Enter re-prompts; correct password opens; Esc while a document was already open restores THAT document at its page; Esc with nothing open shows the hint screen. (No encrypted sample handy → verify the prompt appears with any AES-encrypted sample from the web and note the result in the commit message.)
6. F11 fills the monitor work area edge to edge (taskbar stays visible); F11 again restores the previous size/position. Win+Up native maximize still behaves (independent of F11).

- [ ] **Step 6: Commit**

```pwsh
git add src/window.rs src/main.rs
git commit -m "feat: open dialog, drag-drop, in-window password prompt, error surfaces, F11 fill"
```

---

### Task 11: Documentation, manual test checklist, release build

**Files:**
- Modify: `README.md` (full version)
- Create: `docs/manual-tests.md`

**Interfaces:**
- Consumes: the finished app
- Produces: user-facing docs; a repeatable manual regression checklist; a verified release binary

- [ ] **Step 1: Write the full `README.md`**

```markdown
# pppdf

A borderless, native PDF viewer for pen & paper RPG sessions on Windows.
The page *is* the window: no title bar, no toolbars, no borders — snap a
rulebook next to your VTT and it looks like a page lying on the desktop.

- Full PDF rendering (PDFium — the same engine Chrome uses, as a plain DLL;
  no browser, no web runtime)
- Clickable in-document links; table of contents from the PDF outline
- Single page, always crisp, fit-to-window; Ctrl+wheel zoom for maps
- One window per book — run several instances side by side
- Two files, ~8 MB total, no installation, nothing persisted

## Build

1. Install Rust (stable, 1.82+): https://rustup.rs
2. `.\fetch-pdfium.ps1` (downloads pdfium.dll into the target dirs)
3. `cargo build --release`

"Install": copy `target\release\pppdf.exe` and `target\release\pdfium.dll`
into any folder together. Optionally right-click a PDF → Open with →
choose pppdf.exe.

## Use

`pppdf.exe book.pdf` — or start it empty and right-click / drop a PDF on it.

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
| F11 | Fill the screen (work area) / restore |
| Esc / Ctrl+W | Quit (Esc closes the overlay first) |

## Licenses

pppdf's PDF rendering uses PDFium (BSD-3-Clause / Apache-2.0, © The PDFium
Authors) via prebuilt binaries from
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) (MIT).
```

- [ ] **Step 2: Write `docs/manual-tests.md`**

```markdown
# Manual regression checklist

Window (the flagship — test every release):
- [ ] Zero chrome: no title bar/border at any DPI; square corners; drop shadow visible
- [ ] Drag-move from anywhere on the page; Aero Snap left/right/top; Win+arrows all work
- [ ] Maximized (Win+Up): fills work area exactly, taskbar usable, nothing off-screen
- [ ] All 8 resize grips work at 100% and 150% DPI; none while maximized
- [ ] Min size clamps at 200×200 (try to shrink below)
- [ ] Double-click page never maximizes (resets zoom instead)
- [ ] Minimize/restore via taskbar + Win+Down: animations play
- [ ] Drag between two monitors with different DPI: crisp re-render, correct size (WM_DPICHANGED)
- [ ] F11 fill + restore on both monitors
- [ ] Two instances side by side (one window per book)

Document:
- [ ] Open via argument, Ctrl+O, right-click menu, drag-and-drop, "Open with"
- [ ] Fit-to-window portrait + landscape pages; letterbox color uniform
- [ ] Instant flips on cached pages; heavy art pages render without freezing input
- [ ] Live resize: soft preview while dragging, crisp within ~150 ms of release
- [ ] TOC overlay: tree expand/collapse, keyboard nav, jump+close, "No table of contents" case
- [ ] Links: internal GoTo jumps; https link opens browser; hand cursor only over links
- [ ] Page badge appears on every page change and fades
- [ ] Zoom: cursor-anchored, 500% cap, pan clamped, wheel-at-edge page flip both directions
- [ ] Errors: bogus file (doc survives), missing pdfium.dll (clean exit), password prompt loop
```

- [ ] **Step 3: Release build + final smoke**

```pwsh
cargo build --release
Get-Item target\release\pppdf.exe | Select-Object Name, Length
.\target\release\pppdf.exe $env:TEMP\pppdf_fixture.pdf
```

Expected: exe builds; size roughly 1–2 MB; the release binary opens the fixture, borderless, pages flip. Run through `docs/manual-tests.md` once, top to bottom, and fix anything that fails before proceeding.

Run: `cargo test`
Expected: all green (final confirmation).

- [ ] **Step 4: Commit**

```pwsh
git add README.md docs/manual-tests.md
git commit -m "docs: README with key map, manual regression checklist"
```

---

## Plan self-review notes (for the implementer)

- The `windows`/`pdfium-render` snippets were verified against 0.62.2 / 0.9.2 docs on 2026-07-08. Small signature drift (e.g. an `Option<...>` parameter, a `.0` on a typed constant) is expected occasionally — fix forward at the call site; do NOT change crate versions, and do NOT switch to deprecated 0.8.x pdfium-render idioms.
- Anything ambiguous: the spec (`docs/superpowers/specs/2026-07-08-pppdf-viewer-design.md`) is the authority.
- Windows-message behavior that unit tests cannot cover is covered by each task's manual verification list — actually perform them; the borderless window IS the product.

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
- [ ] Double-click directly on a hyperlink: exactly ONE navigation (no spurious second jump)
- [ ] Open a new PDF (drop/menu) while the password prompt is showing: previously open doc keeps working
- [ ] With a GPU-heavy app running (video playback + a hardware-accelerated browser session): launch and confirm content renders; if the window ever goes white, it must self-heal within ~1 s (device-loss recovery)

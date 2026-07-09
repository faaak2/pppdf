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

Form filling (Pro) — run the Pro build against `tests/fixtures/form.pdf`:
- [ ] Click the FullName field: focus ring appears, caret appears and blinks
- [ ] Type `Hello World`: text appears; Space typed a space (no page flip); G/R/0 type letters
- [ ] Click the City dropdown: the popup list renders inside the page; click an option: it's selected
- [ ] Married checkbox toggles on click; Gender radios are mutually exclusive
- [ ] Tab cycles fields top-to-bottom; Shift+Tab reverses; passing over checkboxes/radios must NOT toggle them
- [ ] Esc leaves the field (caret gone)
- [ ] Ctrl+wheel zoom in, click a field while zoomed: caret lands where clicked; R rotate, type again: still correct
- [ ] Wheel over the page in typing mode: leaves the field, flips the page
- [ ] Ctrl+V pastes clipboard text into the focused field
- [ ] Free edition (`cargo run -- tests/fixtures/form.pdf`): clicking a field shows the toast "Form filling is a pppdf Pro feature" exactly once per run; fields never focus

Save UX (Pro) — same `tests/fixtures/form.pdf`:
- [ ] Type into a field: an amber dot appears top-right (quiet, small); it stays while typing
- [ ] Ctrl+S (in typing mode, and again after Esc/outside it): "Saved" toast appears, dot disappears
- [ ] Type again, press Esc then Esc (quit) with unsaved changes: Save/Discard/Cancel box appears, text exactly "Save changes to this PDF before closing?"
- [ ] Cancel: window stays open, edits and dot are untouched
- [ ] Esc-Esc again, Discard: window closes without writing (reopen to confirm the file is unchanged)
- [ ] Reopen, type into a field, Alt+F4: same Save/Discard/Cancel prompt (WM_CLOSE routes through the same gate)
- [ ] Yes: saves then closes — verify via `pro::forms::read_field_values` or by reopening the file
- [ ] Ctrl+S on a document with no form fields (e.g. a plain PDF) is a silent no-op (no toast, no save)
- [ ] Quitting with no unsaved changes (fresh open, or right after a successful save) closes immediately with no prompt

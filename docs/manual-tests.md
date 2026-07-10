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

Page history (both editions):
- [ ] Click an internal link, then Alt+Left: returns to the origin page; Alt+Right re-does the jump
- [ ] TOC jump, then the mouse back button: returns — including clicks over the draggable (unzoomed) page body
- [ ] G-goto and Home/End create back stops; PgUp/PgDn/wheel paging never does
- [ ] Backspace = back, Shift+Backspace = forward; in the G prompt Backspace still deletes a digit
- [ ] Pro: Backspace inside a focused form field deletes text (never navigates); Alt+Left while typing leaves the field and navigates
- [ ] Alt+F4 still closes (prompting if dirty); Alt+Space still opens the system menu
- [ ] Open another document: history is empty (Backspace does nothing)

Text search (both editions):
- [ ] Ctrl+F opens the bar top-right; typing live-highlights from the 2nd character; counter counts up with "…" while scanning, then settles
- [ ] Enter reveals the current match first, then steps; Shift+Enter / Shift+F3 step back; both directions wrap around
- [ ] Match on another page: the jump keeps the zoom level and scrolls the match into view; page badge shows; Alt+Left returns to the origin
- [ ] Wheel, page flips, clicks, and zooming all leave the bar open; PgUp/PgDn page the document while the bar has focus
- [ ] Esc closes the bar (highlights vanish); Ctrl+F reopens with the last query — the first keystroke replaces it wholesale, Backspace clears it
- [ ] F3 with the bar closed reopens the last query and jumps to its first hit as it arrives
- [ ] Alt+C / Alt+W flip the Aa / W indicators, re-run the search, and change the counts ("Dragon" vs "dragonfire" style probes)
- [ ] A term not in the document: "0 / 0" in red once the scan finishes; a 1-char query shows no counter and no highlights
- [ ] R rotation and Ctrl+wheel zoom with highlights showing: the washes track the text exactly
- [ ] 200-DIP window: the bar clamps inside; clicks ON it don't drag the window, clicks beside it still do
- [ ] Pro: the dirty dot stays visible above the bar's corner; Ctrl+F while typing in a form field leaves the field and opens the bar; clicking a field while the bar is open types into the field (bar stays); Esc leaves the field, second Esc closes the bar
- [ ] Open a password-protected PDF over a live search, Esc back to the document: the search re-runs (counter live again — never stuck on "…")
- [ ] With the bar open: Ctrl+W still quits, Ctrl+O still opens, Ctrl+N still spawns, Ctrl+S (Pro) still saves

Form filling (Pro) — run the Pro build against `tests/fixtures/form.pdf`:
- [ ] Fields open with a pale blue highlight (in a fresh window); `F` cycles to dotted frames, then off, then back (toast names each mode); the marks track zoom and rotation; the chosen mode survives opening another PDF in the same window (viewer preference, not document state)
- [ ] While typing in a field, `f` types the letter (must NOT cycle the indication)
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
- [ ] Multi-page form: flip to page 2+, click a field there — it accepts focus and typing (engine page-switch path), and Tab still cycles that page's fields
- [ ] Fill a field, then open another PDF (Ctrl+O / drag-drop): Save/Discard/Cancel prompt appears ("Save changes to this PDF first?") — Cancel keeps the current doc + edits (nothing loads); Yes saves then opens the new PDF; No discards the edits and opens
- [ ] Ctrl+S with no unsaved changes (fresh open, or right after a save) on a form document: nothing happens — no "Saved" toast, file mtime unchanged

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

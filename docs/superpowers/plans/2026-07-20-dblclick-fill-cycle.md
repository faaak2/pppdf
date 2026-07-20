# Double-Click Fill Cycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A double-click on the plain, unzoomed page background advances the same three-stage F11 window cycle (normal → height-fit → fill → normal).

**Architecture:** One arm changes — the `HTCAPTION` branch of `WM_NCLBUTTONDBLCLK` in `src/window.rs`. Existing hit-test routing (`wants_client_mouse`) already excludes links, form fields, the TOC panel, the find bar, the help overlay and any zoomed state from that arm, so no new guards are introduced and the client-path `WM_LBUTTONDBLCLK` handler is not touched. Spec: `docs/superpowers/specs/2026-07-20-dblclick-fill-cycle-design.md`.

**Tech Stack:** Rust, windows crate (Win32). No new pure logic, so no new unit tests — the gate is both editions building and both full suites staying green, plus manual-test rows.

## Global Constraints

- Do **not** modify the `WM_LBUTTONDBLCLK` handler (window.rs:756) — it must stay byte-identical.
- Do **not** modify `toggle_fill`, `FillStage`, or `hit_test`/`wants_client_mouse`.
- The arm must return `LRESULT(0)` on every `HTCAPTION` path so DefWindowProc can never maximize.
- Prompt rule: an open go-to-page or bookmark-name prompt is dismissed **without** cycling; the cycle happens only when no prompt was open.
- `App::bookmark_entry_active()` exists only under `#[cfg(feature = "pro")]`; the free build tests the goto prompt alone.
- Help copy, verbatim: `("0 · double-click when zoomed", "Back to fit")` and `("F11 · double-click the page", "Fit screen height / fill / restore")` — same two strings in `src/d2d.rs` and `README.md`.
- Every change must pass `cargo build` and `cargo test` both **with and without** `--features pro`.

---

### Task 1: Double-click advances the cycle

**Files:**
- Modify: `src/window.rs:419-434` (the `WM_NCLBUTTONDBLCLK` arm only)

**Interfaces:**
- Consumes: `Ui::toggle_fill(&mut self)` (private method, window.rs), `App::reset_zoom(&mut self) -> bool`, `App::goto_entry_active(&self) -> bool`, `App::bookmark_entry_active(&self) -> bool` (Pro only), `Ui::dismiss_goto_entry(&mut self)`, `Ui::dismiss_bookmark_entry(&mut self)` (Pro only).
- Produces: nothing new; no signature changes.

- [ ] **Step 1: Replace the arm**

In `src/window.rs`, replace this exact block (lines 419-434):

```rust
                WM_NCLBUTTONDBLCLK => {
                    // DefWindowProc would toggle maximize on caption dbl-click.
                    // Swallow it, resetting zoom instead (double-click on the
                    // caption-draggable page body resets to fit).
                    if wparam.0 as u32 == HTCAPTION {
                        self.dismiss_goto_entry();
                        #[cfg(feature = "pro")]
                        self.dismiss_bookmark_entry();
                        if self.app.reset_zoom() {
                            self.sync_renders();
                            self.invalidate();
                        }
                        return LRESULT(0); // never let DefWindowProc maximize
                    }
                    DefWindowProcW(self.hwnd, msg, wparam, lparam)
                }
```

with:

```rust
                WM_NCLBUTTONDBLCLK => {
                    // DefWindowProc would toggle maximize on caption dbl-click.
                    // Swallow it: on the caption-draggable page body a
                    // double-click resets zoom, or — already at fit — advances
                    // the F11 fill cycle. HTCAPTION is only reported where
                    // wants_client_mouse() said no (no link, field, panel,
                    // find bar or help overlay under the pointer, and not
                    // zoomed), so this arm never overrides those handlers.
                    if wparam.0 as u32 == HTCAPTION {
                        // An open prompt is abandoned by the gesture and
                        // nothing else — resizing the window out from under
                        // someone who was typing would be a surprise.
                        // Split by edition rather than `let mut` + `|=`: the
                        // free build would never mutate it (unused_mut).
                        #[cfg(feature = "pro")]
                        let prompt_open =
                            self.app.goto_entry_active() || self.app.bookmark_entry_active();
                        #[cfg(not(feature = "pro"))]
                        let prompt_open = self.app.goto_entry_active();
                        self.dismiss_goto_entry();
                        #[cfg(feature = "pro")]
                        self.dismiss_bookmark_entry();
                        if prompt_open {
                            return LRESULT(0);
                        }
                        // reset_zoom compares against exactly 1.0 while the
                        // hit test allows a 1e-4 epsilon, so a hair of
                        // residual zoom still resets instead of resizing.
                        if self.app.reset_zoom() {
                            self.sync_renders();
                            self.invalidate();
                        } else {
                            self.toggle_fill();
                        }
                        return LRESULT(0); // never let DefWindowProc maximize
                    }
                    DefWindowProcW(self.hwnd, msg, wparam, lparam)
                }
```

- [ ] **Step 2: Build and test both editions**

Run: `cargo build && cargo test` then `cargo build --features pro && cargo test --features pro`
Expected: both build (2 known pre-existing `pdfium-render` vendor warnings on the pro build), all tests green — free 149 unit + 12 integration, pro 161 unit + 23 integration.

- [ ] **Step 3: Commit**

```bash
git add src/window.rs
git commit -m "feat: double-click the page advances the F11 fill cycle"
```

---

### Task 2: Docs — help rows, README, manual tests

**Files:**
- Modify: `src/d2d.rs:34` and `src/d2d.rs:39`, `README.md:77` and `README.md:83`, `docs/manual-tests.md`

**Interfaces:**
- Consumes: the behavior shipped in Task 1.
- Produces: user-facing copy; the Global Constraints strings apply verbatim.

- [ ] **Step 1: Split the two F1 help rows**

In `src/d2d.rs` line 34, replace:

```rust
    ("Double-click or 0", "Back to fit"),
```

with:

```rust
    ("0 \u{b7} double-click when zoomed", "Back to fit"),
```

and at line 39, replace:

```rust
    ("F11", "Fit screen height / fill / restore"),
```

with:

```rust
    ("F11 \u{b7} double-click the page", "Fit screen height / fill / restore"),
```

(The file writes `·` as the escape `\u{b7}` — match the surrounding rows' style, e.g. line 33 `"Pan \u{b7} Ctrl+drag moves the window"`.)

- [ ] **Step 2: Update the README key map**

In `README.md` line 77, replace:

```markdown
| Double-click or 0 | Back to fit |
```

with:

```markdown
| 0 · double-click when zoomed | Back to fit |
```

In `README.md` line 83, replace:

```markdown
| F11 | Fit screen height / fill / restore |
```

with:

```markdown
| F11 · double-click the page | Fit screen height / fill / restore |
```

- [ ] **Step 3: Add manual-test rows**

In `docs/manual-tests.md`, find this line in the Window section:

```markdown
- [ ] F11 cycle on a portrait PDF: height-fit (work-area height, page aspect, nearest edge) → fill → restore; on both monitors
```

Insert these six rows directly after it:

```markdown
- [ ] Double-click the page background drives the same cycle, and interleaves with F11 (one press, one double-click, … keeps advancing)
- [ ] Double-click while zoomed still resets to fit and never resizes the window
- [ ] Double-click over a link (navigates), a form field (Pro: word-select / checkbox), the find bar, and the TOC panel: unchanged, no resize
- [ ] Double-click with the F1 help up dismisses it without resizing
- [ ] Double-click with the G prompt or the Ctrl+B prompt (Pro) open dismisses it without resizing; the next double-click cycles
- [ ] Empty window (no PDF): double-click fills the work area, again restores
```

- [ ] **Step 4: Build and test both editions**

Run: `cargo build && cargo test` then `cargo build --features pro && cargo test --features pro`
Expected: both green (d2d.rs is shared — the rows must compile in both editions).

- [ ] **Step 5: Commit**

```bash
git add src/d2d.rs README.md docs/manual-tests.md
git commit -m "docs: double-click fill cycle in help, README, manual tests"
```

# Page History Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Back/forward navigation through the page *jump* history (links, TOC, `G` goto, Home/End) via Alt+Left/Right, Backspace/Shift+Backspace, and the mouse back/forward buttons — both editions.

**Architecture:** A pure `History` struct (two `Vec<u16>` stacks) lives inside `DocState` in `src/app.rs`; `App::goto` records the departure page on every actual page change, `App::back()/forward()` traverse without recording. `src/window.rs` adds thin input arms (`WM_SYSKEYDOWN`, `WM_APPCOMMAND`, a `VK_BACK` case in `on_key`) that call `Ui::nav_back()/nav_forward()` wrappers mirroring the existing `Ui::flip` ritual. No new UI — the existing page badge is the only feedback.

**Tech Stack:** Rust (stable 1.82+), windows-rs (Win32), no new dependencies.

Spec: `docs/superpowers/specs/2026-07-10-page-history-design.md`.

## Global Constraints

- Both editions must build and pass: `cargo build` / `cargo test` AND `cargo build --features pro` / `cargo test --features pro`.
- This is a both-editions feature: NO `cfg(feature = "pro")` anywhere in new code except the pre-existing `form_kill_focus()` call pattern copied from `Ui::flip` (window.rs:1873).
- `src/app.rs` stays pure logic — no OS types, no `windows::` imports.
- Nothing persisted: history is in-memory, per window, per document.
- History records **jumps only** (everything through `App::goto`); `App::flip` (PgUp/PgDn/arrows/Space/wheel) is never recorded.
- Back/forward land at fit-to-window (zoom 1.0, scroll zero) like every navigation.
- Commit after every green test run; commit messages follow the repo's `feat:`/`docs:` style and end with the Claude co-author trailer.

---

### Task 1: History model in app.rs — record in `goto`, `back()`/`forward()`

**Files:**
- Modify: `src/app.rs` (DocState ~line 134-169, `flip`/`goto` ~line 351-371, tests module from line 776)

**Interfaces:**
- Consumes: existing `App::flip(i32) -> bool`, `App::goto(u16) -> bool`, `DocState`, `Mode`.
- Produces (Task 2 relies on these exact signatures):
  - `pub fn back(&mut self) -> bool` on `App` — pops the back stack, navigates, returns `true` iff an entry was consumed.
  - `pub fn forward(&mut self) -> bool` on `App` — mirror of `back`.
  - `App::goto(page: u16) -> bool` — signature unchanged, now records history.

- [ ] **Step 1: Write the failing tests**

Append inside the existing `mod tests` in `src/app.rs`, after the `// --- go-to-page entry ---` section's tests (i.e. before `// --- password mode ---`):

```rust
    // --- page history (jump back / forward) ---

    #[test]
    fn goto_records_history_flip_does_not() {
        let mut a = app_with_doc(10);
        a.flip(1); // adjacent paging: not recorded
        a.flip(1); // current = 2
        assert!(!a.back(), "flips alone never create history");
        assert!(a.goto(7), "jump");
        assert_eq!(a.doc().unwrap().current, 7);
        assert!(a.back(), "back returns to the jump origin");
        assert_eq!(a.doc().unwrap().current, 2);
    }

    #[test]
    fn back_forward_round_trip() {
        let mut a = app_with_doc(10);
        a.goto(5);
        a.goto(9);
        assert!(a.back());
        assert_eq!(a.doc().unwrap().current, 5);
        assert!(a.back());
        assert_eq!(a.doc().unwrap().current, 0);
        assert!(!a.back(), "history exhausted");
        assert!(a.forward());
        assert_eq!(a.doc().unwrap().current, 5);
        assert!(a.forward());
        assert_eq!(a.doc().unwrap().current, 9);
        assert!(!a.forward(), "forward exhausted");
    }

    #[test]
    fn new_jump_truncates_forward() {
        let mut a = app_with_doc(10);
        a.goto(5);
        assert!(a.back()); // at 0, forward = [5]
        assert!(a.goto(3), "new jump");
        assert!(!a.forward(), "forward branch dropped by the new jump");
        assert!(a.back());
        assert_eq!(a.doc().unwrap().current, 0);
    }

    #[test]
    fn jump_to_current_page_is_not_recorded() {
        let mut a = app_with_doc(10);
        a.goto(5);
        assert!(a.back()); // at 0, forward = [5]
        assert!(!a.goto(0), "already there: no navigation");
        assert!(a.forward(), "a same-page 'jump' must not clear the forward branch");
        assert_eq!(a.doc().unwrap().current, 5);
    }

    #[test]
    fn back_cap_drops_oldest() {
        let mut a = app_with_doc(300);
        for i in 1..=150u16 {
            assert!(a.goto(i));
        }
        let mut steps = 0;
        while a.back() {
            steps += 1;
            assert!(steps <= 100, "cap must bound the walk");
        }
        assert_eq!(steps, 100, "exactly HISTORY_CAP entries survive");
        assert_eq!(a.doc().unwrap().current, 50, "the oldest 50 entries were dropped");
    }

    #[test]
    fn empty_history_noops() {
        let mut a = app_with_doc(3);
        assert!(!a.back());
        assert!(!a.forward());
        assert_eq!(a.doc().unwrap().current, 0);
        assert!(!App::new().back(), "no doc: no-op");
        assert!(!App::new().forward(), "no doc: no-op");
    }

    #[test]
    fn back_navigation_resets_zoom_and_is_not_rerecorded() {
        let mut a = app_with_doc(10);
        a.goto(5);
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 2.0);
        assert!(a.back());
        let d = a.doc().unwrap();
        assert_eq!(d.current, 0);
        assert_eq!(d.zoom, 1.0, "back lands at fit");
        assert_eq!(d.scroll, PointF::default());
        assert!(!a.back(), "the back-navigation itself is never recorded");
    }

    #[test]
    fn back_to_manually_revisited_page_still_consumes() {
        let mut a = app_with_doc(10);
        a.goto(5); // back = [0]
        for _ in 0..5 {
            a.flip(-1); // page back to the origin by hand (not recorded)
        }
        assert_eq!(a.doc().unwrap().current, 0);
        a.zoom_by(PointF { x: 500.0, y: 500.0 }, 2.0);
        // The stale entry (page 0 == current) is consumed and the view resets.
        assert!(a.back(), "entry consumed even when it targets the current page");
        assert_eq!(a.doc().unwrap().current, 0);
        assert_eq!(a.doc().unwrap().zoom, 1.0);
        assert!(!a.back(), "stack drained — no jam on stale entries");
    }

    #[test]
    fn goto_entry_commit_records_history() {
        let mut a = app_with_doc(9);
        a.goto_entry_start();
        a.goto_entry_char('7');
        assert!(a.goto_entry_commit()); // 1-based -> index 6
        assert_eq!(a.doc().unwrap().current, 6);
        assert!(a.back());
        assert_eq!(a.doc().unwrap().current, 0);
    }

    #[test]
    fn wheel_edge_flip_is_not_recorded() {
        let mut a = app_with_doc(3);
        assert_eq!(a.wheel(1), WheelOutcome::Flipped);
        assert!(!a.back(), "wheel flips are adjacent paging, not jumps");
    }

    #[test]
    fn history_survives_password_stash_restore() {
        let mut a = app_with_doc(10);
        a.goto(5);
        let prev = match std::mem::replace(&mut a.mode, Mode::Empty) {
            Mode::Doc(d) => Some(Box::new(d)),
            _ => None,
        };
        a.mode =
            Mode::Password { path: PathBuf::from("locked.pdf"), buf: String::new(), prev };
        a.password_cancel();
        assert!(a.back(), "history came back with the stashed doc");
        assert_eq!(a.doc().unwrap().current, 0);
    }

    #[test]
    fn new_document_starts_with_empty_history() {
        let mut a = app_with_doc(10);
        a.goto(5);
        a.doc_loaded(PathBuf::from("y.pdf"), 10, vec![LETTER; 10], vec![], false);
        assert!(!a.back(), "history dies with the document");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib history 2>&1 | tail -20` (and once without a filter: `cargo test --lib 2>&1 | tail -5`)
Expected: compile error — `no method named `back` found for struct `App`` (the tests reference `App::back`/`App::forward` which don't exist yet). A compile failure of the *test code only* counts as the failing state here.

- [ ] **Step 3: Implement the History model**

3a. Add the `History` struct directly above `pub struct DocState` (app.rs ~line 133):

```rust
/// Jump history: pages left via jumps (links, TOC, `G`, Home/End — anything
/// through `App::goto`) — never via adjacent flips. Browser semantics: a new
/// jump after going back drops the forward branch. In-memory only; lives in
/// `DocState`, so it is per-document and survives the password stash like
/// the rest of the view state.
#[derive(Default)]
pub struct History {
    /// Pages jumped away from, oldest first. Capped at `HISTORY_CAP`.
    back: Vec<u16>,
    /// Pages gone back from — forward targets, nearest last. Bounded by
    /// construction: entries only arrive by popping `back`.
    fwd: Vec<u16>,
}

/// Upper bound on remembered jump origins; the oldest entry is dropped.
const HISTORY_CAP: usize = 100;

impl History {
    /// A new jump away from `from` landed on a different page: remember the
    /// origin, drop the forward branch.
    fn record_jump(&mut self, from: u16) {
        self.fwd.clear();
        self.push_back(from);
    }

    /// Capped push onto the back stack (oldest dropped).
    fn push_back(&mut self, page: u16) {
        if self.back.len() >= HISTORY_CAP {
            self.back.remove(0);
        }
        self.back.push(page);
    }
}
```

3b. Add the field to `DocState` (after `goto_entry`, app.rs ~line 150):

```rust
    /// Jump history for back/forward navigation.
    pub history: History,
```

3c. Initialize it in the `DocState` literal inside `doc_loaded` (app.rs ~line 347, after `goto_entry: None,`):

```rust
            goto_entry: None,
            history: History::default(),
```

(This is the ONLY `DocState { .. }` construction site in the codebase — verified by grep.)

3d. Add a `land` helper to `impl DocState` (after `page_pt_rot`, app.rs ~line 168) and use it from `flip` so the post-navigation reset lives in one place:

```rust
    /// Land on `page`: the post-navigation state every page change shares
    /// (fit zoom, zero scroll).
    fn land(&mut self, page: u16) {
        self.current = page;
        self.zoom = 1.0;
        self.scroll = PointF::default();
    }
```

Rewrite the tail of `flip` (app.rs:352-365) to use it — behavior identical:

```rust
    /// Change page by delta; returns true if the page changed. Resets zoom/scroll.
    /// Adjacent paging — never recorded in the jump history.
    pub fn flip(&mut self, delta: i32) -> bool {
        let Some(d) = self.doc_mut() else { return false };
        if d.pages == 0 {
            return false;
        }
        let target = (d.current as i32 + delta).clamp(0, d.pages as i32 - 1) as u16;
        if target == d.current {
            return false;
        }
        d.land(target);
        true
    }
```

3e. Make `goto` record (replace app.rs:367-371):

```rust
    /// Jump to `page` (clamped), recording the departure point in the jump
    /// history — all jump sources (links, TOC, `G` prompt, Home/End) come
    /// through here. Only an actual page change records: a jump to the
    /// current page leaves the history (including the forward branch) alone.
    pub fn goto(&mut self, page: u16) -> bool {
        let Some(d) = self.doc() else { return false };
        let from = d.current;
        let delta = page.min(d.pages.saturating_sub(1)) as i32 - d.current as i32;
        if !self.flip(delta) {
            return false;
        }
        if let Some(d) = self.doc_mut() {
            d.history.record_jump(from);
        }
        true
    }
```

3f. Add `back`/`forward` right after `goto`:

```rust
    /// Return to the most recent jump origin. Consumes the entry even when
    /// it equals the current page (the user may have paged back by hand —
    /// a navigation is a navigation: the view resets, the badge shows, and
    /// the stack can't jam on stale entries). The traversal itself is never
    /// recorded as a jump. Returns true iff an entry was consumed.
    pub fn back(&mut self) -> bool {
        let Some(d) = self.doc_mut() else { return false };
        let Some(target) = d.history.back.pop() else { return false };
        let from = d.current;
        d.history.fwd.push(from);
        // Belt and braces — page counts can't change mid-session.
        let target = target.min(d.pages.saturating_sub(1));
        d.land(target);
        true
    }

    /// Re-do a jump undone by `back`. Mirror of `back`: the departure page
    /// moves onto the back stack WITHOUT clearing the remaining forward
    /// entries (history traversal, not a new jump).
    pub fn forward(&mut self) -> bool {
        let Some(d) = self.doc_mut() else { return false };
        let Some(target) = d.history.fwd.pop() else { return false };
        let from = d.current;
        d.history.push_back(from);
        let target = target.min(d.pages.saturating_sub(1));
        d.land(target);
        true
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test 2>&1 | tail -5`
Expected: all tests pass (the 12 new ones plus every pre-existing test — `flip_resets_zoom_and_scroll`, `goto_clamps_out_of_range`, `goto_entry_flow_commits_clamped_1_based` etc. must stay green).

Run: `cargo test --features pro 2>&1 | tail -5`
Expected: all pass (pro tests too).

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "feat: page-history model — goto records, back/forward traverse

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Input wiring — Alt+arrows, mouse X-buttons, Backspace

**Files:**
- Modify: `src/window.rs` (constants ~line 43-61, `handle()` match ~line 862-935, `on_key` normal section ~line 1094-1177, `impl Ui` helpers near `flip` ~line 1873)

**Interfaces:**
- Consumes (from Task 1): `App::back() -> bool`, `App::forward() -> bool`.
- Produces: `Ui::nav_back()`, `Ui::nav_forward()`, `Ui::modal_active() -> bool` (private helpers; Task 3 documents the user-facing bindings).

This task is the deliberately thin Win32 layer — there is no pure logic to unit-test (the repo's convention: window.rs has no test module). The test cycle is: both editions compile, the full suite stays green, and a manual smoke run.

- [ ] **Step 1: Add the APPCOMMAND constants**

After `pub const CMD_ALWAYS_ON_TOP: usize = 9;` (window.rs:51):

```rust
/// WM_APPCOMMAND commands (winuser.h) — sent by the mouse back/forward
/// X-buttons (DefWindowProc synthesizes them from unhandled X-button
/// clicks, client and non-client alike) and keyboard browser keys.
const APPCOMMAND_BROWSER_BACKWARD: u32 = 1;
const APPCOMMAND_BROWSER_FORWARD: u32 = 2;
```

- [ ] **Step 2: Add the `modal_active` / `nav_back` / `nav_forward` helpers**

In the second `impl Ui` block, directly after `fn flip` (window.rs:1873-1881):

```rust
    /// True while a modal layer owns the keyboard (password prompt, F1
    /// help, go-to-page entry) — history navigation must not fire
    /// underneath. The TOC overlay is deliberately NOT modal here (it
    /// falls through unhandled keys already); Pro typing mode is handled
    /// by nav_back/nav_forward killing form focus, like the wheel does.
    fn modal_active(&self) -> bool {
        matches!(self.app.mode, crate::app::Mode::Password { .. })
            || self.app.help_visible
            || self.app.goto_entry_active()
    }

    /// Alt+Left / Backspace / mouse back: return to the last jump origin.
    fn nav_back(&mut self) {
        if self.modal_active() {
            return;
        }
        #[cfg(feature = "pro")]
        self.form_kill_focus();
        if self.app.back() {
            self.sync_renders();
            self.show_badge();
            self.invalidate();
        }
    }

    /// Alt+Right / Shift+Backspace / mouse forward: re-do the jump.
    fn nav_forward(&mut self) {
        if self.modal_active() {
            return;
        }
        #[cfg(feature = "pro")]
        self.form_kill_focus();
        if self.app.forward() {
            self.sync_renders();
            self.show_badge();
            self.invalidate();
        }
    }
```

- [ ] **Step 3: Add the `WM_SYSKEYDOWN` arm**

In `handle()`, directly after the `WM_KEYDOWN` arm (window.rs:862-865):

```rust
                WM_SYSKEYDOWN => {
                    // Alt+Left / Alt+Right = history back / forward. Bit 29
                    // of lparam is the Alt (context) flag: set for real
                    // Alt+key chords, clear for the F10 family. ONLY the two
                    // arrows are claimed — everything else falls through so
                    // Alt+F4 (close) and Alt+Space (system menu) keep their
                    // DefWindowProc meaning.
                    let alt = (lparam.0 >> 29) & 1 == 1;
                    let vk = wparam.0 as u16;
                    if alt && vk == VK_LEFT.0 {
                        self.nav_back();
                        LRESULT(0)
                    } else if alt && vk == VK_RIGHT.0 {
                        self.nav_forward();
                        LRESULT(0)
                    } else {
                        DefWindowProcW(self.hwnd, msg, wparam, lparam)
                    }
                }
```

- [ ] **Step 4: Add the `WM_APPCOMMAND` arm**

Directly after the new `WM_SYSKEYDOWN` arm:

```rust
                WM_APPCOMMAND => {
                    // Mouse back/forward buttons. Handled here (not
                    // WM_XBUTTONDOWN) because the unzoomed page body
                    // hit-tests as HTCAPTION, so client X-clicks arrive as
                    // non-client messages — DefWindowProc converts BOTH
                    // client and non-client X-button releases into this one
                    // message. Return TRUE (1) when handled, per the docs.
                    let cmd = ((lparam.0 as u32) >> 16) & !0xF000; // GET_APPCOMMAND_LPARAM
                    match cmd {
                        APPCOMMAND_BROWSER_BACKWARD => {
                            self.nav_back();
                            LRESULT(1)
                        }
                        APPCOMMAND_BROWSER_FORWARD => {
                            self.nav_forward();
                            LRESULT(1)
                        }
                        _ => DefWindowProcW(self.hwnd, msg, wparam, lparam),
                    }
                }
```

- [ ] **Step 5: Add the Backspace arm in `on_key`'s normal section**

After the `VK_END` arm (window.rs:1123-1130) and before the `0x30` reset-zoom arm:

```rust
            _ if vk == VK_BACK.0 && !ctrl => {
                // Backspace = back, Shift+Backspace = forward (browser
                // reflexes; Ctrl+Backspace stays free). The modal layers
                // above already own Backspace where it means "delete"
                // (go-to-page entry, password prompt, Pro form fields).
                if shift {
                    self.nav_forward();
                } else {
                    self.nav_back();
                }
            }
```

Note: `VK_BACK` is already imported (window.rs:27); `ctrl`/`shift` are already computed at window.rs:1092-1093. No import changes needed anywhere in this task — `WM_SYSKEYDOWN` and `WM_APPCOMMAND` come in through the existing `WindowsAndMessaging::*` glob import (window.rs:39).

- [ ] **Step 6: Build and test both editions**

Run: `cargo build 2>&1 | tail -3 && cargo test 2>&1 | tail -3`
Expected: clean build, all tests pass.

Run: `cargo build --features pro 2>&1 | tail -3 && cargo test --features pro 2>&1 | tail -3`
Expected: clean build, all tests pass.

Also confirm zero warnings from the new code in both builds (the repo builds warning-free).

- [ ] **Step 7: Commit**

```bash
git add src/window.rs
git commit -m "feat: back/forward input — Alt+arrows, X-buttons, Backspace

WM_SYSKEYDOWN claims only genuine Alt+Left/Right (Alt+F4 and Alt+Space
untouched); WM_APPCOMMAND covers the mouse buttons over the HTCAPTION
page body; Backspace/Shift+Backspace in on_key behind the modal layers.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Documentation — README, F1 help, manual tests, TODO

**Files:**
- Modify: `README.md` (input table, lines 54-78)
- Modify: `src/d2d.rs` (`HELP_ROWS`, lines 17-38)
- Modify: `docs/manual-tests.md` (new group after the Document section)
- Modify: `TODO.md` (mark the item done)

**Interfaces:**
- Consumes: the bindings shipped in Task 2 (Alt+←/→, Backspace/Shift+Backspace, mouse back/forward).
- Produces: user-facing docs; no code contracts.

- [ ] **Step 1: README input table**

In `README.md`, insert two rows after the `| G, then a number, Enter | Go to a page |` row (line 61):

```markdown
| Alt+← / Backspace / mouse back | Back to where you jumped from |
| Alt+→ / Shift+Backspace / mouse forward | Forward again (re-do the jump) |
```

- [ ] **Step 2: F1 help overlay**

In `src/d2d.rs`, insert two rows in `HELP_ROWS` after the `("G, then a number, Enter", "Go to a page"),` row (line 23) — wording mirrors the README (the comment above `HELP_ROWS` demands they never drift):

```rust
    ("Alt+\u{2190} / Backspace / mouse back", "Back to where you jumped from"),
    ("Alt+\u{2192} / Shift+Backspace / mouse fwd", "Forward again (re-do the jump)"),
```

- [ ] **Step 3: Manual regression checklist**

In `docs/manual-tests.md`, insert a new group between the `Document:` block and the `Form filling (Pro)` block:

```markdown
Page history (both editions):
- [ ] Click an internal link, then Alt+Left: returns to the origin page; Alt+Right re-does the jump
- [ ] TOC jump, then the mouse back button: returns — including clicks over the draggable (unzoomed) page body
- [ ] G-goto and Home/End create back stops; PgUp/PgDn/wheel paging never does
- [ ] Backspace = back, Shift+Backspace = forward; in the G prompt Backspace still deletes a digit
- [ ] Pro: Backspace inside a focused form field deletes text (never navigates); Alt+Left while typing leaves the field and navigates
- [ ] Alt+F4 still closes (prompting if dirty); Alt+Space still opens the system menu
- [ ] Open another document: history is empty (Backspace does nothing)
```

- [ ] **Step 4: TODO.md**

Change the page-history line to done:

```markdown
- [x] Page history — let users navigate back through the page view history
```

- [ ] **Step 5: Build check (d2d.rs changed) and commit**

Run: `cargo build 2>&1 | tail -3 && cargo build --features pro 2>&1 | tail -3`
Expected: both clean.

```bash
git add README.md src/d2d.rs docs/manual-tests.md TODO.md
git commit -m "docs: page history — README, F1 help, manual tests

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Final verification (both editions, live app)

**Files:** none modified — verification only.

**Interfaces:**
- Consumes: the complete feature from Tasks 1-3.
- Produces: evidence the feature works end-to-end.

- [ ] **Step 1: Full suite, both editions**

Run: `cargo test 2>&1 | tail -5 && cargo test --features pro 2>&1 | tail -5`
Expected: everything green in both.

- [ ] **Step 2: Live smoke test**

Build and launch against the form fixture (multi-page; the checked-in
fixture has no outline/links, so drive jumps with `G` and Home/End):

```powershell
cargo build --features pro
./target/debug/pppdf.exe tests/fixtures/form.pdf
```

Manually verify at minimum (from the new manual-tests group):
1. Press `G`, `3`, Enter (jump to page 3) → Alt+Left returns to page 1 → Alt+Right goes to page 3 again; the page badge shows on each.
2. Backspace = back; while the `G` prompt is open, Backspace deletes the digit instead.
3. PgDn a few times → Backspace does NOT walk through those flips.
4. Pro build: click a text field, press Backspace (edits text), Alt+Left (leaves field, navigates if history exists).
5. Alt+F4 closes; Alt+Space opens the system menu.

If any check fails: STOP, fix, re-run the suite, amend nothing — add a fix commit.

- [ ] **Step 3: Spec conformance note**

If implementation deviated from the spec anywhere, append an "Implementation notes" section to `docs/superpowers/specs/2026-07-10-page-history-design.md` documenting each deviation and why (repo convention — see the form-filling spec). Commit as `docs: page history spec conformance notes` (with the co-author trailer) if anything was added.

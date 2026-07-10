# Page history — design

Date: 2026-07-10 · Status: approved
Scope: back/forward navigation through the page *jump* history, both
editions. In-memory, per window, per document — nothing persisted.

## Goal

A reader follows an internal link (or a TOC entry, or a `G` goto, or
Home/End), reads the target, and presses Alt+Left / Backspace / the mouse
back button to return to where they were. Alt+Right / Shift+Backspace /
the mouse forward button re-does the jump. Adjacent page turning
(PgUp/PgDn/arrows/Space/wheel) never pollutes the history.

## Decisions (settled in brainstorming)

- **Jumps only**: history records jumps — link clicks, TOC jumps, `G`
  goto, Home/End — not adjacent flips. This matches reading flow
  ("follow a link, jump back") and how SumatraPDF behaves.
- **Back + forward** with browser semantics: a new jump after going back
  truncates the forward history.
- **Page only**: back/forward land at fit-to-window. Every navigation in
  pppdf resets zoom/scroll/rotation (`App::flip`, src/app.rs); history
  keeps that invariant.
- **Volatile**: lives in `DocState`, dies with the document. The app
  keeps no config (viewer spec's no-persistence rule stands).

## Approach (decided)

Record inside `App::goto`. The codebase already separates the two
navigation kinds: adjacent paging goes through `App::flip(±1)`
(src/app.rs:352); every jump goes through `App::goto(page)`
(src/app.rs:367 — called by follow_link, TOC activate, goto-entry
commit, Home/End). So recording in `goto` gives jump-only history with
zero call-site changes, and new jump sources record by default.

Rejected: an explicit `App::jump()` API beside a silent `goto` (future
jump sources calling `goto` would silently lose history — the worse
failure mode); UI-layer recording in window.rs (breaks the "app.rs is
pure, window-free, unit-testable logic" rule and needs manual lifecycle
management).

## Data model

A small pure struct in `src/app.rs`, two stacks of 0-based `u16` page
indices (pages are `u16` everywhere, cf. `LinkTarget::Page`):

```rust
struct History {
    back: Vec<u16>, // pages jumped away from, oldest first
    fwd:  Vec<u16>, // pages gone back from (re-jump targets)
}
```

`DocState` gains `history: History`, empty at `doc_loaded`. Living in
`DocState` makes lifecycle automatic: fresh per document, gone on close,
survives the password-prompt stash/restore (`Mode::Password { prev }`)
for free.

Rules — history mutates only when the current page actually changes:

- Jump from page A to B, A ≠ B: push A onto `back`, clear `fwd`.
- Jump to the current page: no history change (no entry, `fwd` kept).
- `back()`: pop `back` → target; push current onto `fwd`; navigate.
- `forward()`: pop `fwd` → target; push current onto `back` (without
  clearing `fwd`); navigate.
- A popped target may equal the current page (the user paged back to
  their origin manually before pressing back). The entry is still
  consumed and the navigation still happens — a navigation is a
  navigation: the view resets and the badge shows. This keeps the
  stacks from jamming on stale entries.
- `back` capped at 100 entries, oldest dropped. `fwd` is bounded by
  construction (entries only move over from `back`).

## App API (src/app.rs)

- `App::goto(page)` — records: if the clamped target differs from
  `current`, push `current` onto `back`, clear `fwd`, then navigate as
  today. Callers unchanged.
- `App::back() -> bool` / `App::forward() -> bool` — pop the respective
  stack and navigate via a non-recording internal path (the shared flip
  internals, so zoom/scroll/rot reset and badge semantics stay
  identical). Return `true` iff an entry was consumed — the UI
  re-renders and badges on `true`. Empty stack → `false`, no state
  change.
- `App::flip(±1)` and wheel-edge flips (`App::wheel`) — untouched, never
  record.
- Back targets are clamped like any `goto` target (belt and braces —
  page counts cannot change mid-session).

## Input wiring (src/window.rs)

| Input | Mechanism |
|---|---|
| Alt+Left / Alt+Right | New `WM_SYSKEYDOWN` arm; handles only VK_LEFT/VK_RIGHT with Alt down, everything else falls through to `DefWindowProcW` (Alt+F4, Alt+Space unaffected). Swallowed while a modal layer is active (password prompt, F1 help, goto entry). |
| Mouse back/forward (X-buttons) | New `WM_APPCOMMAND` arm for `APPCOMMAND_BROWSER_BACKWARD/FORWARD`, returning handled. `DefWindowProc` synthesizes `WM_APPCOMMAND` from unhandled X-button clicks — client *and* non-client (the unzoomed page body hit-tests as `HTCAPTION`) — so this single arm covers all click paths. |
| Backspace / Shift+Backspace | back / forward in `on_key`'s normal section. The modal layers earlier in `on_key` keep Backspace's meaning in the goto-entry prompt, the password prompt (WM_CHAR), and Pro form fields. Ignored when Ctrl is held (reserved). |

New `Ui::nav_back()` / `Ui::nav_forward()` wrappers mirror the existing
`Ui::flip` ritual: kill Pro form focus → `app.back()/forward()` → if
changed, `sync_renders()` + `show_badge()` + `invalidate()`. An
empty-history no-op is silent — no toast, per the minimal-transient-UI
philosophy (the fading page badge is the only feedback).

## Edge cases

- Pro typing mode: Alt+arrows and mouse X-buttons kill form focus, then
  navigate (same policy as the mouse wheel today). Backspace inside a focused field
  arrives via the form layer / WM_CHAR and edits text, never history.
- Double-click on a link fires exactly one navigation (existing manual
  test) → exactly one history entry.
- `doc_loaded` opens at page 0 with empty history; the initial position
  is not an entry (there is nothing to go back *to* until the first
  jump).
- TOC overlay open: not modal for history. Alt+Left/Right and mouse
  X-buttons navigate normally; the overlay stays open (plain Left/Right
  keep their collapse/expand meaning inside the overlay).
- Both editions: shared code only. No `cfg(feature = "pro")` anywhere in
  this feature beyond the pre-existing form-focus kill inside the Ui
  navigation ritual.

## UI & docs

No new UI. Successful back/forward shows the existing page badge
("N / total"). Documentation touch points:

- README shortcut table: new rows for Alt+Left/Right, Backspace /
  Shift+Backspace, mouse back/forward buttons.
- F1 help overlay text (src/d2d.rs) gets the same additions.
- docs/manual-tests.md gets a "Page history" group.

## Testing

- **Unit** (`#[cfg(test)]` in src/app.rs, pure logic, no window):
  - `goto` records; `flip` and wheel-edge flip don't.
  - back → forward round-trip restores both positions.
  - New jump after back clears `fwd`.
  - Jump to current page: no entry, `fwd` intact.
  - Cap: 101 jumps → oldest entry evicted, len == 100.
  - back/forward on empty stacks: `false`, state unchanged.
  - goto-entry commit (`G` prompt) records.
  - back to a popped target equal to the current page: entry consumed,
    returns `true`.
  - History survives the password-prompt stash/restore of `DocState`.
- **Integration**: none needed — no PDFium interaction.
- **Manual** (docs/manual-tests.md additions): internal link then
  Alt+Left returns / Alt+Right re-jumps; TOC jump then mouse back
  returns; Backspace = back on a plain doc; Backspace still deletes in
  the `G` prompt and in a Pro form field; Alt+F4 still closes; PgUp/PgDn
  paging never creates back stops.
- **Both editions**: `cargo build` and `cargo test`, each with and
  without `--features pro`.

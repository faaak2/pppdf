# Double-click drives the F11 fill cycle — design

Date: 2026-07-20. Status: approved design, pre-implementation.

A double-click on the plain page background advances the same three-stage
window cycle as F11 (normal → height-fit → fill → normal). Double-click
keeps its current "back to fit" meaning whenever the page is zoomed. Both
editions; window management is not Pro.

## Decisions taken (with the user)

- **Only when the page is not zoomed.** Zoomed, double-click still resets
  zoom to fit, exactly as today.
- **No existing handler may be overridden** — form fields, links, the find
  bar, the TOC panel and the help overlay keep their current double-click
  behavior.
- **Empty window (no PDF): yes, it cycles**, matching F11 there
  (two-state fill ↔ restore, since there is no page aspect to fit).
- **An open prompt is dismissed only.** If the go-to-page prompt (G) or the
  bookmark-name prompt (Ctrl+B, Pro) is open, the double-click closes it and
  does **not** resize; a second double-click then cycles.

## Where it hooks in

The `HTCAPTION` arm of **`WM_NCLBUTTONDBLCLK`** (window.rs:419) — and
nothing else. The client-path `WM_LBUTTONDBLCLK` handler (window.rs:756) is
left byte-identical, so every carve-out it already owns survives: form-field
input (Pro word-selection and checkbox clicks), the find-bar swallow, link
navigation, and the reset-to-fit that a zoomed double-click performs.

**Why that placement needs no new guards.** `hit_test` returns `HTCAPTION`
only when `wants_client_mouse` (window.rs:1207) is false, which requires all
of: help overlay not visible, pointer not over the TOC panel, zoom at 1.0,
not over a link, not over a form field, not on the find bar. Resize grips
return their own `HTLEFT`/`HTTOP`/… codes and never reach this arm. "The
plain, unzoomed page background" is therefore already computed by existing
routing; the feature is a consequence of it, not a new special case.

## Behavior of the arm

In order:

1. If the go-to-page or bookmark-name prompt is open: dismiss it, repaint,
   return. No cycle. (`App::goto_entry_active()` is unconditional;
   `App::bookmark_entry_active()` exists only under `#[cfg(feature =
   "pro")]`, so the free build tests the goto prompt alone — the same
   cfg seam `modal_active` already uses.)
2. Else if `App::reset_zoom()` reports a change: sync renders, repaint.
   (`wants_client_mouse` treats zoom as unzoomed within a `1e-4` epsilon
   while `reset_zoom` compares against exactly 1.0, so a hair of residual
   zoom still resets rather than resizing — this ordering preserves today's
   behavior in that hairline case.)
3. Else: `toggle_fill()` — advance the F11 cycle one stage.

The arm keeps returning `LRESULT(0)` in every path, so DefWindowProc can
never maximize the window on a caption double-click.

`toggle_fill` itself is unchanged: same `FillStage` state, same
`saved_rect`, same monitor-info failure handling. The cycle is shared —
pressing F11 once and then double-clicking advances to the next stage, it
does not restart.

## Out of scope (YAGNI)

A separate double-click-only cycle state, double-click on the TOC panel or
help overlay doing anything new, a configurable gesture, double-click to
maximize natively.

## Testing

The change is OS-shim message routing, so it carries **no new pure logic
and no new unit tests** — the gate is both editions building and both full
suites staying green, plus manual rows in `docs/manual-tests.md`:

- Double-click the page background cycles height-fit → fill → restore, and
  interleaves with F11 (one press, one double-click, etc.).
- Double-click while zoomed still resets to fit and never resizes the
  window.
- Double-click over a link (navigates), a form field (Pro: word-select /
  checkbox), the find bar, the TOC panel, and the help overlay (dismisses)
  behaves as before, with no window resize.
- Double-click with the G prompt or the Ctrl+B prompt open dismisses it
  without resizing; the next double-click cycles.
- Empty window: double-click fills, again restores.

## Documentation touch-list

- F1 help rows (`d2d.rs`): split the two meanings —
  `("0 · double-click when zoomed", "Back to fit")` and
  `("F11 · double-click the page", "Fit screen height / fill / restore")`.
- README key map: the same two rows, matching wording.

# F11 three-stage fill cycle — design

Date: 2026-07-20. Status: approved design, pre-implementation.

F11 grows a middle stage: before filling the work area, it first sizes the
window to the screen's height at the loaded PDF's aspect ratio, docked to the
nearest screen edge. Both editions; window management is not Pro.

## Decisions taken (with the user)

- **Cycle shape: three stages** — `normal → height-fit → fill → normal`.
  Every state is reachable by pressing F11 again; the third press restores
  the rect saved at the first.
- **Height basis: the work area** (screen minus taskbar), exactly like
  docking left/right and like the current F11 fill. Not the full monitor
  resolution.
- **Placement: snap to the nearest edge** — flush against the left or right
  work-area edge, whichever the window's center is closer to.

## Behavior

1. **Height-fit** (new). Saves the current window rect, then sizes the
   window to the work-area height with
   `width = height × current page aspect` (rotated: quarter-turn rotations
   swap the page dimensions). Width is clamped to the work-area width —
   an ultra-wide page degenerates into today's fill, which is fine because
   the viewer letterboxes internally. A tall-thin page needs no width
   floor in the math: the existing `WM_GETMINMAXINFO` guard
   (`MIN_SIZE_DIP`) is enforced by `DefWindowProc` on `SetWindowPos`, so
   the window can't become a sliver. The window sits flush at the nearest
   work-area edge, top-aligned.
2. **Fill.** Unchanged from today: work area edge to edge, taskbar visible,
   `saved_rect` kept.
3. **Restore.** Unchanged from today: back to the saved rect.

Edge rules:

- **No document / zero pages:** no aspect exists, so F11 keeps today's
  two-state behavior — fill ↔ restore. The height-fit stage is skipped
  entirely (the cycle enum still advances `Normal → Filled` directly).
- Manual moves/resizes between presses do **not** reset the cycle (same
  semantics as today: F11 from the fill stage always restores the saved
  rect, wherever the window is now).
- Page turns, rotation, or doc loads while in height-fit do **not**
  live-refit. Press F11 around the cycle to re-fit. Loading a new document
  does not reset the cycle state either — the enum only changes on F11.
- Multi-monitor: everything derives from `MONITOR_DEFAULTTONEAREST` at
  press time, as today. The monitor is re-queried on every press, so
  moving the window between monitors mid-cycle fills the monitor it is
  actually on.
- Native maximize (Win+Up) stays independent of the cycle, as today.

## Mechanics

- `Ui` gains `fill_stage: FillStage` (`enum FillStage { Normal, HeightFit,
  Filled }`, default `Normal`), alongside the existing
  `saved_rect: Option<RECT>` which keeps its restore-data role.
  `saved_rect` is written once on the `Normal →` transition and taken on
  the `→ Normal` transition.
- **Pure math in `layout.rs`** (house style: no OS types there):

  ```rust
  /// Height-fit rect: work-area height, width = height × aspect (clamped
  /// to work width), flush at the work-area edge nearest to cur_center_x.
  /// Tuples are (left, top, right, bottom) in physical px.
  pub fn height_fit_rect(
      work: (i32, i32, i32, i32),
      aspect: f32,          // page w/h, rotation already applied, > 0
      cur_center_x: i32,    // current window center, physical px
  ) -> (i32, i32, i32, i32)
  ```

- `App` gains a small public accessor `page_aspect(&self) -> Option<f32>`:
  rotated width/height of the current page (`DocState::page_pt_rot` is
  private today; the accessor wraps it), `None` when no document or zero
  pages.
- `toggle_fill` (window.rs) becomes a three-arm match on `fill_stage`,
  converting `RECT` ↔ tuples at the boundary. The `Normal` arm asks
  `app.page_aspect()`; `None` routes straight to the fill arm's logic.

## Out of scope (YAGNI)

Live re-fit on page turn/rotation, remembering the stage across sessions,
a distinct shortcut per stage, Esc exiting the cycle, animating the
transitions.

## Testing

- `layout.rs` unit tests for `height_fit_rect`: portrait aspect on a
  1080p work area; landscape aspect wide enough to clamp; nearest-edge
  left vs right (center exactly at the midpoint picks left); work areas
  with non-zero origin (taskbar on the left/top, second monitor at
  negative coords); degenerate aspect ≤ 0 treated as clamp-to-work-width.
- `app.rs` tests for `page_aspect`: portrait page, rotated page swaps,
  no doc → `None`, zero-page doc → `None`.
- The cycle's arm logic lives in the OS shim, covered by
  `docs/manual-tests.md` rows: full cycle on a portrait PDF; nearest-edge
  snap both directions; ultra-wide page clamp; no-doc two-state fallback;
  taskbar visible in height-fit and fill.
- Both editions build and test green (`cargo build`/`cargo test` ±
  `--features pro`).

## Documentation touch-list

- F1 help row (`d2d.rs`): `("F11", "Fit screen height / fill / restore")`.
- README F11 row to match.
- `docs/manual-tests.md`: replace the existing F11 row(s) with the cycle
  rows above.

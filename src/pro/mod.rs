//! Paid-edition features. This whole tree compiles only with
//! `--features pro` (gated once, in lib.rs) — the free binary does not
//! contain it. Keep shared code free of pro logic; pro code calls *into*
//! the shared modules, never the other way around, so the free build can
//! never grow an accidental dependency on this tree.
//!
//! Planned home for: form-fillable elements, user-created labels and
//! input fields. (Scaffold only — features land here next.)

pub mod forms;

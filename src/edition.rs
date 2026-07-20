//! Edition gate: one codebase, two builds. `cargo build` is the free viewer;
//! `cargo build --features pro` compiles in the paid features. Everything
//! user-visible that differs between the editions is named here, so the rest
//! of the code never tests the feature flag for branding.
//!
//! Pro *functionality* lives in the `pro` module (gated at the module
//! boundary), not behind scattered `cfg`s in shared code.

use windows::core::{w, PCWSTR};

/// True in `--features pro` builds. Prefer gating whole modules with
/// `#[cfg(feature = "pro")]`; use this only for small runtime branches
/// (labels, menu entries) where a module split would be overkill.
pub const IS_PRO: bool = cfg!(feature = "pro");

/// Product name for window title, dialogs, and log-ish text.
pub const NAME: &str = if IS_PRO { "pppdf Pro" } else { "pppdf" };

/// Which edition this build is, spelled out for the About box. `NAME` already
/// carries "Pro", but only Pro's name does — a free user reading "pppdf" sees
/// the *absence* of a word, which is not the same as being told what they have.
pub const EDITION: &str = if IS_PRO { "Pro edition" } else { "Free edition" };

/// `NAME` as a compile-time UTF-16 string for Win32 calls.
/// Kept in sync with `NAME` by the test below.
#[cfg(feature = "pro")]
pub const NAME_W: PCWSTR = w!("pppdf Pro");
#[cfg(not(feature = "pro"))]
pub const NAME_W: PCWSTR = w!("pppdf");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_tracks_feature() {
        assert_eq!(IS_PRO, cfg!(feature = "pro"));
        assert_eq!(NAME.ends_with(" Pro"), IS_PRO);
    }

    #[test]
    fn edition_label_states_which_build_this_is() {
        assert_eq!(EDITION == "Pro edition", IS_PRO);
        // Both editions must say something; neither may go unlabeled.
        assert!(EDITION.ends_with(" edition"), "{EDITION}");
    }

    #[test]
    fn wide_name_matches_narrow_name() {
        // NAME and NAME_W are declared separately (w! needs a literal);
        // this is the guard that keeps them from drifting apart.
        let wide = unsafe { NAME_W.to_string() }.expect("NAME_W is valid UTF-16");
        assert_eq!(wide, NAME);
    }
}

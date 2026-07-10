//! Library facade so integration tests can reach the internals.
pub mod app;
pub mod d2d;
pub mod dbglog;
pub mod edition;
pub mod layout;
pub mod overlay;
pub mod pdf;
#[cfg(feature = "pro")]
pub mod pro;
pub mod register;
pub mod search;
pub mod types;
pub mod window;

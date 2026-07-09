//! Opt-in diagnostic trace log. Disabled (zero side effects beyond one env
//! read) unless the PPPDF_LOG environment variable is set: `1` logs to
//! %TEMP%\pppdf-debug.log, any other value is used as the log file path.
//! Used to trace the input -> form engine -> render -> paint pipeline on
//! machines where a problem can't be reproduced under the test suite.

use std::fs::File;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static LOG: OnceLock<Option<(Mutex<File>, Instant)>> = OnceLock::new();

fn slot() -> Option<&'static (Mutex<File>, Instant)> {
    LOG.get_or_init(|| {
        let v = std::env::var("PPPDF_LOG").ok()?;
        let path = if v == "1" {
            std::env::temp_dir().join("pppdf-debug.log")
        } else {
            std::path::PathBuf::from(v)
        };
        let f = File::create(path).ok()?;
        Some((Mutex::new(f), Instant::now()))
    })
    .as_ref()
}

pub fn enabled() -> bool {
    slot().is_some()
}

pub fn log(args: std::fmt::Arguments) {
    if let Some((f, t0)) = slot() {
        if let Ok(mut f) = f.lock() {
            let ms = t0.elapsed().as_micros() as f64 / 1000.0;
            let _ = writeln!(f, "[{ms:10.3}] {args}");
            let _ = f.flush(); // a crash must not eat the tail of the trace
        }
    }
}

/// `dlog!("fmt", args...)` — formats lazily; no-op unless PPPDF_LOG is set.
#[macro_export]
macro_rules! dlog {
    ($($arg:tt)*) => {
        if $crate::dbglog::enabled() {
            $crate::dbglog::log(format_args!($($arg)*));
        }
    };
}

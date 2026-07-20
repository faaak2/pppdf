//! Paid-edition features. This whole tree compiles only with
//! `--features pro` (gated once, in lib.rs) — the free binary does not
//! contain it. Keep shared code free of pro logic; pro code calls *into*
//! the shared modules, never the other way around, so the free build can
//! never grow an accidental dependency on this tree.
//!
//! Planned home for: form-fillable elements, user-created labels and
//! input fields. (Scaffold only — features land here next.)

pub mod forms;

pub mod bookmarks;

/// Write `bytes` atomically over `path`: temp file in the target's own
/// directory, then rename with antivirus-lock retries (same idiom as
/// pdf.rs's dll extraction). The original is untouched on any failure.
/// Shared by the form save and the bookmark save.
pub(crate) fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("path has no parent directory")?;
    let tmp = dir.join(format!(".pppdf-save-{}.tmp", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, bytes) {
        // Don't leave a partial temp file behind (disk full etc.).
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    let mut last = None;
    for attempt in 0..5u64 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(20 * (attempt + 1)));
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(last.map(|e| e.to_string()).unwrap_or_else(|| "rename failed".into()))
}

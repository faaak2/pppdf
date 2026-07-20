//! Pro custom bookmarks: writes the `pppdf.bookmarks` attachment on the
//! PDF thread. The READ side (pdf::describe) is shared code — the free
//! build displays bookmarks; only creating/removing them is Pro.

use crate::bookmarks::{serialize, ATTACHMENT_NAME};
use crate::types::Bookmark;
use pdfium_render::prelude::*;
use std::path::Path;

/// Replace (or, with an empty set, remove) the bookmarks attachment on
/// one in-memory document. The caller decides what gets saved where.
fn set_attachment(doc: &mut PdfDocument, bookmarks: &[Bookmark]) -> Result<(), String> {
    let atts = doc.attachments_mut();
    // Delete-then-create is not transactional: if the create below fails
    // after the delete, the LIVE doc is left attachment-less while DocState
    // still holds the set — a later Ctrl+S would persist without bookmarks
    // until the next successful toggle rewrites them (self-healing, and the
    // attachment API failing on an open doc is an extreme edge).
    // Delete every existing pppdf.bookmarks entry, scanning backwards —
    // deletion shifts the indices above the deleted slot.
    let mut i = atts.len();
    while i > 0 {
        i -= 1;
        let name = match atts.get(i) {
            Ok(a) => a.name(),
            Err(_) => continue,
        };
        if name == ATTACHMENT_NAME {
            atts.delete_at_index(i).map_err(|e| format!("delete attachment: {e:?}"))?;
        }
    }
    if !bookmarks.is_empty() {
        atts.create_attachment_from_bytes(ATTACHMENT_NAME, &serialize(bookmarks))
            .map_err(|e| format!("create attachment: {e:?}"))?;
    }
    Ok(())
}

/// The double-write behind auto-save-on-toggle (see the 2026-07-20 spec):
///
/// 1. Update the attachment on the LIVE document (in memory only) so any
///    later Ctrl+S form save carries the current bookmarks — the two save
///    paths can never wipe each other.
/// 2. Shadow-load the ON-DISK bytes, update the attachment there, save
///    the shadow atomically. The disk copy lacks uncommitted form edits
///    by definition — this is what enforces "save only bookmarks".
///
/// The shadow document gets no FormEngine: pdfium-render's own form-fill
/// init on it installs no callbacks into our thread-local state, and it
/// is dropped before returning.
pub fn save_bookmarks(
    pdfium: &Pdfium,
    live: &mut PdfDocument<'static>,
    path: &Path,
    password: Option<&str>,
    bookmarks: &[Bookmark],
) -> Result<(), String> {
    set_attachment(live, bookmarks)?;
    let disk = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut shadow = pdfium
        .load_pdf_from_byte_vec(disk, password)
        .map_err(|e| format!("reload from disk: {e:?}"))?;
    set_attachment(&mut shadow, bookmarks)?;
    let out = shadow.save_to_bytes().map_err(|e| format!("{e:?}"))?;
    super::atomic_write(path, &out)
}

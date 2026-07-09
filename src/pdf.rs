//! The PDF thread: owns ALL pdfium state; speaks PdfCmd in, PdfReply out.

use crate::layout::{RectF, SizeU};
use crate::types::{LinkTarget, OutlineItem, PageLink, PdfCmd, PdfReply};
use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::OnceLock;

pub fn spawn(reply_tx: Sender<PdfReply>, wake: impl Fn() + Send + 'static) -> Sender<PdfCmd> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("pdf".into())
        .spawn(move || run(cmd_rx, reply_tx, wake))
        .expect("spawn pdf thread");
    cmd_tx
}

/// pdfium.dll compiled into the exe, so a shipped `pppdf.exe` is a single
/// self-contained file. `fetch-pdfium.ps1` drops the dll at assets/pdfium.dll
/// for this include; rustc re-runs pdf.rs when those bytes change.
const PDFIUM_DLL: &[u8] = include_bytes!("../assets/pdfium.dll");

/// Tags each temp file uniquely within this process, so even two threads
/// extracting at once (they never do today — pdfium binds once per process —
/// but the guarantee is cheap) can't collide on the same temp path.
static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Locate pdfium and bind to it, in priority order:
///   1. pdfium.dll beside the exe, or up to two ancestors up — covers dev
///      runs (`cargo run` / `cargo test`, where the dll sits in target\debug
///      but the test exe is under target\debug\deps) and lets a user drop a
///      newer dll next to the exe to override the bundled one. A dll that is
///      present but won't load (wrong architecture, truncated, locked) does
///      NOT abort startup — we fall through to the embedded copy.
///   2. the copy embedded in this exe, unpacked once to %LOCALAPPDATA%\pppdf
///      — this is the path a shipped single-file pppdf.exe takes.
///   3. any pdfium already on the system search path.
fn bind_pdfium() -> Result<Pdfium, PdfiumError> {
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(Path::to_path_buf);
        for _ in 0..3 {
            let Some(d) = dir else { break };
            let candidate = Pdfium::pdfium_platform_library_name_at_path(&d);
            if candidate.exists() {
                if let Ok(bindings) = Pdfium::bind_to_library(&candidate) {
                    return Ok(Pdfium::new(bindings));
                }
                break; // found one, but it won't load — try the embedded copy
            }
            dir = d.parent().map(Path::to_path_buf);
        }
    }
    if let Some(pdfium) = bind_embedded() {
        return Ok(pdfium);
    }
    Ok(Pdfium::new(Pdfium::bind_to_system_library()?))
}

/// Unpack the embedded pdfium.dll to a per-user, per-version file and bind to
/// it. If binding a copy that was already on disk fails (e.g. it was corrupted
/// at rest without changing length), force a fresh extraction and try once
/// more so a damaged cache self-heals instead of wedging every launch. Returns
/// None if nothing binds, leaving the caller to try the system library.
fn bind_embedded() -> Option<Pdfium> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("pppdf");
    let name = format!("pdfium-{}.dll", env!("CARGO_PKG_VERSION"));
    for force in [false, true] {
        if let Ok(path) = extract_pdfium_to(&dir, PDFIUM_DLL, &name, force) {
            if let Ok(bindings) = Pdfium::bind_to_library(&path) {
                return Some(Pdfium::new(bindings));
            }
        }
    }
    None
}

/// Write `bytes` to `dir\name` and return that path. Unless `force`, an
/// existing copy of the right length is reused as-is (the version-stamped name
/// makes matching length imply matching content). The write goes to a unique
/// temp file that is renamed into place, so a concurrent process never sees a
/// half-written dll. Antivirus tends to open a freshly written executable to
/// scan it, briefly locking it so the rename fails with a sharing violation;
/// we retry a few times before giving up.
fn extract_pdfium_to(dir: &Path, bytes: &[u8], name: &str, force: bool) -> std::io::Result<PathBuf> {
    let final_path = dir.join(name);
    if !force {
        if let Ok(meta) = std::fs::metadata(&final_path) {
            if meta.len() == bytes.len() as u64 {
                return Ok(final_path);
            }
        }
    }
    std::fs::create_dir_all(dir)?;
    let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!("{name}.{}.{seq}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    let mut last_err = None;
    for attempt in 0..5u64 {
        match std::fs::rename(&tmp, &final_path) {
            Ok(()) => return Ok(final_path),
            // Another process already placed a good copy there (its copy may be
            // loaded and locked, which blocks our replace) — accept it, unless
            // we were explicitly asked to overwrite a known-bad file.
            Err(_) if !force && final_path.exists() => {
                let _ = std::fs::remove_file(&tmp);
                return Ok(final_path);
            }
            // Likely a transient AV/indexer lock on the temp file; back off and
            // retry rather than failing the only bind path a shipped exe has.
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(20 * (attempt + 1)));
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(last_err.unwrap_or_else(|| std::io::Error::other("rename failed")))
}

/// Process-wide binding. pdfium-render 0.9.x bindings are a global singleton
/// (a second bind returns PdfiumLibraryBindingsAlreadyInitialized), so bind
/// exactly once and share the leaked instance — this also lets multiple test
/// functions in one test process each spawn a PDF thread safely.
fn pdfium() -> Result<&'static Pdfium, String> {
    static PDFIUM: OnceLock<Result<&'static Pdfium, String>> = OnceLock::new();
    PDFIUM
        .get_or_init(|| {
            bind_pdfium()
                .map(|p| &*Box::leak(Box::new(p)))
                .map_err(|e| format!("{e:?}"))
        })
        .clone()
}

fn run(cmd_rx: Receiver<PdfCmd>, reply_tx: Sender<PdfReply>, wake: impl Fn()) {
    let pdfium: &'static Pdfium = match pdfium() {
        Ok(p) => p,
        Err(e) => {
            let _ = reply_tx.send(PdfReply::LoadFailed {
                path: PathBuf::new(),
                reason: format!("pdfium.dll could not be loaded: {e}"),
            });
            wake();
            return;
        }
    };
    let mut doc: Option<PdfDocument<'static>> = None;
    // A command consumed by the preemption check below, carried into the next iteration.
    let mut carry: Option<PdfCmd> = None;

    'outer: loop {
        let first = match carry.take() {
            Some(c) => c,
            None => match cmd_rx.recv() {
                Ok(c) => c,
                Err(_) => break, // UI hung up
            },
        };
        let mut batch = vec![first];
        loop {
            match cmd_rx.try_recv() {
                Ok(c) => batch.push(c),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'outer,
            }
        }

        // Coalesce. Shutdown wins outright. A Load supersedes everything queued
        // before it. Only the LAST visible render is kept. EVERY dropped render
        // gets a RenderFailed("superseded") so the UI can clear its pending set
        // — including renders dropped by a Load: if that load then fails (bad
        // file, password), no Loaded reply arrives to reset the UI, and without
        // the supersede replies those requests would be stranded as pending
        // forever. When the load succeeds, the extra replies are harmless
        // (Loaded clears everything anyway).
        let mut load: Option<(PathBuf, Option<String>)> = None;
        let mut visible: Option<(u16, SizeU, u8)> = None;
        let mut prefetch: Vec<(u16, SizeU, u8)> = Vec::new();
        let mut superseded: Vec<u16> = Vec::new();
        for cmd in batch {
            match cmd {
                PdfCmd::Shutdown => break 'outer,
                PdfCmd::Load { path, password } => {
                    if let Some((old, ..)) = visible.take() {
                        superseded.push(old);
                    }
                    superseded.extend(prefetch.drain(..).map(|(page, ..)| page));
                    load = Some((path, password));
                }
                PdfCmd::Render { page, px, rot, visible: true } => {
                    if let Some((old, ..)) = visible.replace((page, px, rot)) {
                        superseded.push(old);
                    }
                }
                PdfCmd::Render { page, px, rot, visible: false } => prefetch.push((page, px, rot)),
            }
        }
        // Dedup prefetches per page (keep the newest) and drop ones that
        // duplicate the visible page. Dropping silently is safe here only
        // because the surviving same-page request's reply clears the UI's
        // pending set by page.
        let visible_page = visible.map(|(p, ..)| p);
        let mut seen: std::collections::HashSet<u16> = std::collections::HashSet::new();
        let mut kept: Vec<(u16, SizeU, u8)> = Vec::new();
        for &(page, px, rot) in prefetch.iter().rev() {
            if Some(page) != visible_page && seen.insert(page) {
                kept.push((page, px, rot));
            }
        }
        kept.reverse();
        let prefetch = kept;
        for page in superseded {
            let _ = reply_tx.send(PdfReply::RenderFailed { page, reason: "superseded".into() });
        }

        if let Some((path, password)) = load {
            // Keep the old document until the new one opens successfully, so a
            // failed open leaves the current book fully usable.
            match pdfium.load_pdf_from_file(&path, password.as_deref()) {
                Ok(d) => {
                    if d.pages().len() <= 0 {
                        let _ = reply_tx.send(PdfReply::LoadFailed {
                            path,
                            reason: "document has no pages".into(),
                        });
                    } else {
                        let _ = reply_tx.send(describe(&d, &path));
                        doc = Some(d);
                    }
                }
                Err(PdfiumError::PdfiumLibraryInternalError(
                    PdfiumInternalError::PasswordError,
                )) => {
                    let _ = reply_tx.send(PdfReply::NeedsPassword { path });
                }
                Err(e) => {
                    let _ = reply_tx.send(PdfReply::LoadFailed { path, reason: format!("{e:?}") });
                }
            }
            wake();
        }

        let work: Vec<(u16, SizeU, u8)> = visible.into_iter().chain(prefetch).collect();
        let mut dropped_after: Option<usize> = None;
        for (i, (page, px, rot)) in work.iter().enumerate() {
            let Some(d) = &doc else {
                // No document (e.g. the first-ever load failed): every
                // remaining request still needs a reply or the UI's
                // pending set would be stranded forever.
                dropped_after = Some(i);
                break;
            };
            let _ = reply_tx.send(render_page(d, *page, *px, *rot));
            wake();
            // Preempt: if new commands arrived mid-batch (e.g. a rapid page
            // flip), stop rendering stale work and loop around.
            match cmd_rx.try_recv() {
                Ok(c) => {
                    carry = Some(c);
                    dropped_after = Some(i + 1);
                    break;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => break 'outer,
            }
        }
        if let Some(from) = dropped_after {
            for (page, ..) in &work[from..] {
                let _ = reply_tx
                    .send(PdfReply::RenderFailed { page: *page, reason: "superseded".into() });
            }
            wake();
        }
    }
}

/// Page count, all page sizes in points, and the outline flattened pre-order.
fn describe(doc: &PdfDocument, path: &Path) -> PdfReply {
    let pages = doc.pages().len().clamp(0, u16::MAX as i32) as u16;
    let page_sizes: Vec<(f32, f32)> = match doc.pages().page_sizes() {
        Ok(sizes) => sizes.iter().map(|r| (r.width().value, r.height().value)).collect(),
        Err(_) => vec![(612.0, 792.0); pages as usize],
    };
    let mut outline = Vec::new();
    // root() is the FIRST top-level bookmark; walk it and its sibling chain.
    let mut current = doc.bookmarks().root();
    while let Some(b) = current {
        let next = b.next_sibling();
        walk_bookmark(&b, 0, &mut outline);
        current = next;
    }
    PdfReply::Loaded { path: path.to_path_buf(), pages, page_sizes, outline }
}

fn walk_bookmark(b: &PdfBookmark, depth: u8, out: &mut Vec<OutlineItem>) {
    out.push(OutlineItem {
        title: b.title().unwrap_or_default(),
        page: resolve_bookmark_page(b),
        depth,
        has_children: b.first_child().is_some(),
    });
    for child in b.iter_direct_children() {
        walk_bookmark(&child, depth.saturating_add(1), out);
    }
}

/// Direct /Dest first, then a GoTo action's destination.
fn resolve_bookmark_page(b: &PdfBookmark) -> Option<u16> {
    if let Some(dest) = b.destination() {
        return dest.page_index().ok().map(to_u16);
    }
    if let Some(action) = b.action() {
        if let Some(local) = action.as_local_destination_action() {
            return local.destination().ok().and_then(|d| d.page_index().ok()).map(to_u16);
        }
    }
    None
}

fn to_u16(i: PdfPageIndex) -> u16 {
    i.clamp(0, u16::MAX as i32) as u16
}

fn render_page(doc: &PdfDocument, page_idx: u16, px: SizeU, rot: u8) -> PdfReply {
    let page = match doc.pages().get(page_idx as PdfPageIndex) {
        Ok(p) => p,
        Err(e) => return PdfReply::RenderFailed { page: page_idx, reason: format!("{e:?}") },
    };
    // set_fixed_size renders into a bitmap of EXACTLY px — for every
    // rotation. This is load-bearing: set_target_size computes per-axis
    // scales against the unrotated page and re-rounds them per axis, so at
    // 90°/270° the output could drift several px from the request on
    // high-aspect pages. wanted_renders() would never accept such a reply
    // (SIZE_TOLERANCE) and would re-request forever — a render livelock.
    // The caller caps px at MAX_RENDER_DIM (layout::render_size), so the
    // auto-scaling maximum_width/height constraints aren't needed here.
    let mut config = PdfRenderConfig::new()
        .set_fixed_size(px.w as Pixels, px.h as Pixels)
        .set_format(PdfBitmapFormat::BGRA)
        // Default is true, which writes RGBA; Direct2D wants real BGRA.
        .set_reverse_byte_order(false);
    if rot % 4 != 0 {
        let rotation = match rot % 4 {
            1 => PdfPageRenderRotation::Degrees90,
            2 => PdfPageRenderRotation::Degrees180,
            _ => PdfPageRenderRotation::Degrees270,
        };
        config = config.rotate(rotation, true);
    }
    let bitmap = match page.render_with_config(&config) {
        Ok(b) => b,
        Err(e) => return PdfReply::RenderFailed { page: page_idx, reason: format!("{e:?}") },
    };
    let (w, h) = (bitmap.width() as usize, bitmap.height() as usize);
    if w == 0 || h == 0 {
        return PdfReply::RenderFailed { page: page_idx, reason: "empty bitmap".into() };
    }
    let raw = bitmap.as_raw_bytes();
    let stride = raw.len() / h;
    // Direct2D wants tight rows; repack only if pdfium padded the stride.
    let bgra = if stride == w * 4 {
        raw
    } else {
        let mut tight = Vec::with_capacity(w * h * 4);
        for row in 0..h {
            let start = row * stride;
            tight.extend_from_slice(&raw[start..start + w * 4]);
        }
        tight
    };
    PdfReply::Rendered {
        page: page_idx,
        px: SizeU { w: w as u32, h: h as u32 },
        rot,
        bgra,
        links: extract_links(&page),
    }
}

/// Link rects (PDF points, bottom-left origin) plus resolved targets.
fn extract_links(page: &PdfPage) -> Vec<PageLink> {
    let mut out = Vec::new();
    for link in page.links().iter() {
        let Ok(rect) = link.rect() else { continue };
        let target = if let Some(dest) = link.destination() {
            dest.page_index().ok().map(|i| LinkTarget::Page(to_u16(i)))
        } else if let Some(action) = link.action() {
            match action {
                PdfAction::LocalDestination(local) => local
                    .destination()
                    .ok()
                    .and_then(|d| d.page_index().ok())
                    .map(|i| LinkTarget::Page(to_u16(i))),
                PdfAction::Uri(uri) => uri.uri().ok().map(LinkTarget::Uri),
                _ => None, // Launch / RemoteDestination / Embedded: ignored per spec
            }
        } else {
            None
        };
        if let Some(target) = target {
            out.push(PageLink {
                rect_pt: RectF {
                    left: rect.left().value,
                    top: rect.top().value,
                    right: rect.right().value,
                    bottom: rect.bottom().value,
                },
                target,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::extract_pdfium_to;
    use std::sync::Arc;

    #[test]
    fn extract_writes_reuses_forces_and_replaces_bad_copy() {
        let dir = std::env::temp_dir().join(format!("pppdf-extract-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bytes = b"pretend-this-is-a-dll";
        let name = "pdfium-test.dll";

        // First call creates the file with exactly the given bytes.
        let p1 = extract_pdfium_to(&dir, bytes, name, false).unwrap();
        assert!(p1.exists());
        assert_eq!(std::fs::read(&p1).unwrap(), bytes);

        // Reuse fast-path must NOT rewrite: plant same-length *different*
        // content, then confirm a non-forced call returns it untouched.
        let sentinel = vec![b'X'; bytes.len()];
        std::fs::write(&p1, &sentinel).unwrap();
        let p2 = extract_pdfium_to(&dir, bytes, name, false).unwrap();
        assert_eq!(p2, p1);
        assert_eq!(std::fs::read(&p2).unwrap(), sentinel, "reuse must skip the write");

        // force = true overwrites even a same-length existing file (self-heal).
        let p3 = extract_pdfium_to(&dir, bytes, name, true).unwrap();
        assert_eq!(std::fs::read(&p3).unwrap(), bytes);

        // A wrong-length leftover is always replaced, even without force.
        std::fs::write(&p1, b"short").unwrap();
        let p4 = extract_pdfium_to(&dir, bytes, name, false).unwrap();
        assert_eq!(std::fs::read(&p4).unwrap(), bytes);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_writers_never_yield_a_torn_file() {
        let dir = Arc::new(std::env::temp_dir().join(format!("pppdf-race-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&*dir);
        // A payload large enough that a half-written copy would be observable.
        let bytes: Arc<Vec<u8>> = Arc::new((0..64 * 1024).map(|i| (i % 251) as u8).collect());
        let name = "pdfium-race.dll";

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let dir = Arc::clone(&dir);
                let bytes = Arc::clone(&bytes);
                std::thread::spawn(move || {
                    let p = extract_pdfium_to(&dir, &bytes, name, false).unwrap();
                    // Whichever copy won the race, every reader sees the whole
                    // payload via the atomic rename — never a partial file.
                    assert_eq!(std::fs::read(&p).unwrap(), *bytes);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let _ = std::fs::remove_dir_all(&*dir);
    }
}

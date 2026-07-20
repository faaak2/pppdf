//! The PDF thread: owns ALL pdfium state; speaks PdfCmd in, PdfReply out.

use crate::layout::{RectF, SizeU};
use crate::types::{
    Bookmark, FieldKind, FormField, LinkTarget, OutlineItem, PageLink, PdfCmd, PdfReply,
    SearchMatch, SearchOptions,
};
#[cfg(feature = "pro")]
use crate::types::FormEvent;
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

/// The in-progress streaming text scan (PdfCmd::Search). One page is
/// scanned per main-loop pass so commands keep page-granularity latency,
/// exactly like the render preempt point.
struct ScanState {
    id: u32,
    query: String,
    options: SearchOptions,
    /// First page of the scan; it walks start..pages then wraps 0..start.
    start: u16,
    /// Pages covered so far; the scan is done when this reaches the count.
    scanned: u16,
}

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
pub(crate) fn pdfium() -> Result<&'static Pdfium, String> {
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
    // The streaming text scan, advanced one page per idle loop pass.
    let mut active_search: Option<ScanState> = None;
    // The last visible render request, re-issued when the form layer
    // invalidates (pro). Kept only in the pro build — the free build never
    // reads it, so gating it keeps that build's behavior byte-identical.
    #[cfg(feature = "pro")]
    let mut last_visible: Option<(u16, SizeU, u8)> = None;
    // Throttle for unsolicited (timer/invalidate-driven) re-renders: see
    // maybe_render_unsolicited. Investigated 2026-07-09
    // (.superpowers/sdd/flood-investigation.md) — without this, a PDFium
    // timer firing every ~1ms (see ffi_set_timer's floor clamp) drove a
    // full-page render on every tick, pegging a CPU core.
    #[cfg(feature = "pro")]
    let mut last_unsolicited: Option<std::time::Instant> = None;
    #[cfg(feature = "pro")]
    let mut current_path: Option<PathBuf> = None;
    // The password the current document was opened with — needed to
    // shadow-load the on-disk bytes for a bookmark save. Kept only on
    // the PDF thread, only in the Pro build.
    #[cfg(feature = "pro")]
    let mut current_password: Option<String> = None;
    #[cfg(feature = "pro")]
    let mut engine: Option<crate::pro::forms::FormEngine> = None;

    'outer: loop {
        let first: Option<PdfCmd> = match carry.take() {
            Some(c) => Some(c),
            // A scan is active: never block — poll, and when nothing is
            // queued, advance the scan one page instead.
            None if active_search.is_some() => match cmd_rx.try_recv() {
                Ok(c) => Some(c),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => break 'outer,
            },
            None => {
                #[cfg(feature = "pro")]
                {
                    match wait_cmd(
                        &cmd_rx,
                        &mut engine,
                        &doc,
                        last_visible,
                        &mut last_unsolicited,
                        &reply_tx,
                        &wake,
                    ) {
                        Some(c) => Some(c),
                        None => break 'outer,
                    }
                }
                #[cfg(not(feature = "pro"))]
                {
                    match cmd_rx.recv() {
                        Ok(c) => Some(c),
                        Err(_) => break 'outer, // UI hung up
                    }
                }
            }
        };
        let Some(first) = first else {
            // Idle pass with an active scan: cover exactly one page.
            scan_step(&mut active_search, &doc, &reply_tx, &wake);
            // wait_cmd's timeout-arm duties must not starve while the scan
            // spins outside it: keep form timers firing (caret blink) and
            // flush throttle-deferred repaints. maybe_render_unsolicited is
            // internally throttled (33 ms), so per-page calls are safe.
            #[cfg(feature = "pro")]
            if let Some(eng) = engine.as_mut() {
                eng.fire_due_timers();
                maybe_render_unsolicited(
                    eng,
                    &doc,
                    last_visible,
                    &mut last_unsolicited,
                    &reply_tx,
                    &wake,
                );
            }
            continue 'outer;
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
        #[cfg(feature = "pro")]
        // Each entry is (visible view = (page, px, rot), event). The view
        // rides along so a form edit re-renders the on-screen page even
        // after a cache-hit flip left `last_visible` stale.
        let mut form_events: Vec<((u16, SizeU, u8), FormEvent)> = Vec::new();
        #[cfg(feature = "pro")]
        let mut save_requested = false;
        #[cfg(feature = "pro")]
        let mut bookmark_saves: Vec<(PathBuf, Vec<crate::types::Bookmark>)> = Vec::new();
        let mut queued_search: Option<ScanState> = None;
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
                PdfCmd::Search { id, query, options, start_page } => {
                    // Only the LAST queued Search survives (visible-render
                    // semantics); earlier ones are superseded silently — the
                    // UI already moved on when it issued the newer id.
                    queued_search =
                        Some(ScanState { id, query, options, start: start_page, scanned: 0 });
                }
                PdfCmd::CancelSearch => {
                    // Order-sensitive, like Load: kills the active scan and
                    // any Search queued BEFORE it in this batch; a Search
                    // AFTER it in the same batch survives (Esc-then-Ctrl+F
                    // burst must not strand the new scan).
                    queued_search = None;
                    active_search = None;
                }
                #[cfg(feature = "pro")]
                PdfCmd::Form { page, px, rot, ev } => form_events.push(((page, px, rot), ev)),
                #[cfg(feature = "pro")]
                PdfCmd::SaveDoc => save_requested = true,
                #[cfg(feature = "pro")]
                PdfCmd::SaveBookmarks { path, bookmarks } => {
                    // FIFO, NEVER coalesced: the UI counts one reply per
                    // command into saves_inflight (a dropped reply would
                    // block window destruction forever).
                    bookmark_saves.push((path, bookmarks));
                }
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
        // A Load replaces the document: any scan (active or queued, either
        // side of it in the batch) targets the OLD document and dies with
        // it. The UI drops stale ids anyway; no reply is owed.
        if load.is_some() {
            active_search = None;
            queued_search = None;
        }
        if let Some(s) = queued_search.take() {
            active_search = Some(s);
        }

        // Bookmark saves execute BEFORE any Load in this batch: every
        // SaveBookmarks was sent while the pre-Load document was current
        // (the UI can't know a newer document yet — Loaded hasn't been
        // pumped). The path guard turns any residual mismatch (double
        // Load, races) into a graceful error, never a wrong-file write.
        #[cfg(feature = "pro")]
        for (bpath, bmarks) in bookmark_saves.drain(..) {
            let result = match (doc.as_mut(), &current_path) {
                (Some(d), Some(p)) if *p == bpath => crate::pro::bookmarks::save_bookmarks(
                    pdfium,
                    d,
                    &bpath,
                    current_password.as_deref(),
                    &bmarks,
                ),
                _ => Err("document changed".into()),
            };
            let _ = reply_tx.send(PdfReply::BookmarksSaved { result });
            wake();
        }

        if let Some((path, password)) = load {
            // Keep the old document (and its form engine) until the new one
            // opens successfully, so a failed open leaves the current book
            // fully usable.
            // Load from bytes: pdfium then holds no file handle, so the
            // original stays writable (in-place save) and un-locked on disk.
            let loaded = std::fs::read(&path)
                .map_err(PdfiumError::IoError)
                .and_then(|bytes| pdfium.load_pdf_from_byte_vec(bytes, password.as_deref()));
            match loaded {
                Ok(d) => {
                    if d.pages().len() <= 0 {
                        let _ = reply_tx.send(PdfReply::LoadFailed {
                            path,
                            reason: "document has no pages".into(),
                        });
                    } else {
                        // Teardown the form engine on the OLD document before
                        // we drop or replace it — the engine holds a raw page
                        // handle into it. Only reached once the new document
                        // is known-good, so a failed open never tears down
                        // the still-current document's engine.
                        #[cfg(feature = "pro")]
                        if let Some(mut eng) = engine.take() {
                            eng.teardown(pdfium.bindings());
                        }
                        #[cfg(feature = "pro")]
                        {
                            last_visible = None;
                        }
                        // `describe` borrows `path`; send first, then keep it.
                        let _ = reply_tx.send(describe(&d, &path));
                        #[cfg(feature = "pro")]
                        {
                            current_path = Some(path);
                        }
                        #[cfg(feature = "pro")]
                        {
                            current_password = password;
                        }
                        doc = Some(d);
                        #[cfg(feature = "pro")]
                        {
                            engine = doc
                                .as_mut()
                                .and_then(|d| crate::pro::forms::FormEngine::new(d, reply_tx.clone()));
                        }
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

        // Remember the visible request so the form layer can re-issue it when
        // its widgets invalidate. `visible` is Copy, so this read is cheap and
        // leaves it available for the render loop below.
        #[cfg(feature = "pro")]
        if let Some(v) = visible {
            last_visible = Some(v);
        }

        #[cfg(feature = "pro")]
        {
            if !form_events.is_empty() || save_requested || visible.is_some() {
                crate::dlog!(
                    "pdf: batch form_events={} visible={visible:?} save={save_requested} engine={}",
                    form_events.len(),
                    engine.is_some()
                );
            }
            // The view (page, px, rot) the latest form event was tagged with
            // is what is on screen right now. Captured before draining so the
            // unsolicited-render branch below can adopt it as `last_visible`
            // — the fix for edits on a cache-hit-flipped page repainting the
            // stale page/size. Only the LAST event's view matters: an
            // intra-batch flip makes it the final on-screen page, while each
            // event is still dispatched to its own page in the drain loop.
            let form_view = form_events.last().map(|&(view, _)| view);
            if let Some(eng) = engine.as_mut() {
                for ((page, _, _), ev) in form_events.drain(..) {
                    eng.handle_event(pdfium.bindings(), page, ev);
                }
            }
            // SaveDoc must ALWAYS get exactly one Saved reply, even with no
            // engine/document/path — otherwise a UI waiting on the reply
            // would hang forever.
            if save_requested {
                let result = match (engine.as_mut(), &doc, &current_path) {
                    (Some(eng), Some(d), Some(p)) => eng.save(pdfium.bindings(), d, p),
                    _ => Err("no document open".into()),
                };
                let _ = reply_tx.send(PdfReply::Saved { result });
                wake();
            }
            // Events (or fired timers) dirtied the widgets. If this batch
            // already has a visible-page render queued below, that render
            // covers the current state — consume the flag now. This upholds
            // the take-only-at-render invariant: the visible render below
            // is unconditional when a document exists, and `engine` being
            // Some implies `doc` is Some (the engine is created only
            // alongside a successfully opened document and torn down with
            // it). Otherwise this would be an UNSOLICITED render (nothing
            // else in this batch is about to repaint the page), so it goes
            // through the same throttle as wait_cmd's timer-driven renders;
            // maybe_render_unsolicited leaves the flag SET when it cannot
            // or may not render yet, so a later pass still catches up.
            if let Some(eng) = engine.as_mut() {
                if visible.is_some() {
                    eng.take_invalidated();
                } else {
                    // No visible Render this batch to refresh the memo, but a
                    // form event named the on-screen view — adopt it so the
                    // unsolicited re-render targets what's displayed, not the
                    // page/size we last explicitly rendered. Done ONLY here
                    // (not when a visible render is present) so a real render
                    // still owns `last_visible`.
                    if let Some(view) = form_view {
                        last_visible = Some(view);
                    }
                    maybe_render_unsolicited(
                        eng,
                        &doc,
                        last_visible,
                        &mut last_unsolicited,
                        &reply_tx,
                        &wake,
                    );
                }
            }
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
            #[cfg(feature = "pro")]
            let _ = reply_tx.send(render_page_pro(d, engine.as_ref(), *page, *px, *rot));
            #[cfg(not(feature = "pro"))]
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

    // Thread is exiting (Shutdown or the UI hung up): release the engine's raw
    // page handle before its document drops.
    #[cfg(feature = "pro")]
    if let Some(mut eng) = engine.take() {
        eng.teardown(pdfium.bindings());
    }
}

/// Block for the next command; while form timers exist, wake at their
/// deadlines to blink the caret (fire timers -> pdfium invalidates -> the
/// visible page re-renders, throttled — see maybe_render_unsolicited).
/// Returns None when the UI hung up.
#[cfg(feature = "pro")]
fn wait_cmd(
    cmd_rx: &Receiver<PdfCmd>,
    engine: &mut Option<crate::pro::forms::FormEngine>,
    doc: &Option<PdfDocument<'static>>,
    last_visible: Option<(u16, SizeU, u8)>,
    last_unsolicited: &mut Option<std::time::Instant>,
    reply_tx: &Sender<PdfReply>,
    wake: &impl Fn(),
) -> Option<PdfCmd> {
    loop {
        let deadline = engine.as_ref().and_then(|e| e.next_deadline());
        let timeout = match deadline {
            Some(d) => d.saturating_duration_since(std::time::Instant::now()),
            None => {
                // No timers left — but a throttled unsolicited render may
                // have left the invalidated flag set with nothing to wake
                // us (e.g. a SaveDoc landed inside the 33ms window: save()
                // force-kills focus, which kills the caret timer, and sets
                // the flag; the main-loop repaint got throttled). An
                // untimed recv() here would strand that deferred repaint
                // (stale focus-ring/caret pixels) until some unrelated
                // command arrived. Bounded-wait instead so the flush is
                // always due — but ONLY when a repaint is actually
                // possible: without a doc/last_visible the flag can only
                // ever be serviced once a Render command arrives, and that
                // wakes recv() anyway (a bounded wait would just spin idle).
                let repaint_pending = engine.as_ref().is_some_and(|e| e.peek_invalidated())
                    && doc.is_some()
                    && last_visible.is_some();
                if !repaint_pending {
                    return cmd_rx.recv().ok();
                }
                MIN_UNSOLICITED_RENDER_INTERVAL
            }
        };
        match cmd_rx.recv_timeout(timeout) {
            Ok(c) => return Some(c),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some(eng) = engine.as_mut() {
                    eng.fire_due_timers();
                    maybe_render_unsolicited(eng, doc, last_visible, last_unsolicited, reply_tx, wake);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    }
}

/// Minimum spacing between UNSOLICITED (timer/invalidate-driven, not
/// directly requested by the UI) page re-renders. 33ms = ~30fps — plenty
/// for a caret blink (PDFium's own cadence is ~500ms) while bounding the
/// worst case: a pathological PDFium timer requesting a near-0ms interval
/// (investigated 2026-07-09, .superpowers/sdd/flood-investigation.md) would
/// otherwise drive a full-page render on every tick and peg a CPU core
/// (454 renders/s measured pre-fix).
#[cfg(feature = "pro")]
const MIN_UNSOLICITED_RENDER_INTERVAL: std::time::Duration = std::time::Duration::from_millis(33);

/// Render the visible page if the form layer's widgets were invalidated
/// AND the unsolicited-render throttle allows it now. Shared by wait_cmd's
/// timer-fire path and the main loop's post-event invalidate check — both
/// render in response to PDFium state changes that the UI never explicitly
/// requested, so both must be throttled the same way.
///
/// CRITICAL: when the throttle suppresses a render, the invalidated flag is
/// left SET (never taken) — the caller must not otherwise consume it in
/// that case. A later pass (the next timer wake, at least
/// MIN_UNSOLICITED_RENDER_INTERVAL away, or the next command batch) will
/// see it still set and repaint the final state. The flag is only consumed
/// (via take_invalidated) when a render actually happens here.
#[cfg(feature = "pro")]
fn maybe_render_unsolicited(
    eng: &mut crate::pro::forms::FormEngine,
    doc: &Option<PdfDocument<'static>>,
    last_visible: Option<(u16, SizeU, u8)>,
    last_unsolicited: &mut Option<std::time::Instant>,
    reply_tx: &Sender<PdfReply>,
    wake: &impl Fn(),
) {
    let now = std::time::Instant::now();
    let due = last_unsolicited
        .map(|last| now.duration_since(last) >= MIN_UNSOLICITED_RENDER_INTERVAL)
        .unwrap_or(true);
    if !due {
        crate::dlog!("pdf: unsolicited throttled (flag={})", eng.peek_invalidated());
        return; // Throttled: leave the invalidated flag set for a later pass.
    }
    if !eng.peek_invalidated() {
        return; // Nothing to repaint; don't touch the throttle clock.
    }
    crate::dlog!("pdf: unsolicited render dispatch last_visible={last_visible:?}");
    if let (Some(d), Some((page, px, rot))) = (doc, last_visible) {
        // Consume the flag only here, at the moment the render is actually
        // dispatched. With no doc or no visible-render memo the flag must
        // survive for the pass that CAN repaint (the eventual visible
        // Render command's batch consumes it).
        eng.take_invalidated();
        let _ = reply_tx.send(render_page_pro(d, Some(&*eng), page, px, rot));
        wake();
        *last_unsolicited = Some(now);
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
    PdfReply::Loaded {
        path: path.to_path_buf(),
        pages,
        page_sizes,
        outline,
        has_forms: doc.form().is_some(),
        bookmarks: read_bookmarks(doc, pages),
    }
}

/// The `pppdf.bookmarks` attachment parsed into user bookmarks. Shared by
/// both editions — the free build displays them read-only; only Pro
/// writes them (pro::bookmarks).
fn read_bookmarks(doc: &PdfDocument, pages: u16) -> Vec<Bookmark> {
    for att in doc.attachments().iter() {
        if att.name() == crate::bookmarks::ATTACHMENT_NAME {
            if let Ok(bytes) = att.save_to_bytes() {
                return crate::bookmarks::parse(&bytes, pages);
            }
        }
    }
    Vec::new()
}

/// Advance the streaming scan by exactly ONE page: send SearchHits for a
/// page with hits, and SearchDone when every page has been covered. Every
/// reply is followed by wake().
fn scan_step(
    scan: &mut Option<ScanState>,
    doc: &Option<PdfDocument<'static>>,
    reply_tx: &Sender<PdfReply>,
    wake: &impl Fn(),
) {
    let Some(s) = scan.as_mut() else { return };
    let Some(d) = doc else {
        // Unreachable in practice (the UI only searches an open document
        // and a Load kills the scan) — but never strand the UI's spinner.
        let _ = reply_tx.send(PdfReply::SearchDone { id: s.id });
        wake();
        *scan = None;
        return;
    };
    let pages = d.pages().len().clamp(0, u16::MAX as i32) as u16;
    if pages == 0 || s.scanned >= pages {
        let _ = reply_tx.send(PdfReply::SearchDone { id: s.id });
        wake();
        *scan = None;
        return;
    }
    // Wrap order: start..end then 0..start — the hits nearest the reader
    // stream in first, so Enter can jump almost immediately mid-scan.
    let page = ((s.start as u32 + s.scanned as u32) % pages as u32) as u16;
    s.scanned += 1;
    let done = s.scanned >= pages;
    let id = s.id;
    let matches = search_page(d, page, &s.query, s.options);
    if !matches.is_empty() {
        let _ = reply_tx.send(PdfReply::SearchHits { id, page, matches });
        wake();
    }
    if done {
        let _ = reply_tx.send(PdfReply::SearchDone { id });
        wake();
        *scan = None;
    }
}

/// All hits on one page, in document order, as plain rect lists (PDF
/// points, bottom-left origin — the PageLink convention). A page whose
/// text fails to load is skipped silently, per the corrupt-page policy.
fn search_page(
    doc: &PdfDocument,
    page_idx: u16,
    query: &str,
    options: SearchOptions,
) -> Vec<SearchMatch> {
    let Ok(page) = doc.pages().get(page_idx as PdfPageIndex) else { return Vec::new() };
    let Ok(text) = page.text() else { return Vec::new() };
    let opts = PdfSearchOptions::new()
        .match_case(options.match_case)
        .match_whole_word(options.whole_word);
    let Ok(search) = text.search(query, &opts) else { return Vec::new() };
    let mut out = Vec::new();
    while let Some(seg) = search.find_next() {
        // One hit can span multiple rects (line wraps / font changes).
        let mut rects = Vec::new();
        for i in seg.as_range() {
            if let Ok(segment) = seg.get(i) {
                let b = segment.bounds();
                rects.push(RectF {
                    left: b.left().value,
                    top: b.top().value,
                    right: b.right().value,
                    bottom: b.bottom().value,
                });
            }
        }
        if !rects.is_empty() {
            out.push(SearchMatch { rects });
        }
    }
    out
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
        fields: extract_form_fields(doc, &page),
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

/// Stable PDFium ABI constants not re-exported by pdfium-render's prelude.
const ANNOT_SUBTYPE_WIDGET: i32 = 20; // FPDF_ANNOT_WIDGET
const FORMFIELD_TEXT: i32 = 6; // FPDF_FORMFIELD_TEXTFIELD
#[cfg(feature = "pro")]
const RENDER_FLAG_ANNOT: i32 = 1; // FPDF_ANNOT (render annotations)
#[cfg(feature = "pro")]
const BITMAP_FORMAT_BGRA: i32 = 4; // FPDFBitmap_BGRA

/// Render like `render_page`, but when the form engine is live on THIS
/// page, raster + form overlay go through the ENGINE's page handle via raw
/// bindings. FPDF_LoadPage returns a DISTINCT page object per call, and
/// FPDF_FFLDraw draws the live editor overlay (focus ring, caret, the
/// focused widget's in-edit text, open combo popups) only into the page
/// view attached to the handle it is given — pdfium-render's render path
/// loads a private page handle, so the focused widget's live state never
/// reached the pixels: typed text stayed invisible until its value
/// committed on focus-loss (the 2026-07-09 "cannot type" bug, part 2).
#[cfg(feature = "pro")]
fn render_page_pro(
    doc: &PdfDocument,
    engine: Option<&crate::pro::forms::FormEngine>,
    page_idx: u16,
    px: SizeU,
    rot: u8,
) -> PdfReply {
    let Some((form_h, page_h)) = engine.and_then(|e| e.render_handles(page_idx)) else {
        return render_page(doc, page_idx, px, rot);
    };
    let b = doc.bindings();
    let (w, h) = (px.w as i32, px.h as i32);
    if w <= 0 || h <= 0 {
        return PdfReply::RenderFailed { page: page_idx, reason: "empty bitmap".into() };
    }
    let bgra = unsafe {
        let bmp = b.FPDFBitmap_CreateEx(w, h, BITMAP_FORMAT_BGRA, std::ptr::null_mut(), 0);
        if bmp.is_null() {
            return PdfReply::RenderFailed {
                page: page_idx,
                reason: "FPDFBitmap_CreateEx failed".into(),
            };
        }
        let _ = b.FPDFBitmap_FillRect(bmp, 0, 0, w, h, 0xFFFF_FFFF); // opaque white
        // Same call pair + flags pdfium-render's form-aware path uses
        // (FPDF_ANNOT on both is PDFium's canonical embedder sequence);
        // `rot` is quarter-turns CW == FPDF's rotate arg, and px is already
        // the output (rotation-swapped) size per PdfCmd::Render's contract.
        b.FPDF_RenderPageBitmap(bmp, page_h, 0, 0, w, h, rot as i32, RENDER_FLAG_ANNOT);
        b.FPDF_FFLDraw(form_h, bmp, page_h, 0, 0, w, h, rot as i32, RENDER_FLAG_ANNOT);
        let stride = b.FPDFBitmap_GetStride(bmp) as usize;
        let buf = b.FPDFBitmap_GetBuffer(bmp) as *const u8;
        if buf.is_null() {
            b.FPDFBitmap_Destroy(bmp);
            return PdfReply::RenderFailed { page: page_idx, reason: "no bitmap buffer".into() };
        }
        let (wu, hu) = (w as usize, h as usize);
        let mut tight = Vec::with_capacity(wu * hu * 4);
        for row in 0..hu {
            tight.extend_from_slice(std::slice::from_raw_parts(buf.add(row * stride), wu * 4));
        }
        b.FPDFBitmap_Destroy(bmp);
        tight
    };
    // Links/fields come off a separate high-level page, exactly like the
    // normal path — read-only walks, indifferent to which page object.
    let page = match doc.pages().get(page_idx as PdfPageIndex) {
        Ok(p) => p,
        Err(e) => return PdfReply::RenderFailed { page: page_idx, reason: format!("{e:?}") },
    };
    PdfReply::Rendered {
        page: page_idx,
        px,
        rot,
        bgra,
        links: extract_links(&page),
        fields: extract_form_fields(doc, &page),
    }
}

/// Widget-annotation rects (PDF points, bottom-left origin) + coarse kind.
/// Read-only and shared by both editions: the free build needs the rects for
/// its "Pro feature" hint, the pro build for hit-testing and cursors.
fn extract_form_fields(doc: &PdfDocument, page: &PdfPage) -> Vec<FormField> {
    let Some(form) = doc.form() else { return Vec::new() };
    let b = doc.bindings();
    let (form_h, page_h) = (form.handle(), page.page_handle());
    let mut out = Vec::new();
    unsafe {
        for i in 0..b.FPDFPage_GetAnnotCount(page_h) {
            let annot = b.FPDFPage_GetAnnot(page_h, i);
            if annot.is_null() {
                continue;
            }
            if b.FPDFAnnot_GetSubtype(annot) == ANNOT_SUBTYPE_WIDGET {
                let mut r = FS_RECTF { left: 0.0, top: 0.0, right: 0.0, bottom: 0.0 };
                if b.FPDFAnnot_GetRect(annot, &mut r) != 0 {
                    let kind = if b.FPDFAnnot_GetFormFieldType(form_h, annot) == FORMFIELD_TEXT {
                        FieldKind::Text
                    } else {
                        FieldKind::Other
                    };
                    out.push(FormField {
                        rect_pt: RectF { left: r.left, top: r.top, right: r.right, bottom: r.bottom },
                        kind,
                    });
                }
            }
            b.FPDFPage_CloseAnnot(annot);
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

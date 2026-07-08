//! The PDF thread: owns ALL pdfium state; speaks PdfCmd in, PdfReply out.

use crate::layout::{RectF, SizeU, MAX_RENDER_DIM};
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

/// Locate pdfium.dll: the exe's directory first, then up to two ancestors
/// (covers `cargo test`, where the test exe is in target\debug\deps but the
/// dll sits in target\debug), then the system search path.
fn bind_pdfium() -> Result<Pdfium, PdfiumError> {
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(Path::to_path_buf);
        for _ in 0..3 {
            let Some(d) = dir else { break };
            let candidate = Pdfium::pdfium_platform_library_name_at_path(&d);
            if candidate.exists() {
                return Ok(Pdfium::new(Pdfium::bind_to_library(&candidate)?));
            }
            dir = d.parent().map(Path::to_path_buf);
        }
    }
    Ok(Pdfium::new(Pdfium::bind_to_system_library()?))
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
        let mut visible: Option<(u16, SizeU)> = None;
        let mut prefetch: Vec<(u16, SizeU)> = Vec::new();
        let mut superseded: Vec<u16> = Vec::new();
        for cmd in batch {
            match cmd {
                PdfCmd::Shutdown => break 'outer,
                PdfCmd::Load { path, password } => {
                    if let Some((old, _)) = visible.take() {
                        superseded.push(old);
                    }
                    superseded.extend(prefetch.drain(..).map(|(page, _)| page));
                    load = Some((path, password));
                }
                PdfCmd::Render { page, px, visible: true } => {
                    if let Some((old, _)) = visible.replace((page, px)) {
                        superseded.push(old);
                    }
                }
                PdfCmd::Render { page, px, visible: false } => prefetch.push((page, px)),
            }
        }
        for page in superseded {
            let _ = reply_tx.send(PdfReply::RenderFailed { page, reason: "superseded".into() });
        }

        if let Some((path, password)) = load {
            // Keep the old document until the new one opens successfully, so a
            // failed open leaves the current book fully usable.
            match pdfium.load_pdf_from_file(&path, password.as_deref()) {
                Ok(d) => {
                    let _ = reply_tx.send(describe(&d, &path));
                    doc = Some(d);
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

        let work: Vec<(u16, SizeU)> = visible.into_iter().chain(prefetch).collect();
        let mut dropped_after: Option<usize> = None;
        for (i, (page, px)) in work.iter().enumerate() {
            let Some(d) = &doc else { break };
            let _ = reply_tx.send(render_page(d, *page, *px));
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
            for (page, _) in &work[from..] {
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

fn render_page(doc: &PdfDocument, page_idx: u16, px: SizeU) -> PdfReply {
    let page = match doc.pages().get(page_idx as PdfPageIndex) {
        Ok(p) => p,
        Err(e) => return PdfReply::RenderFailed { page: page_idx, reason: format!("{e:?}") },
    };
    let config = PdfRenderConfig::new()
        .set_target_size(px.w as Pixels, px.h as Pixels)
        .set_maximum_width(MAX_RENDER_DIM as Pixels)
        .set_maximum_height(MAX_RENDER_DIM as Pixels)
        .set_format(PdfBitmapFormat::BGRA)
        // Default is true, which writes RGBA; Direct2D wants real BGRA.
        .set_reverse_byte_order(false);
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

//! End-to-end smoke test through a real pdfium.dll. Requires pdfium.dll in
//! target\debug (run .\fetch-pdfium.ps1 once first — see README).

mod common;

use pppdf::layout::SizeU;
use pppdf::pdf;
use pppdf::types::{LinkTarget, PdfCmd, PdfReply};
use std::sync::mpsc;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(20);

#[test]
fn load_render_outline_links() {
    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});

    // --- load ---
    cmd_tx
        .send(PdfCmd::Load { path: common::fixture_path(), password: None })
        .unwrap();
    let (pages, page_sizes, outline) = match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { pages, page_sizes, outline, .. } => (pages, page_sizes, outline),
        other => panic!("expected Loaded, got {other:?}"),
    };
    assert_eq!(pages, 3);
    assert_eq!(page_sizes.len(), 3);
    assert!((page_sizes[0].0 - 612.0).abs() < 0.5);
    assert!((page_sizes[0].1 - 792.0).abs() < 0.5);

    // --- outline: titles, depths, targets ---
    let titles: Vec<_> = outline.iter().map(|o| o.title.as_str()).collect();
    assert_eq!(titles, vec!["Chapter One", "Chapter Two", "Section 2.1"]);
    let depths: Vec<_> = outline.iter().map(|o| o.depth).collect();
    assert_eq!(depths, vec![0, 0, 1]);
    assert_eq!(outline[0].page, Some(1)); // Chapter One -> page 2 (0-based 1)
    assert_eq!(outline[2].page, Some(2));
    assert!(outline[1].has_children);
    assert!(!outline[0].has_children);

    // --- render page 1 (index 0) at exact pixel size ---
    let px = SizeU { w: 612, h: 792 };
    cmd_tx.send(PdfCmd::Render { page: 0, px, visible: true }).unwrap();
    let (rpage, rpx, bgra, links) = match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Rendered { page, px, bgra, links } => (page, px, bgra, links),
        other => panic!("expected Rendered, got {other:?}"),
    };
    assert_eq!(rpage, 0);
    assert_eq!(rpx, px);
    assert_eq!(bgra.len(), (px.w * px.h * 4) as usize);
    // The content stream fills a pure-red rect; in BGRA byte order that is
    // [B~0, G~0, R~255, ...] — asserting BOTH rendering and channel order.
    let red_bgra = bgra.chunks_exact(4).any(|p| p[0] < 60 && p[1] < 60 && p[2] > 200);
    assert!(red_bgra, "no red-in-BGRA pixels - rendering or byte order wrong");

    // --- links on page 1 ---
    assert_eq!(links.len(), 2);
    let goto = links.iter().find(|l| matches!(l.target, LinkTarget::Page(_))).unwrap();
    assert_eq!(goto.target, LinkTarget::Page(2));
    // PDF rect [72 700 200 720]: left/bottom/right/top.
    assert!((goto.rect_pt.left - 72.0).abs() < 1.0);
    assert!((goto.rect_pt.bottom - 700.0).abs() < 1.0);
    assert!((goto.rect_pt.top - 720.0).abs() < 1.0);
    let uri = links.iter().find(|l| matches!(l.target, LinkTarget::Uri(_))).unwrap();
    assert_eq!(uri.target, LinkTarget::Uri("https://example.com/".into()));

    // --- shutdown is clean ---
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
}

#[test]
fn load_failure_reports_reason() {
    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    let bogus = std::env::temp_dir().join("pppdf_not_a_pdf.pdf");
    std::fs::write(&bogus, b"this is not a pdf").unwrap();
    cmd_tx.send(PdfCmd::Load { path: bogus, password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::LoadFailed { reason, .. } => assert!(!reason.is_empty()),
        other => panic!("expected LoadFailed, got {other:?}"),
    }
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
}

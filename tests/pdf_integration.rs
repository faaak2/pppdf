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
    cmd_tx.send(PdfCmd::Render { page: 0, px, rot: 0, visible: true }).unwrap();
    let (rpage, rpx, bgra, links) = match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Rendered { page, px, bgra, links, .. } => (page, px, bgra, links),
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

    // --- rotated render: 90° CW must come back with the axes swapped ---
    // The requested px is the desired OUTPUT size (already landscape); the
    // reply must match it, proving the rotation plumbing through pdfium
    // (target size is internally un-swapped back to page terms).
    let px_rot = SizeU { w: 792, h: 612 };
    cmd_tx.send(PdfCmd::Render { page: 0, px: px_rot, rot: 1, visible: true }).unwrap();
    let (rpx, rrot, bgra_rot) = match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Rendered { px, rot, bgra, .. } => (px, rot, bgra),
        other => panic!("expected Rendered, got {other:?}"),
    };
    assert_eq!(rrot, 1);
    assert_eq!(rpx, px_rot, "rotated output size must be the swapped page size");
    assert_eq!(bgra_rot.len(), (px_rot.w * px_rot.h * 4) as usize);
    // The red rect (100 600 200 100 re: x 100..300, y-up 600..700) draws in
    // the upright bitmap's TOP-LEFT quadrant. Rotated 90° CW it must land in
    // the TOP-RIGHT quadrant of the landscape bitmap — and be gone from the
    // top-left.
    let quadrant_has_red = |x0: u32, y0: u32| {
        (y0..y0 + px_rot.h / 2).any(|y| {
            (x0..x0 + px_rot.w / 2).any(|x| {
                let i = ((y * px_rot.w + x) * 4) as usize;
                bgra_rot[i] < 60 && bgra_rot[i + 1] < 60 && bgra_rot[i + 2] > 200
            })
        })
    };
    assert!(quadrant_has_red(px_rot.w / 2, 0), "red must rotate into the top-right");
    assert!(!quadrant_has_red(0, 0), "red must vanish from the top-left");

    // --- shutdown is clean ---
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
}

/// Anti-livelock invariant: whatever output size the UI requests — including
/// awkward sizes for a rotated high-aspect page — the reply must be EXACTLY
/// that size. wanted_renders() accepts a cached render only within
/// SIZE_TOLERANCE (2 px); with the old target-size render path, pdfium's
/// per-axis re-rounding drifted up to ~5 px at 90°/270° on a 10:1 page, so
/// the UI re-requested the identical render forever.
#[test]
fn rotated_render_size_is_exact_even_for_high_aspect_pages() {
    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    cmd_tx.send(PdfCmd::Load { path: common::tall_fixture_path(), password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { pages, page_sizes, .. } => {
            assert_eq!(pages, 1);
            assert_eq!(page_sizes[0], (100.0, 1000.0));
        }
        other => panic!("expected Loaded, got {other:?}"),
    }
    for (w, h, rot) in [(985, 98, 1), (973, 97, 1), (501, 50, 3), (98, 985, 2), (100, 1000, 0)] {
        let px = SizeU { w, h };
        cmd_tx.send(PdfCmd::Render { page: 0, px, rot, visible: true }).unwrap();
        match reply_rx.recv_timeout(WAIT).unwrap() {
            PdfReply::Rendered { px: rpx, rot: rrot, bgra, .. } => {
                assert_eq!(rrot, rot);
                assert_eq!(rpx, px, "rot {rot}: output size must match the request EXACTLY");
                assert_eq!(bgra.len(), (px.w * px.h * 4) as usize);
            }
            other => panic!("expected Rendered, got {other:?}"),
        }
    }
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
}

/// The real form fixture must report has_forms and per-render field rects;
/// the generated (form-free) fixture must not.
#[test]
fn form_fixture_reports_fields() {
    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    let form_pdf = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/form.pdf");
    cmd_tx.send(PdfCmd::Load { path: form_pdf, password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { has_forms, .. } => assert!(has_forms, "form.pdf must set has_forms"),
        other => panic!("expected Loaded, got {other:?}"),
    }
    cmd_tx.send(PdfCmd::Render { page: 0, px: SizeU { w: 612, h: 792 }, rot: 0, visible: true }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Rendered { fields, .. } => {
            // 8 named fields plus one extra widget (a radio group contributes
            // one widget per option); observed and pinned exactly.
            assert_eq!(fields.len(), 9, "expected exactly 9 widgets, got {}", fields.len());
            assert!(
                fields.iter().filter(|f| f.kind == pppdf::types::FieldKind::Text).count() >= 3,
                "FullName, ID, Notes must be Text kind"
            );
            for f in &fields {
                assert!(f.rect_pt.right > f.rect_pt.left && f.rect_pt.top > f.rect_pt.bottom);
            }
        }
        other => panic!("expected Rendered, got {other:?}"),
    }
    // The link-test fixture has no form.
    cmd_tx.send(PdfCmd::Load { path: common::fixture_path(), password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { has_forms, .. } => assert!(!has_forms),
        other => panic!("expected Loaded, got {other:?}"),
    }
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

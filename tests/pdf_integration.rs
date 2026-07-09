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

/// Pro: toggling a checkbox and typing into a text field must mark the
/// document dirty (FFI_OnChange -> FormDirty) and trigger an unsolicited
/// re-render of the visible page (FFI_Invalidate -> Rendered).
///
/// Note on the checkbox: with real PDFium a text field commits its value —
/// and so fires FFI_OnChange — only on focus-loss/Enter, not per keystroke,
/// whereas a checkbox toggles (and notifies) on the click itself. So the
/// dirty signal is driven by the checkbox (matching this test's own intent),
/// and the caret blink from focusing the text field drives the unsolicited
/// re-render.
#[cfg(feature = "pro")]
#[test]
fn form_events_mark_dirty_and_rerender() {
    use pppdf::types::{FieldKind, FormEvent, FormField};

    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    let form_pdf = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/form.pdf");
    cmd_tx.send(PdfCmd::Load { path: form_pdf, password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { has_forms, .. } => assert!(has_forms),
        other => panic!("expected Loaded, got {other:?}"),
    }
    // Establish the visible-render memo + get field rects.
    cmd_tx.send(PdfCmd::Render { page: 0, px: SizeU { w: 612, h: 792 }, rot: 0, visible: true }).unwrap();
    let fields: Vec<FormField> = match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Rendered { fields, .. } => fields,
        other => panic!("expected Rendered, got {other:?}"),
    };
    let center = |f: &FormField| {
        ((f.rect_pt.left + f.rect_pt.right) / 2.0, (f.rect_pt.top + f.rect_pt.bottom) / 2.0)
    };
    let text = fields.iter().find(|f| f.kind == FieldKind::Text).expect("a text field");
    let checkbox = fields.iter().find(|f| f.kind == FieldKind::Other).expect("a checkbox widget");
    let (cx, cy) = center(text);
    let (bx, by) = center(checkbox);
    // Toggle the checkbox: its value change fires FFI_OnChange -> FormDirty.
    cmd_tx.send(PdfCmd::Form { page: 0, ev: FormEvent::MouseDown { x: bx, y: by, modifiers: 0 } }).unwrap();
    cmd_tx.send(PdfCmd::Form { page: 0, ev: FormEvent::MouseUp { x: bx, y: by, modifiers: 0 } }).unwrap();
    // Click into the text field and type 'A': focusing it starts the caret
    // blink, whose FFI_Invalidate drives an unsolicited visible re-render.
    cmd_tx.send(PdfCmd::Form { page: 0, ev: FormEvent::MouseDown { x: cx, y: cy, modifiers: 0 } }).unwrap();
    cmd_tx.send(PdfCmd::Form { page: 0, ev: FormEvent::MouseUp { x: cx, y: cy, modifiers: 0 } }).unwrap();
    cmd_tx.send(PdfCmd::Form { page: 0, ev: FormEvent::Char { c: 'A' as u16, modifiers: 0 } }).unwrap();
    // Expect FormDirty and at least one unsolicited Rendered within the batch replies.
    let (mut dirty, mut rerendered) = (false, false);
    let deadline = std::time::Instant::now() + WAIT;
    while std::time::Instant::now() < deadline && !(dirty && rerendered) {
        match reply_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(PdfReply::FormDirty) => dirty = true,
            Ok(PdfReply::Rendered { .. }) => rerendered = true,
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(dirty, "form input must produce FormDirty");
    assert!(rerendered, "form input must trigger an unsolicited visible re-render");
    cmd_tx.send(PdfCmd::Form { page: 0, ev: FormEvent::KillFocus }).unwrap();
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
}

/// The full pro loop: click + type into FullName, toggle the Married
/// checkbox, save in place, reload from disk, and read the values back.
///
/// AUTHORING NOTE (fixture-empirical, do not weaken): the fixture's
/// `ResetButton` resets the whole form (including FullName) when clicked, so
/// this test must NOT blanket-click every `Other`-kind widget — it clicks
/// only the specific widget identified as the Married checkbox. Order is
/// FullName click+type, then the Married click; since ResetButton is never
/// touched, the relative order of those two doesn't interact with the reset
/// action.
#[cfg(feature = "pro")]
#[test]
fn fill_save_reload_roundtrip() {
    use pppdf::types::{FieldKind, FormEvent, FormField};

    // Work on a copy — the test saves in place.
    let src =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/form.pdf");
    let work = std::env::temp_dir().join(format!("pppdf-roundtrip-{}.pdf", std::process::id()));
    std::fs::copy(&src, &work).unwrap();

    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    cmd_tx.send(PdfCmd::Load { path: work.clone(), password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { .. } => {}
        other => panic!("expected Loaded, got {other:?}"),
    }
    cmd_tx
        .send(PdfCmd::Render { page: 0, px: SizeU { w: 612, h: 792 }, rot: 0, visible: true })
        .unwrap();
    let fields: Vec<FormField> = match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Rendered { fields, .. } => fields,
        other => panic!("expected Rendered, got {other:?}"),
    };
    let center = |f: &FormField| {
        ((f.rect_pt.left + f.rect_pt.right) / 2.0, (f.rect_pt.top + f.rect_pt.bottom) / 2.0)
    };

    // Highest text field on the page = FullName (fields are page-ordered;
    // rank by rect top descending to be layout-robust).
    let mut texts: Vec<&FormField> = fields.iter().filter(|f| f.kind == FieldKind::Text).collect();
    texts.sort_by(|a, b| b.rect_pt.top.total_cmp(&a.rect_pt.top));
    let fullname = texts[0];
    let (cx, cy) = center(fullname);

    // Identify Married. A rect-shape heuristic (near-square, smallest area)
    // is AMBIGUOUS here: the fixture has three identically-sized 20x20
    // Other-kind widgets — the two Gender radio options (Male/Female, side
    // by side in the same row) plus the Married checkbox (alone in its own
    // row) — so "smallest near-square" ties across all three and doesn't
    // pick Married reliably.
    //
    // Pinned instead by dumping tests/fixtures/form.pdf's raw object
    // structure (uncompressed): page /Annots order is
    // [FullName, ID, Gender-Male(17), Gender-Female(18), Married(7), City,
    // Language, Notes, ResetButton], with rects:
    //   17 (Gender/Male):   Rect[202.66 594.78 222.66 614.78]  (top 614.78)
    //   18 (Gender/Female): Rect[277.66 594.78 297.66 614.78]  (top 614.78)
    //   7  (Married):       Rect[202.66 554.78 222.66 574.78]  (top 574.78)
    // The two Gender widgets share a row (same top); Married is the only
    // near-square Other widget whose row has no sibling — that's the
    // structural signal pinned below, confirmed empirically: clicking the
    // rect at top~614.78 left~202.66 (a Gender option) left Married==false
    // and Gender=="Off" (a first RED run of this test caught exactly that).
    let others: Vec<&FormField> = fields.iter().filter(|f| f.kind == FieldKind::Other).collect();
    let squares: Vec<&FormField> = others
        .iter()
        .filter(|f| {
            let w = f.rect_pt.right - f.rect_pt.left;
            let h = f.rect_pt.top - f.rect_pt.bottom;
            h > 0.0 && w > 0.0 && (w / h) >= 0.5 && (w / h) <= 2.0
        })
        .copied()
        .collect();
    let married = squares
        .iter()
        .find(|f| squares.iter().filter(|g| (g.rect_pt.top - f.rect_pt.top).abs() < 1.0).count() == 1)
        .copied()
        .unwrap_or_else(|| {
            for f in &fields {
                eprintln!("field kind={:?} rect={:?}", f.kind, f.rect_pt);
            }
            panic!("Married checkbox (the unpaired near-square Other widget) not found");
        });
    let (mx, my) = center(married);

    for ev in [
        FormEvent::MouseDown { x: cx, y: cy, modifiers: 0 },
        FormEvent::MouseUp { x: cx, y: cy, modifiers: 0 },
        FormEvent::Char { c: 'A' as u16, modifiers: 0 },
        FormEvent::Char { c: 'b' as u16, modifiers: 0 },
    ] {
        cmd_tx.send(PdfCmd::Form { page: 0, ev }).unwrap();
    }
    // Kill focus before moving to a different widget: this both commits
    // FullName's typed text (matching how save() itself force-kills focus)
    // and stops the caret-blink timer FullName's focus started. Skipping
    // this and clicking straight into Married was tried first and produced
    // a sustained flood of unsolicited Rendered replies (observed: a stuck
    // multi-minute run pegging a CPU core with continuous re-renders) —
    // i.e. a real hazard when switching focus between widgets without an
    // intervening commit, not just test flakiness. See task-4-report.md.
    cmd_tx.send(PdfCmd::Form { page: 0, ev: FormEvent::KillFocus }).unwrap();
    for ev in [
        FormEvent::MouseDown { x: mx, y: my, modifiers: 0 },
        FormEvent::MouseUp { x: mx, y: my, modifiers: 0 },
    ] {
        cmd_tx.send(PdfCmd::Form { page: 0, ev }).unwrap();
    }

    cmd_tx.send(PdfCmd::SaveDoc).unwrap();
    let deadline = std::time::Instant::now() + WAIT;
    let mut saved = false;
    while std::time::Instant::now() < deadline && !saved {
        match reply_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(PdfReply::Saved { result }) => {
                result.expect("save must succeed");
                saved = true;
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(saved, "no Saved reply");
    cmd_tx.send(PdfCmd::Shutdown).unwrap();

    let values = pppdf::pro::forms::read_field_values(&work).unwrap();
    assert_eq!(values.get("FullName"), Some(&Some("Ab".to_string())));
    assert_eq!(values.get("Married"), Some(&Some("true".to_string())));
    std::fs::remove_file(&work).ok();
}

/// Regression for the focus-transition render flood (see
/// .superpowers/sdd/flood-investigation.md, dated 2026-07-09): PDFium keeps
/// a caret-blink timer alive across a focus transition (clicking straight
/// from one widget into another, without an intervening `KillFocus`), and
/// if that timer's requested interval is ~0 the old code clamped it to 1ms
/// and rendered the full page on every tick — a CPU-pegging flood measured
/// at 1363 Rendered replies in 3s (454/s) with a forced tiny interval.
///
/// This test drives the NATURAL trigger (no forced interval): click into
/// the top-most Text field (starts the caret timer), then click a
/// near-square `Other` widget WITHOUT sending `KillFocus` in between (the
/// investigation confirmed a widget click alone does not kill the caret
/// timer). Whatever interval this environment's pdfium.dll actually
/// requests, the fix must bound the render rate to ~30/s (1 per 33ms); we
/// assert a generous ceiling of 80 renders in 2s (vs. the 900+ that a
/// pegged-core flood at ~454/s would produce in the same window).
#[cfg(feature = "pro")]
#[test]
fn focus_transition_without_killfocus_does_not_flood_renders() {
    use pppdf::types::{FieldKind, FormEvent, FormField};

    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    let form_pdf = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/form.pdf");
    cmd_tx.send(PdfCmd::Load { path: form_pdf, password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { has_forms, .. } => assert!(has_forms),
        other => panic!("expected Loaded, got {other:?}"),
    }
    cmd_tx
        .send(PdfCmd::Render { page: 0, px: SizeU { w: 612, h: 792 }, rot: 0, visible: true })
        .unwrap();
    let fields: Vec<FormField> = match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Rendered { fields, .. } => fields,
        other => panic!("expected Rendered, got {other:?}"),
    };
    let center = |f: &FormField| {
        ((f.rect_pt.left + f.rect_pt.right) / 2.0, (f.rect_pt.top + f.rect_pt.bottom) / 2.0)
    };

    // Top-most Text field (same idiom as fill_save_reload_roundtrip's
    // FullName identification: rank by rect top descending).
    let mut texts: Vec<&FormField> = fields.iter().filter(|f| f.kind == FieldKind::Text).collect();
    texts.sort_by(|a, b| b.rect_pt.top.total_cmp(&a.rect_pt.top));
    let text_field = texts[0];
    let (tx, ty) = center(text_field);

    // The unpaired near-square Other widget (same heuristic as
    // fill_save_reload_roundtrip's Married identification: near-square,
    // and the only one in its row — the two Gender radio options share a
    // row and are excluded by that "only one at this top" test).
    let others: Vec<&FormField> = fields.iter().filter(|f| f.kind == FieldKind::Other).collect();
    let squares: Vec<&FormField> = others
        .iter()
        .filter(|f| {
            let w = f.rect_pt.right - f.rect_pt.left;
            let h = f.rect_pt.top - f.rect_pt.bottom;
            h > 0.0 && w > 0.0 && (w / h) >= 0.5 && (w / h) <= 2.0
        })
        .copied()
        .collect();
    let other_widget = squares
        .iter()
        .find(|f| squares.iter().filter(|g| (g.rect_pt.top - f.rect_pt.top).abs() < 1.0).count() == 1)
        .copied()
        .unwrap_or_else(|| {
            for f in &fields {
                eprintln!("field kind={:?} rect={:?}", f.kind, f.rect_pt);
            }
            panic!("unpaired near-square Other widget not found");
        });
    let (ox, oy) = center(other_widget);

    // Focus the text field: starts PDFium's caret-blink timer.
    cmd_tx.send(PdfCmd::Form { page: 0, ev: FormEvent::MouseDown { x: tx, y: ty, modifiers: 0 } }).unwrap();
    cmd_tx.send(PdfCmd::Form { page: 0, ev: FormEvent::MouseUp { x: tx, y: ty, modifiers: 0 } }).unwrap();
    // Click straight into the Other widget WITHOUT an intervening
    // KillFocus — the natural trigger for the flood (a widget click alone
    // does not kill the caret timer per the investigation).
    cmd_tx.send(PdfCmd::Form { page: 0, ev: FormEvent::MouseDown { x: ox, y: oy, modifiers: 0 } }).unwrap();
    cmd_tx.send(PdfCmd::Form { page: 0, ev: FormEvent::MouseUp { x: ox, y: oy, modifiers: 0 } }).unwrap();

    // Drain replies for a bounded window, counting Rendered messages.
    let drain_window = Duration::from_secs(2);
    let deadline = std::time::Instant::now() + drain_window;
    let mut rendered_count = 0u32;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match reply_rx.recv_timeout(remaining) {
            Ok(PdfReply::Rendered { .. }) => rendered_count += 1,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert!(
        rendered_count <= 80,
        "focus transition without KillFocus flooded {rendered_count} Rendered replies in {drain_window:?} \
         (pre-fix measured 1363 renders / 3s = 454/s with a forced tiny timer interval; \
         bounded ceiling is 80 for a 2s window at the ~30/s throttle)"
    );

    cmd_tx.send(PdfCmd::Form { page: 0, ev: FormEvent::KillFocus }).unwrap();
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

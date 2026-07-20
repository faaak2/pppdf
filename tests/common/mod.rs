//! Builds a small fixture PDF entirely in memory: 3 US-Letter pages, each
//! with a filled pure-red rectangle (red is asymmetric across channels, so a
//! render proves BGRA vs RGBA byte order — black could not); page 1 has a
//! GoTo link and a URI link; outline: "Chapter One" -> p2, "Chapter Two" ->
//! p3 with child "Section 2.1". Object numbers are fixed; byte offsets for
//! the xref are computed, never hand-counted.

pub fn build_fixture_pdf() -> Vec<u8> {
    fn stream_obj(content: &str) -> String {
        format!("<< /Length {} >>\nstream\n{}\nendstream", content.len(), content)
    }

    let objs: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 9 0 R >>".into()),
        (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>".into()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R \
             /Annots [13 0 R 14 0 R] >>"
                .into(),
        ),
        (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R >>".into()),
        (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 8 0 R >>".into()),
        (6, stream_obj("1 0 0 rg 100 600 200 100 re f")),
        (7, stream_obj("1 0 0 rg 100 500 300 150 re f")),
        (8, stream_obj("1 0 0 rg 200 300 150 300 re f")),
        (9, "<< /Type /Outlines /First 10 0 R /Last 11 0 R /Count 3 >>".into()),
        (
            10,
            "<< /Title (Chapter One) /Parent 9 0 R /Next 11 0 R \
             /Dest [4 0 R /XYZ null null null] >>"
                .into(),
        ),
        (
            11,
            "<< /Title (Chapter Two) /Parent 9 0 R /Prev 10 0 R /First 12 0 R \
             /Last 12 0 R /Count 1 /Dest [5 0 R /XYZ null null null] >>"
                .into(),
        ),
        (
            12,
            "<< /Title (Section 2.1) /Parent 11 0 R /Dest [5 0 R /XYZ null null null] >>".into(),
        ),
        (
            13,
            "<< /Type /Annot /Subtype /Link /Rect [72 700 200 720] /Border [0 0 0] \
             /A << /S /GoTo /D [5 0 R /XYZ null null null] >> >>"
                .into(),
        ),
        (
            14,
            "<< /Type /Annot /Subtype /Link /Rect [300 650 500 670] /Border [0 0 0] \
             /A << /S /URI /URI (https://example.com/) >> >>"
                .into(),
        ),
    ];

    serialize_pdf(&objs)
}

/// One 100x1000pt page (10:1 aspect, like a receipt or banner). Regression
/// fixture for rotated rendering: high aspect ratios amplify any rounding
/// drift between the requested and returned pixel sizes.
pub fn build_tall_pdf() -> Vec<u8> {
    fn stream_obj(content: &str) -> String {
        format!("<< /Length {} >>\nstream\n{}\nendstream", content.len(), content)
    }
    let objs: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 1000] /Contents 4 0 R >>".into()),
        (4, stream_obj("1 0 0 rg 10 900 80 80 re f")),
    ];
    serialize_pdf(&objs)
}

fn serialize_pdf(objs: &[(u32, String)]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![0usize; objs.len() + 1];
    for (id, body) in objs {
        offsets[*id as usize] = out.len();
        out.extend_from_slice(format!("{id} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_pos = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objs.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for id in 1..=objs.len() {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[id]).as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objs.len() + 1,
            xref_pos
        )
        .as_bytes(),
    );
    out
}

/// Write the fixture to a temp file and return its path.
pub fn fixture_path() -> std::path::PathBuf {
    let path = std::env::temp_dir().join("pppdf_fixture.pdf");
    std::fs::write(&path, build_fixture_pdf()).expect("write fixture pdf");
    path
}

/// Write the tall 10:1 fixture to a temp file and return its path.
pub fn tall_fixture_path() -> std::path::PathBuf {
    let path = std::env::temp_dir().join("pppdf_tall_fixture.pdf");
    std::fs::write(&path, build_tall_pdf()).expect("write tall fixture pdf");
    path
}

/// Three pages of real text for search tests (Helvetica, a standard-14
/// font — nothing to embed). Ground truth for the query "dragon":
///   page 0: "The dragon sleeps" / "Another dragon here"  -> 2 hits
///   page 1: "Nothing to see here"                        -> 0 hits
///   page 2: "Dragon rider" / "dragonfire burns"          -> case/word probes
/// Totals: default (insensitive substring) = 4; match_case = 3 (drops
/// "Dragon"); whole_word = 3 (drops "dragonfire"); both = 2.
pub fn build_text_pdf() -> Vec<u8> {
    fn stream_obj(content: &str) -> String {
        format!("<< /Length {} >>\nstream\n{}\nendstream", content.len(), content)
    }
    let font = "/Resources << /Font << /F1 9 0 R >> >>";
    let objs: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
        (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>".into()),
        (3, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R {font} >>")),
        (4, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R {font} >>")),
        (5, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 8 0 R {font} >>")),
        (
            6,
            stream_obj(
                "BT /F1 24 Tf 72 700 Td (The dragon sleeps) Tj 0 -50 Td (Another dragon here) Tj ET",
            ),
        ),
        (7, stream_obj("BT /F1 24 Tf 72 700 Td (Nothing to see here) Tj ET")),
        (
            8,
            stream_obj(
                "BT /F1 24 Tf 72 700 Td (Dragon rider) Tj 0 -50 Td (dragonfire burns) Tj ET",
            ),
        ),
        (9, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into()),
    ];
    serialize_pdf(&objs)
}

/// Write the text fixture to a temp file and return its path.
pub fn text_fixture_path() -> std::path::PathBuf {
    let path = std::env::temp_dir().join("pppdf_text_fixture.pdf");
    std::fs::write(&path, build_text_pdf()).expect("write text fixture pdf");
    path
}

/// A WIDE fixture for genuinely-mid-scan tests: `n` pages, a "dragon" hit
/// on page 0 only. The first SearchHits arrives almost immediately while
/// the scan still has n-1 pages to cover — long enough (at 200 pages,
/// hundreds of ms) that a CancelSearch sent after that first hit reliably
/// lands mid-scan. Object layout: 1 catalog, 2 pages, 3..3+n page objects,
/// 3+n..3+2n content streams, 3+2n the font — contiguous ids, as
/// serialize_pdf's xref builder requires.
pub fn build_wide_text_pdf(n: u32) -> Vec<u8> {
    fn stream_obj(content: &str) -> String {
        format!("<< /Length {} >>\nstream\n{}\nendstream", content.len(), content)
    }
    let font_id = 3 + 2 * n;
    let kids: String =
        (0..n).map(|i| format!("{} 0 R", 3 + i)).collect::<Vec<_>>().join(" ");
    let mut objs: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
        (2, format!("<< /Type /Pages /Kids [{kids}] /Count {n} >>")),
    ];
    for i in 0..n {
        objs.push((
            3 + i,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R \
                 /Resources << /Font << /F1 {font_id} 0 R >> >> >>",
                3 + n + i
            ),
        ));
    }
    for i in 0..n {
        let text =
            if i == 0 { "Here be a dragon".to_string() } else { format!("Filler page {i}") };
        objs.push((3 + n + i, stream_obj(&format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET"))));
    }
    objs.push((font_id, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into()));
    serialize_pdf(&objs)
}

/// Write the 200-page wide fixture to a temp file and return its path.
pub fn wide_text_fixture_path() -> std::path::PathBuf {
    let path = std::env::temp_dir().join("pppdf_wide_text_fixture.pdf");
    std::fs::write(&path, build_wide_text_pdf(200)).expect("write wide text fixture pdf");
    path
}

/// A TWO-page AcroForm, each page one text field ("f0"/"f1") at the same
/// rect. `second_h` sets page 2's height: pass 792 for two identical-size
/// pages (a flip to page 2 is then a pure cache hit — the precondition for
/// the stale-`last_visible` render bug), or a different height to exercise
/// the mixed-size path where the stale view also carries the wrong px.
/// `NeedAppearances` makes PDFium regenerate the field appearance so typed
/// glyphs render.
#[cfg(feature = "pro")]
pub fn build_two_page_form_pdf(second_h: u32) -> Vec<u8> {
    fn stream_obj(content: &str) -> String {
        format!("<< /Length {} >>\nstream\n{}\nendstream", content.len(), content)
    }
    let widget = |id: u32, name: &str, page_ref: u32, top: u32, bottom: u32| {
        (
            id,
            format!(
                "<< /Type /Annot /Subtype /Widget /FT /Tx /T ({name}) /Ff 0 \
                 /Rect [100 {bottom} 500 {top}] /P {page_ref} 0 R /DA (/Helv 12 Tf 0 g) >>"
            ),
        )
    };
    // Field near the top of each page, valid whatever the page height.
    let (p1_top, p1_bot) = (752, 712);
    let (p2_top, p2_bot) = (second_h - 40, second_h - 80);
    let objs: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 9 0 R >>".into()),
        (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".into()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R \
             /Annots [7 0 R] >>"
                .into(),
        ),
        (
            4,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 {second_h}] /Contents 6 0 R \
                 /Annots [8 0 R] >>"
            ),
        ),
        (5, stream_obj("0 0 1 rg 40 40 60 60 re f")),
        (6, stream_obj("0 0 1 rg 40 40 60 60 re f")),
        widget(7, "f0", 3, p1_top, p1_bot),
        widget(8, "f1", 4, p2_top, p2_bot),
        (
            9,
            "<< /Fields [7 0 R 8 0 R] /NeedAppearances true /DA (/Helv 12 Tf 0 g) \
             /DR << /Font << /Helv 10 0 R >> >> >>"
                .into(),
        ),
        (10, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Name /Helv >>".into()),
    ];
    serialize_pdf(&objs)
}

/// Write a two-page form fixture (page 2 height `second_h`) and return its
/// path, keyed by height so same-size and mixed-size variants don't collide.
#[cfg(feature = "pro")]
pub fn two_page_form_fixture_path(second_h: u32) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("pppdf_two_page_form_{second_h}.pdf"));
    std::fs::write(&path, build_two_page_form_pdf(second_h)).expect("write two-page form fixture");
    path
}

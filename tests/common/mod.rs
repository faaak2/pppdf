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

    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![0usize; objs.len() + 1];
    for (id, body) in &objs {
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

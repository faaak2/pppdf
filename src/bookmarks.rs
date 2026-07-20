//! Custom-bookmark file format and display ordering. Pure logic — no OS
//! types, no PDFium. The attachment is written by `pro::bookmarks` and
//! read by `pdf::describe()` (both editions).
//!
//! Format (UTF-8 text, attachment `pppdf.bookmarks`):
//!   pppdf-bookmarks v1
//!   12<TAB>Fireball table
//!   70<TAB>Chapter 4 notes
//! Page numbers are 1-BASED in the file (human-readable if extracted);
//! 0-based everywhere in code. Names can't contain tabs/newlines — the
//! WM_CHAR intake filter (`c >= 0x20`) guarantees it, and parse() strips
//! control chars anyway.

use crate::types::Bookmark;
use std::cmp::Ordering;

pub const ATTACHMENT_NAME: &str = "pppdf.bookmarks";
pub const HEADER: &str = "pppdf-bookmarks v1";
/// Max bookmark-name length in chars (the entry prompt enforces it too).
pub const NAME_CAP: usize = 64;

/// Default name committed on an empty Enter: "Page N" (1-based).
pub fn default_name(page: u16) -> String {
    format!("Page {}", page as u32 + 1)
}

pub fn serialize(bookmarks: &[Bookmark]) -> Vec<u8> {
    let mut out = String::with_capacity(32 + bookmarks.len() * 24);
    out.push_str(HEADER);
    out.push('\n');
    for b in bookmarks {
        out.push_str(&format!("{}\t{}\n", b.page as u32 + 1, b.name));
    }
    out.into_bytes()
}

/// Lenient parse. Malformed lines are skipped; out-of-range pages are
/// DROPPED (clamping could collide with a real bookmark on the last
/// page); names are control-stripped and capped; one bookmark per page
/// (first wins); result is page-sorted. An unknown header version reads
/// as empty — v1 writers then replace it wholesale, which is the
/// documented forward-compat contract.
pub fn parse(bytes: &[u8], pages: u16) -> Vec<Bookmark> {
    let Ok(text) = std::str::from_utf8(bytes) else { return Vec::new() };
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(HEADER) {
        return Vec::new();
    }
    let mut out: Vec<Bookmark> = Vec::new();
    for line in lines {
        let Some((page_s, raw_name)) = line.split_once('\t') else { continue };
        let Ok(page_1) = page_s.trim().parse::<u32>() else { continue };
        if page_1 == 0 || page_1 > pages as u32 {
            continue;
        }
        let page = (page_1 - 1) as u16;
        let name: String = raw_name.chars().filter(|c| !c.is_control()).take(NAME_CAP).collect();
        if name.is_empty() || out.iter().any(|b| b.page == page) {
            continue;
        }
        out.push(Bookmark { page, name });
    }
    out.sort_by_key(|b| b.page);
    out
}

/// Natural, case-insensitive name comparison: runs of ASCII digits
/// compare NUMERICALLY (leading zeros ignored, so "2" < "10" and
/// "007" == "7"), everything else compares by lowercased chars. Only
/// ASCII digits are numeric; Unicode digits compare as text.
pub fn natural_name_cmp(a: &str, b: &str) -> Ordering {
    fn take_digits(it: &mut std::iter::Peekable<std::str::Chars>) -> String {
        let mut s = String::new();
        while let Some(&c) = it.peek() {
            if c.is_ascii_digit() {
                s.push(c);
                it.next();
            } else {
                break;
            }
        }
        s
    }
    let (mut ia, mut ib) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ia.peek().copied(), ib.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let ra = take_digits(&mut ia);
                    let rb = take_digits(&mut ib);
                    let (ta, tb) = (ra.trim_start_matches('0'), rb.trim_start_matches('0'));
                    // More significant digits = bigger number; equal length
                    // compares digit-lexically (same as numerically).
                    let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                    if ord != Ordering::Equal {
                        return ord;
                    }
                } else {
                    let ord = ca.to_lowercase().cmp(cb.to_lowercase());
                    if ord != Ordering::Equal {
                        return ord;
                    }
                    ia.next();
                    ib.next();
                }
            }
        }
    }
}

/// Total display order for the TOC's Bookmarks section: natural name
/// order, ties broken by page (unique per set), then raw name.
pub fn display_cmp(a: &Bookmark, b: &Bookmark) -> Ordering {
    natural_name_cmp(&a.name, &b.name)
        .then_with(|| a.page.cmp(&b.page))
        .then_with(|| a.name.cmp(&b.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bm(page: u16, name: &str) -> Bookmark {
        Bookmark { page, name: name.into() }
    }

    #[test]
    fn serialize_writes_header_and_1_based_tab_lines() {
        let bytes = serialize(&[bm(11, "Fireball table"), bm(69, "Chapter 4 notes")]);
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text, "pppdf-bookmarks v1\n12\tFireball table\n70\tChapter 4 notes\n");
    }

    #[test]
    fn parse_round_trips_serialize() {
        let set = vec![bm(0, "Alpha"), bm(11, "Beta"), bm(499, "Gamma")];
        assert_eq!(parse(&serialize(&set), 500), set);
    }

    #[test]
    fn empty_set_serializes_to_header_only_and_parses_back_empty() {
        let bytes = serialize(&[]);
        assert_eq!(String::from_utf8(bytes.clone()).unwrap(), "pppdf-bookmarks v1\n");
        assert!(parse(&bytes, 10).is_empty());
    }

    #[test]
    fn parse_is_lenient_and_drops_bad_lines() {
        let text = "pppdf-bookmarks v1\n\
                    5\tGood\n\
                    no-tab-line\n\
                    x\tNot a number\n\
                    0\tPage zero is invalid (1-based file)\n\
                    999\tOut of range dropped, not clamped\n\
                    5\tDuplicate page: first wins\n\
                    7\t\n\
                    8\tKeep\n";
        let got = parse(text.as_bytes(), 10);
        assert_eq!(got, vec![bm(4, "Good"), bm(7, "Keep")]);
    }

    #[test]
    fn parse_unknown_header_reads_empty() {
        assert!(parse(b"pppdf-bookmarks v2\n5\tFuture", 10).is_empty());
        assert!(parse(b"", 10).is_empty());
        assert!(parse(b"\xff\xfe not utf8 \xff", 10).is_empty());
    }

    #[test]
    fn parse_strips_control_chars_and_caps_names() {
        let long = "x".repeat(100);
        let text = format!("pppdf-bookmarks v1\n3\tA\u{7}B\n4\t{long}\n");
        let got = parse(text.as_bytes(), 10);
        assert_eq!(got[0].name, "AB");
        assert_eq!(got[1].name.chars().count(), NAME_CAP);
    }

    #[test]
    fn parse_output_is_page_sorted() {
        let text = "pppdf-bookmarks v1\n9\tLast\n2\tFirst\n5\tMid\n";
        let pages: Vec<u16> = parse(text.as_bytes(), 10).iter().map(|b| b.page).collect();
        assert_eq!(pages, vec![1, 4, 8]);
    }

    #[test]
    fn natural_sort_orders_numbers_numerically() {
        // The user's exact requirement: "1 - something" above "10 - something".
        assert_eq!(natural_name_cmp("1 - something", "10 - something"), Ordering::Less);
        assert_eq!(natural_name_cmp("2 - x", "10 - x"), Ordering::Less);
        assert_eq!(natural_name_cmp("Page 7", "Page 10"), Ordering::Less);
        assert_eq!(natural_name_cmp("Page 10", "Page 7"), Ordering::Greater);
    }

    #[test]
    fn natural_sort_ignores_case_and_leading_zeros() {
        assert_eq!(natural_name_cmp("alpha", "ALPHA"), Ordering::Equal);
        assert_eq!(natural_name_cmp("Beta", "alpha"), Ordering::Greater);
        assert_eq!(natural_name_cmp("007", "7"), Ordering::Equal);
        assert_eq!(natural_name_cmp("ch007 end", "ch7 start"), Ordering::Less, "e < s");
    }

    #[test]
    fn natural_sort_mixed_segments_and_prefixes() {
        assert_eq!(natural_name_cmp("ch2", "ch2b"), Ordering::Less, "prefix sorts first");
        assert_eq!(natural_name_cmp("a10b2", "a10b10"), Ordering::Less);
        assert_eq!(natural_name_cmp("12", "abc"), Ordering::Less, "digit < letter (ASCII)");
        // Non-ASCII digits deliberately compare as text, per the doc comment.
        assert_eq!(natural_name_cmp("٢", "١٠"), Ordering::Greater, "Arabic-Indic digits are text");
    }

    #[test]
    fn display_cmp_breaks_name_ties_by_page() {
        let (a, b) = (bm(3, "Same"), bm(7, "same"));
        assert_eq!(display_cmp(&a, &b), Ordering::Less);
        let mut v = vec![bm(7, "same"), bm(3, "Same")];
        v.sort_by(display_cmp);
        assert_eq!(v[0].page, 3);
    }

    #[test]
    fn default_name_is_1_based() {
        assert_eq!(default_name(0), "Page 1");
        assert_eq!(default_name(41), "Page 42");
    }
}

# Custom Bookmarks (Pro) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ctrl+B toggles a named bookmark on the current page (Pro); bookmarks are stored inside the PDF as a `pppdf.bookmarks` attachment, auto-saved on toggle without dragging unsaved form edits along, and shown name-sorted in a "Bookmarks" section at the top of the TOC panel (read-only in the free edition).

**Architecture:** A shared pure module (`src/bookmarks.rs`) owns the line-based file format and natural-sort comparator. The PDF thread reads the attachment at load (`describe()`, both editions) and — Pro only — writes it via a double-write: update the attachment on the live in-memory document (so Ctrl+S form saves carry it) *and* shadow-load the on-disk bytes, update, and atomically save (so form edits never leak into a bookmark save). The UI keeps bookmarks on `DocState`, drives toggle/name-entry through pure `App` methods modeled on the goto-page prompt, and renders the section by prepending synthetic `OutlineItem`s in `toggle_overlay`.

**Tech Stack:** Rust, windows-rs (Win32/Direct2D), vendored pdfium-render 0.9.2 (attachment API already exposed — no new vendor patches). Zero new dependencies.

**Spec:** `docs/superpowers/specs/2026-07-20-custom-bookmarks-design.md` — read it before starting any task.

## Global Constraints

- **Zero new dependencies.** `windows` and `pdfium-render` stay the only runtime deps (`Cargo.toml` is not touched).
- **Both editions must build and test green at the end of every task:** `cargo build && cargo test` and `cargo build --features pro && cargo test --features pro`. A fresh clone needs `.\fetch-pdfium.ps1` once before anything compiles; `assets/pdfium.dll` is already present in this working tree.
- **Pro gating:** Pro-only *logic* lives in `src/pro/` (gated once in `lib.rs`). Small cfg'd fields/match-arms in `app.rs`/`window.rs`/`pdf.rs`/`d2d.rs`/`types.rs` are the accepted seam pattern (see OVERVIEW.md "Free vs Pro"). The free exe must not contain Pro strings — the free Ctrl+B toast lives in a `#[cfg(not(feature = "pro"))]` arm.
- **`vendor/` is untouchable.** The attachment API needed (`attachments()`, `attachments_mut()`, `create_attachment_from_bytes`, `delete_at_index`, `PdfAttachment::name()/save_to_bytes()`) already exists in the vendored copy — verified.
- **Protocol invariants:** exactly ONE `BookmarksSaved` reply per `SaveBookmarks` command (never coalesced, never silently dropped — `saves_inflight` counts on it); `SaveBookmarks` executes against the pre-`Load` document (path guard turns any mismatch into `Err`, never a wrong-document write).
- **Exact user-visible strings** (copy verbatim): toast `Bookmark removed`, toast `Bookmarked: {name}`, toast `Couldn't save bookmarks`, free-edition toast `Custom bookmarks are a pppdf Pro feature`, TOC header row `Bookmarks`, default name `Page {N}` (1-based), README row `Add / remove a bookmark on this page`.
- **File format** (attachment `pppdf.bookmarks`, UTF-8): header line `pppdf-bookmarks v1`, then one `page<TAB>name` line per bookmark, page numbers 1-based in the file, 0-based everywhere in code. Lenient parse; out-of-range pages dropped; unknown header → empty.
- **Integration tests** touching PDFium must hold `PDFIUM_TEST_LOCK` for their whole body and call `join_pdf_thread()` before releasing it (see `tests/pdf_integration.rs:14-41`). Never send `Shutdown` before awaiting all expected replies.
- **No console:** use `crate::dlog!` for tracing, never `println!`.

## File Structure

| File | Role in this feature |
|---|---|
| `src/bookmarks.rs` (create) | Shared pure logic: `ATTACHMENT_NAME`, `HEADER`, `NAME_CAP`, `serialize`, `parse`, `natural_name_cmp`, `display_cmp`, `default_name`. No OS types, no PDFium. |
| `src/pro/bookmarks.rs` (create) | Pro write path: `save_bookmarks` (double-write), `set_attachment` helper. PDFium FFI via pdfium-render high-level API. |
| `src/types.rs` | `Bookmark` struct (shared); `Loaded.bookmarks`; Pro-gated `PdfCmd::SaveBookmarks` / `PdfReply::BookmarksSaved`. |
| `src/lib.rs` | `pub mod bookmarks;` (the single Pro gate is untouched). |
| `src/app.rs` | `DocState.bookmarks` (shared) + `bookmark_entry` (Pro); `doc_loaded` new param; combined list in `toggle_overlay`; Pro toggle/entry methods; tests. |
| `src/overlay.rs` | `pub header_idx: Option<usize>` on `OutlineState`; test for page-less activate. |
| `src/pdf.rs` | `read_bookmarks` in `describe()` (shared); Pro: `current_password` retention, `SaveBookmarks` batch handling + replies. |
| `src/pro/mod.rs` | `pub mod bookmarks;` + `pub(crate) fn atomic_write` (extracted from `FormEngine::save`). |
| `src/pro/forms/mod.rs` | `save()` refactored onto `atomic_write` (behavior identical). |
| `src/window.rs` | Ctrl+B arms (Pro + free), bookmark-entry modal rung, WM_CHAR branch, dismiss sites, `modal_active`, `bookmark_toggle_ui`, `send_bookmarks`, `show_toast_centered` + `badge_centered`, `BookmarksSaved` reply arm. |
| `src/d2d.rs` | `draw_badge` centered variant; `draw_bookmark_entry` (Pro); header-row styling in `draw_overlay`; `PRO_HELP_ROWS` row. |
| `tests/pdf_integration.rs` | 5 new Pro integration tests. |
| `README.md`, `docs/manual-tests.md`, `TODO.md` | Doc sync (Task 8). |

---

### Task 1: `Bookmark` type + format/sort module

**Files:**
- Modify: `src/types.rs` (top, after the `OutlineItem` block ending line 12)
- Create: `src/bookmarks.rs`
- Modify: `src/lib.rs:2` (add module line)

**Interfaces:**
- Consumes: `crate::types::Bookmark` (defined here).
- Produces (used by every later task):
  - `types::Bookmark { pub page: u16, pub name: String }` — `#[derive(Clone, Debug, PartialEq)]`, `page` 0-based.
  - `bookmarks::ATTACHMENT_NAME: &str = "pppdf.bookmarks"`
  - `bookmarks::NAME_CAP: usize = 64`
  - `bookmarks::serialize(bookmarks: &[Bookmark]) -> Vec<u8>`
  - `bookmarks::parse(bytes: &[u8], pages: u16) -> Vec<Bookmark>` (page-sorted, deduped first-wins, lenient)
  - `bookmarks::natural_name_cmp(a: &str, b: &str) -> std::cmp::Ordering`
  - `bookmarks::display_cmp(a: &Bookmark, b: &Bookmark) -> std::cmp::Ordering`
  - `bookmarks::default_name(page: u16) -> String` → `"Page {page+1}"`

- [ ] **Step 1: Add the `Bookmark` type to `src/types.rs`**

Insert directly after the `OutlineItem` struct (after line 12), before the `use crate::layout::...` line:

```rust
/// One user-created bookmark (Pro creates them; both editions display
/// them). Persisted inside the PDF as the `pppdf.bookmarks` attachment —
/// see `crate::bookmarks` for the format.
#[derive(Clone, Debug, PartialEq)]
pub struct Bookmark {
    /// 0-based page index.
    pub page: u16,
    pub name: String,
}
```

- [ ] **Step 2: Register the new module in `src/lib.rs`**

After `pub mod app;` (line 2) add:

```rust
pub mod bookmarks;
```

- [ ] **Step 3: Write `src/bookmarks.rs` with failing-first tests included**

Create the file with this exact content (implementation + tests in one step is fine for a brand-new pure module; the test run below is the red/green gate):

```rust
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
```

- [ ] **Step 4: Run the new tests in both editions**

Run: `cargo test bookmarks:: --lib`
Expected: 12 passed.
Run: `cargo test bookmarks:: --lib --features pro`
Expected: 12 passed.
Run: `cargo build && cargo build --features pro`
Expected: both compile (warnings-free for pppdf's own code; two known upstream dead-code warnings from the vendored crate are acceptable on full rebuilds).

- [ ] **Step 5: Commit**

```powershell
git add src/types.rs src/bookmarks.rs src/lib.rs
git commit -m @'
feat(bookmarks): shared Bookmark type, file format and natural sort

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

### Task 2: Load-path plumbing — bookmarks ride `PdfReply::Loaded`

**Files:**
- Modify: `src/types.rs:110-116` (`Loaded` variant)
- Modify: `src/app.rs` (`DocState`, `doc_loaded`, all test call sites)
- Modify: `src/pdf.rs:609-630` (`describe()` + new `read_bookmarks`)
- Modify: `src/window.rs:2170-2179` (`Loaded` reply arm)
- Test: `src/app.rs` (unit), `tests/pdf_integration.rs:53-57` (tiny assert addition)

**Interfaces:**
- Consumes: `bookmarks::{ATTACHMENT_NAME, parse}` (Task 1), `types::Bookmark` (Task 1).
- Produces:
  - `PdfReply::Loaded { path, pages, page_sizes, outline, has_forms, bookmarks: Vec<Bookmark> }`
  - `DocState.bookmarks: Vec<Bookmark>` (unconditional, kept page-sorted)
  - `App::doc_loaded(path, pages, page_sizes, outline, has_forms, bookmarks: Vec<Bookmark>)` — **signature change**; every caller updated in this task.

- [ ] **Step 1: Extend the `Loaded` variant in `src/types.rs`**

```rust
    Loaded {
        path: PathBuf,
        pages: u16,
        page_sizes: Vec<(f32, f32)>,
        outline: Vec<OutlineItem>,
        has_forms: bool,
        /// User bookmarks parsed from the `pppdf.bookmarks` attachment.
        /// Unconditional: the free edition displays them read-only.
        bookmarks: Vec<Bookmark>,
    },
```

(`Bookmark` is in the same file — no import needed.)

- [ ] **Step 2: Add the failing unit test to `src/app.rs`'s shared `tests` module**

Add after `new_document_starts_with_empty_history` (near line 1638). Also add `Bookmark` to app.rs's types import on line 8: `use crate::types::{Bookmark, FieldKind, FormField, LinkTarget, OutlineItem, PageLink};`

```rust
    #[test]
    fn doc_loaded_stores_bookmarks_and_next_load_replaces_them() {
        let mut a = App::new();
        a.set_client(SizeU { w: 1000, h: 1000 });
        a.doc_loaded(
            PathBuf::from("x.pdf"),
            10,
            vec![LETTER; 10],
            vec![],
            false,
            vec![Bookmark { page: 3, name: "Camp".into() }],
        );
        assert_eq!(a.doc().unwrap().bookmarks, vec![Bookmark { page: 3, name: "Camp".into() }]);
        a.doc_loaded(PathBuf::from("y.pdf"), 10, vec![LETTER; 10], vec![], false, vec![]);
        assert!(a.doc().unwrap().bookmarks.is_empty(), "bookmarks die with the document");
    }
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo test doc_loaded_stores_bookmarks --lib`
Expected: FAIL to compile — `this method takes 5 arguments but 6 arguments were supplied` (E0061).

- [ ] **Step 4: Implement — `DocState` field + `doc_loaded` parameter**

In `src/app.rs`, `DocState` (after `pub has_forms: bool,` line 173):

```rust
    /// User bookmarks (page-sorted; page is unique). Both editions display
    /// them in the TOC; only Pro mutates them (Ctrl+B).
    pub bookmarks: Vec<Bookmark>,
```

`doc_loaded` (line 369): add the parameter and initializer:

```rust
    pub fn doc_loaded(
        &mut self,
        path: PathBuf,
        pages: u16,
        page_sizes: Vec<(f32, f32)>,
        outline: Vec<OutlineItem>,
        has_forms: bool,
        bookmarks: Vec<Bookmark>,
    ) {
```

and in the `DocState` literal (after `has_forms,`):

```rust
            bookmarks,
```

- [ ] **Step 5: Update every `doc_loaded` caller**

Grep first: `cargo build 2>&1` will list them, or run Grep for `doc_loaded(`. Callers to update by appending the new final argument:

1. `src/window.rs` `Loaded` arm (~line 2170) — destructure and pass through:
```rust
                PdfReply::Loaded { path, pages, page_sizes, outline, has_forms, bookmarks } => {
                    self.app.doc_loaded(path, pages, page_sizes, outline, has_forms, bookmarks);
```
2. `src/app.rs` tests — append `vec![]` as the 6th argument at each site: the `app_with_doc` helper (~line 966), `zero_page_doc_navigation_is_safe` (~1025), `render_reply_loop_converges_under_budget_pressure` (~1264), `real_render_failure_is_not_rerequested` (~1313), `new_document_starts_with_empty_history` (~1636), `search_open_close_stashes_and_restores_the_query` (~1834), pro test `dirty_flag_lifecycle` (~1917). (Line numbers drift as you edit — grep `doc_loaded(` and update ALL remaining 5-arg calls.)

- [ ] **Step 6: Read the attachment in `src/pdf.rs::describe()`**

Add `Bookmark` to pdf.rs's types import (line 4-7): `use crate::types::{Bookmark, FieldKind, FormField, LinkTarget, OutlineItem, PageLink, PdfCmd, PdfReply, SearchMatch, SearchOptions};`

In `describe()` change the return expression:

```rust
    PdfReply::Loaded {
        path: path.to_path_buf(),
        pages,
        page_sizes,
        outline,
        has_forms: doc.form().is_some(),
        bookmarks: read_bookmarks(doc, pages),
    }
```

and add below `describe()`:

```rust
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
```

- [ ] **Step 7: Pin the empty case in the existing integration test**

In `tests/pdf_integration.rs::load_render_outline_links` (line 53-57), extend the `Loaded` match to also grab and assert bookmarks:

```rust
    let (pages, page_sizes, outline) = match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { pages, page_sizes, outline, bookmarks, .. } => {
            assert!(bookmarks.is_empty(), "fixture has no bookmarks attachment");
            (pages, page_sizes, outline)
        }
        other => panic!("expected Loaded, got {other:?}"),
    };
```

- [ ] **Step 8: Verify both editions fully**

Run: `cargo test --lib`
Expected: all unit tests pass, including `doc_loaded_stores_bookmarks_and_next_load_replaces_them`.
Run: `cargo test --lib --features pro`
Expected: pass.
Run: `cargo test --test pdf_integration load_render_outline_links`
Expected: 1 passed (real-DLL read path: no attachment → empty vec).
Run: `cargo build && cargo build --features pro`
Expected: both green.

- [ ] **Step 9: Commit**

```powershell
git add src/types.rs src/app.rs src/pdf.rs src/window.rs tests/pdf_integration.rs
git commit -m @'
feat(bookmarks): read pppdf.bookmarks attachment at load, store on DocState

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

### Task 3: TOC panel — "Bookmarks" section on top

**Files:**
- Modify: `src/overlay.rs` (`OutlineState.header_idx`, one test)
- Modify: `src/app.rs:532-538` (`toggle_overlay`), tests
- Modify: `src/window.rs:1433-1442` (`overlay_key` Enter) and `:1676-1694` (`on_click` Title arm)
- Modify: `src/d2d.rs:593-650` (`draw_overlay` header styling)

**Interfaces:**
- Consumes: `DocState.bookmarks` (Task 2), `bookmarks::display_cmp` (Task 1).
- Produces:
  - `OutlineState.header_idx: Option<usize>` — pub field, `None` by default; `toggle_overlay` sets `Some(0)` when bookmarks exist. d2d styles that row; it is data-only (no behavior change inside overlay.rs).
  - Behavior contract for later tasks: activating any row whose `page` is `None` toggles its children (if any) and KEEPS the panel open — at both the keyboard and mouse call sites.

- [ ] **Step 1: Failing tests first**

`src/overlay.rs` tests module — add:

```rust
    #[test]
    fn pageless_header_row_activates_to_none_and_header_idx_defaults_none() {
        let mut s = OutlineState::new(vec![
            OutlineItem { title: "Bookmarks".into(), page: None, depth: 0, has_children: true },
            item("B1", 3, 1, false),
        ]);
        assert_eq!(s.header_idx, None, "callers opt in; new() never sets it");
        assert_eq!(s.activate(), None, "header row has no page");
        s.move_selection(1, 1000.0);
        assert_eq!(s.activate(), Some(3));
        // The depth-0 default-expansion rule covers the header for free.
        assert_eq!(s.visible(), vec![0, 1]);
    }
```

`src/app.rs` shared tests module — add:

```rust
    #[test]
    fn toggle_overlay_prepends_name_sorted_bookmarks_with_header() {
        let mut a = App::new();
        a.set_client(SizeU { w: 1000, h: 1000 });
        a.doc_loaded(
            PathBuf::from("x.pdf"),
            20,
            vec![LETTER; 20],
            vec![OutlineItem { title: "Ch 1".into(), page: Some(0), depth: 0, has_children: false }],
            false,
            vec![
                Bookmark { page: 3, name: "10 - Appendix".into() },
                Bookmark { page: 5, name: "2 - Maps".into() },
            ],
        );
        a.toggle_overlay();
        let d = a.doc().unwrap();
        let ov = d.overlay.as_ref().unwrap();
        let titles: Vec<&str> = ov.items.iter().map(|it| it.title.as_str()).collect();
        // Header, bookmarks in NATURAL name order (2 before 10), then the
        // document outline unchanged.
        assert_eq!(titles, vec!["Bookmarks", "2 - Maps", "10 - Appendix", "Ch 1"]);
        assert_eq!(ov.header_idx, Some(0));
        assert_eq!(ov.items[0].page, None);
        assert!(ov.items[0].has_children);
        assert_eq!((ov.items[1].page, ov.items[1].depth), (Some(5), 1));
        assert_eq!(ov.visible(), vec![0, 1, 2, 3], "header auto-expands (depth-0 rule)");
        // DocState's own vec stays page-sorted — display order is derived.
        assert_eq!(d.bookmarks[0].page, 3);
        // Without bookmarks: no header, outline only, empty-message case intact.
        a.toggle_overlay(); // close
        a.doc_mut().unwrap().bookmarks.clear();
        a.toggle_overlay();
        let ov2 = a.doc().unwrap().overlay.as_ref().unwrap();
        assert_eq!(ov2.items.len(), 1);
        assert_eq!(ov2.header_idx, None);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test pageless_header --lib; cargo test toggle_overlay_prepends --lib`
Expected: FAIL to compile — `no field header_idx on type OutlineState` (E0609).

- [ ] **Step 3: Implement `header_idx` in `src/overlay.rs`**

Struct (line 20-27) — add field after `pub scroll: f32,`:

```rust
    /// Index of the "Bookmarks" section-header row, when one exists.
    /// Set by App::toggle_overlay (never by new()); d2d styles that row.
    pub header_idx: Option<usize>,
```

`new()` (line 30-33):

```rust
        Self { items, expanded, selected: 0, scroll: 0.0, header_idx: None }
```

- [ ] **Step 4: Implement the combined list in `App::toggle_overlay` (src/app.rs:532-538)**

Replace the whole method:

```rust
    pub fn toggle_overlay(&mut self) {
        let Some(d) = self.doc_mut() else { return };
        d.overlay = match d.overlay.take() {
            Some(_) => None,
            None => {
                // User bookmarks first (name-sorted, natural order), under a
                // section-header row; the document outline follows unchanged.
                // Built fresh on every open, so bookmark edits while the
                // panel is closed need no invalidation anywhere.
                let mut items: Vec<OutlineItem> = Vec::new();
                let mut header_idx = None;
                if !d.bookmarks.is_empty() {
                    header_idx = Some(0);
                    items.push(OutlineItem {
                        title: "Bookmarks".into(),
                        page: None,
                        depth: 0,
                        has_children: true,
                    });
                    let mut sorted: Vec<&crate::types::Bookmark> = d.bookmarks.iter().collect();
                    sorted.sort_by(|a, b| crate::bookmarks::display_cmp(a, b));
                    items.extend(sorted.into_iter().map(|b| OutlineItem {
                        title: b.name.clone(),
                        page: Some(b.page),
                        depth: 1,
                        has_children: false,
                    }));
                }
                items.extend(d.outline.iter().cloned());
                let mut ov = OutlineState::new(items);
                ov.header_idx = header_idx;
                Some(ov)
            }
        };
    }
```

- [ ] **Step 5: Run the two tests green**

Run: `cargo test pageless_header --lib; cargo test toggle_overlay_prepends --lib`
Expected: both PASS. Also run `cargo test --lib` — the full overlay/app suites must stay green (the empty-outline and tree tests are unaffected: no bookmarks → identical behavior).

- [ ] **Step 6: Page-less activation keeps the panel open (src/window.rs)**

`overlay_key`, replace the `VK_RETURN` arm (lines 1433-1442):

```rust
            _ if vk == VK_RETURN.0 => {
                match ov.activate() {
                    Some(page) => {
                        d.overlay = None;
                        if self.app.goto(page) {
                            self.sync_renders();
                            self.show_badge();
                        }
                    }
                    // A row with no page target (the Bookmarks header, or an
                    // outline entry whose destination didn't resolve): toggle
                    // its children instead of uselessly closing the panel.
                    None => {
                        let sel = ov.selected;
                        ov.toggle(sel);
                    }
                }
            }
```

`on_click`, replace the Title branch (the `else` block at lines 1681-1691):

```rust
            } else {
                ov.select(idx);
                match ov.activate() {
                    Some(page) => {
                        d.overlay = None; // jump closes the panel
                        if self.app.goto(page) {
                            self.sync_renders();
                            self.show_badge();
                        }
                    }
                    None => ov.toggle(idx), // header row: click toggles, panel stays
                }
            }
```

- [ ] **Step 7: Header styling in `src/d2d.rs::draw_overlay`**

Replace the final `draw_text` of the row title (line 647):

```rust
            let ink = if ov.header_idx == Some(idx) {
                color(0.74, 0.78, 0.96, 1.0) // section header: the help accent
            } else {
                color(1.0, 1.0, 1.0, 0.9)
            };
            self.draw_text(rt, &it.title, &fmt, title_rect, ink)?;
```

(The `if ov.items.is_empty()` → "No table of contents" branch at d2d.rs:607 needs NO change: bookmarks live inside `items`, so a non-empty bookmarks set already suppresses the message; both-empty still shows it — exactly the spec.)

- [ ] **Step 8: Verify both editions**

Run: `cargo test --lib && cargo test --lib --features pro`
Expected: all pass.
Run: `cargo build && cargo build --features pro`
Expected: green.

- [ ] **Step 9: Commit**

```powershell
git add src/overlay.rs src/app.rs src/window.rs src/d2d.rs
git commit -m @'
feat(bookmarks): Bookmarks section atop the TOC panel, name-sorted

Header row styled in accent; activating a page-less row now toggles its
children instead of dismissing the panel (keyboard and mouse).

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

### Task 4: Extract `pro::atomic_write` (refactor, no behavior change)

**Files:**
- Modify: `src/pro/mod.rs`
- Modify: `src/pro/forms/mod.rs:220-251` (`FormEngine::save`)

**Interfaces:**
- Produces: `pub(crate) fn pro::atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String>` — temp file `.pppdf-save-{pid}.tmp` in the target's directory, 5 rename retries with 20ms*(n+1) backoff, temp cleaned up on failure, original untouched on any failure. Task 5's bookmark save shares it.

- [ ] **Step 1: Add the helper to `src/pro/mod.rs`**

Append after `pub mod forms;`:

```rust
pub mod bookmarks;

/// Write `bytes` atomically over `path`: temp file in the target's own
/// directory, then rename with antivirus-lock retries (same idiom as
/// pdf.rs's dll extraction). The original is untouched on any failure.
/// Shared by the form save and the bookmark save.
pub(crate) fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("path has no parent directory")?;
    let tmp = dir.join(format!(".pppdf-save-{}.tmp", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, bytes) {
        // Don't leave a partial temp file behind (disk full etc.).
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    let mut last = None;
    for attempt in 0..5u64 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(20 * (attempt + 1)));
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(last.map(|e| e.to_string()).unwrap_or_else(|| "rename failed".into()))
}
```

Also create a **placeholder-free stub** `src/pro/bookmarks.rs` so the module line compiles (Task 5 fills it):

```rust
//! Pro custom bookmarks: writes the `pppdf.bookmarks` attachment on the
//! PDF thread. The READ side (pdf::describe) is shared code — the free
//! build displays bookmarks; only creating/removing them is Pro.
```

- [ ] **Step 2: Refactor `FormEngine::save` onto it**

Replace the body of `save` in `src/pro/forms/mod.rs` (keep signature and the doc comment; drop the now-redundant tail):

```rust
    pub fn save(
        &mut self,
        b: &dyn PdfiumLibraryBindings,
        doc: &PdfDocument,
        path: &std::path::Path,
    ) -> Result<(), String> {
        unsafe {
            b.FORM_ForceToKillFocus(self.form);
        }
        CB.with(|s| s.borrow_mut().invalidated = true); // caret/focus ring gone
        let bytes = doc.save_to_bytes().map_err(|e| format!("{e:?}"))?;
        crate::pro::atomic_write(path, &bytes)
    }
```

- [ ] **Step 3: Verify the refactor against the real save tests**

Run: `cargo build --features pro && cargo build`
Expected: both compile (free build is untouched — `pro/` isn't in it).
Run: `cargo test --features pro --test pdf_integration fill_save_reload_roundtrip`
Expected: 1 passed (the end-to-end save still works byte-for-byte the same way).
Run: `cargo test --features pro --test pdf_integration throttled_invalidate_flushes_after_save_kills_timers`
Expected: 1 passed.

- [ ] **Step 4: Commit**

```powershell
git add src/pro/mod.rs src/pro/forms/mod.rs src/pro/bookmarks.rs
git commit -m @'
refactor(pro): extract atomic_write from FormEngine::save for reuse

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

### Task 5: Pro save path — `SaveBookmarks` / `BookmarksSaved` + the double-write

**Files:**
- Modify: `src/types.rs` (two Pro-gated protocol variants)
- Modify: `src/pro/bookmarks.rs` (the real implementation)
- Modify: `src/pdf.rs` (password retention, batch collection, execution, replies)
- Test: `tests/pdf_integration.rs` (5 new Pro tests)

**Interfaces:**
- Consumes: `bookmarks::{serialize, ATTACHMENT_NAME}` (Task 1), `pro::atomic_write` (Task 4).
- Produces:
  - `PdfCmd::SaveBookmarks { path: PathBuf, bookmarks: Vec<Bookmark> }` (Pro) — `path` is the UI's `DocState.path`; the PDF thread refuses mismatches.
  - `PdfReply::BookmarksSaved { result: Result<(), String> }` (Pro) — exactly one per command.
  - `pro::bookmarks::save_bookmarks(pdfium: &Pdfium, live: &mut PdfDocument<'static>, path: &Path, password: Option<&str>, bookmarks: &[Bookmark]) -> Result<(), String>`

- [ ] **Step 1: Protocol variants in `src/types.rs`**

In `PdfCmd`, after the `SaveDoc` variant (line 101-103):

```rust
    /// Persist the full user-bookmark set into the document file. `path`
    /// names the document the UI meant — the PDF thread refuses to write
    /// if it no longer matches the open document (a Load may be racing).
    /// Every command gets exactly one `BookmarksSaved` reply.
    #[cfg(feature = "pro")]
    SaveBookmarks { path: PathBuf, bookmarks: Vec<Bookmark> },
```

In `PdfReply`, after the `Saved` variant (line 146-150):

```rust
    /// Result of a `SaveBookmarks` request. One reply per command, always.
    #[cfg(feature = "pro")]
    BookmarksSaved {
        result: Result<(), String>,
    },
```

- [ ] **Step 2: Write the failing integration test (the roundtrip)**

Append to `tests/pdf_integration.rs`:

```rust
/// Pro bookmarks: SaveBookmarks writes the attachment into the file on
/// disk; a fresh load reads it back through PdfReply::Loaded.
#[cfg(feature = "pro")]
#[test]
fn bookmarks_save_reload_roundtrip() {
    use pppdf::types::Bookmark;

    let _pdfium = pdfium_lock();
    // Own subdirectory: atomic_write's temp name is per-process, and other
    // tests save under temp_dir too (see throttled_invalidate's comment).
    let work_dir = std::env::temp_dir().join(format!("pppdf-bm-rt-{}", std::process::id()));
    std::fs::create_dir_all(&work_dir).unwrap();
    let work = work_dir.join("book.pdf");
    std::fs::write(&work, common::build_fixture_pdf()).unwrap();

    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    cmd_tx.send(PdfCmd::Load { path: work.clone(), password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { bookmarks, .. } => assert!(bookmarks.is_empty()),
        other => panic!("expected Loaded, got {other:?}"),
    }
    let set = vec![
        Bookmark { page: 2, name: "10 - Dungeons".into() },
        Bookmark { page: 0, name: "2 - Town".into() },
    ];
    cmd_tx.send(PdfCmd::SaveBookmarks { path: work.clone(), bookmarks: set }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::BookmarksSaved { result } => result.expect("bookmark save must succeed"),
        other => panic!("expected BookmarksSaved, got {other:?}"),
    }
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
    join_pdf_thread(&reply_rx);

    // A brand-new PDF thread loading from disk must see them (page-sorted
    // by parse(), regardless of the order they were sent in).
    let (reply_tx2, reply_rx2) = mpsc::channel();
    let cmd_tx2 = pdf::spawn(reply_tx2, || {});
    cmd_tx2.send(PdfCmd::Load { path: work.clone(), password: None }).unwrap();
    match reply_rx2.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { bookmarks, .. } => {
            assert_eq!(
                bookmarks,
                vec![
                    Bookmark { page: 0, name: "2 - Town".into() },
                    Bookmark { page: 2, name: "10 - Dungeons".into() },
                ]
            );
        }
        other => panic!("expected Loaded, got {other:?}"),
    }
    cmd_tx2.send(PdfCmd::Shutdown).unwrap();
    join_pdf_thread(&reply_rx2);
    std::fs::remove_dir_all(&work_dir).ok();
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo build --features pro`
Expected: FAIL — `error[E0004]: non-exhaustive patterns: PdfCmd::SaveBookmarks { .. } not covered` in pdf.rs's batch-coalescing match (Step 1 added the variant; the PDF thread doesn't handle it yet). That compile error is the red state.

- [ ] **Step 4: Implement `src/pro/bookmarks.rs`**

Replace the stub's content (keep the module doc comment):

```rust
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
```

- [ ] **Step 5: Wire the PDF thread (`src/pdf.rs`)**

5a. Add the password memo next to `current_path` (line 186-187):

```rust
    #[cfg(feature = "pro")]
    let mut current_path: Option<PathBuf> = None;
    /// The password the current document was opened with — needed to
    /// shadow-load the on-disk bytes for a bookmark save. Kept only on
    /// the PDF thread, only in the Pro build.
    #[cfg(feature = "pro")]
    let mut current_password: Option<String> = None;
```

5b. Batch collection — next to the other Pro accumulators (line 268-271):

```rust
        #[cfg(feature = "pro")]
        let mut bookmark_saves: Vec<(PathBuf, Vec<crate::types::Bookmark>)> = Vec::new();
```

and the match arm (after the `PdfCmd::SaveDoc` arm, line 306-307):

```rust
                #[cfg(feature = "pro")]
                PdfCmd::SaveBookmarks { path, bookmarks } => {
                    // FIFO, NEVER coalesced: the UI counts one reply per
                    // command into saves_inflight (a dropped reply would
                    // block window destruction forever).
                    bookmark_saves.push((path, bookmarks));
                }
```

5c. Execution — insert IMMEDIATELY BEFORE the `if let Some((path, password)) = load {` block (line 338):

```rust
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
```

5d. Password retention — in the Load success arm, right after `current_path = Some(path);` (line 370-373):

```rust
                        #[cfg(feature = "pro")]
                        {
                            current_password = password;
                        }
```

Compile note: `password` is still owned at that point (the loader borrowed it via `as_deref()`); the free build must not reference it after the load, so keep this inside the existing `#[cfg(feature = "pro")]` block structure — add it as its own cfg'd block exactly as shown. If the borrow checker complains about a move while `loaded` still borrows, bind `let password_keep = password.clone();` before the `let loaded = ...` statement and assign `current_password = password_keep;` instead — cfg'd the same way.

- [ ] **Step 6: Run the roundtrip test green**

Run: `cargo test --features pro --test pdf_integration bookmarks_save_reload_roundtrip`
Expected: 1 passed. (This proves: attachment creation works against the real DLL, atomic write works, `describe()` reads it back.)

- [ ] **Step 7: The money test — bookmark save excludes unsaved form edits**

Append to `tests/pdf_integration.rs`:

```rust
/// THE core spec guarantee: a bookmark save must not persist form edits.
/// Type into FullName and commit it into the LIVE document (KillFocus),
/// never Ctrl+S — then save a bookmark. On disk: bookmark present,
/// FullName still empty (the shadow-write went from disk bytes).
#[cfg(feature = "pro")]
#[test]
fn bookmark_save_excludes_unsaved_form_edits() {
    use pppdf::types::{Bookmark, FieldKind, FormEvent, FormField};

    let _pdfium = pdfium_lock();
    let src =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/form.pdf");
    let work_dir = std::env::temp_dir().join(format!("pppdf-bm-excl-{}", std::process::id()));
    std::fs::create_dir_all(&work_dir).unwrap();
    let work = work_dir.join("form.pdf");
    std::fs::copy(&src, &work).unwrap();

    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    cmd_tx.send(PdfCmd::Load { path: work.clone(), password: None }).unwrap();
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
    // Top-most Text field = FullName (the fixture idiom used by every
    // form test in this file).
    let mut texts: Vec<&FormField> = fields.iter().filter(|f| f.kind == FieldKind::Text).collect();
    texts.sort_by(|a, b| b.rect_pt.top.total_cmp(&a.rect_pt.top));
    let f = texts[0];
    let (cx, cy) =
        ((f.rect_pt.left + f.rect_pt.right) / 2.0, (f.rect_pt.top + f.rect_pt.bottom) / 2.0);
    for ev in [
        FormEvent::MouseMove { x: cx, y: cy, modifiers: 0 },
        FormEvent::MouseDown { x: cx, y: cy, modifiers: 0 },
        FormEvent::MouseUp { x: cx, y: cy, modifiers: 0 },
        FormEvent::Char { c: 'A' as u16, modifiers: 0 },
        FormEvent::Char { c: 'b' as u16, modifiers: 0 },
        // Commit the value into the LIVE doc's field — but never save it.
        FormEvent::KillFocus,
    ] {
        cmd_tx.send(PdfCmd::Form { page: 0, ev }).unwrap();
    }
    cmd_tx
        .send(PdfCmd::SaveBookmarks {
            path: work.clone(),
            bookmarks: vec![Bookmark { page: 0, name: "Camp".into() }],
        })
        .unwrap();
    // Drain until the BookmarksSaved lands (form events may interleave
    // FormDirty / unsolicited Rendered replies before it).
    let deadline = std::time::Instant::now() + WAIT;
    let mut saved = false;
    while std::time::Instant::now() < deadline && !saved {
        match reply_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(PdfReply::BookmarksSaved { result }) => {
                result.expect("bookmark save must succeed");
                saved = true;
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(saved, "no BookmarksSaved reply");
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
    join_pdf_thread(&reply_rx);

    // Disk: the typed text must NOT be there...
    let values = pppdf::pro::forms::read_field_values(&work).unwrap();
    assert_ne!(
        values.get("FullName"),
        Some(&Some("Ab".to_string())),
        "bookmark save leaked an unsaved form edit to disk"
    );
    // ...but the bookmark must be.
    let (reply_tx2, reply_rx2) = mpsc::channel();
    let cmd_tx2 = pdf::spawn(reply_tx2, || {});
    cmd_tx2.send(PdfCmd::Load { path: work.clone(), password: None }).unwrap();
    match reply_rx2.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { bookmarks, .. } => {
            assert_eq!(bookmarks, vec![Bookmark { page: 0, name: "Camp".into() }]);
        }
        other => panic!("expected Loaded, got {other:?}"),
    }
    cmd_tx2.send(PdfCmd::Shutdown).unwrap();
    join_pdf_thread(&reply_rx2);
    std::fs::remove_dir_all(&work_dir).ok();
}
```

- [ ] **Step 8: Three more integration tests (preserve / delete-all / path guard)**

Append:

```rust
/// The other half of the double-write: a later FORM save (Ctrl+S) must
/// carry the bookmarks along — the live doc's attachment was updated at
/// toggle time, so SaveDoc can never wipe them.
#[cfg(feature = "pro")]
#[test]
fn form_save_preserves_bookmarks() {
    use pppdf::types::{Bookmark, FieldKind, FormEvent, FormField};

    let _pdfium = pdfium_lock();
    let src =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/form.pdf");
    let work_dir = std::env::temp_dir().join(format!("pppdf-bm-keep-{}", std::process::id()));
    std::fs::create_dir_all(&work_dir).unwrap();
    let work = work_dir.join("form.pdf");
    std::fs::copy(&src, &work).unwrap();

    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    cmd_tx.send(PdfCmd::Load { path: work.clone(), password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { .. } => {}
        other => panic!("expected Loaded, got {other:?}"),
    }
    cmd_tx
        .send(PdfCmd::SaveBookmarks {
            path: work.clone(),
            bookmarks: vec![Bookmark { page: 0, name: "Keep me".into() }],
        })
        .unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::BookmarksSaved { result } => result.expect("bookmark save must succeed"),
        other => panic!("expected BookmarksSaved, got {other:?}"),
    }
    // Now a form edit + a FORM save (the fill_save_reload_roundtrip idiom,
    // FullName only).
    cmd_tx
        .send(PdfCmd::Render { page: 0, px: SizeU { w: 612, h: 792 }, rot: 0, visible: true })
        .unwrap();
    let fields: Vec<FormField> = loop {
        match reply_rx.recv_timeout(WAIT).unwrap() {
            PdfReply::Rendered { fields, .. } => break fields,
            _ => {}
        }
    };
    let mut texts: Vec<&FormField> = fields.iter().filter(|f| f.kind == FieldKind::Text).collect();
    texts.sort_by(|a, b| b.rect_pt.top.total_cmp(&a.rect_pt.top));
    let f = texts[0];
    let (cx, cy) =
        ((f.rect_pt.left + f.rect_pt.right) / 2.0, (f.rect_pt.top + f.rect_pt.bottom) / 2.0);
    for ev in [
        FormEvent::MouseMove { x: cx, y: cy, modifiers: 0 },
        FormEvent::MouseDown { x: cx, y: cy, modifiers: 0 },
        FormEvent::MouseUp { x: cx, y: cy, modifiers: 0 },
        FormEvent::Char { c: 'A' as u16, modifiers: 0 },
        FormEvent::Char { c: 'b' as u16, modifiers: 0 },
        FormEvent::KillFocus,
    ] {
        cmd_tx.send(PdfCmd::Form { page: 0, ev }).unwrap();
    }
    cmd_tx.send(PdfCmd::SaveDoc).unwrap();
    let deadline = std::time::Instant::now() + WAIT;
    let mut saved = false;
    while std::time::Instant::now() < deadline && !saved {
        match reply_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(PdfReply::Saved { result }) => {
                result.expect("form save must succeed");
                saved = true;
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(saved, "no Saved reply");
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
    join_pdf_thread(&reply_rx);

    // Disk now holds BOTH the form value and the bookmark.
    let values = pppdf::pro::forms::read_field_values(&work).unwrap();
    assert_eq!(values.get("FullName"), Some(&Some("Ab".to_string())));
    let (reply_tx2, reply_rx2) = mpsc::channel();
    let cmd_tx2 = pdf::spawn(reply_tx2, || {});
    cmd_tx2.send(PdfCmd::Load { path: work.clone(), password: None }).unwrap();
    match reply_rx2.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { bookmarks, .. } => {
            assert_eq!(bookmarks, vec![Bookmark { page: 0, name: "Keep me".into() }]);
        }
        other => panic!("expected Loaded, got {other:?}"),
    }
    cmd_tx2.send(PdfCmd::Shutdown).unwrap();
    join_pdf_thread(&reply_rx2);
    std::fs::remove_dir_all(&work_dir).ok();
}

/// Removing the last bookmark deletes the attachment entirely (no empty
/// blob left behind) — observable as an empty set after reload.
#[cfg(feature = "pro")]
#[test]
fn removing_all_bookmarks_deletes_the_attachment() {
    use pppdf::types::Bookmark;

    let _pdfium = pdfium_lock();
    let work_dir = std::env::temp_dir().join(format!("pppdf-bm-del-{}", std::process::id()));
    std::fs::create_dir_all(&work_dir).unwrap();
    let work = work_dir.join("book.pdf");
    std::fs::write(&work, common::build_fixture_pdf()).unwrap();

    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    cmd_tx.send(PdfCmd::Load { path: work.clone(), password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { .. } => {}
        other => panic!("expected Loaded, got {other:?}"),
    }
    cmd_tx
        .send(PdfCmd::SaveBookmarks {
            path: work.clone(),
            bookmarks: vec![Bookmark { page: 1, name: "Temp".into() }],
        })
        .unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::BookmarksSaved { result } => result.expect("first save"),
        other => panic!("expected BookmarksSaved, got {other:?}"),
    }
    cmd_tx.send(PdfCmd::SaveBookmarks { path: work.clone(), bookmarks: vec![] }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::BookmarksSaved { result } => result.expect("empty-set save"),
        other => panic!("expected BookmarksSaved, got {other:?}"),
    }
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
    join_pdf_thread(&reply_rx);

    let (reply_tx2, reply_rx2) = mpsc::channel();
    let cmd_tx2 = pdf::spawn(reply_tx2, || {});
    cmd_tx2.send(PdfCmd::Load { path: work.clone(), password: None }).unwrap();
    match reply_rx2.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { bookmarks, .. } => assert!(bookmarks.is_empty()),
        other => panic!("expected Loaded, got {other:?}"),
    }
    cmd_tx2.send(PdfCmd::Shutdown).unwrap();
    join_pdf_thread(&reply_rx2);
    std::fs::remove_dir_all(&work_dir).ok();
}

/// The wrong-document guard: a SaveBookmarks whose path doesn't match the
/// open document answers Err — never writes anywhere.
#[cfg(feature = "pro")]
#[test]
fn bookmark_save_for_a_different_path_errors() {
    use pppdf::types::Bookmark;

    let _pdfium = pdfium_lock();
    let (reply_tx, reply_rx) = mpsc::channel();
    let cmd_tx = pdf::spawn(reply_tx, || {});
    cmd_tx.send(PdfCmd::Load { path: common::fixture_path(), password: None }).unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::Loaded { .. } => {}
        other => panic!("expected Loaded, got {other:?}"),
    }
    let elsewhere = std::env::temp_dir().join("pppdf-bm-nowhere.pdf");
    std::fs::remove_file(&elsewhere).ok(); // stale leftovers must not fake the assert below
    cmd_tx
        .send(PdfCmd::SaveBookmarks {
            path: elsewhere.clone(),
            bookmarks: vec![Bookmark { page: 0, name: "X".into() }],
        })
        .unwrap();
    match reply_rx.recv_timeout(WAIT).unwrap() {
        PdfReply::BookmarksSaved { result } => {
            assert!(result.is_err(), "mismatched path must refuse to write");
        }
        other => panic!("expected BookmarksSaved, got {other:?}"),
    }
    assert!(!elsewhere.exists(), "nothing may be written to the wrong path");
    cmd_tx.send(PdfCmd::Shutdown).unwrap();
    join_pdf_thread(&reply_rx);
}
```

- [ ] **Step 9: Run the full Pro integration suite + both builds**

Run: `cargo test --features pro --test pdf_integration`
Expected: all pass — the 4 new bookmark tests AND every pre-existing form/search test (the batch-loop change must not disturb them).
Run: `cargo build && cargo test --lib`
Expected: free edition still green (it contains none of this).

- [ ] **Step 10: Commit**

```powershell
git add src/types.rs src/pro/bookmarks.rs src/pdf.rs tests/pdf_integration.rs
git commit -m @'
feat(bookmarks): SaveBookmarks protocol + attachment double-write on the PDF thread

Live-doc attachment update keeps Ctrl+S form saves carrying bookmarks;
the shadow-load-from-disk write keeps bookmark saves from leaking
unsaved form edits. Path guard + one-reply-per-command invariant.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

### Task 6: App-side toggle + name-entry state (pure logic, Pro)

**Files:**
- Modify: `src/app.rs` (`DocState.bookmark_entry`, Pro methods, `pro_tests`)

**Interfaces:**
- Consumes: `bookmarks::{default_name, NAME_CAP}` (Task 1), `DocState.bookmarks` (Task 2).
- Produces (all `#[cfg(feature = "pro")]`, used by Task 7):
  - `DocState.bookmark_entry: Option<String>` (Pro field; `Some` = prompt open)
  - `App::bookmark_on_current(&self) -> Option<usize>`
  - `App::bookmark_remove_current(&mut self) -> Option<String>` (removed name)
  - `App::bookmark_entry_start(&mut self) -> bool` (false without doc / zero pages)
  - `App::bookmark_entry_active(&self) -> bool`
  - `App::bookmark_entry_char(&mut self, c: char)` (control chars ignored; `NAME_CAP` chars max)
  - `App::bookmark_entry_backspace(&mut self)`
  - `App::bookmark_entry_cancel(&mut self)`
  - `App::bookmark_entry_commit(&mut self) -> Option<String>` (committed name; empty/whitespace buffer → `default_name(current)`; inserts page-sorted; `None` if prompt wasn't open or page got bookmarked meanwhile)

- [ ] **Step 1: Failing tests in `src/app.rs::tests::pro_tests`**

```rust
        #[test]
        fn bookmark_toggle_and_entry_flow() {
            let mut a = app_with_doc(10);
            a.flip(3); // current = 3
            assert_eq!(a.bookmark_on_current(), None);
            assert!(a.bookmark_entry_start());
            assert!(a.bookmark_entry_active());
            for c in "Lair".chars() {
                a.bookmark_entry_char(c);
            }
            a.bookmark_entry_char('\u{7}'); // control chars ignored
            a.bookmark_entry_backspace();
            assert_eq!(a.doc().unwrap().bookmark_entry.as_deref(), Some("Lai"));
            assert_eq!(a.bookmark_entry_commit(), Some("Lai".to_string()));
            assert!(!a.bookmark_entry_active(), "commit closes the prompt");
            assert_eq!(a.doc().unwrap().bookmarks, vec![Bookmark { page: 3, name: "Lai".into() }]);
            assert_eq!(a.bookmark_on_current(), Some(0));
            // Toggle off: remove returns the name.
            assert_eq!(a.bookmark_remove_current(), Some("Lai".to_string()));
            assert!(a.doc().unwrap().bookmarks.is_empty());
            assert_eq!(a.bookmark_remove_current(), None, "nothing left to remove");
        }

        #[test]
        fn bookmark_entry_empty_commit_uses_default_name() {
            let mut a = app_with_doc(50);
            a.flip(41); // current = 41 -> "Page 42"
            a.bookmark_entry_start();
            assert_eq!(a.bookmark_entry_commit(), Some("Page 42".to_string()));
            assert_eq!(a.doc().unwrap().bookmarks[0].name, "Page 42");
            // Whitespace-only counts as empty too. flip(1): 41 -> 42.
            a.flip(1);
            a.bookmark_entry_start();
            a.bookmark_entry_char(' ');
            assert_eq!(a.bookmark_entry_commit(), Some("Page 43".to_string()));
        }

        #[test]
        fn bookmark_entry_caps_at_64_chars() {
            let mut a = app_with_doc(3);
            a.bookmark_entry_start();
            for _ in 0..100 {
                a.bookmark_entry_char('x');
            }
            assert_eq!(
                a.doc().unwrap().bookmark_entry.as_ref().unwrap().chars().count(),
                crate::bookmarks::NAME_CAP
            );
            a.bookmark_entry_cancel();
            assert!(!a.bookmark_entry_active());
            assert!(a.doc().unwrap().bookmarks.is_empty(), "cancel adds nothing");
        }

        #[test]
        fn bookmark_commit_inserts_page_sorted() {
            let mut a = app_with_doc(10);
            for page in [7u16, 2, 5] {
                a.goto(page);
                a.bookmark_entry_start();
                a.bookmark_entry_commit();
            }
            let pages: Vec<u16> = a.doc().unwrap().bookmarks.iter().map(|b| b.page).collect();
            assert_eq!(pages, vec![2, 5, 7], "DocState vec stays page-sorted");
        }

        #[test]
        fn bookmark_entry_guards_zero_pages_and_no_doc() {
            assert!(!App::new().bookmark_entry_start());
            let mut a = App::new();
            a.set_client(SizeU { w: 1000, h: 1000 });
            a.doc_loaded(PathBuf::from("empty.pdf"), 0, vec![], vec![], false, vec![]);
            assert!(!a.bookmark_entry_start(), "zero-page doc refuses");
            assert_eq!(a.bookmark_remove_current(), None);
        }

        #[test]
        fn bookmarks_survive_password_stash_and_die_with_doc() {
            let mut a = app_with_doc(10);
            a.bookmark_entry_start();
            a.bookmark_entry_commit(); // "Page 1" on page 0
            let prev = match std::mem::replace(&mut a.mode, Mode::Empty) {
                Mode::Doc(d) => Some(Box::new(d)),
                _ => None,
            };
            a.mode =
                Mode::Password { path: PathBuf::from("locked.pdf"), buf: String::new(), prev };
            assert_eq!(a.bookmark_on_current(), None, "no doc mode: inert");
            a.password_cancel();
            assert_eq!(a.doc().unwrap().bookmarks.len(), 1, "came back with the stash");
            a.doc_loaded(PathBuf::from("y.pdf"), 5, vec![LETTER; 5], vec![], false, vec![]);
            assert!(a.doc().unwrap().bookmarks.is_empty());
        }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib --features pro bookmark_`
Expected: FAIL to compile — `no method named bookmark_entry_start` (E0599).

- [ ] **Step 3: Implement**

`DocState` — add after the `goto_entry` field (app.rs:184):

```rust
    /// Pro: Some = the Ctrl+B bookmark-name prompt is open, holding the
    /// typed name.
    #[cfg(feature = "pro")]
    pub bookmark_entry: Option<String>,
```

`doc_loaded`'s `DocState` literal — add next to `goto_entry: None,`:

```rust
            #[cfg(feature = "pro")]
            bookmark_entry: None,
```

New section in `impl App`, directly after the go-to-page entry section (after `goto_entry_commit`, app.rs:804):

```rust
    // --- custom bookmarks (Pro: Ctrl+B toggle + name entry prompt) ---

    /// Index into `bookmarks` of the current page's bookmark, if any.
    #[cfg(feature = "pro")]
    pub fn bookmark_on_current(&self) -> Option<usize> {
        let d = self.doc()?;
        d.bookmarks.iter().position(|b| b.page == d.current)
    }

    /// Remove the current page's bookmark, returning its name.
    #[cfg(feature = "pro")]
    pub fn bookmark_remove_current(&mut self) -> Option<String> {
        let idx = self.bookmark_on_current()?;
        Some(self.doc_mut()?.bookmarks.remove(idx).name)
    }

    /// Open the name prompt. Refused without a document or on a
    /// zero-page document (the flip/goto guard).
    #[cfg(feature = "pro")]
    pub fn bookmark_entry_start(&mut self) -> bool {
        let Some(d) = self.doc_mut() else { return false };
        if d.pages == 0 {
            return false;
        }
        d.bookmark_entry = Some(String::new());
        true
    }

    #[cfg(feature = "pro")]
    pub fn bookmark_entry_active(&self) -> bool {
        self.doc().is_some_and(|d| d.bookmark_entry.is_some())
    }

    /// Append a typed char (control chars ignored — the WM_CHAR filter
    /// catches them first, this is belt-and-braces; capped at NAME_CAP).
    #[cfg(feature = "pro")]
    pub fn bookmark_entry_char(&mut self, c: char) {
        if let Some(buf) = self.doc_mut().and_then(|d| d.bookmark_entry.as_mut()) {
            if !c.is_control() && buf.chars().count() < crate::bookmarks::NAME_CAP {
                buf.push(c);
            }
        }
    }

    #[cfg(feature = "pro")]
    pub fn bookmark_entry_backspace(&mut self) {
        if let Some(buf) = self.doc_mut().and_then(|d| d.bookmark_entry.as_mut()) {
            buf.pop();
        }
    }

    #[cfg(feature = "pro")]
    pub fn bookmark_entry_cancel(&mut self) {
        if let Some(d) = self.doc_mut() {
            d.bookmark_entry = None;
        }
    }

    /// Enter: add the bookmark on the current page and close the prompt.
    /// An empty (or whitespace) buffer commits the default name
    /// ("Page N", 1-based). Returns the committed name for the toast;
    /// None when no prompt was open (or the page raced into a bookmark).
    #[cfg(feature = "pro")]
    pub fn bookmark_entry_commit(&mut self) -> Option<String> {
        let d = self.doc_mut()?;
        let buf = d.bookmark_entry.take()?;
        let page = d.current;
        if d.bookmarks.iter().any(|b| b.page == page) {
            return None; // already bookmarked: toggle semantics say no dupes
        }
        let trimmed = buf.trim();
        let name = if trimmed.is_empty() {
            crate::bookmarks::default_name(page)
        } else {
            trimmed.to_string()
        };
        let pos = d.bookmarks.iter().position(|b| b.page > page).unwrap_or(d.bookmarks.len());
        d.bookmarks.insert(pos, crate::types::Bookmark { page, name: name.clone() });
        Some(name)
    }
```

- [ ] **Step 4: Run green, both editions**

Run: `cargo test --lib --features pro`
Expected: all pass (6 new + all existing).
Run: `cargo test --lib && cargo build`
Expected: free edition untouched, green.

- [ ] **Step 5: Commit**

```powershell
git add src/app.rs
git commit -m @'
feat(bookmarks): Pro toggle + name-entry state machine on App

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

### Task 7: Input + drawing wiring (Ctrl+B, prompt pill, centered toasts, reply arm)

**Files:**
- Modify: `src/window.rs` (many small seams — every one listed below)
- Modify: `src/d2d.rs` (`draw_badge` centered variant, `draw_bookmark_entry`, dispatch)

**Interfaces:**
- Consumes: everything from Tasks 5-6.
- Produces (internal to the window/renderer pair):
  - `Ui.badge_centered: bool` (Pro field), `Ui::show_toast_centered(String)` (Pro)
  - `Ui::bookmark_toggle_ui()` / `Ui::send_bookmarks()` / `Ui::dismiss_bookmark_entry()` (Pro)
  - `Ui::badge_display(&self) -> Option<(&str, f32, bool)>` — third element = centered (always `false` in free)
  - `Renderer::draw(&mut self, hwnd, app, badge: Option<(&str, f32, bool)>) -> Result<bool>` — **signature change**, both call sites in window.rs pass `badge_display()` unchanged
  - `Renderer::draw_badge(rt, text, alpha, centered: bool, client)`
  - `Renderer::draw_bookmark_entry(rt, buf, page_1: u32, client)` (Pro)

This task is Win32 glue — automated coverage is the compile + existing suites; behavior is pinned by the manual checklist added in Task 8. Steps below are grouped by seam; build after each group.

- [ ] **Step 1: Centered-badge plumbing (both editions compile)**

`src/window.rs` `Ui` struct — after `saves_inflight` (line 157-158):

```rust
    /// Pro: draw the current toast bottom-CENTER (the bookmark toasts sit
    /// in the goto-pill spot) instead of bottom-right.
    #[cfg(feature = "pro")]
    pub badge_centered: bool,
```

`run()`'s `Ui` initializer — after `saves_inflight: 0,`:

```rust
            #[cfg(feature = "pro")]
            badge_centered: false,
```

`show_toast` (line 2131-2137) — reset the flag first:

```rust
    /// Transient bottom-right pill with arbitrary text (badge family).
    fn show_toast(&mut self, text: String) {
        #[cfg(feature = "pro")]
        {
            self.badge_centered = false;
        }
        self.badge = Some((text, 0));
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_BADGE, 33, None);
        }
        self.invalidate();
    }

    /// Pro: same toast, but bottom-center (the bookmark add/remove
    /// feedback — spec'd to sit where the goto pill sits).
    #[cfg(feature = "pro")]
    fn show_toast_centered(&mut self, text: String) {
        self.show_toast(text);
        self.badge_centered = true;
    }
```

`badge_display` (line 2146-2155):

```rust
    fn badge_display(&self) -> Option<(&str, f32, bool)> {
        let centered = {
            #[cfg(feature = "pro")]
            {
                self.badge_centered
            }
            #[cfg(not(feature = "pro"))]
            {
                false
            }
        };
        self.badge.as_ref().map(|(text, ticks)| {
            let alpha = if *ticks <= BADGE_HOLD_TICKS {
                1.0
            } else {
                1.0 - (*ticks - BADGE_HOLD_TICKS) as f32 / BADGE_FADE_TICKS as f32
            };
            (text.as_str(), alpha.clamp(0.0, 1.0), centered)
        })
    }
```

`src/d2d.rs` — `draw` (line 254) and `draw_inner` (line 268-272) badge param becomes `Option<(&str, f32, bool)>`; the dispatch at line 327-329:

```rust
                    if let Some((text, alpha, centered)) = badge {
                        self.draw_badge(rt, text, alpha, centered, client)?;
                    }
```

`draw_badge` (line 685-715) — new param + rect selection:

```rust
    fn draw_badge(
        &mut self,
        rt: &ID2D1HwndRenderTarget,
        text: &str,
        alpha: f32,
        centered: bool,
        client: SizeU,
    ) -> Result<()> {
        if alpha <= 0.0 {
            return Ok(());
        }
        let s = self.scale();
        // Width tracks the text so longer toasts (save confirmation, the
        // free-edition Pro hint) fit; the page badge keeps its old size.
        let bw = ((text.chars().count() as f32 * 8.0 + 44.0) * s)
            .max(110.0 * s)
            .min(client.w as f32 - 16.0 * s);
        let (bh, m) = (36.0 * s, 24.0 * s);
        // Bottom-right by default; bottom-center for the bookmark toasts
        // (the goto-pill spot).
        let r = if centered {
            let cx = client.w as f32 / 2.0;
            RectF {
                left: cx - bw / 2.0,
                top: client.h as f32 - bh - m,
                right: cx + bw / 2.0,
                bottom: client.h as f32 - m,
            }
        } else {
            RectF {
                left: client.w as f32 - bw - m,
                top: client.h as f32 - bh - m,
                right: client.w as f32 - m,
                bottom: client.h as f32 - m,
            }
        };
        unsafe {
            let rr = D2D1_ROUNDED_RECT { rect: rectf(r), radiusX: bh / 2.0, radiusY: bh / 2.0 };
            let bg = rt.CreateSolidColorBrush(&color(0.0, 0.0, 0.0, 0.7 * alpha), None)?;
            rt.FillRoundedRectangle(&rr, &bg);
        }
        let fmt = self.text_format(14.0, true)?;
        self.draw_text(rt, text, &fmt, r, color(1.0, 1.0, 1.0, 0.9 * alpha))
    }
```

Build check: `cargo build && cargo build --features pro` — expected green.

- [ ] **Step 2: Ctrl+B in the global rung (Pro action + free toast)**

`src/window.rs` `on_key` global match — insert after the `0x4F if ctrl => self.open_dialog(), // Ctrl+O` arm (line 1321):

```rust
            #[cfg(feature = "pro")]
            0x42 if ctrl => self.bookmark_toggle_ui(), // Ctrl+B
            #[cfg(not(feature = "pro"))]
            0x42 if ctrl => {
                // Deliberate chord -> feedback on EVERY press (unlike the
                // incidental form-field click hint, which is one-shot).
                if self.app.doc().is_some() {
                    self.show_toast("Custom bookmarks are a pppdf Pro feature".into());
                }
            }
```

And the helpers (put them next to `save_doc`, after line 1942):

```rust
    /// Ctrl+B: toggle a bookmark on the current page. Bookmarked ->
    /// remove + auto-save; not bookmarked -> open the name prompt
    /// (commit auto-saves). Auto-save writes ONLY bookmarks — the PDF
    /// thread shadow-writes from the on-disk bytes, so pending form
    /// edits stay pending (Ctrl+S's job).
    #[cfg(feature = "pro")]
    fn bookmark_toggle_ui(&mut self) {
        let Some(d) = self.app.doc() else { return };
        if d.pages == 0 {
            return;
        }
        if self.app.bookmark_on_current().is_some() {
            if self.app.bookmark_remove_current().is_some() {
                self.send_bookmarks();
                self.show_toast_centered("Bookmark removed".into());
                self.invalidate();
            }
            return;
        }
        // Opening the prompt: it owns the keyboard. Commit any form edit,
        // close the TOC panel and the find bar first — two text surfaces
        // must never stack (the goto-prompt/Ctrl+F courtesy, extended).
        self.form_kill_focus();
        if let Some(d) = self.app.doc_mut() {
            d.overlay = None;
        }
        self.search_close_ui();
        if self.app.bookmark_entry_start() {
            self.invalidate();
        }
    }

    /// Ship the full current set to the PDF thread. Counts into
    /// saves_inflight exactly like a form save: window destruction must
    /// wait for the write (the PDF thread is detached).
    #[cfg(feature = "pro")]
    fn send_bookmarks(&mut self) {
        if let Some(d) = self.app.doc() {
            self.saves_inflight += 1;
            self.send_pdf(PdfCmd::SaveBookmarks {
                path: d.path.clone(),
                bookmarks: d.bookmarks.clone(),
            });
        }
    }
```

- [ ] **Step 3: The modal entry rung + WM_CHAR intake**

`on_key` — insert this rung directly AFTER the go-to-page rung's closing `}` (after line 1242) and BEFORE the Pro form-focus rung:

```rust
        // Bookmark-name entry (Ctrl+B on an unbookmarked page): modal like
        // the goto prompt — Enter commits (empty = "Page N"), Esc cancels,
        // Ctrl+F hands off to the find bar, all else swallowed (printables
        // arrive via WM_CHAR).
        #[cfg(feature = "pro")]
        if self.app.bookmark_entry_active() {
            match vk {
                _ if vk == VK_RETURN.0 => {
                    if let Some(name) = self.app.bookmark_entry_commit() {
                        self.send_bookmarks();
                        self.show_toast_centered(format!("Bookmarked: {name}"));
                    }
                }
                _ if vk == VK_ESCAPE.0 => self.app.bookmark_entry_cancel(),
                _ if vk == VK_BACK.0 => self.app.bookmark_entry_backspace(),
                _ if vk == 0x46 && unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 => {
                    self.app.bookmark_entry_cancel();
                    self.search_open_ui();
                }
                _ => {}
            }
            self.invalidate();
            return;
        }
```

WM_CHAR (line 1032-1100) — insert AFTER the password `if matches!(...)` block ends (line 1076) and BEFORE the find-bar block (line 1083):

```rust
                    // Bookmark-name entry: consumes printable chars with the
                    // find-bar filter (drops control bytes; BMP-only via
                    // char::from_u32 — the accepted shared limitation).
                    // Sits above the find bar, though the two can't be open
                    // at once (bookmark_toggle_ui closes the bar).
                    #[cfg(feature = "pro")]
                    if self.app.bookmark_entry_active() {
                        let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                        let c32 = wparam.0 as u32;
                        if !ctrl && c32 >= 0x20 {
                            if let Some(c) = char::from_u32(c32) {
                                self.app.bookmark_entry_char(c);
                                self.invalidate();
                            }
                        }
                        return LRESULT(0);
                    }
```

- [ ] **Step 4: Dismissal + modality edges**

Add the helper next to `dismiss_goto_entry` (line 1555-1560):

```rust
    /// Close the bookmark-name prompt if open — same rationale as
    /// dismiss_goto_entry: mouse interaction and the context menu must
    /// not stack surfaces on top of an invisible keyboard owner.
    #[cfg(feature = "pro")]
    fn dismiss_bookmark_entry(&mut self) {
        if self.app.bookmark_entry_active() {
            self.app.bookmark_entry_cancel();
            self.invalidate();
        }
    }
```

Call it at every `dismiss_goto_entry()` call site — there are exactly 5; add directly below each:

1. `WM_NCLBUTTONDBLCLK` arm (line ~406)
2. `WM_MOUSEWHEEL` arm (line ~497)
3. `WM_LBUTTONDOWN` arm (line ~567)
4. `WM_LBUTTONDBLCLK` arm (line ~787)
5. `show_menu` (line ~1563)

each as:

```rust
                    #[cfg(feature = "pro")]
                    self.dismiss_bookmark_entry();
```

(indentation to match the site). `modal_active` (line 2353-2357) — block history nav while the prompt is up:

```rust
    fn modal_active(&self) -> bool {
        #[cfg(feature = "pro")]
        if self.app.bookmark_entry_active() {
            return true;
        }
        matches!(self.app.mode, crate::app::Mode::Password { .. })
            || self.app.help_visible
            || self.app.goto_entry_active()
    }
```

- [ ] **Step 5: `BookmarksSaved` reply arm**

In `pump_pdf_replies`, after the whole `PdfReply::Saved { result } => match result { ... },` arm (line 2256-2330):

```rust
                #[cfg(feature = "pro")]
                PdfReply::BookmarksSaved { result } => match result {
                    Ok(()) => {
                        self.saves_inflight = self.saves_inflight.saturating_sub(1);
                        // Same deferred-intent discipline as Saved(Ok), minus
                        // the toast (the toggle already gave feedback) and
                        // minus any form_dirty logic (bookmark saves never
                        // touch form state).
                        if let Some(path) = self.pending_open.take() {
                            self.open_document(path);
                            self.reply_rx = Some(rx);
                            return;
                        }
                        if self.saves_inflight > 0 {
                            self.reply_rx = Some(rx);
                            return;
                        }
                        if self.pending_close {
                            self.pending_close = false;
                            if self.app.form_dirty {
                                self.quit();
                            } else {
                                unsafe {
                                    let _ = DestroyWindow(self.hwnd);
                                }
                            }
                            self.reply_rx = Some(rx);
                            return;
                        }
                    }
                    Err(e) => {
                        self.saves_inflight = self.saves_inflight.saturating_sub(1);
                        crate::dlog!("ui: bookmark save failed: {e}");
                        // The bookmark stays in DocState (the TOC keeps
                        // working this session; the next successful toggle
                        // rewrites the full set). A failing target file
                        // deserves eyes, not a silent deferred close/open —
                        // same conservative stance as Saved(Err).
                        self.pending_close = false;
                        self.pending_open = None;
                        self.show_toast("Couldn't save bookmarks".into());
                    }
                },
```

- [ ] **Step 6: The prompt pill (`src/d2d.rs`)**

Add after `draw_goto` (line 683):

```rust
    /// The Ctrl+B bookmark-name prompt: the goto pill's twin, bottom-
    /// center. An empty buffer shows the default name Enter would commit.
    #[cfg(feature = "pro")]
    fn draw_bookmark_entry(
        &mut self,
        rt: &ID2D1HwndRenderTarget,
        buf: &str,
        page_1: u32,
        client: SizeU,
    ) -> Result<()> {
        let s = self.scale();
        let text = if buf.is_empty() {
            format!("Bookmark: _ (Enter = \"Page {page_1}\")")
        } else {
            format!("Bookmark: {buf}_")
        };
        let bw = ((text.chars().count() as f32 * 8.0 + 44.0) * s)
            .max(240.0 * s)
            .min(client.w as f32 - 16.0 * s);
        let (bh, m) = (36.0 * s, 24.0 * s);
        let cx = client.w as f32 / 2.0;
        let r = RectF {
            left: cx - bw / 2.0,
            top: client.h as f32 - bh - m,
            right: cx + bw / 2.0,
            bottom: client.h as f32 - m,
        };
        unsafe {
            let rr = D2D1_ROUNDED_RECT { rect: rectf(r), radiusX: bh / 2.0, radiusY: bh / 2.0 };
            let bg = rt.CreateSolidColorBrush(&color(0.0, 0.0, 0.0, 0.8), None)?;
            rt.FillRoundedRectangle(&rr, &bg);
            let border = rt.CreateSolidColorBrush(&color(1.0, 1.0, 1.0, 0.25), None)?;
            rt.DrawRoundedRectangle(&rr, &border, 1.0 * s, None);
        }
        let fmt = self.text_format(14.0, true)?;
        self.draw_text(rt, &text, &fmt, r, color(1.0, 1.0, 1.0, 0.92))
    }
```

Dispatch in `draw_inner`'s `Mode::Doc` arm, after the `d.goto_entry` block (line 330-332):

```rust
                    #[cfg(feature = "pro")]
                    if let Some(buf) = &d.bookmark_entry {
                        self.draw_bookmark_entry(rt, buf, d.current as u32 + 1, client)?;
                    }
```

- [ ] **Step 7: Verify both editions compile and every suite passes**

Run: `cargo build && cargo test`
Expected: free edition green (free gets: Ctrl+B toast arm, centered-badge plumbing compiled out to `false`, everything else untouched).
Run: `cargo build --features pro && cargo test --features pro`
Expected: Pro green, including all integration tests.

- [ ] **Step 8: Smoke-run the real app (manual, quick)**

Run: `cargo run --features pro -- tests/fixtures/form.pdf`
Check: Ctrl+B opens the bottom-center pill showing `Bookmark: _ (Enter = "Page 1")`; typing works; Enter toasts `Bookmarked: ...` bottom-center; Tab opens the TOC with a styled "Bookmarks" header; Enter on the header collapses/expands without closing the panel; clicking the bookmark row jumps and closes; Ctrl+B again toasts `Bookmark removed`; close and reopen the file — the bookmark survived. Then `cargo run -- tests/fixtures/form.pdf` (free): Ctrl+B toasts the Pro hint bottom-right on every press; the previously saved bookmark still shows in the TOC read-only.
(If anything misbehaves, fix before committing — this is the only pre-Task-8 end-to-end check of the Win32 glue.)

- [ ] **Step 9: Commit**

```powershell
git add src/window.rs src/d2d.rs
git commit -m @'
feat(bookmarks): Ctrl+B toggle, name prompt pill, centered toasts, free-edition hint

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

### Task 8: Documentation sync + full verification

**Files:**
- Modify: `README.md:85-88` (keybinding table)
- Modify: `src/d2d.rs:44-49` (`PRO_HELP_ROWS`)
- Modify: `docs/manual-tests.md`
- Modify: `TODO.md:12`

**Interfaces:** none — the byte-sync rule is the contract: README row text and the `PRO_HELP_ROWS` entry must match byte-for-byte minus the ` (Pro)` suffix.

- [ ] **Step 1: README keybinding row**

Insert into the table after the `| Ctrl+S | Save filled form in place (Pro) |` row (line 88):

```markdown
| Ctrl+B | Add / remove a bookmark on this page (Pro) |
```

- [ ] **Step 2: `PRO_HELP_ROWS` (src/d2d.rs:45-49)**

Append the row (byte-identical to the README's Action text minus ` (Pro)`):

```rust
    ("Ctrl+B", "Add / remove a bookmark on this page"),
```

- [ ] **Step 3: Manual test checklist**

In `docs/manual-tests.md`: append to the search-robustness line 53 so it reads:

```markdown
- [ ] With the bar open: Ctrl+W still quits, Ctrl+O still opens, Ctrl+N still spawns, Ctrl+S (Pro) still saves, Ctrl+B (Pro) still toggles a bookmark
```

and add a new section after the "Save UX (Pro)" section (end of file):

```markdown
Custom bookmarks (Pro) — any multi-page PDF in a writable folder:
- [ ] Ctrl+B on an unbookmarked page: bottom-center prompt pill appears showing `Bookmark: _ (Enter = "Page N")`; typing fills it; Backspace deletes; Esc cancels (no bookmark)
- [ ] Enter commits: "Bookmarked: {name}" toast bottom-center; empty Enter commits the default "Page N"
- [ ] Ctrl+B on a bookmarked page removes it: "Bookmark removed" toast bottom-center (both toasts fade like the page badge)
- [ ] Tab: TOC shows a "Bookmarks" header (accent-colored) above the outline; entries sorted by name with numeric awareness ("2 - x" above "10 - x"); Enter/click on the header collapses/expands without closing the panel; Enter/click on a bookmark jumps, closes the panel, records history (Alt+Left returns)
- [ ] Bookmarks persist: close the window, reopen the same file — bookmarks intact; remove all, reopen — gone
- [ ] Only bookmarks are saved: type into a form field (no Ctrl+S), Ctrl+B, reopen the file — bookmark present, the typed text NOT saved; then re-type and Ctrl+S — both present after reopen
- [ ] Read-only target (mark the file read-only): Ctrl+B toasts "Couldn't save bookmarks"; the bookmark still shows in this session's TOC
- [ ] Encrypted PDF: bookmark an open password-protected file, close, reopen (re-enter the password) — bookmark intact
- [ ] While the prompt is open: mouse wheel / click / right-click menu dismiss it; Ctrl+F cancels it and opens the find bar; mouse back/forward do nothing
- [ ] G goto prompt open: Ctrl+B is swallowed (no prompt stacking); find bar open: Ctrl+B closes the bar before opening the prompt
- [ ] Free edition: Ctrl+B toasts "Custom bookmarks are a pppdf Pro feature" (bottom-right) on EVERY press; Pro-created bookmarks still display in the free TOC and navigate
```

- [ ] **Step 4: TODO.md**

Tick the roadmap item (line 12):

```markdown
- [x] Set custom bookmarks
```

- [ ] **Step 5: Byte-sync check**

Compare the README row's Action text `Add / remove a bookmark on this page (Pro)` with the `PRO_HELP_ROWS` string `Add / remove a bookmark on this page` — identical minus the ` (Pro)` suffix, matching the established pattern of README lines 86-88 vs `PRO_HELP_ROWS`.

- [ ] **Step 6: Full final verification, both editions**

Run: `cargo build --release && cargo test`
Expected: green.
Run: `cargo build --release --features pro && cargo test --features pro`
Expected: green — including all 5 bookmark integration tests and every pre-existing test.

- [ ] **Step 7: Commit**

```powershell
git add README.md src/d2d.rs docs/manual-tests.md TODO.md
git commit -m @'
docs(bookmarks): README row, F1 help row, manual checklist; tick roadmap item

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

## Post-plan notes for the executor

- **Line numbers drift.** They are anchors from the pre-implementation tree; always locate the named function/arm, not the absolute line.
- **Do not touch `vendor/`** under any circumstance. The two upstream dead-code warnings on full rebuilds are expected; the zero-warnings bar applies to pppdf's own code only.
- **Integration tests are serialized by `PDFIUM_TEST_LOCK`** — never remove the lock or the `join_pdf_thread` calls; never send `Shutdown` before draining the replies a test expects.
- If PDFium's attachment API behaves unexpectedly against the real DLL (e.g. `create_attachment_from_bytes` failing on the generated fixture), stop and report — do not switch storage mechanisms unilaterally; the spec decision (Approach A) was explicit.

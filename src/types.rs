//! Types shared between the UI thread and the PDF thread.

/// One entry of the PDF outline, pre-order flattened.
#[derive(Clone, Debug, PartialEq)]
pub struct OutlineItem {
    pub title: String,
    /// 0-based target page, if the entry resolves to one.
    pub page: Option<u16>,
    /// 0 = top level.
    pub depth: u8,
    pub has_children: bool,
}

use crate::layout::{RectF, SizeU};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub enum LinkTarget {
    /// 0-based page index inside this document.
    Page(u16),
    Uri(String),
}

/// A clickable region on a page. `rect_pt` is in PDF points
/// (origin bottom-left, y up, so `top > bottom` numerically).
#[derive(Clone, Debug, PartialEq)]
pub struct PageLink {
    pub rect_pt: RectF,
    pub target: LinkTarget,
}

/// Coarse widget classification: enough for cursor choice and hit behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Other,
}

/// One form-widget annotation on a page. `rect_pt` uses the same convention
/// as `PageLink::rect_pt` (PDF points, origin bottom-left, top > bottom).
#[derive(Clone, Debug, PartialEq)]
pub struct FormField {
    pub rect_pt: RectF,
    pub kind: FieldKind,
}

/// Search options forwarded to PDFium's native matcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SearchOptions {
    pub match_case: bool,
    pub whole_word: bool,
}

/// One search hit. `rects` uses the same convention as `PageLink::rect_pt`
/// (PDF points, origin bottom-left, top > bottom) — a Vec because one hit
/// can span line wraps or font changes (PDFium merges boxes only within a
/// line/style run).
#[derive(Clone, Debug, PartialEq)]
pub struct SearchMatch {
    pub rects: Vec<RectF>,
}

/// One interactive form input event, in PDF page coordinates (points,
/// origin bottom-left — what App::pdf_point_at produces).
#[cfg(feature = "pro")]
#[derive(Clone, Debug)]
pub enum FormEvent {
    MouseDown { x: f32, y: f32, modifiers: i32 },
    MouseUp { x: f32, y: f32, modifiers: i32 },
    MouseMove { x: f32, y: f32, modifiers: i32 },
    /// Real double-click on a TEXT field (word selection). Button-like
    /// widgets never receive this — PDFium's editor ignores DblClk on
    /// them, so window.rs sends a plain MouseDown instead (rapid clicks
    /// on a checkbox must toggle it every time).
    MouseDblClk { x: f32, y: f32, modifiers: i32 },
    Char { c: u16, modifiers: i32 },
    KeyDown { vk: u16, modifiers: i32 },
    /// Ctrl+V: replace the focused field's selection with clipboard text.
    ReplaceSelection { text: String },
    /// Move form focus to the annotation at a point WITHOUT button events —
    /// Tab cycling must never click (and thus toggle) the widgets it passes.
    Focus { x: f32, y: f32 },
    KillFocus,
}

/// UI thread -> PDF thread.
#[derive(Debug)]
pub enum PdfCmd {
    Load { path: PathBuf, password: Option<String> },
    /// `px` is the desired OUTPUT size — already swapped for 90°/270°.
    /// `rot` = clockwise quarter-turns (0..=3) to render the page at.
    Render { page: u16, px: SizeU, rot: u8, visible: bool },
    /// Start (or replace) the streaming text scan. `start_page` = the page
    /// the scan begins at; it wraps around the document end. Both editions.
    Search { id: u32, query: String, options: SearchOptions, start_page: u16 },
    /// Kill the active scan and any `Search` queued before this command.
    CancelSearch,
    /// One interactive form event targeting `page`.
    #[cfg(feature = "pro")]
    Form { page: u16, ev: FormEvent },
    /// Save the current document back to its source path.
    #[cfg(feature = "pro")]
    SaveDoc,
    Shutdown,
}

/// PDF thread -> UI thread.
#[derive(Debug)]
pub enum PdfReply {
    Loaded {
        path: PathBuf,
        pages: u16,
        page_sizes: Vec<(f32, f32)>,
        outline: Vec<OutlineItem>,
        has_forms: bool,
    },
    NeedsPassword {
        path: PathBuf,
    },
    LoadFailed {
        path: PathBuf,
        reason: String,
    },
    Rendered {
        page: u16,
        px: SizeU,
        /// The quarter-turn rotation this bitmap was rendered at.
        rot: u8,
        bgra: Vec<u8>,
        links: Vec<PageLink>,
        fields: Vec<FormField>,
    },
    RenderFailed {
        page: u16,
        reason: String,
    },
    /// One scanned page's hits (sent only for pages WITH hits). Stale `id`s
    /// are dropped by the UI.
    SearchHits { id: u32, page: u16, matches: Vec<SearchMatch> },
    /// The scan with this `id` covered every page.
    SearchDone { id: u32 },
    /// A form field's value changed (FFI_OnChange) — the open document now
    /// differs from what is on disk.
    #[cfg(feature = "pro")]
    FormDirty,
    /// Result of a `SaveDoc` request.
    #[cfg(feature = "pro")]
    Saved {
        result: Result<(), String>,
    },
}

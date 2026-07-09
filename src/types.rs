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

/// UI thread -> PDF thread.
#[derive(Debug)]
pub enum PdfCmd {
    Load { path: PathBuf, password: Option<String> },
    /// `px` is the desired OUTPUT size — already swapped for 90°/270°.
    /// `rot` = clockwise quarter-turns (0..=3) to render the page at.
    Render { page: u16, px: SizeU, rot: u8, visible: bool },
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
}

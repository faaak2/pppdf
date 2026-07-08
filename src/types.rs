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

/// UI thread -> PDF thread.
#[derive(Debug)]
pub enum PdfCmd {
    Load { path: PathBuf, password: Option<String> },
    Render { page: u16, px: SizeU, visible: bool },
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
        bgra: Vec<u8>,
        links: Vec<PageLink>,
    },
    RenderFailed {
        page: u16,
        reason: String,
    },
}

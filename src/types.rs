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

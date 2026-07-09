//! Interactive AcroForm engine: drives PDFium's form-fill machinery (the
//! environment pdfium-render already initialized at load) by forwarding
//! events through the raw FORM_On* bindings. Everything here runs on the
//! PDF thread — PDFium invokes the FPDF_FORMFILLINFO callbacks synchronously
//! on the calling thread, so plain thread_locals hold the callback state.

use crate::types::{FormEvent, PdfReply};
use pdfium_render::prelude::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::os::raw::c_int;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

struct Timer {
    interval: Duration,
    next_fire: Instant,
    func: unsafe extern "C" fn(c_int),
}

#[derive(Default)]
struct CallbackState {
    invalidated: bool,
    reply_tx: Option<Sender<PdfReply>>,
    timers: BTreeMap<i32, Timer>,
    next_timer_id: i32,
}

thread_local! {
    static CB: RefCell<CallbackState> = RefCell::new(CallbackState::default());
}

unsafe extern "C" fn ffi_invalidate(
    _this: *mut FPDF_FORMFILLINFO,
    _page: FPDF_PAGE,
    _l: f64,
    _t: f64,
    _r: f64,
    _b: f64,
) {
    CB.with(|s| s.borrow_mut().invalidated = true);
}

unsafe extern "C" fn ffi_on_change(_this: *mut FPDF_FORMFILLINFO) {
    CB.with(|s| {
        let mut s = s.borrow_mut();
        s.invalidated = true; // a change always needs fresh pixels
        if let Some(tx) = &s.reply_tx {
            let _ = tx.send(PdfReply::FormDirty);
        }
    });
}

unsafe extern "C" fn ffi_set_timer(
    _this: *mut FPDF_FORMFILLINFO,
    elapse_ms: c_int,
    func: Option<unsafe extern "C" fn(c_int)>,
) -> c_int {
    let Some(func) = func else { return 0 };
    // Floor (not just a min-1ms clamp): investigated 2026-07-09
    // (.superpowers/sdd/flood-investigation.md) — PDFium sometimes requests
    // a ~0ms timer during a form focus transition (e.g. deferred caret
    // setup). A `.max(1)` clamp let that become a 1ms repeating timer,
    // which `wait_cmd` fired on every wake, each fire triggering a
    // full-page `render_page` — a CPU-pegging flood (454 renders/s
    // measured with a forced ~0ms request) that persists until
    // `KillFocus`. PDFium's own caret blink is ~500ms, so a 100ms floor is
    // imperceptible for legitimate timers (caret blink, deferred work) and
    // caps the worst case at ~10 renders/s by itself; the render-coalescing
    // throttle in pdf.rs is the primary defense and brings that down to
    // ~30/s regardless of the requested interval.
    let interval = Duration::from_millis((elapse_ms.max(0) as u64).clamp(100, 60_000));
    CB.with(|s| {
        let mut s = s.borrow_mut();
        s.next_timer_id += 1;
        let id = s.next_timer_id;
        s.timers.insert(id, Timer { interval, next_fire: Instant::now() + interval, func });
        id
    })
}

unsafe extern "C" fn ffi_kill_timer(_this: *mut FPDF_FORMFILLINFO, id: c_int) {
    CB.with(|s| {
        s.borrow_mut().timers.remove(&id);
    });
}

/// Owns the raw page handle interactive events target. The FORM handle and
/// document handle belong to pdfium-render's PdfDocument/PdfForm — we only
/// borrow their values; teardown() must run before the document is dropped
/// or replaced.
pub struct FormEngine {
    form: FPDF_FORMHANDLE,
    doc: FPDF_DOCUMENT,
    /// (page index, raw page handle) FORM events currently target.
    page: Option<(u16, FPDF_PAGE)>,
}

impl FormEngine {
    /// None when the document has no form. Installs the interactive
    /// callbacks into the FPDF_FORMFILLINFO pdfium-render registered at
    /// init — PDFium reads these function pointers at call time, so
    /// post-init installation is valid.
    pub fn new(doc: &mut PdfDocument<'static>, reply_tx: Sender<PdfReply>) -> Option<Self> {
        let doc_h = doc.handle();
        let form = doc.form_mut()?;
        let form_h = form.handle();
        let info = form.form_fill_info_mut();
        info.FFI_Invalidate = Some(ffi_invalidate);
        info.FFI_OnChange = Some(ffi_on_change);
        info.FFI_SetTimer = Some(ffi_set_timer);
        info.FFI_KillTimer = Some(ffi_kill_timer);
        CB.with(|s| {
            *s.borrow_mut() =
                CallbackState { reply_tx: Some(reply_tx), ..CallbackState::default() };
        });
        Some(Self { form: form_h, doc: doc_h, page: None })
    }

    /// Raw page handle for `idx`, switching the form's page context if
    /// needed. PDFium refcounts pages per index, so this handle aliases the
    /// one the render path opens — focus and caret state drawn by FFLDraw
    /// during rendering belong to the same page view we feed events to.
    fn ensure_page(&mut self, b: &dyn PdfiumLibraryBindings, idx: u16) -> Option<FPDF_PAGE> {
        if let Some((cur, h)) = self.page {
            if cur == idx {
                return Some(h);
            }
            unsafe {
                b.FORM_OnBeforeClosePage(h, self.form);
                b.FPDF_ClosePage(h);
            }
            self.page = None;
        }
        let h = unsafe { b.FPDF_LoadPage(self.doc, idx as c_int) };
        if h.is_null() {
            return None;
        }
        unsafe {
            b.FORM_OnAfterLoadPage(h, self.form);
        }
        self.page = Some((idx, h));
        Some(h)
    }

    pub fn handle_event(&mut self, b: &dyn PdfiumLibraryBindings, page: u16, ev: FormEvent) {
        let Some(h) = self.ensure_page(b, page) else { return };
        unsafe {
            match ev {
                FormEvent::MouseDown { x, y, modifiers } => {
                    b.FORM_OnLButtonDown(self.form, h, modifiers, x as f64, y as f64);
                }
                FormEvent::MouseUp { x, y, modifiers } => {
                    b.FORM_OnLButtonUp(self.form, h, modifiers, x as f64, y as f64);
                }
                FormEvent::MouseMove { x, y, modifiers } => {
                    b.FORM_OnMouseMove(self.form, h, modifiers, x as f64, y as f64);
                }
                FormEvent::Char { c, modifiers } => {
                    b.FORM_OnChar(self.form, h, c as c_int, modifiers);
                }
                FormEvent::KeyDown { vk, modifiers } => {
                    b.FORM_OnKeyDown(self.form, h, vk as c_int, modifiers);
                }
                FormEvent::KillFocus => {
                    b.FORM_ForceToKillFocus(self.form);
                }
            }
        }
    }

    /// Commit the in-progress edit, then write the document atomically over
    /// `path`: full save to a temp file in the same directory, then rename.
    /// The original is untouched on any failure. Rename is retried a few
    /// times — antivirus briefly locks freshly written files (same idiom as
    /// pdf.rs's pdfium extraction).
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
        let dir = path.parent().ok_or("path has no parent directory")?;
        let tmp = dir.join(format!(".pppdf-save-{}.tmp", std::process::id()));
        if let Err(e) = std::fs::write(&tmp, &bytes) {
            // Don't leave a partial temp file behind (disk full etc.) —
            // mirrors the retry-exhaustion cleanup below.
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

    pub fn take_invalidated(&mut self) -> bool {
        CB.with(|s| std::mem::take(&mut s.borrow_mut().invalidated))
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        CB.with(|s| s.borrow().timers.values().map(|t| t.next_fire).min())
    }

    /// Fire every due timer callback (caret blink etc.); PDFium reacts by
    /// invoking FFI_Invalidate, which take_invalidated() then reports.
    pub fn fire_due_timers(&mut self) {
        let now = Instant::now();
        // Collect first: the callback may call ffi_set_timer/ffi_kill_timer,
        // which re-borrows CB.
        let due: Vec<(i32, unsafe extern "C" fn(c_int))> = CB.with(|s| {
            let mut s = s.borrow_mut();
            let mut due = Vec::new();
            for (id, t) in s.timers.iter_mut() {
                if t.next_fire <= now {
                    t.next_fire = now + t.interval;
                    due.push((*id, t.func));
                }
            }
            due
        });
        for (id, func) in due {
            unsafe { func(id) };
        }
    }

    /// Must run before the PdfDocument is dropped or replaced.
    pub fn teardown(&mut self, b: &dyn PdfiumLibraryBindings) {
        if let Some((_, h)) = self.page.take() {
            unsafe {
                b.FORM_OnBeforeClosePage(h, self.form);
                b.FPDF_ClosePage(h);
            }
        }
        CB.with(|s| *s.borrow_mut() = CallbackState::default());
    }
}

/// Read all form field values from a PDF on disk. Test/diagnostic helper —
/// uses the process-wide pdfium binding.
pub fn read_field_values(
    path: &std::path::Path,
) -> Result<std::collections::HashMap<String, Option<String>>, String> {
    let pdfium = crate::pdf::pdfium()?;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let doc = pdfium.load_pdf_from_byte_vec(bytes, None).map_err(|e| format!("{e:?}"))?;
    let form = doc.form().ok_or("document has no form")?;
    let pages = doc.pages();
    Ok(form.field_values(pages))
}

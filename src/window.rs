//! The borderless Win32 window: class, WndProc, chromeless tricks, input.

use crate::app::App;
use crate::d2d::Renderer;
use crate::layout::{hit_zone, PointF, SizeU, Zone};
use crate::types::{LinkTarget, PdfCmd, PdfReply};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND, DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, MonitorFromWindow, UpdateWindow, HBRUSH, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemServices::MK_CONTROL;
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VK_BACK, VK_CONTROL, VK_DOWN, VK_END, VK_ESCAPE,
    VK_F1, VK_F11, VK_F3, VK_HOME, VK_LEFT, VK_NEXT, VK_NUMPAD0, VK_PRIOR, VK_RETURN, VK_RIGHT,
    VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
// Pro-only VKs: typing mode forwards these to the form. Free build never
// references them, so gate the import to keep it warning-free.
#[cfg(feature = "pro")]
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DELETE, VK_MENU};
use windows::Win32::UI::Shell::{
    DragAcceptFiles, DragFinish, DragQueryFileW, SHAppBarMessage, ShellExecuteW, HDROP,
    ABM_GETAUTOHIDEBAREX, ABM_GETSTATE, ABS_AUTOHIDE, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_APP_PDF_REPLY: u32 = WM_APP + 1;

pub const CMD_OPEN: usize = 1;
pub const CMD_TOC: usize = 2;
pub const CMD_FIT: usize = 3;
pub const CMD_MINIMIZE: usize = 4;
pub const CMD_CLOSE: usize = 5;
pub const CMD_NEW_WINDOW: usize = 6;
pub const CMD_OPEN_NEW_WINDOW: usize = 7;
pub const CMD_HELP: usize = 8;
pub const CMD_ALWAYS_ON_TOP: usize = 9;
pub const CMD_ABOUT: usize = 10;

/// WM_APPCOMMAND commands (winuser.h) — sent by the mouse back/forward
/// X-buttons (DefWindowProc synthesizes them from unhandled X-button
/// clicks, client and non-client alike) and keyboard browser keys.
const APPCOMMAND_BROWSER_BACKWARD: u32 = 1;
const APPCOMMAND_BROWSER_FORWARD: u32 = 2;

const GRIP_DIP: f32 = 8.0;
const MIN_SIZE_DIP: i32 = 200;

/// Page-shaped default window (w:h), the proportions pppdf has always opened
/// at. Startup keeps this ratio whatever the screen is.
const DEFAULT_ASPECT: f32 = 850.0 / 1100.0;
/// Startup height as a share of the screen: tall enough to read a page, short
/// enough to leave the desktop visible around it.
const DEFAULT_SCREEN_SHARE: f32 = 0.75;

/// Startup size in physical px from a screen size in physical px. Height is
/// [`DEFAULT_SCREEN_SHARE`] of the screen, width follows [`DEFAULT_ASPECT`].
/// Split out from the Win32 metrics call so the arithmetic is testable.
fn default_size_for_screen(sw: i32, sh: i32) -> (i32, i32) {
    // No usable metrics (headless/odd session): the historical fixed size.
    if sw <= 0 || sh <= 0 {
        return (850, 1100);
    }
    let mut h = sh as f32 * DEFAULT_SCREEN_SHARE;
    let mut w = h * DEFAULT_ASPECT;
    // A rotated (portrait) monitor is narrow enough that a page-shaped window
    // this tall would hang off both sides — give up height to keep the ratio.
    let max_w = sw as f32 * DEFAULT_SCREEN_SHARE;
    if w > max_w {
        w = max_w;
        h = w / DEFAULT_ASPECT;
    }
    let floor = MIN_SIZE_DIP as f32;
    (w.max(floor).round() as i32, h.max(floor).round() as i32)
}

/// Startup size for the primary monitor — where `CW_USEDEFAULT` puts a window.
/// The manifest makes the process per-monitor DPI aware from the first line of
/// `main`, so these metrics are physical pixels and need no DPI scaling: the
/// window comes out the same share of the screen at any DPI or resolution.
fn default_window_size() -> (i32, i32) {
    let (sw, sh) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    default_size_for_screen(sw, sh)
}

pub const TIMER_RESIZE: usize = 1;
pub const TIMER_BADGE: usize = 2;
/// One-shot retry after a Direct2D device loss: throttles the
/// recreate-and-repaint so a sustained loss can't spin WM_PAINT at 100% CPU.
pub const TIMER_RECREATE: usize = 3;

/// In-progress left-button drag-pan: origin point, scroll at drag start, and
/// whether the pointer has moved past the OS drag threshold (click vs. pan).
pub struct Drag {
    start: PointF,
    start_scroll: PointF,
    moved: bool,
}

/// Where the F11 cycle currently stands. `saved_rect` (the pre-cycle window
/// rect) is written on leaving Normal and taken on returning to it.
#[derive(Clone, Copy, PartialEq)]
pub enum FillStage {
    Normal,
    HeightFit,
    Filled,
}

pub struct Ui {
    pub hwnd: HWND,
    pub app: App,
    pub renderer: Option<Renderer>,
    pub pdf_tx: Option<Sender<PdfCmd>>,
    pub reply_rx: Option<Receiver<PdfReply>>,
    pub initial_pdf: Option<PathBuf>,
    /// Bottom-right "N / total" pill: (text, ticks since shown at 33 ms/tick).
    pub badge: Option<(String, u32)>,
    /// Active drag-pan state while the left button is held over a zoomed page.
    pub drag: Option<Drag>,
    pub suppress_click: bool,
    /// A click just dismissed a modal surface (F1 help) or activated a TOC
    /// entry, closing the panel. If that click turns out to be the first of
    /// a double-click, the second one hit-tests as HTCAPTION against the
    /// now-bare page and would cycle the window — swallow it once.
    pub swallow_dblclk: bool,
    /// Pre-F11 window rect, so the cycle's final press restores it exactly.
    pub saved_rect: Option<RECT>,
    /// F11 cycle stage: Normal -> HeightFit -> Filled -> Normal.
    pub fill_stage: FillStage,
    /// Leftover sub-notch WM_MOUSEWHEEL delta (precision touchpads send many
    /// small deltas instead of whole 120-unit notches); accumulated across
    /// events so they aren't truncated to zero forever.
    pub wheel_residual: i32,
    /// Whether the window is pinned above others (WS_EX_TOPMOST). Toggled from
    /// the context menu; not persisted (the app keeps no config).
    pub always_on_top: bool,
    /// Pro: left button went down inside a form field; route move/up to the
    /// form until release.
    #[cfg(feature = "pro")]
    pub form_mouse: bool,
    /// Pro: a Save/Discard/Cancel prompt chose "Save" while closing — the
    /// window should destroy itself once the pending SaveDoc completes,
    /// instead of showing the "Saved" toast. Mutually exclusive with
    /// pending_open (last user intent wins).
    #[cfg(feature = "pro")]
    pub pending_close: bool,
    /// Pro: a Save/Discard/Cancel prompt chose "Save" (or a save was already
    /// in flight) while an Open was requested — replace the document with this
    /// path once the pending SaveDoc resolves, instead of silently discarding
    /// the unsaved edits. Mutually exclusive with pending_close.
    #[cfg(feature = "pro")]
    pub pending_open: Option<std::path::PathBuf>,
    /// Pro: number of dispatched SaveDoc commands whose Saved reply hasn't
    /// arrived. A counter, not a bool: overlapping saves (a 2nd Ctrl+S while
    /// the 1st is still in flight) must both be accounted for, or the first
    /// Saved(Ok) would clear the latch while another write is still running.
    /// quit() and the open gate defer while > 0 — destroying the window exits
    /// the process and would kill the detached PDF thread possibly mid-write.
    #[cfg(feature = "pro")]
    pub saves_inflight: u32,
    /// Pro: draw the current toast bottom-CENTER (the bookmark toasts sit
    /// in the goto-pill spot) instead of bottom-right.
    #[cfg(feature = "pro")]
    pub badge_centered: bool,
    /// Free: the one-per-run "Pro feature" hint has been shown.
    #[cfg(not(feature = "pro"))]
    pub hint_shown: bool,
}

const BADGE_HOLD_TICKS: u32 = 21; // ~700 ms at 33 ms/tick
const BADGE_FADE_TICKS: u32 = 10; // ~330 ms fade

pub fn run(app: App, initial_pdf: Option<PathBuf>) -> Result<()> {
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class_name = w!("pppdf_window");

        // Embedded via build.rs (winresource, id "1"). A missing/failed load
        // just falls back to the default icon — never worth crashing over.
        let icon = LoadIconW(Some(instance.into()), PCWSTR(1 as *const u16)).unwrap_or_default();

        let wc = WNDCLASSEXW {
            cbSize: core::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: icon,
            hIconSm: icon,
            hbrBackground: HBRUSH::default(), // we paint everything ourselves
            lpszClassName: class_name,
            ..Default::default()
        };
        // NEVER put this call inside debug_assert! — the assertion (and the
        // call!) would vanish in release builds and window creation would fail.
        let atom = RegisterClassExW(&wc);
        debug_assert!(atom != 0);

        let mut ui = Box::new(Ui {
            hwnd: HWND::default(),
            app,
            renderer: None,
            pdf_tx: None,
            reply_rx: None,
            initial_pdf,
            badge: None,
            drag: None,
            suppress_click: false,
            swallow_dblclk: false,
            saved_rect: None,
            fill_stage: FillStage::Normal,
            wheel_residual: 0,
            always_on_top: false,
            #[cfg(feature = "pro")]
            form_mouse: false,
            #[cfg(feature = "pro")]
            pending_close: false,
            #[cfg(feature = "pro")]
            pending_open: None,
            #[cfg(feature = "pro")]
            saves_inflight: 0,
            #[cfg(feature = "pro")]
            badge_centered: false,
            #[cfg(not(feature = "pro"))]
            hint_shown: false,
        });

        // Every flag here is load-bearing for native snap/animations; the
        // caption never shows because WM_NCCALCSIZE claims the whole rect.
        let style =
            WS_POPUP | WS_THICKFRAME | WS_CAPTION | WS_SYSMENU | WS_MAXIMIZEBOX | WS_MINIMIZEBOX;
        let (win_w, win_h) = default_window_size();
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            crate::edition::NAME_W,
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            win_w,
            win_h,
            None,
            None,
            Some(instance),
            Some(&mut *ui as *mut Ui as *const core::ffi::c_void),
        )?;

        // We sized against the primary monitor, but CW_USEDEFAULT can land the
        // window on another one (multi-monitor cascade). Re-derive from the
        // monitor it actually reached, so "75 % of the screen" means the screen
        // the window is on — a 4K-primary/1080p-secondary setup would otherwise
        // open a window taller than the display. Still hidden at this point, so
        // the correction costs nothing visible and precedes the first paint.
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi =
            MONITORINFO { cbSize: core::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(monitor, &mut mi).as_bool() {
            let m = mi.rcMonitor;
            let (w, h) = default_size_for_screen(m.right - m.left, m.bottom - m.top);
            if (w, h) != (win_w, win_h) {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    0,
                    0,
                    w,
                    h,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }

        // Spec: square corners — the page is a perfect rectangle.
        let pref = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const core::ffi::c_void,
            core::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );
        // Invisible 1px frame extension: restores the DWM drop shadow.
        let margins =
            MARGINS { cxLeftWidth: 1, cxRightWidth: 1, cyTopHeight: 1, cyBottomHeight: 1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);

        DragAcceptFiles(hwnd, true);
        // Present the first Direct2D frame into the window surface BEFORE the
        // window is shown. The hwnd render target is created lazily in the
        // first WM_PAINT, and its first present is far heavier than a GDI fill
        // (it spins up the D3D device + hwnd render target). Left to the
        // ordinary async paint, the window becomes visible showing its
        // uninitialised (white) redirection surface while that first present
        // is still in flight and the window-open composition can drop it — in
        // empty mode nothing ever invalidates the window again, so a dropped
        // first present leaves it permanently white. Painting here initialises
        // the surface with the real (dark) scene up front, so DWM composites
        // content from the very first frame of the open animation.
        ui.paint_now();
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let cs = lparam.0 as *const CREATESTRUCTW;
            let ui = (*cs).lpCreateParams as *mut Ui;
            (*ui).hwnd = hwnd;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ui as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let ui = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ui;
        if ui.is_null() {
            // e.g. WM_GETMINMAXINFO arrives before WM_NCCREATE
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        (*ui).handle(msg, wparam, lparam)
    }
}

impl Ui {
    fn dpi_scale(&self) -> f32 {
        unsafe { GetDpiForWindow(self.hwnd) as f32 / 96.0 }
    }

    fn client_size(&self) -> SizeU {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        SizeU { w: (rc.right - rc.left).max(0) as u32, h: (rc.bottom - rc.top).max(0) as u32 }
    }

    fn maximized(&self) -> bool {
        let mut wp = WINDOWPLACEMENT {
            length: core::mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        (unsafe { GetWindowPlacement(self.hwnd, &mut wp).is_ok() })
            && wp.showCmd == SW_SHOWMAXIMIZED.0 as u32
    }

    pub fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        unsafe {
            match msg {
                // === chromeless core ===
                WM_NCCALCSIZE => {
                    // Claim the whole window rect as client area (zero chrome).
                    // When maximized, clamp to the work area so client pixels
                    // don't hang off-monitor, and leave 2px for an auto-hide
                    // taskbar to fly out.
                    //
                    // NOTE: must return 0 for BOTH wparam values. When wparam
                    // is FALSE, lparam is a plain RECT* (no maximize handling
                    // needed/possible), but falling through to DefWindowProcW
                    // in that case would let it compute the normal captioned
                    // non-client area, bringing back the title bar and border.
                    if wparam.0 != 0 && self.maximized() {
                        let params = &mut *(lparam.0 as *mut NCCALCSIZE_PARAMS);
                        let monitor = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
                        let mut mi = MONITORINFO {
                            cbSize: core::mem::size_of::<MONITORINFO>() as u32,
                            ..Default::default()
                        };
                        if GetMonitorInfoW(monitor, &mut mi).as_bool() {
                            let mut rect = mi.rcWork;
                            apply_autohide_taskbar_insets(&mut rect, &mi);
                            params.rgrc[0] = rect;
                        }
                    }
                    LRESULT(0)
                }
                WM_NCHITTEST => {
                    let sx = (lparam.0 & 0xFFFF) as i16 as i32; // screen coords
                    let sy = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                    let hit = self.hit_test(sx, sy);
                    if crate::dbglog::enabled() {
                        // Fires on every mouse move: log transitions only.
                        thread_local! {
                            static LAST: std::cell::Cell<u32> =
                                const { std::cell::Cell::new(u32::MAX) };
                        }
                        if LAST.with(|l| l.replace(hit)) != hit {
                            crate::dlog!("ui: nchittest screen=({sx},{sy}) -> {hit}");
                        }
                    }
                    LRESULT(hit as isize)
                }
                WM_NCLBUTTONDOWN => {
                    // A fresh press is never the second click of a
                    // double-click, so this self-corrects a stale latch.
                    self.swallow_dblclk = false;
                    // Unzoomed page background hit-tests as HTCAPTION (that's
                    // what makes the borderless window draggable), so a
                    // click-off-field there never reaches WM_LBUTTONDOWN's
                    // kill-focus path — typing mode would survive and keep
                    // eating the app shortcuts (TOC, rotate...). Leave typing
                    // mode first, then let DefWindowProc run the drag.
                    #[cfg(feature = "pro")]
                    if wparam.0 as u32 == HTCAPTION {
                        crate::dlog!("ui: nclbuttondown caption -> kill form focus");
                        self.form_kill_focus();
                    }
                    DefWindowProcW(self.hwnd, msg, wparam, lparam)
                }
                WM_NCLBUTTONDBLCLK => {
                    // DefWindowProc would toggle maximize on caption dbl-click.
                    // Swallow it: on the caption-draggable page body a
                    // double-click resets zoom, or — already at fit — advances
                    // the F11 fill cycle. HTCAPTION means the pointer is over
                    // plain unzoomed page background: it says nothing about
                    // whether the TOC panel or find bar is open, only that
                    // they aren't under the pointer.
                    if wparam.0 as u32 == HTCAPTION {
                        // The click that opened this gesture may itself have
                        // torn down the surface it landed on (F1 help, a TOC
                        // entry), leaving this second click over bare page.
                        if core::mem::take(&mut self.swallow_dblclk) {
                            return LRESULT(0);
                        }
                        // A modal layer owning the keyboard is dismissed
                        // where dismissible, and nothing else — resizing the
                        // window out from under someone who was typing would
                        // be a surprise. Password mode has no dismissal at
                        // all: the gesture must stay wholly inert there, as
                        // F11 already is.
                        let modal_open = self.modal_active();
                        self.dismiss_goto_entry();
                        #[cfg(feature = "pro")]
                        self.dismiss_bookmark_entry();
                        if modal_open {
                            return LRESULT(0);
                        }
                        // Nearly always false: zoom_by clamps to >= 1.0 and
                        // snaps anything within 1e-4 down to exactly 1.0, so
                        // zoom sits either at 1.0 or above the hit test's
                        // epsilon. The one writer that can land inside the
                        // band is goto's clamp(1.0, max_zoom(..)). Kept so a
                        // future zoom writer breaking that invariant degrades
                        // to "reset" rather than an unexpected resize.
                        if self.app.reset_zoom() {
                            self.sync_renders();
                            self.invalidate();
                        } else {
                            self.toggle_fill();
                        }
                        return LRESULT(0); // never let DefWindowProc maximize
                    }
                    DefWindowProcW(self.hwnd, msg, wparam, lparam)
                }
                WM_GETMINMAXINFO => {
                    let mmi = lparam.0 as *mut MINMAXINFO;
                    let dpi = GetDpiForWindow(self.hwnd) as i32;
                    (*mmi).ptMinTrackSize.x = MIN_SIZE_DIP * dpi / 96;
                    (*mmi).ptMinTrackSize.y = MIN_SIZE_DIP * dpi / 96;
                    LRESULT(0)
                }

                // === right-click menu ===
                WM_NCRBUTTONUP => {
                    let sx = (lparam.0 & 0xFFFF) as i16 as i32;
                    let sy = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                    self.show_menu(sx, sy);
                    LRESULT(0)
                }
                WM_RBUTTONUP => {
                    let mut pt = windows::Win32::Foundation::POINT {
                        x: (lparam.0 & 0xFFFF) as i16 as i32,
                        y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
                    };
                    let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut pt);
                    self.show_menu(pt.x, pt.y);
                    LRESULT(0)
                }
                WM_CONTEXTMENU => LRESULT(0), // we run our own menu above

                // === lifecycle / painting ===
                WM_CREATE => {
                    let hwnd_raw = self.hwnd.0 as isize;
                    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
                    let wake = move || {
                        let _ = PostMessageW(
                            Some(HWND(hwnd_raw as *mut core::ffi::c_void)),
                            WM_APP_PDF_REPLY,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    };
                    self.pdf_tx = Some(crate::pdf::spawn(reply_tx, wake));
                    self.reply_rx = Some(reply_rx);
                    // Do NOT silently swallow renderer init failure: a dead
                    // renderer paints nothing, so the window would just sit
                    // there white with no other symptom. Surface it instead.
                    self.renderer = match Renderer::new() {
                        Ok(r) => Some(r),
                        Err(e) => {
                            let msg: Vec<u16> =
                                format!("pppdf: Direct2D renderer failed to initialise:\n{e}\0")
                                    .encode_utf16()
                                    .collect();
                            MessageBoxW(
                                Some(self.hwnd),
                                PCWSTR(msg.as_ptr()),
                                crate::edition::NAME_W,
                                MB_OK | MB_ICONERROR,
                            );
                            None
                        }
                    };
                    self.app.set_client(self.client_size());
                    self.app.dpi = GetDpiForWindow(self.hwnd);
                    // CRITICAL: forward the window's actual DPI to the renderer NOW.
                    // WM_DPICHANGED never fires on a single monitor, and without this the
                    // overlay/text draw at 96-DPI scale while hit-testing uses the real DPI
                    // (misaligned clicks on every >100% display).
                    if let Some(r) = &mut self.renderer {
                        r.set_dpi(self.app.dpi);
                    }
                    if let Some(path) = self.initial_pdf.take() {
                        self.open_document(path);
                    }
                    LRESULT(0)
                }
                WM_APP_PDF_REPLY => {
                    self.pump_pdf_replies();
                    LRESULT(0)
                }
                WM_MOUSEWHEEL => {
                    #[cfg(feature = "pro")]
                    self.form_kill_focus();
                    // Wheel navigation abandons a half-typed go-to-page entry —
                    // the prompt must not sit open while pages flip under it.
                    self.dismiss_goto_entry();
                    #[cfg(feature = "pro")]
                    self.dismiss_bookmark_entry();
                    // Precision touchpads send many small deltas (±20-60); accumulate to
                    // whole 120-unit notches instead of truncating each event to zero.
                    let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
                    self.wheel_residual += delta;
                    let steps = self.wheel_residual / 120;
                    self.wheel_residual -= steps * 120;
                    let notches = -steps; // wheel down = +1
                    let mut pt = windows::Win32::Foundation::POINT {
                        x: (lparam.0 & 0xFFFF) as i16 as i32,
                        y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
                    };
                    let _ = windows::Win32::Graphics::Gdi::ScreenToClient(self.hwnd, &mut pt);
                    let ctrl = (wparam.0 & 0xFFFF) as u32 & MK_CONTROL.0 != 0;
                    if ctrl {
                        let cursor = PointF { x: pt.x as f32, y: pt.y as f32 };
                        // 1.1 per notch; wheel up (negative notches here) zooms in.
                        let factor = 1.1_f32.powi(-notches);
                        if self.app.zoom_by(cursor, factor) {
                            self.sync_renders(); // crisp render at the new size; soft preview meanwhile
                            self.invalidate();
                        }
                        return LRESULT(0);
                    }
                    let s = self.dpi_scale();
                    let over_panel = self.app.doc().map_or(false, |d| {
                        d.overlay.is_some() && (pt.x as f32) < crate::overlay::PANEL_W * s
                    });
                    if over_panel {
                        let panel_h = self.app.client.h as f32 / s;
                        if let Some(ov) = self.app.doc_mut().and_then(|d| d.overlay.as_mut()) {
                            ov.scroll_by(notches as f32 * 2.0 * crate::overlay::ROW_H, panel_h);
                        }
                        self.invalidate();
                    } else if notches != 0 {
                        match self.app.wheel(notches) {
                            crate::app::WheelOutcome::Flipped => {
                                self.sync_renders();
                                self.show_badge();
                                self.invalidate();
                            }
                            crate::app::WheelOutcome::Redraw => {
                                self.sync_renders();
                                self.invalidate();
                            }
                            crate::app::WheelOutcome::None => {}
                        }
                    }
                    LRESULT(0)
                }
                WM_LBUTTONDOWN => {
                    // A fresh press is never the second click of a
                    // double-click, so this self-corrects a stale latch.
                    self.swallow_dblclk = false;
                    // A click anywhere dismisses the help overlay (it's modal).
                    if self.app.help_visible {
                        self.app.toggle_help();
                        self.invalidate();
                        // This click removed the overlay; if it was click 1 of
                        // a double-click, don't let click 2 cycle the window.
                        self.swallow_dblclk = true;
                        return LRESULT(0);
                    }
                    // Clicks on the find bar are a no-op (keyboard-only UI):
                    // swallow them so they don't fall through to fields,
                    // links, or a pan-drag underneath the bar.
                    {
                        let p = self.mouse_point(lparam);
                        if self.app.search_active()
                            && crate::search::bar_rect(self.app.client, self.dpi_scale())
                                .contains(p)
                        {
                            return LRESULT(0);
                        }
                    }
                    // A click also abandons the go-to-page entry (then acts normally).
                    self.dismiss_goto_entry();
                    #[cfg(feature = "pro")]
                    self.dismiss_bookmark_entry();
                    // Form fields win over links and window dragging.
                    #[cfg(feature = "pro")]
                    {
                        let p = self.mouse_point(lparam);
                        crate::dlog!(
                            "ui: lbuttondown p=({},{}) field_hit={:?} pdf_pt={:?} form_focus={}",
                            p.x,
                            p.y,
                            self.app.field_hit(p),
                            self.app.pdf_point_at(p).map(|q| (q.x, q.y)),
                            self.app.form_focus
                        );
                        if let (Some((idx, kind)), Some(pt)) =
                            (self.app.field_hit(p), self.app.pdf_point_at(p))
                        {
                            if kind == crate::types::FieldKind::Other {
                                // A click on a non-text widget (checkbox,
                                // radio, combo, push button) is almost always
                                // a mutation, and the FormDirty reply
                                // confirming it is ASYNC — a fast Alt+F4 could
                                // close before it pumps. Text-field clicks are
                                // focus-only; the keyboard-side marking covers
                                // their edits. Over-marking is safe;
                                // under-marking loses data.
                                self.app.form_dirty = true;
                            }
                            self.app.form_tab_idx = Some(idx);
                            self.app.form_focus = true;
                            self.form_mouse = true;
                            // Capture like the pan-drag path: without it, a
                            // release outside the window never delivers
                            // WM_LBUTTONUP, stranding form_mouse=true (the
                            // NEXT click's mouse-up would be eaten), and
                            // in-field selection drags die at the window edge.
                            SetCapture(self.hwnd);
                            let m = Self::form_modifiers();
                            // PDFium builds a widget's interactive editor
                            // (caret, per-keystroke invalidation) on
                            // mouse-ENTER or a clean focus grab — but NOT on
                            // a bare click that moves focus from another
                            // widget: that click edited invisibly (typed
                            // text never repainted; the 2026-07-09 "cannot
                            // type" bug). Hover moves are forwarded too, but
                            // a click must be self-sufficient — a touchpad
                            // tap arrives with no prior client-space move.
                            self.send_form(crate::types::FormEvent::MouseMove {
                                x: pt.x,
                                y: pt.y,
                                modifiers: m,
                            });
                            self.send_form(crate::types::FormEvent::MouseDown {
                                x: pt.x,
                                y: pt.y,
                                modifiers: m,
                            });
                            return LRESULT(0);
                        }
                        // Clicking off-field leaves typing mode (commit), then
                        // the click acts normally (drag/pan/link).
                        self.form_kill_focus();
                    }
                    #[cfg(not(feature = "pro"))]
                    {
                        let p = self.mouse_point(lparam);
                        if self.app.field_at(p).is_some() {
                            if !self.hint_shown {
                                self.hint_shown = true;
                                self.show_toast("Form filling is a pppdf Pro feature".into());
                            }
                            return LRESULT(0);
                        }
                    }
                    self.suppress_click = false;
                    let p = self.mouse_point(lparam);
                    let over_panel = self.app.doc().map_or(false, |d| {
                        d.overlay.is_some() && p.x < crate::overlay::PANEL_W * self.dpi_scale()
                    });
                    let zoomed = self.app.doc().map_or(false, |d| d.zoom > 1.0 + 1e-4);
                    let ctrl = (wparam.0 & 0xFFFF) as u32 & MK_CONTROL.0 != 0;
                    if zoomed && ctrl && !over_panel {
                        // Move the borderless window from a zoomed state: hand the drag to
                        // the OS as a caption drag (winit ships this exact pattern).
                        let _ = ReleaseCapture();
                        let _ = PostMessageW(
                            Some(self.hwnd),
                            WM_NCLBUTTONDOWN,
                            WPARAM(HTCAPTION as usize),
                            LPARAM(0),
                        );
                    } else if zoomed && !over_panel {
                        SetCapture(self.hwnd);
                        let scroll = self.app.doc().map(|d| d.scroll).unwrap_or_default();
                        self.drag = Some(Drag { start: p, start_scroll: scroll, moved: false });
                    }
                    LRESULT(0)
                }
                WM_MOUSEMOVE => {
                    #[cfg(feature = "pro")]
                    if self.form_mouse && (wparam.0 as u32 & 0x0001) != 0 {
                        // MK_LBUTTON: drag inside a field = text selection.
                        let p = self.mouse_point(lparam);
                        if let Some(pt) = self.app.pdf_point_at(p) {
                            self.send_form(crate::types::FormEvent::MouseMove {
                                x: pt.x,
                                y: pt.y,
                                modifiers: Self::form_modifiers(),
                            });
                        }
                        return LRESULT(0);
                    }
                    // Buttonless hover over a form document: keep PDFium's
                    // mouse-enter/leave state current (hover highlight, and
                    // the lazily-created widget editor — see WM_LBUTTONDOWN).
                    // Naturally sparse when unzoomed: client-space moves only
                    // arrive where hit_test returns HTCLIENT (fields/links).
                    #[cfg(feature = "pro")]
                    if (wparam.0 as u32 & 0x0001) == 0
                        && self.drag.is_none()
                        && self.app.doc().is_some_and(|d| d.has_forms)
                    {
                        let p = self.mouse_point(lparam);
                        if let Some(pt) = self.app.pdf_point_at(p) {
                            self.send_form(crate::types::FormEvent::MouseMove {
                                x: pt.x,
                                y: pt.y,
                                modifiers: Self::form_modifiers(),
                            });
                        }
                    }
                    let p = self.mouse_point(lparam);
                    if let Some(drag) = &mut self.drag {
                        let (dx, dy) = (p.x - drag.start.x, p.y - drag.start.y);
                        let (tx, ty) =
                            (GetSystemMetrics(SM_CXDRAG) as f32, GetSystemMetrics(SM_CYDRAG) as f32);
                        if dx.abs() > tx || dy.abs() > ty {
                            drag.moved = true;
                        }
                        let moved = drag.moved;
                        let start_scroll = drag.start_scroll;
                        if moved {
                            let want =
                                PointF { x: start_scroll.x - dx, y: start_scroll.y - dy };
                            if let (Some(px), Some(_)) = (self.app.current_px(), self.app.doc()) {
                                let client = self.app.client;
                                if let Some(d) = self.app.doc_mut() {
                                    d.scroll = crate::layout::clamp_scroll(px, client, want);
                                }
                            }
                            self.invalidate();
                        }
                    }
                    LRESULT(0)
                }
                WM_CAPTURECHANGED => {
                    self.drag = None;
                    // Capture lost mid-field-drag (Alt-Tab, another window
                    // grabbing capture): drop the form-mouse latch so the next
                    // click's WM_LBUTTONUP isn't consumed by the form branch.
                    #[cfg(feature = "pro")]
                    {
                        self.form_mouse = false;
                    }
                    LRESULT(0)
                }
                WM_LBUTTONDBLCLK => {
                    crate::dlog!("ui: lbuttondblclk");
                    // Over a field, a dblclk is form INPUT, not a zoom
                    // gesture: the OS turns every second rapid click into
                    // DBLCLK, and swallowing it made half the clicks on a
                    // checkbox vanish. Text fields get the real double-click
                    // (word selection); button-likes get a plain MouseDown
                    // (their editor ignores DblClk — a click must toggle).
                    #[cfg(feature = "pro")]
                    {
                        let p = self.mouse_point(lparam);
                        if let (Some((idx, kind)), Some(pt)) =
                            (self.app.field_hit(p), self.app.pdf_point_at(p))
                        {
                            if kind == crate::types::FieldKind::Other {
                                // Same optimistic dirty-marking as a click.
                                self.app.form_dirty = true;
                            }
                            self.app.form_tab_idx = Some(idx);
                            self.app.form_focus = true;
                            // The paired WM_LBUTTONUP must forward MouseUp.
                            self.form_mouse = true;
                            SetCapture(self.hwnd);
                            let m = Self::form_modifiers();
                            let ev = if kind == crate::types::FieldKind::Text {
                                crate::types::FormEvent::MouseDblClk {
                                    x: pt.x,
                                    y: pt.y,
                                    modifiers: m,
                                }
                            } else {
                                crate::types::FormEvent::MouseDown {
                                    x: pt.x,
                                    y: pt.y,
                                    modifiers: m,
                                }
                            };
                            self.send_form(ev);
                            return LRESULT(0);
                        }
                        self.form_kill_focus();
                    }
                    // Double-clicks on the find bar are swallowed too —
                    // without this, dblclk falls through to reset_zoom and
                    // throws away the zoom (WM_LBUTTONDOWN's swallow has no
                    // effect on the separate DBLCLK message).
                    {
                        let p = self.mouse_point(lparam);
                        if self.app.search_active()
                            && crate::search::bar_rect(self.app.client, self.dpi_scale())
                                .contains(p)
                        {
                            return LRESULT(0);
                        }
                    }
                    self.dismiss_goto_entry();
                    #[cfg(feature = "pro")]
                    self.dismiss_bookmark_entry();
                    // Client-space double click (only reachable when zoomed / over links).
                    if self.app.reset_zoom() {
                        self.sync_renders();
                        self.invalidate();
                    }
                    // A dblclk is followed by a second WM_LBUTTONUP with no
                    // matching BUTTONDOWN; without this flag that UP would fire
                    // on_click() again against the just-reset page geometry
                    // (spurious link navigation).
                    self.suppress_click = true;
                    LRESULT(0)
                }
                WM_LBUTTONUP => {
                    #[cfg(feature = "pro")]
                    crate::dlog!(
                        "ui: lbuttonup form_mouse={} suppress={}",
                        self.form_mouse,
                        self.suppress_click
                    );
                    #[cfg(feature = "pro")]
                    if std::mem::take(&mut self.form_mouse) {
                        let p = self.mouse_point(lparam);
                        if let Some(pt) = self.app.pdf_point_at(p) {
                            self.send_form(crate::types::FormEvent::MouseUp {
                                x: pt.x,
                                y: pt.y,
                                modifiers: Self::form_modifiers(),
                            });
                        }
                        // form_mouse was already taken, so the synchronous
                        // WM_CAPTURECHANGED this delivers is a no-op here.
                        let _ = ReleaseCapture();
                        return LRESULT(0);
                    }
                    let suppress = std::mem::take(&mut self.suppress_click);
                    // ORDER MATTERS: take the drag state BEFORE ReleaseCapture().
                    // ReleaseCapture delivers WM_CAPTURECHANGED synchronously (reentering
                    // this WndProc), and that arm clears self.drag — taking it afterwards
                    // would always see None and every pan release would count as a click.
                    let was_pan = self.drag.take().map_or(false, |d| d.moved);
                    let _ = ReleaseCapture();
                    if !was_pan && !suppress {
                        let p = self.mouse_point(lparam);
                        self.on_click(p);
                    }
                    LRESULT(0)
                }
                WM_SETCURSOR => {
                    if (lparam.0 & 0xFFFF) as u32 == HTCLIENT {
                        let mut pt = windows::Win32::Foundation::POINT::default();
                        let _ = GetCursorPos(&mut pt);
                        let _ = windows::Win32::Graphics::Gdi::ScreenToClient(self.hwnd, &mut pt);
                        let p = PointF { x: pt.x as f32, y: pt.y as f32 };
                        let over_overlay = self.app.doc().map_or(false, |d| {
                            d.overlay.is_some() && p.x < crate::overlay::PANEL_W * self.dpi_scale()
                        });
                        if !over_overlay && self.app.link_at(p).is_some() {
                            if let Ok(hand) = LoadCursorW(None, IDC_HAND) {
                                let _ = SetCursor(Some(hand));
                            }
                            return LRESULT(1);
                        }
                        #[cfg(feature = "pro")]
                        if !over_overlay {
                            if let Some(kind) = self.app.field_at(p) {
                                let cursor = match kind {
                                    crate::types::FieldKind::Text => IDC_IBEAM,
                                    crate::types::FieldKind::Other => IDC_HAND,
                                };
                                if let Ok(c) = LoadCursorW(None, cursor) {
                                    let _ = SetCursor(Some(c));
                                }
                                return LRESULT(1);
                            }
                        }
                    }
                    DefWindowProcW(self.hwnd, msg, wparam, lparam)
                }
                WM_TIMER if wparam.0 == TIMER_RECREATE => {
                    // Retry the paint after a device loss: ensure_rt recreates the
                    // target. If the device recovered, the next frame draws normally;
                    // if it's still lost, WM_PAINT re-arms this timer (self-healing).
                    let _ = KillTimer(Some(self.hwnd), TIMER_RECREATE);
                    self.invalidate();
                    LRESULT(0)
                }
                WM_TIMER if wparam.0 == TIMER_RESIZE => {
                    let _ = KillTimer(Some(self.hwnd), TIMER_RESIZE);
                    self.sync_renders();
                    self.invalidate();
                    LRESULT(0)
                }
                WM_TIMER if wparam.0 == TIMER_BADGE => {
                    let done = match &mut self.badge {
                        Some((_, ticks)) => {
                            *ticks += 1;
                            *ticks > BADGE_HOLD_TICKS + BADGE_FADE_TICKS
                        }
                        None => true,
                    };
                    if done {
                        self.badge = None;
                        let _ = KillTimer(Some(self.hwnd), TIMER_BADGE);
                    }
                    self.invalidate();
                    LRESULT(0)
                }
                WM_EXITSIZEMOVE => {
                    let _ = KillTimer(Some(self.hwnd), TIMER_RESIZE);
                    self.sync_renders();
                    self.invalidate();
                    LRESULT(0)
                }
                WM_SIZE => {
                    let size = self.client_size();
                    self.app.set_client(size);
                    // Re-clamp a zoomed pan against the new client size, or the page draws
                    // off-center with a blank band until the next wheel/drag.
                    if let Some(px) = self.app.current_px() {
                        let client = self.app.client;
                        if let Some(d) = self.app.doc_mut() {
                            d.scroll = crate::layout::clamp_scroll(px, client, d.scroll);
                        }
                    }
                    if let Some(r) = &mut self.renderer {
                        r.resize(size.w, size.h);
                    }
                    SetTimer(Some(self.hwnd), TIMER_RESIZE, 150, None);
                    self.invalidate();
                    LRESULT(0)
                }
                WM_DPICHANGED => {
                    self.app.dpi = (wparam.0 & 0xFFFF) as u32;
                    if let Some(r) = &mut self.renderer {
                        r.set_dpi((wparam.0 & 0xFFFF) as u32);
                    }
                    let suggested = &*(lparam.0 as *const RECT);
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        suggested.left,
                        suggested.top,
                        suggested.right - suggested.left,
                        suggested.bottom - suggested.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                    LRESULT(0)
                }
                WM_ERASEBKGND => LRESULT(1),
                WM_PAINT => {
                    let mut ps = PAINTSTRUCT::default();
                    let _ = BeginPaint(self.hwnd, &mut ps);
                    let mut lost = false;
                    if let Some(mut r) = self.renderer.take() {
                        lost = r.draw(self.hwnd, &self.app, self.badge_display()).unwrap_or(false);
                        self.renderer = Some(r);
                    }
                    let _ = EndPaint(self.hwnd, &ps);
                    // Direct2D reported device loss and dropped its target. Nothing
                    // else may invalidate the window (Empty/Password mode, or a Doc
                    // between render replies), so schedule a repaint ourselves to
                    // recreate the target — otherwise one transient loss under GPU
                    // contention would leave the window permanently white. The timer
                    // throttles the retry so a sustained loss can't busy-loop.
                    if lost {
                        SetTimer(Some(self.hwnd), TIMER_RECREATE, 100, None);
                    }
                    LRESULT(0)
                }
                WM_KEYDOWN => {
                    self.on_key(wparam.0 as u16);
                    LRESULT(0)
                }
                WM_SYSKEYDOWN => {
                    // Alt+Left / Alt+Right = history back / forward. Bit 29
                    // of lparam is the Alt (context) flag: set for real
                    // Alt+key chords, clear for the F10 family. ONLY the two
                    // arrows are claimed — everything else falls through so
                    // Alt+F4 (close) and Alt+Space (system menu) keep their
                    // DefWindowProc meaning.
                    let alt = (lparam.0 >> 29) & 1 == 1;
                    let vk = wparam.0 as u16;
                    if alt && vk == VK_LEFT.0 {
                        self.nav_back();
                        LRESULT(0)
                    } else if alt && vk == VK_RIGHT.0 {
                        self.nav_forward();
                        LRESULT(0)
                    } else if alt && (vk == 0x43 || vk == 0x57) && self.app.search_active() {
                        // Alt+C / Alt+W: match-case / whole-word toggles.
                        let eff = self.app.doc_mut().and_then(|d| d.search.as_mut()).map(|st| {
                            if vk == 0x43 {
                                st.toggle_case()
                            } else {
                                st.toggle_word()
                            }
                        });
                        let on = self.app.doc().and_then(|d| d.search.as_ref()).map_or(
                            false,
                            |st| {
                                if vk == 0x43 {
                                    st.options.match_case
                                } else {
                                    st.options.whole_word
                                }
                            },
                        );
                        self.show_toast(
                            match (vk, on) {
                                (0x43, true) => "Match case: on",
                                (0x43, false) => "Match case: off",
                                (_, true) => "Whole words: on",
                                (_, false) => "Whole words: off",
                            }
                            .into(),
                        );
                        if let Some(eff) = eff {
                            self.apply_search_effect(eff);
                        }
                        LRESULT(0)
                    } else {
                        DefWindowProcW(self.hwnd, msg, wparam, lparam)
                    }
                }
                WM_APPCOMMAND => {
                    // Mouse back/forward buttons. Handled here (not
                    // WM_XBUTTONDOWN) because the unzoomed page body
                    // hit-tests as HTCAPTION, so client X-clicks arrive as
                    // non-client messages — DefWindowProc converts BOTH
                    // client and non-client X-button releases into this one
                    // message. Return TRUE (1) when handled, per the docs.
                    let cmd = ((lparam.0 as u32) >> 16) & !0xF000; // GET_APPCOMMAND_LPARAM
                    match cmd {
                        APPCOMMAND_BROWSER_BACKWARD => {
                            self.nav_back();
                            LRESULT(1)
                        }
                        APPCOMMAND_BROWSER_FORWARD => {
                            self.nav_forward();
                            LRESULT(1)
                        }
                        _ => DefWindowProcW(self.hwnd, msg, wparam, lparam),
                    }
                }
                WM_CHAR => {
                    #[cfg(feature = "pro")]
                    crate::dlog!(
                        "ui: wm_char c={:#x} form_focus={}",
                        wparam.0 as u32,
                        self.app.form_focus
                    );
                    #[cfg(feature = "pro")]
                    if self.app.form_focus
                        && !matches!(self.app.mode, crate::app::Mode::Password { .. })
                    {
                        let c = wparam.0 as u32;
                        // Tab/Esc handled in WM_KEYDOWN; Ctrl chords (incl.
                        // Ctrl+S=0x13, Ctrl+V=0x16) must not type control
                        // bytes — \r (Enter) and \x08 (Backspace) pass.
                        // (already inside handle()'s unsafe block)
                        let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                        if c != 0x09 && c != 0x1B && !ctrl {
                            // PDFium only fires FFI_OnChange for a TEXT field
                            // when its value COMMITS (focus leaves the field)
                            // — not per keystroke. Mark dirty here so a typed
                            // character isn't silently lost if the window
                            // closes before the field ever loses focus.
                            self.app.form_dirty = true;
                            self.send_form(crate::types::FormEvent::Char {
                                c: c as u16,
                                modifiers: Self::form_modifiers(),
                            });
                        }
                        return LRESULT(0);
                    }
                    if matches!(self.app.mode, crate::app::Mode::Password { .. }) {
                        let c = char::from_u32(wparam.0 as u32).unwrap_or('\0');
                        match c {
                            '\r' => {
                                if let Some((path, pw)) = self.app.password_take() {
                                    self.send_pdf(PdfCmd::Load { path, password: Some(pw) });
                                }
                            }
                            '\u{8}' => self.app.password_backspace(),
                            c if !c.is_control() => self.app.password_char(c),
                            _ => {}
                        }
                        self.invalidate();
                    }
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
                    // Find bar: the first shared-code WM_CHAR consumer. One
                    // filter (`!ctrl && c >= 0x20`) drops Enter/Tab/Esc/
                    // Backspace chars and the control BYTES Ctrl chords
                    // produce (Ctrl+F=0x06 etc.); char::from_u32 rejects
                    // unpaired surrogate halves, so input is BMP-only —
                    // the password path's existing limitation.
                    if self.app.search_active() {
                        let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                        let c32 = wparam.0 as u32;
                        if !ctrl && c32 >= 0x20 {
                            if let Some(c) = char::from_u32(c32) {
                                let eff = self
                                    .app
                                    .doc_mut()
                                    .and_then(|d| d.search.as_mut())
                                    .map(|st| st.push_char(c));
                                if let Some(eff) = eff {
                                    self.apply_search_effect(eff);
                                }
                            }
                        }
                        return LRESULT(0);
                    }
                    LRESULT(0)
                }
                WM_DROPFILES => {
                    let hdrop = HDROP(wparam.0 as *mut core::ffi::c_void);
                    let mut buf = [0u16; 1024];
                    let n = DragQueryFileW(hdrop, 0, Some(&mut buf));
                    DragFinish(hdrop);
                    if n > 0 {
                        self.open_document(PathBuf::from(String::from_utf16_lossy(
                            &buf[..n as usize],
                        )));
                    }
                    LRESULT(0)
                }
                WM_CLOSE => {
                    // Alt+F4 / external close (taskbar, Task Manager "Close
                    // window") goes through the same save-prompt gate as Esc
                    // and Ctrl+W.
                    self.quit();
                    LRESULT(0)
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    LRESULT(0)
                }
                _ => DefWindowProcW(self.hwnd, msg, wparam, lparam),
            }
        }
    }

    /// Screen point -> HT* code. Grips on the outer band, HTCLIENT over
    /// interactive content (extended by Tasks 8/9), HTCAPTION elsewhere.
    fn hit_test(&self, sx: i32, sy: i32) -> u32 {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rc);
        }
        let size =
            SizeU { w: (rc.right - rc.left).max(0) as u32, h: (rc.bottom - rc.top).max(0) as u32 };
        let p = PointF { x: (sx - rc.left) as f32, y: (sy - rc.top) as f32 };

        if !self.maximized() {
            let grip = GRIP_DIP * self.dpi_scale();
            match hit_zone(p, size, grip) {
                Zone::TopLeft => return HTTOPLEFT,
                Zone::Top => return HTTOP,
                Zone::TopRight => return HTTOPRIGHT,
                Zone::Left => return HTLEFT,
                Zone::Right => return HTRIGHT,
                Zone::BottomLeft => return HTBOTTOMLEFT,
                Zone::Bottom => return HTBOTTOM,
                Zone::BottomRight => return HTBOTTOMRIGHT,
                Zone::Interior => {}
            }
        }
        if self.wants_client_mouse(p) {
            return HTCLIENT;
        }
        HTCAPTION
    }

    /// Should this client-space point get normal mouse handling instead of
    /// dragging the window? Extended in Task 8 (links, overlay) and Task 9
    /// (zoomed state).
    fn wants_client_mouse(&self, p: PointF) -> bool {
        // While help is up, clicks must reach the client so they can dismiss it
        // (otherwise a click on the "caption" area would drag the window).
        if self.app.help_visible {
            return true;
        }
        if let Some(d) = self.app.doc() {
            if d.overlay.is_some() && p.x < crate::overlay::PANEL_W * self.dpi_scale() {
                return true;
            }
            if d.zoom > 1.0 + 1e-4 {
                return true; // Task 9: dragging pans instead of moving the window
            }
            if self.app.link_at(p).is_some() {
                return true;
            }
            if self.app.field_at(p).is_some() {
                return true; // form fields: pro types into them, free shows the hint
            }
            if d.search.is_some()
                && crate::search::bar_rect(self.app.client, self.dpi_scale()).contains(p)
            {
                return true; // clicks on the find bar must not drag the window
            }
        }
        false
    }

    fn on_key(&mut self, vk: u16) {
        if matches!(self.app.mode, crate::app::Mode::Password { .. }) {
            if vk == VK_ESCAPE.0 {
                // Restores the stashed doc if one was open, else Empty.
                self.app.password_cancel();
                // The Load that displaced the doc killed its scan without a
                // SearchDone — re-issue so `scanning` can't spin forever.
                self.resync_open_search();
                self.sync_renders();
                self.invalidate();
            }
            return; // printable input + Enter/Backspace arrive via WM_CHAR
        }
        // F1 help is modal: while it's up, swallow every key; F1/Esc dismiss.
        if self.app.help_visible {
            if vk == VK_F1.0 || vk == VK_ESCAPE.0 {
                self.app.toggle_help();
                self.invalidate();
            }
            return;
        }
        // Go-to-page entry is modal like the password prompt: digits build the
        // number, Enter jumps, Esc cancels, everything else is swallowed so a
        // stray Space can't flip the page mid-entry.
        if self.app.goto_entry_active() {
            match vk {
                _ if vk == VK_RETURN.0 => {
                    if self.app.goto_entry_commit() {
                        self.sync_renders();
                        self.show_badge();
                    }
                }
                _ if vk == VK_ESCAPE.0 => self.app.goto_entry_cancel(),
                _ if vk == VK_BACK.0 => self.app.goto_entry_backspace(),
                _ if (0x30..=0x39).contains(&vk) => {
                    self.app.goto_entry_char((b'0' + (vk - 0x30) as u8) as char);
                }
                _ if (VK_NUMPAD0.0..=VK_NUMPAD0.0 + 9).contains(&vk) => {
                    self.app.goto_entry_char((b'0' + (vk - VK_NUMPAD0.0) as u8) as char);
                }
                _ if vk == 0x46 && unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 => {
                    // Ctrl+F cancels the half-typed goto prompt and opens
                    // the find bar (they must not stack).
                    self.app.goto_entry_cancel();
                    self.search_open_ui();
                }
                _ => {}
            }
            self.invalidate();
            return;
        }
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
        // Typing mode: the focused field owns the keyboard. Only Esc
        // (commit + leave) and F1 stay app shortcuts; Space/G/R/0 type text.
        #[cfg(feature = "pro")]
        if self.app.form_focus {
            let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            match vk {
                _ if vk == VK_ESCAPE.0 => self.form_kill_focus(),
                _ if vk == VK_F1.0 => {
                    self.app.toggle_help();
                    self.invalidate();
                }
                _ if vk == VK_TAB.0 => self.form_tab(shift),
                _ if vk == VK_LEFT.0
                    || vk == VK_RIGHT.0
                    || vk == VK_UP.0
                    || vk == VK_DOWN.0
                    || vk == VK_HOME.0
                    || vk == VK_END.0 =>
                {
                    self.send_form(crate::types::FormEvent::KeyDown {
                        vk,
                        modifiers: Self::form_modifiers(),
                    });
                }
                _ if vk == VK_DELETE.0 => {
                    // PDFium only fires FFI_OnChange for a TEXT field when its
                    // value COMMITS (focus leaves the field) — not per
                    // keystroke. Delete can mutate the field's content right
                    // now, so mark dirty optimistically rather than waiting
                    // for a commit-time signal that may never arrive before
                    // the window closes (would silently lose the edit).
                    self.app.form_dirty = true;
                    self.send_form(crate::types::FormEvent::KeyDown {
                        vk,
                        modifiers: Self::form_modifiers(),
                    });
                }
                0x56 if ctrl => self.form_paste(), // Ctrl+V (copy/cut deferred)
                0x53 if ctrl => self.save_doc(), // Ctrl+S
                0x42 if ctrl => self.bookmark_toggle_ui(), // Ctrl+B (commits the field first)
                0x46 if ctrl => {
                    // Ctrl+F leaves the field (commit) and opens the find bar.
                    self.form_kill_focus();
                    self.search_open_ui();
                }
                _ => {} // printable keys arrive via WM_CHAR
            }
            return;
        }
        // Find bar (persistent, semi-modal): position matters — a clicked
        // form field wins the keyboard (the rung above), and the bar
        // shadows the global plain-key shortcuts below (G/R/F/0/Space type
        // into the query via WM_CHAR).
        if self.search_key(vk) {
            return;
        }
        if vk == VK_F1.0 {
            self.app.toggle_help();
            self.invalidate();
            return;
        }
        if self.overlay_key(vk) {
            return;
        }
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        match vk {
            _ if vk == VK_ESCAPE.0 => self.quit(),
            0x57 if ctrl => self.quit(), // Ctrl+W
            #[cfg(feature = "pro")]
            0x53 if ctrl => self.save_doc(), // Ctrl+S
            0x4E if ctrl => self.spawn_new_window(None), // Ctrl+N
            0x4F if ctrl && shift => {
                // Ctrl+Shift+O — must precede the plain Ctrl+O arm
                if let Some(path) = self.pick_pdf() {
                    self.spawn_new_window(Some(path));
                }
            }
            0x4F if ctrl => self.open_dialog(), // Ctrl+O
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
            _ if vk == VK_F11.0 => self.toggle_fill(),
            _ if vk == VK_TAB.0 => {
                self.app.toggle_overlay();
                self.invalidate();
            }
            _ if vk == VK_NEXT.0 || vk == VK_RIGHT.0 => self.flip(1),
            _ if vk == VK_PRIOR.0 || vk == VK_LEFT.0 => self.flip(-1),
            _ if vk == VK_SPACE.0 => self.flip(if shift { -1 } else { 1 }),
            _ if vk == VK_HOME.0 => {
                let changed = self.app.goto(0);
                if changed {
                    self.sync_renders();
                    self.show_badge();
                    self.invalidate();
                }
            }
            _ if vk == VK_END.0 => {
                let last = self.app.doc().map(|d| d.pages.saturating_sub(1)).unwrap_or(0);
                if self.app.goto(last) {
                    self.sync_renders();
                    self.show_badge();
                    self.invalidate();
                }
            }
            _ if vk == VK_BACK.0 && !ctrl => {
                // Backspace = back, Shift+Backspace = forward (browser
                // reflexes; Ctrl+Backspace stays free). The modal layers
                // above already own Backspace where it means "delete"
                // (go-to-page entry, password prompt, Pro form fields).
                if shift {
                    self.nav_forward();
                } else {
                    self.nav_back();
                }
            }
            _ if vk == VK_F3.0 => {
                // F3 with the bar closed: reopen the last query and jump to
                // its first hit when it arrives. With no remembered query
                // this just opens an empty bar (no scan, no pending jump).
                if self.app.doc().is_some() {
                    self.search_open_ui();
                    if let Some(st) = self.app.doc_mut().and_then(|d| d.search.as_mut()) {
                        if st.legal() {
                            // Set AFTER search_open_ui's sync_search reset.
                            st.pending_jump = true;
                        }
                    }
                    self.invalidate();
                }
            }
            _ if vk == 0x30 || vk == VK_NUMPAD0.0 => {
                // '0': reset to fit
                if self.app.reset_zoom() {
                    self.sync_renders();
                    self.invalidate();
                }
            }
            // Plain letters only: Ctrl+R carries "reload" muscle memory from
            // browsers — silently rotating (and resetting zoom) would surprise.
            0x52 if !ctrl => {
                // 'R': rotate the view 90° clockwise
                if self.app.rotate_cw() {
                    self.sync_renders();
                    self.invalidate();
                }
            }
            0x47 if !ctrl => {
                // 'G': go to page — not while the TOC overlay is up (its own
                // keys own the keyboard there)
                if self.app.doc().is_some_and(|d| d.overlay.is_none())
                    && self.app.goto_entry_start()
                {
                    self.invalidate();
                }
            }
            0x46 if ctrl => self.search_open_ui(), // Ctrl+F: find in the book
            // 'F': cycle how form widgets are marked (highlight / dotted
            // frames / off). Only meaningful on a form-bearing document —
            // on anything else the key stays inert (no confusing toast).
            // In typing mode 'f' never reaches here (the focused field owns
            // printable keys via WM_CHAR).
            #[cfg(feature = "pro")]
            0x46 if !ctrl => {
                if self.app.doc().is_some_and(|d| d.has_forms) {
                    let mode = self.app.cycle_field_indication();
                    self.show_toast(
                        match mode {
                            crate::app::FieldIndication::Highlight => "Fields: highlight",
                            crate::app::FieldIndication::Frame => "Fields: frames",
                            crate::app::FieldIndication::Off => "Fields: unmarked",
                        }
                        .into(),
                    );
                    self.invalidate();
                }
            }
            _ => {}
        }
    }

    /// Handle a key inside the open overlay. Returns true if consumed.
    fn overlay_key(&mut self, vk: u16) -> bool {
        let s = self.dpi_scale();
        let panel_h = self.app.client.h as f32 / s;
        let Some(d) = self.app.doc_mut() else { return false };
        let Some(ov) = d.overlay.as_mut() else { return false };
        match vk {
            _ if vk == VK_DOWN.0 => ov.move_selection(1, panel_h),
            _ if vk == VK_UP.0 => ov.move_selection(-1, panel_h),
            _ if vk == VK_RIGHT.0 => ov.expand_selected(),
            _ if vk == VK_LEFT.0 => ov.collapse_selected(),
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
            _ if vk == VK_ESCAPE.0 => d.overlay = None,
            _ if vk == VK_TAB.0 => d.overlay = None,
            _ => return false, // PgUp/PgDn etc. still page the document
        }
        self.invalidate();
        true
    }

    fn quit(&mut self) {
        // A save is in flight: destroying the window now would exit the
        // process, killing the detached PDF thread possibly mid-write. Defer
        // the close until the save resolves — Saved(Ok)+pending_close
        // destroys the window (or re-runs this gate if new edits landed
        // meanwhile), Saved(Err)+pending_close clears it and stays open with
        // the error box. Exactly the semantics wanted here.
        #[cfg(feature = "pro")]
        if self.saves_inflight > 0 {
            self.pending_close = true;
            self.pending_open = None; // last intent: the close supersedes any deferred open
            return;
        }
        #[cfg(feature = "pro")]
        if self.app.form_dirty && self.app.doc().is_some() {
            let choice = unsafe {
                MessageBoxW(
                    Some(self.hwnd),
                    w!("Save changes to this PDF before closing?"),
                    crate::edition::NAME_W,
                    MB_YESNOCANCEL | MB_ICONWARNING,
                )
            };
            match choice {
                IDYES => {
                    self.pending_close = true;
                    self.pending_open = None; // close supersedes any deferred open
                    self.save_doc();
                }
                IDNO => unsafe {
                    let _ = DestroyWindow(self.hwnd);
                },
                _ => {} // IDCANCEL: stay open, keep edits
            }
            return;
        }
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    /// F11 cycles the window through three stages: height-fit (work-area
    /// height at the current page's rotated aspect, flush at the nearest
    /// screen edge), fill (work area edge to edge, taskbar visible), then
    /// back to the size/position saved when the cycle started. Without a
    /// document there is no aspect: height-fit is skipped and F11 stays the
    /// old two-state fill/restore. Independent of native maximize (Win+Up).
    fn toggle_fill(&mut self) {
        unsafe {
            // Win+Up leaves WS_MAXIMIZE set, and SetWindowPos does not clear
            // it: the window would take the cycle's geometry while hit_test
            // still suppresses all 8 grips and WM_NCCALCSIZE still clamps to
            // the work area — visibly normal, silently unresizable. Drop
            // maximize first so the cycle owns plain geometry. Ordering
            // matters: this runs before GetWindowRect so saved_rect captures
            // the restored rect, not the maximized one.
            if self.maximized() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
            if self.fill_stage == FillStage::Filled {
                self.fill_stage = FillStage::Normal;
                if let Some(saved) = self.saved_rect.take() {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        saved.left,
                        saved.top,
                        saved.right - saved.left,
                        saved.bottom - saved.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                return;
            }
            let mut rc = RECT::default();
            let _ = GetWindowRect(self.hwnd, &mut rc);
            let monitor = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: core::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut mi).as_bool() {
                return; // no monitor info: stay in the current stage
            }
            let w = mi.rcWork;
            if self.fill_stage == FillStage::Normal {
                self.saved_rect = Some(rc);
                if let Some(aspect) = self.app.page_aspect() {
                    let (l, t, r, b) = crate::layout::height_fit_rect(
                        (w.left, w.top, w.right, w.bottom),
                        aspect,
                        rc.left + (rc.right - rc.left) / 2,
                    );
                    self.fill_stage = FillStage::HeightFit;
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        l,
                        t,
                        r - l,
                        b - t,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                    return;
                }
                // No document: skip height-fit, fall through to fill.
            }
            self.fill_stage = FillStage::Filled;
            let _ = SetWindowPos(
                self.hwnd,
                None,
                w.left,
                w.top,
                w.right - w.left,
                w.bottom - w.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    /// Pin the window above (or release it from) all non-topmost windows.
    /// SWP_NOMOVE|SWP_NOSIZE keep geometry; the F11-fill and DPI-change paths
    /// both pass SWP_NOZORDER, so this pin survives them.
    fn toggle_always_on_top(&mut self) {
        self.always_on_top = !self.always_on_top;
        let after = if self.always_on_top { HWND_TOPMOST } else { HWND_NOTOPMOST };
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(after),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    /// Close the go-to-page prompt if it is open. Mouse interaction and the
    /// context menu abandon the entry (they could otherwise stack a second
    /// modal — e.g. the TOC via the menu — on top of it, leaving the keyboard
    /// owned by an invisible prompt).
    fn dismiss_goto_entry(&mut self) {
        if self.app.goto_entry_active() {
            self.app.goto_entry_cancel();
            self.invalidate();
        }
    }

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

    fn show_menu(&mut self, sx: i32, sy: i32) {
        self.dismiss_goto_entry();
        #[cfg(feature = "pro")]
        self.dismiss_bookmark_entry();
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, w!("Open…\tCtrl+O"));
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                CMD_OPEN_NEW_WINDOW,
                w!("Open in New Window…\tCtrl+Shift+O"),
            );
            let _ = AppendMenuW(menu, MF_STRING, CMD_NEW_WINDOW, w!("New Window\tCtrl+N"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_TOC, w!("Table of Contents\tTab"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_FIT, w!("Fit to Window\t0"));
            let aot = MF_STRING | if self.always_on_top { MF_CHECKED } else { MF_UNCHECKED };
            let _ = AppendMenuW(menu, aot, CMD_ALWAYS_ON_TOP, w!("Always on Top"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_HELP, w!("Help\tF1"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_ABOUT, w!("About"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, CMD_MINIMIZE, w!("Minimize"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_CLOSE, w!("Close\tEsc"));
            let _ = SetForegroundWindow(self.hwnd);
            let cmd =
                TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, sx, sy, None, self.hwnd, None);
            let _ = DestroyMenu(menu);
            self.on_command(cmd.0 as usize);
        }
    }

    fn on_command(&mut self, cmd: usize) {
        match cmd {
            CMD_MINIMIZE => unsafe {
                let _ = ShowWindow(self.hwnd, SW_MINIMIZE);
            },
            CMD_CLOSE => self.quit(),
            CMD_TOC => {
                self.app.toggle_overlay();
                self.invalidate();
            }
            CMD_FIT => {
                if self.app.reset_zoom() {
                    self.sync_renders();
                    self.invalidate();
                }
            }
            CMD_ALWAYS_ON_TOP => self.toggle_always_on_top(),
            CMD_OPEN => self.open_dialog(),
            CMD_HELP => {
                self.app.help_visible = true;
                self.invalidate();
            }
            CMD_ABOUT => self.about_box(),
            CMD_NEW_WINDOW => self.spawn_new_window(None),
            CMD_OPEN_NEW_WINDOW => {
                if let Some(path) = self.pick_pdf() {
                    self.spawn_new_window(Some(path));
                }
            }
            _ => {}
        }
    }

    fn follow_link(&mut self, target: LinkTarget) {
        match target {
            LinkTarget::Page(p) => {
                if self.app.goto(p) {
                    self.sync_renders();
                    self.show_badge();
                    self.invalidate();
                }
            }
            LinkTarget::Uri(uri) => {
                // Spec: default browser for web links; other schemes ignored.
                if uri.starts_with("http://") || uri.starts_with("https://") {
                    unsafe {
                        let _ = ShellExecuteW(
                            None,
                            w!("open"),
                            &HSTRING::from(uri.as_str()),
                            None,
                            None,
                            SW_SHOWNORMAL,
                        );
                    }
                }
            }
        }
    }

    fn mouse_point(&self, lparam: LPARAM) -> PointF {
        PointF {
            x: (lparam.0 & 0xFFFF) as i16 as f32,
            y: ((lparam.0 >> 16) & 0xFFFF) as i16 as f32,
        }
    }

    /// Dispatch a left click: the overlay panel first (it covers the page),
    /// then page links.
    fn on_click(&mut self, p: PointF) {
        let s = self.dpi_scale();
        // 1) Overlay panel
        let overlay_hit = self.app.doc().and_then(|d| d.overlay.as_ref()).and_then(|ov| {
            if p.x < crate::overlay::PANEL_W * s {
                let local = PointF { x: p.x / s, y: p.y / s };
                let panel_h = self.app.client.h as f32 / s;
                Some(match ov.hit_test(local, panel_h) {
                    crate::overlay::OverlayHit::Chevron(i) => (true, i),
                    crate::overlay::OverlayHit::Title(i) => (false, i),
                    crate::overlay::OverlayHit::None => return None,
                })
            } else {
                None
            }
        });
        if let Some((is_chevron, idx)) = overlay_hit {
            let d = self.app.doc_mut().unwrap();
            let ov = d.overlay.as_mut().unwrap();
            if is_chevron {
                ov.toggle(idx);
            } else {
                ov.select(idx);
                match ov.activate() {
                    Some(page) => {
                        d.overlay = None; // jump closes the panel
                        // The panel is gone, so a second click of a
                        // double-click would land on bare page background and
                        // cycle the window. Swallow that one.
                        self.swallow_dblclk = true;
                        if self.app.goto(page) {
                            self.sync_renders();
                            self.show_badge();
                        }
                    }
                    None => ov.toggle(idx), // header row: click toggles, panel stays
                }
            }
            self.invalidate();
            return;
        }
        // Don't treat page clicks as links while the overlay covers that spot.
        let overlay_open_here = self
            .app
            .doc()
            .map_or(false, |d| d.overlay.is_some() && p.x < crate::overlay::PANEL_W * s);
        if overlay_open_here {
            return;
        }
        // 2) Page links
        if let Some(target) = self.app.link_at(p) {
            self.follow_link(target);
        }
    }
}

impl Ui {
    pub fn open_document(&mut self, path: PathBuf) {
        // Data-loss guard (Pro): replacing the document nukes any unsaved form
        // edits, so gate the open the same way quit() gates the close — before
        // any existing logic runs.
        #[cfg(feature = "pro")]
        {
            if self.saves_inflight > 0 {
                // A save is resolving. Defer the open until it lands: the save
                // must finish before we drop the doc (like quit()'s deferral),
                // and Saved(Ok) re-runs open_document so this gate re-evaluates.
                self.pending_open = Some(path);
                self.pending_close = false; // the open supersedes any deferred close
                return;
            }
            if self.app.form_dirty && self.app.doc().is_some() {
                let choice = unsafe {
                    MessageBoxW(
                        Some(self.hwnd),
                        w!("Save changes to this PDF first?"),
                        crate::edition::NAME_W,
                        MB_YESNOCANCEL | MB_ICONWARNING,
                    )
                };
                match choice {
                    IDYES => {
                        // Save first; the open runs when Saved(Ok) fires.
                        self.pending_open = Some(path);
                        self.pending_close = false; // the open supersedes any deferred close
                        self.save_doc();
                        return;
                    }
                    IDNO => {} // discard edits: fall through and open now
                    _ => return, // IDCANCEL / dismissed: keep the current doc + edits
                }
            }
        }
        // Opening a different file while a password prompt is up abandons
        // the prompt; restore the stashed doc first so it isn't silently
        // lost (password_cancel is a no-op outside Password mode).
        self.app.password_cancel();
        if self.app.doc().is_none() {
            self.app.mode = crate::app::Mode::Loading { path: path.clone() };
        }
        self.send_pdf(PdfCmd::Load { path, password: None });
        self.invalidate();
    }

    fn open_dialog(&mut self) {
        if let Some(path) = self.pick_pdf() {
            self.open_document(path);
        }
    }

    /// Modal "Open PDF" file picker. None if the user cancelled.
    fn pick_pdf(&self) -> Option<PathBuf> {
        let mut file_buf = [0u16; 1024];
        let mut ofn = OPENFILENAMEW {
            lStructSize: core::mem::size_of::<OPENFILENAMEW>() as u32,
            hwndOwner: self.hwnd,
            // w!() const-encodes the embedded NULs of the classic filter format.
            lpstrFilter: w!("PDF documents (*.pdf)\0*.pdf\0All files (*.*)\0*.*\0"),
            nFilterIndex: 1,
            lpstrFile: PWSTR(file_buf.as_mut_ptr()),
            nMaxFile: file_buf.len() as u32,
            lpstrTitle: w!("Open PDF"),
            Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST,
            ..Default::default()
        };
        if unsafe { GetOpenFileNameW(&mut ofn) }.as_bool() {
            let len = file_buf.iter().position(|&c| c == 0).unwrap_or(file_buf.len());
            Some(PathBuf::from(String::from_utf16_lossy(&file_buf[..len])))
        } else {
            None
        }
    }

    /// Launch a fresh pppdf process — each window is its own process, keeping
    /// documents fully isolated (the pdfium binding is a process-global
    /// singleton, and a hung PDF can never stall other windows).
    fn spawn_new_window(&self, path: Option<PathBuf>) {
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                self.error_box(&format!("Couldn't start a new window:\n{e}"));
                return;
            }
        };
        let mut cmd = std::process::Command::new(exe);
        if let Some(path) = path {
            cmd.arg(path);
        }
        if let Err(e) = cmd.spawn() {
            self.error_box(&format!("Couldn't start a new window:\n{e}"));
        }
    }

    /// The imprint shown by About. Branding comes from `edition` and the
    /// version straight from Cargo, making this the one place in the UI that
    /// answers "which build is this?" — hence the explicit edition line.
    /// Pure so the answer can be pinned by a test in both editions.
    fn about_text() -> String {
        format!(
            "{} {}\n{}\n{}\n\n\
             Created by Sebastian K\u{e4}stner\n\
             seka@digital-distributions.com\n\n\
             Renders with PDFium.",
            crate::edition::NAME,
            env!("CARGO_PKG_VERSION"),
            crate::edition::EDITION,
            env!("CARGO_PKG_DESCRIPTION"),
        )
    }

    /// Right-click → About.
    fn about_box(&self) {
        let wide: Vec<u16> = format!("{}\0", Self::about_text()).encode_utf16().collect();
        unsafe {
            MessageBoxW(
                Some(self.hwnd),
                PCWSTR(wide.as_ptr()),
                crate::edition::NAME_W,
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }

    fn error_box(&self, text: &str) {
        let wide: Vec<u16> = format!("{text}\0").encode_utf16().collect();
        unsafe {
            MessageBoxW(
                Some(self.hwnd),
                PCWSTR(wide.as_ptr()),
                crate::edition::NAME_W,
                MB_OK | MB_ICONERROR,
            );
        }
    }

    fn send_pdf(&self, cmd: PdfCmd) {
        if let Some(tx) = &self.pdf_tx {
            let _ = tx.send(cmd);
        }
    }

    /// FWL_EVENTFLAG modifier bits for FORM_On* (fpdf_fwlevent.h: Shift=1,
    /// Control=2, Alt=4).
    #[cfg(feature = "pro")]
    fn form_modifiers() -> i32 {
        let mut m = 0;
        unsafe {
            if GetKeyState(VK_SHIFT.0 as i32) < 0 {
                m |= 1;
            }
            if GetKeyState(VK_CONTROL.0 as i32) < 0 {
                m |= 2;
            }
            if GetKeyState(VK_MENU.0 as i32) < 0 {
                m |= 4;
            }
        }
        m
    }

    #[cfg(feature = "pro")]
    fn send_form(&self, ev: crate::types::FormEvent) {
        if let Some(d) = self.app.doc() {
            let page = d.current;
            crate::dlog!("ui: send_form page={page} ev={ev:?}");
            self.send_pdf(PdfCmd::Form { page, ev });
        }
    }

    /// Leave typing mode, committing the in-progress edit.
    #[cfg(feature = "pro")]
    fn form_kill_focus(&mut self) {
        if self.app.form_focus {
            self.app.form_focus = false;
            self.send_form(crate::types::FormEvent::KillFocus);
        }
    }

    /// Move focus to the next field's center (Tab cycling). Uses FORM_OnFocus
    /// — focus WITHOUT button events — because a synthesized MouseDown/Up
    /// pair is a real click: on a checkbox/radio it would TOGGLE the widget
    /// as Tab passes over it. Focusing mutates nothing (no dirty-marking).
    #[cfg(feature = "pro")]
    fn form_tab(&mut self, backwards: bool) {
        if let Some(pt) = self.app.form_tab_target(backwards) {
            self.send_form(crate::types::FormEvent::Focus { x: pt.x, y: pt.y });
            self.app.form_focus = true;
        }
    }

    /// Ctrl+V: paste clipboard text into the focused field.
    #[cfg(feature = "pro")]
    fn form_paste(&mut self) {
        if let Some(text) = clipboard_text(self.hwnd) {
            // PDFium only fires FFI_OnChange for a TEXT field when its value
            // COMMITS (focus leaves the field) — not per edit. Mark dirty
            // here so a paste isn't silently lost if the window closes
            // before the field ever loses focus.
            self.app.form_dirty = true;
            self.send_form(crate::types::FormEvent::ReplaceSelection { text });
        }
    }

    /// Ctrl+S: save the (form-bearing) document in place.
    #[cfg(feature = "pro")]
    fn save_doc(&mut self) {
        let has_forms = self.app.doc().map_or(false, |d| d.has_forms);
        if !has_forms {
            return;
        }
        // Nothing changed since the last save/open: a reflex Ctrl+S must not
        // rewrite the file (needless disk write + mtime churn, and a spurious
        // "Saved" toast). The quit()/open-gate callers only reach save_doc from
        // behind their OWN form_dirty checks, so this never strands a
        // pending_close/pending_open waiting on a save that was skipped.
        if !self.app.form_dirty {
            return;
        }
        self.app.form_focus = false; // the save commits + kills focus PDF-side
        // Clear the dirty flag at DISPATCH, not on Saved(Ok): the save
        // snapshots the document as of NOW, so edits made while it is in
        // flight re-mark the flag and must survive. A completion-time clear
        // would clobber exactly those edits (type -> Ctrl+S -> type a
        // correction -> Saved(Ok) clears -> Alt+F4 closes silently, the
        // correction lost). Saved(Err) restores the flag.
        self.app.form_dirty = false;
        self.saves_inflight += 1; // quit()/open defer until every Saved reply lands
        self.send_pdf(PdfCmd::SaveDoc);
    }

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
                if let Some(d) = self.app.doc_mut() {
                    d.overlay = None; // panel would show the removed bookmark
                }
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

    // --- text search (Ctrl+F find bar) ---

    /// Open the find bar (Ctrl+F / F3): closes the TOC panel first — the
    /// bar owns Enter/arrows while open, and a keyboard-dead panel is worse
    /// than a closed one (same courtesy as cancelling the goto prompt).
    /// Re-issues the remembered query's scan when the bar newly opens.
    fn search_open_ui(&mut self) {
        if self.app.doc().is_none() {
            return;
        }
        if let Some(d) = self.app.doc_mut() {
            d.overlay = None;
        }
        let was_active = self.app.search_active();
        if self.app.search_open() {
            if !was_active {
                self.sync_search();
            }
            self.invalidate();
        }
    }

    /// Close the bar, remembering the query (Esc path).
    fn search_close_ui(&mut self) {
        if self.app.search_close() {
            self.send_pdf(PdfCmd::CancelSearch);
            self.invalidate();
        }
    }

    /// Issue the scan matching the bar's current query, or cancel when it
    /// is illegal (< 2 chars). The single choke point every query mutation
    /// funnels through — begin_scan's atomic reset is what keeps a new
    /// query's results from merging with the old ones.
    fn sync_search(&mut self) {
        let Some((current, legal)) = self
            .app
            .doc()
            .and_then(|d| d.search.as_ref().map(|st| (d.current, st.legal())))
        else {
            return;
        };
        if legal {
            let id = self.app.next_search_id();
            let payload = self.app.doc_mut().and_then(|d| d.search.as_mut()).map(|st| {
                st.begin_scan(id);
                (st.query.clone(), st.options)
            });
            if let Some((query, options)) = payload {
                self.send_pdf(PdfCmd::Search { id, query, options, start_page: current });
            }
        } else {
            if let Some(st) = self.app.doc_mut().and_then(|d| d.search.as_mut()) {
                st.clear_results();
            }
            self.send_pdf(PdfCmd::CancelSearch);
        }
    }

    /// Apply a QueryEffect returned by a bar edit/toggle.
    fn apply_search_effect(&mut self, eff: crate::search::QueryEffect) {
        match eff {
            crate::search::QueryEffect::Rescan => self.sync_search(),
            // clear_results already ran inside SearchState (edited()).
            crate::search::QueryEffect::Cancel => self.send_pdf(PdfCmd::CancelSearch),
            crate::search::QueryEffect::Nothing => {}
        }
        self.invalidate();
    }

    /// Enter / Shift+Enter / F3 / Shift+F3 while the bar is open.
    fn search_step_ui(&mut self, backward: bool) {
        let step = match self.app.doc_mut().and_then(|d| d.search.as_mut()) {
            Some(st) => st.step(backward),
            None => return,
        };
        match step {
            crate::search::Step::Jump(page, idx) => self.search_jump(page, idx),
            crate::search::Step::Armed | crate::search::Step::Nothing => self.invalidate(),
        }
    }

    /// Land on a match: goto_match keeps zoom and reveals the union bbox;
    /// a page change records history and badges like every jump source.
    fn search_jump(&mut self, page: u16, idx: usize) {
        let bbox = self
            .app
            .doc()
            .and_then(|d| d.search.as_ref())
            .and_then(|st| st.union_bbox(page, idx));
        let Some(bbox) = bbox else { return };
        let changed = self.app.goto_match(page, bbox);
        self.sync_renders();
        if changed {
            self.show_badge();
        }
        self.invalidate();
    }

    /// Any path that restores a stashed DocState (password cancel, a failed
    /// load over an open doc) re-issues an open search with a fresh id: the
    /// Load that displaced the doc killed its scan WITHOUT a SearchDone, so
    /// the restored `scanning` flag would otherwise spin forever.
    ///
    /// password_take (Enter with a typed password) ALSO restores the stash,
    /// but deliberately does NOT resync: the retry Load it sends right after
    /// would kill the fresh scan immediately, and every continuation repairs
    /// the stale flag anyway — Loaded rebuilds DocState, LoadFailed lands in
    /// the resync below, NeedsPassword re-stashes until a cancel does.
    fn resync_open_search(&mut self) {
        if self.app.search_active() {
            self.sync_search();
        }
    }

    /// Keys while the find bar is open. Returns true when consumed. The bar
    /// is semi-modal: PgUp/PgDn still page the document, and every Ctrl
    /// chord except Ctrl+V/Ctrl+F falls through (Ctrl+W quits, Ctrl+O/N
    /// open, Ctrl+S saves — browser find-bar behavior).
    fn search_key(&mut self, vk: u16) -> bool {
        if !self.app.search_active() {
            return false;
        }
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        match vk {
            _ if vk == VK_ESCAPE.0 => self.search_close_ui(),
            _ if vk == VK_RETURN.0 || vk == VK_F3.0 => self.search_step_ui(shift),
            _ if vk == VK_BACK.0 && !ctrl => {
                let eff =
                    self.app.doc_mut().and_then(|d| d.search.as_mut()).map(|st| st.backspace());
                if let Some(eff) = eff {
                    self.apply_search_effect(eff);
                }
            }
            0x56 if ctrl => {
                // Ctrl+V: paste into the query.
                if let Some(text) = clipboard_text(self.hwnd) {
                    let eff = self
                        .app
                        .doc_mut()
                        .and_then(|d| d.search.as_mut())
                        .map(|st| st.paste(&text));
                    if let Some(eff) = eff {
                        self.apply_search_effect(eff);
                    }
                }
            }
            0x46 if ctrl => {
                // Ctrl+F while open: re-arm select-all emulation.
                self.app.search_open();
                self.invalidate();
            }
            _ if ctrl => return false, // other Ctrl chords keep their global meaning
            _ if vk == VK_NEXT.0 || vk == VK_PRIOR.0 => return false, // page the doc
            _ if vk == VK_TAB.0
                || vk == VK_LEFT.0
                || vk == VK_RIGHT.0
                || vk == VK_UP.0
                || vk == VK_DOWN.0
                || vk == VK_HOME.0
                || vk == VK_END.0 => {} // swallowed: no caret model; no surprise jumps
            _ => {} // printables arrive via WM_CHAR
        }
        true
    }

    /// Ask the PDF thread for whatever the current state needs rendered.
    pub fn sync_renders(&mut self) {
        for req in self.app.wanted_renders() {
            self.app.mark_pending(&req);
            self.send_pdf(PdfCmd::Render {
                page: req.page,
                px: req.px,
                rot: req.rot,
                visible: req.visible,
            });
        }
    }

    fn invalidate(&self) {
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(self.hwnd), None, false);
        }
    }

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

    fn show_badge(&mut self) {
        if let Some(d) = self.app.doc() {
            let text = format!("{} / {}", d.current + 1, d.pages);
            self.show_toast(text);
        }
    }

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

    /// Render one frame directly (outside WM_PAINT). Used once before the
    /// window is shown so the surface holds real content from the first frame.
    pub fn paint_now(&mut self) {
        if let Some(mut r) = self.renderer.take() {
            let _ = r.draw(self.hwnd, &self.app, self.badge_display());
            self.renderer = Some(r);
        }
    }

    fn pump_pdf_replies(&mut self) {
        let Some(rx) = self.reply_rx.take() else { return };
        while let Ok(reply) = rx.try_recv() {
            match reply {
                PdfReply::Loaded { path, pages, page_sizes, outline, has_forms, bookmarks } => {
                    self.app.doc_loaded(path, pages, page_sizes, outline, has_forms, bookmarks);
                    // Drop the renderer's memoized device bitmap with the
                    // document. Its `gen` key already prevents stale pixels;
                    // this releases the old document's GPU bitmap promptly.
                    if let Some(r) = &mut self.renderer {
                        r.clear_page_cache();
                    }
                    self.show_badge();
                }
                PdfReply::Rendered { page, px, rot, bgra, links, fields } => {
                    // gen is stamped by rendered() — 0 here is a placeholder.
                    self.app.rendered(crate::app::CachedPage {
                        page,
                        px,
                        rot,
                        bgra,
                        links,
                        fields,
                        gen: 0,
                    });
                }
                PdfReply::RenderFailed { page, reason } => {
                    self.app.render_failed(page, reason == "superseded");
                }
                PdfReply::SearchHits { id, page, matches } => {
                    let target = self
                        .app
                        .doc_mut()
                        .and_then(|d| d.search.as_mut())
                        .and_then(|st| st.hits(id, page, matches));
                    if let Some((page, idx)) = target {
                        self.search_jump(page, idx); // a pending jump fired
                    }
                }
                PdfReply::SearchDone { id } => {
                    let target = self
                        .app
                        .doc_mut()
                        .and_then(|d| d.search.as_mut())
                        .and_then(|st| st.done(id));
                    if let Some((page, idx)) = target {
                        self.search_jump(page, idx); // frontier jump wrapped
                    }
                }
                PdfReply::NeedsPassword { path } => {
                    // Stash a currently open document so Esc can restore it
                    // (the PDF thread kept it loaded, too — Task 4).
                    let prev = match std::mem::replace(&mut self.app.mode, crate::app::Mode::Empty)
                    {
                        crate::app::Mode::Doc(d) => Some(Box::new(d)),
                        _ => None,
                    };
                    self.app.mode =
                        crate::app::Mode::Password { path, buf: String::new(), prev };
                }
                PdfReply::LoadFailed { path, reason } => {
                    if path.as_os_str().is_empty() {
                        // pdfium.dll itself failed to load — nothing can work.
                        self.error_box(
                            "pdfium.dll could not be loaded.\n\nIt must sit in the same \
                             folder as pppdf.exe. Run fetch-pdfium.ps1 or re-copy the dll, \
                             then start pppdf again.",
                        );
                        self.quit();
                    } else {
                        self.error_box(&format!(
                            "Could not open\n{}\n\n{reason}",
                            path.display()
                        ));
                        // A previously open document is untouched (the PDF thread
                        // kept it); only fall back to Empty if nothing is open.
                        if matches!(self.app.mode, crate::app::Mode::Loading { .. }) {
                            self.app.mode = crate::app::Mode::Empty;
                        } else {
                            // The surviving open doc may have had its scan
                            // killed by the failed Load — re-issue it.
                            self.resync_open_search();
                        }
                    }
                }
                #[cfg(feature = "pro")]
                PdfReply::FormDirty => {
                    crate::dlog!("ui: reply FormDirty");
                    self.app.form_dirty = true;
                }
                #[cfg(feature = "pro")]
                PdfReply::Saved { result } => match result {
                    Ok(()) => {
                        // Each dispatched save lands its own reply; decrement
                        // per reply so the latch only clears once the LAST
                        // overlapping save resolves.
                        self.saves_inflight = self.saves_inflight.saturating_sub(1);
                        // A pending open was gated on this save. Re-run
                        // open_document FIRST (before the pending_close/toast
                        // logic) so its own gate re-evaluates now that this save
                        // landed: if another save is still in flight it
                        // re-defers; if the user re-dirtied mid-flight it
                        // re-prompts; otherwise it proceeds cleanly. The open is
                        // the acknowledgment — no "Saved" toast for this reply.
                        if let Some(path) = self.pending_open.take() {
                            self.open_document(path);
                            // open_document may have loaded a new doc, dispatched
                            // another save, or shown a modal — this reply is
                            // fully handled either way. Put the receiver back and
                            // stop (same early discipline the pending_close
                            // branch uses); remaining replies re-pump via their
                            // own WM_APP_PDF_REPLY wakes.
                            self.reply_rx = Some(rx);
                            return;
                        }
                        // An earlier overlapping Ctrl+S is still in flight: don't
                        // act on pending_close or show a toast until every write
                        // has resolved — tearing the window down now would kill
                        // the PDF thread mid-write.
                        if self.saves_inflight > 0 {
                            self.reply_rx = Some(rx);
                            return;
                        }
                        // form_dirty was cleared when the save was DISPATCHED
                        // (save_doc) and must NOT be cleared here: if it is
                        // set again, the user edited while the save was in
                        // flight and those edits are NOT in the file just
                        // written.
                        if self.pending_close {
                            self.pending_close = false;
                            if self.app.form_dirty {
                                // Deliberate edit after choosing "Yes" in the
                                // close prompt: re-run the close gate —
                                // prompting again is correct, the new edit is
                                // unsaved.
                                self.quit();
                            } else {
                                unsafe {
                                    let _ = DestroyWindow(self.hwnd);
                                }
                            }
                            // Either path may have destroyed the window: put
                            // the receiver back and stop — don't keep draining
                            // or run sync_renders/invalidate against a dead
                            // hwnd. Remaining replies are re-pumped by their
                            // own WM_APP_PDF_REPLY wakes.
                            self.reply_rx = Some(rx);
                            return;
                        }
                        self.show_toast("Saved".into());
                    }
                    Err(e) => {
                        self.saves_inflight = self.saves_inflight.saturating_sub(1);
                        // Unsaved changes definitely exist — restore the flag
                        // that save_doc cleared optimistically at dispatch.
                        self.app.form_dirty = true;
                        self.pending_close = false;
                        // Abandon any deferred open too: the save just FAILED, so
                        // the edits are still unsaved — proceeding with the open
                        // would discard exactly the data the failed save proved
                        // is at risk.
                        self.pending_open = None;
                        self.error_box(&format!("Could not save:\n{e}"));
                    }
                },
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
            }
        }
        self.reply_rx = Some(rx);
        self.sync_renders();
        self.invalidate();
    }

    fn flip(&mut self, delta: i32) {
        #[cfg(feature = "pro")]
        self.form_kill_focus();
        if self.app.flip(delta) {
            self.sync_renders();
            self.show_badge();
            self.invalidate();
        }
    }

    /// True while a modal layer owns the keyboard (password prompt, F1
    /// help, go-to-page entry) — history navigation must not fire
    /// underneath. The TOC overlay is deliberately NOT modal here (it
    /// falls through unhandled keys already); Pro typing mode is handled
    /// by nav_back/nav_forward killing form focus, like the wheel does.
    fn modal_active(&self) -> bool {
        #[cfg(feature = "pro")]
        if self.app.bookmark_entry_active() {
            return true;
        }
        matches!(self.app.mode, crate::app::Mode::Password { .. })
            || self.app.help_visible
            || self.app.goto_entry_active()
    }

    /// Alt+Left / Backspace / mouse back: return to the last jump origin.
    fn nav_back(&mut self) {
        if self.modal_active() {
            return;
        }
        #[cfg(feature = "pro")]
        self.form_kill_focus();
        if self.app.back() {
            self.sync_renders();
            self.show_badge();
            self.invalidate();
        }
    }

    /// Alt+Right / Shift+Backspace / mouse forward: re-do the jump.
    fn nav_forward(&mut self) {
        if self.modal_active() {
            return;
        }
        #[cfg(feature = "pro")]
        self.form_kill_focus();
        if self.app.forward() {
            self.sync_renders();
            self.show_badge();
            self.invalidate();
        }
    }
}

/// Plain-text clipboard contents (CF_UNICODETEXT), if any. Used by Pro form
/// paste and the find bar.
fn clipboard_text(hwnd: HWND) -> Option<String> {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let out = GetClipboardData(13 /* CF_UNICODETEXT */).ok().and_then(|h| {
            let hg = HGLOBAL(h.0 as *mut core::ffi::c_void);
            let ptr = GlobalLock(hg) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            let _ = GlobalUnlock(hg);
            Some(s)
        });
        let _ = CloseClipboard();
        out
    }
}

/// Inset the maximized client rect by 2px on any monitor edge hosting an
/// auto-hide taskbar, so the bar can still fly out (Windows Terminal GH#1438).
fn apply_autohide_taskbar_insets(rect: &mut RECT, mi: &MONITORINFO) {
    unsafe {
        let mut state_data = APPBARDATA {
            cbSize: core::mem::size_of::<APPBARDATA>() as u32,
            ..Default::default()
        };
        let state = SHAppBarMessage(ABM_GETSTATE, &mut state_data) as u32;
        if state & ABS_AUTOHIDE == 0 {
            return;
        }
        let has_bar = |edge: u32| -> bool {
            let mut data = APPBARDATA {
                cbSize: core::mem::size_of::<APPBARDATA>() as u32,
                uEdge: edge,
                rc: mi.rcMonitor,
                ..Default::default()
            };
            SHAppBarMessage(ABM_GETAUTOHIDEBAREX, &mut data) != 0
        };
        // ABE_LEFT=0, ABE_TOP=1, ABE_RIGHT=2, ABE_BOTTOM=3
        if has_bar(1) {
            rect.top += 2;
        }
        if has_bar(3) {
            rect.bottom -= 2;
        }
        if has_bar(0) {
            rect.left += 2;
        }
        if has_bar(2) {
            rect.right -= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Height tracks the screen and the page shape survives, at every
    /// resolution — the point of sizing the window relative to the display.
    #[test]
    fn startup_size_is_three_quarters_of_screen_height() {
        for (sw, sh) in [(1920, 1080), (2560, 1440), (3840, 2160), (1366, 768)] {
            let (w, h) = default_size_for_screen(sw, sh);
            assert_eq!(h, (sh as f32 * 0.75).round() as i32, "height on {sw}x{sh}");
            let ratio = w as f32 / h as f32;
            assert!((ratio - DEFAULT_ASPECT).abs() < 0.01, "aspect on {sw}x{sh}: {ratio}");
            assert!(w < sw && h < sh, "must fit on {sw}x{sh}, got {w}x{h}");
        }
    }

    /// A rotated monitor is narrower than the page shape wants; width leads
    /// instead, and the ratio still holds.
    #[test]
    fn portrait_screen_gives_up_height_to_keep_the_ratio() {
        let (sw, sh) = (1080, 1920);
        let (w, h) = default_size_for_screen(sw, sh);
        assert!(w <= (sw as f32 * 0.75).round() as i32, "width {w} must fit {sw}");
        assert!(h < (sh as f32 * 0.75).round() as i32, "height should have been given up");
        assert!((w as f32 / h as f32 - DEFAULT_ASPECT).abs() < 0.01);
    }

    /// About must name the edition outright in both builds — the free one
    /// can't rely on "pppdf" lacking the word "Pro" to say what it is.
    #[test]
    fn about_names_the_edition_and_version() {
        let t = Ui::about_text();
        assert!(t.contains(crate::edition::NAME), "{t}");
        assert!(t.contains(env!("CARGO_PKG_VERSION")), "{t}");
        assert!(t.contains(crate::edition::EDITION), "{t}");
        assert_eq!(t.contains("Pro edition"), crate::edition::IS_PRO, "{t}");
        assert!(t.contains("Sebastian K\u{e4}stner"), "{t}");
        assert!(t.contains("seka@digital-distributions.com"), "{t}");
    }

    /// No metrics (headless session) must not yield a degenerate window.
    #[test]
    fn missing_metrics_fall_back_to_the_fixed_size() {
        assert_eq!(default_size_for_screen(0, 0), (850, 1100));
        assert_eq!(default_size_for_screen(-1, -1), (850, 1100));
    }

    /// Even an absurdly small screen can't produce a window under the minimum
    /// the resize clamp enforces.
    #[test]
    fn tiny_screen_still_clears_the_minimum() {
        let (w, h) = default_size_for_screen(120, 100);
        assert!(w >= MIN_SIZE_DIP && h >= MIN_SIZE_DIP, "got {w}x{h}");
    }
}

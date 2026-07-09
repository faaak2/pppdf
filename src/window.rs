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
    VK_F1, VK_F11, VK_HOME, VK_LEFT, VK_NEXT, VK_NUMPAD0, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT,
    VK_SPACE, VK_TAB, VK_UP,
};
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

const GRIP_DIP: f32 = 8.0;
const MIN_SIZE_DIP: i32 = 200;

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
    /// Pre-F11 window rect, so a second F11 restores it exactly.
    pub saved_rect: Option<RECT>,
    /// Leftover sub-notch WM_MOUSEWHEEL delta (precision touchpads send many
    /// small deltas instead of whole 120-unit notches); accumulated across
    /// events so they aren't truncated to zero forever.
    pub wheel_residual: i32,
    /// Whether the window is pinned above others (WS_EX_TOPMOST). Toggled from
    /// the context menu; not persisted (the app keeps no config).
    pub always_on_top: bool,
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
            saved_rect: None,
            wheel_residual: 0,
            always_on_top: false,
        });

        // Every flag here is load-bearing for native snap/animations; the
        // caption never shows because WM_NCCALCSIZE claims the whole rect.
        let style =
            WS_POPUP | WS_THICKFRAME | WS_CAPTION | WS_SYSMENU | WS_MAXIMIZEBOX | WS_MINIMIZEBOX;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            crate::edition::NAME_W,
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            850,
            1100,
            None,
            None,
            Some(instance),
            Some(&mut *ui as *mut Ui as *const core::ffi::c_void),
        )?;

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
                    LRESULT(self.hit_test(sx, sy) as isize)
                }
                WM_NCLBUTTONDBLCLK => {
                    // DefWindowProc would toggle maximize on caption dbl-click.
                    // Swallow it, resetting zoom instead (double-click on the
                    // caption-draggable page body resets to fit).
                    if wparam.0 as u32 == HTCAPTION {
                        self.dismiss_goto_entry();
                        if self.app.reset_zoom() {
                            self.sync_renders();
                            self.invalidate();
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
                    // Wheel navigation abandons a half-typed go-to-page entry —
                    // the prompt must not sit open while pages flip under it.
                    self.dismiss_goto_entry();
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
                    // A click anywhere dismisses the help overlay (it's modal).
                    if self.app.help_visible {
                        self.app.toggle_help();
                        self.invalidate();
                        return LRESULT(0);
                    }
                    // A click also abandons the go-to-page entry (then acts normally).
                    self.dismiss_goto_entry();
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
                    LRESULT(0)
                }
                WM_LBUTTONDBLCLK => {
                    self.dismiss_goto_entry();
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
                WM_CHAR => {
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
        }
        false
    }

    fn on_key(&mut self, vk: u16) {
        if matches!(self.app.mode, crate::app::Mode::Password { .. }) {
            if vk == VK_ESCAPE.0 {
                // Restores the stashed doc if one was open, else Empty.
                self.app.password_cancel();
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
                _ => {}
            }
            self.invalidate();
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
            0x4E if ctrl => self.spawn_new_window(None), // Ctrl+N
            0x4F if ctrl && shift => {
                // Ctrl+Shift+O — must precede the plain Ctrl+O arm
                if let Some(path) = self.pick_pdf() {
                    self.spawn_new_window(Some(path));
                }
            }
            0x4F if ctrl => self.open_dialog(), // Ctrl+O
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
                let page = ov.activate();
                d.overlay = None;
                if let Some(page) = page {
                    if self.app.goto(page) {
                        self.sync_renders();
                        self.show_badge();
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

    fn quit(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    /// F11: fill the current monitor's work area edge to edge (taskbar stays
    /// visible); a second press restores the previous size/position.
    /// Independent of native maximize (Win+Up).
    fn toggle_fill(&mut self) {
        unsafe {
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
                return;
            }
            let mut rc = RECT::default();
            let _ = GetWindowRect(self.hwnd, &mut rc);
            let monitor = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: core::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(monitor, &mut mi).as_bool() {
                self.saved_rect = Some(rc);
                let w = mi.rcWork;
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

    fn show_menu(&mut self, sx: i32, sy: i32) {
        self.dismiss_goto_entry();
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
                let page = ov.activate();
                d.overlay = None; // jump closes the panel
                if let Some(page) = page {
                    if self.app.goto(page) {
                        self.sync_renders();
                        self.show_badge();
                    }
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

    fn show_badge(&mut self) {
        if let Some(d) = self.app.doc() {
            self.badge = Some((format!("{} / {}", d.current + 1, d.pages), 0));
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_BADGE, 33, None);
            }
            self.invalidate();
        }
    }

    fn badge_display(&self) -> Option<(&str, f32)> {
        self.badge.as_ref().map(|(text, ticks)| {
            let alpha = if *ticks <= BADGE_HOLD_TICKS {
                1.0
            } else {
                1.0 - (*ticks - BADGE_HOLD_TICKS) as f32 / BADGE_FADE_TICKS as f32
            };
            (text.as_str(), alpha.clamp(0.0, 1.0))
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
                PdfReply::Loaded { path, pages, page_sizes, outline, has_forms } => {
                    self.app.doc_loaded(path, pages, page_sizes, outline, has_forms);
                    // The renderer memoizes a device bitmap keyed (page, px);
                    // a new same-sized document would otherwise show the OLD
                    // document's page. Drop the memo with the document.
                    if let Some(r) = &mut self.renderer {
                        r.clear_page_cache();
                    }
                    self.show_badge();
                }
                PdfReply::Rendered { page, px, rot, bgra, links, fields } => {
                    self.app.rendered(crate::app::CachedPage { page, px, rot, bgra, links, fields });
                }
                PdfReply::RenderFailed { page, reason } => {
                    self.app.render_failed(page, reason == "superseded");
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
                        }
                    }
                }
                // Form-fill replies: the UI acts on these in later tasks
                // (dirty-state tracking / save feedback). Accept them now so
                // the pro build's reply match stays exhaustive.
                #[cfg(feature = "pro")]
                PdfReply::FormDirty => {}
                #[cfg(feature = "pro")]
                PdfReply::Saved { .. } => {}
            }
        }
        self.reply_rx = Some(rx);
        self.sync_renders();
        self.invalidate();
    }

    fn flip(&mut self, delta: i32) {
        if self.app.flip(delta) {
            self.sync_renders();
            self.show_badge();
            self.invalidate();
        }
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

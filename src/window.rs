//! The borderless Win32 window: class, WndProc, chromeless tricks, input.

use crate::app::App;
use crate::layout::{hit_zone, PointF, SizeU, Zone};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND, DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, GetMonitorInfoW,
    MonitorFromWindow, HBRUSH, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_ESCAPE};
use windows::Win32::UI::Shell::{
    DragAcceptFiles, SHAppBarMessage, ABM_GETAUTOHIDEBAREX, ABM_GETSTATE, ABS_AUTOHIDE,
    APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_APP_PDF_REPLY: u32 = WM_APP + 1;

pub const CMD_OPEN: usize = 1;
pub const CMD_TOC: usize = 2;
pub const CMD_FIT: usize = 3;
pub const CMD_MINIMIZE: usize = 4;
pub const CMD_CLOSE: usize = 5;

const GRIP_DIP: f32 = 8.0;
const MIN_SIZE_DIP: i32 = 200;

pub struct Ui {
    pub hwnd: HWND,
    pub app: App,
}

pub fn run(app: App) -> Result<()> {
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class_name = w!("pppdf_window");

        let wc = WNDCLASSEXW {
            cbSize: core::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH::default(), // we paint everything ourselves
            lpszClassName: class_name,
            ..Default::default()
        };
        // NEVER put this call inside debug_assert! — the assertion (and the
        // call!) would vanish in release builds and window creation would fail.
        let atom = RegisterClassExW(&wc);
        debug_assert!(atom != 0);

        let mut ui = Box::new(Ui { hwnd: HWND::default(), app });

        // Every flag here is load-bearing for native snap/animations; the
        // caption never shows because WM_NCCALCSIZE claims the whole rect.
        let style =
            WS_POPUP | WS_THICKFRAME | WS_CAPTION | WS_SYSMENU | WS_MAXIMIZEBOX | WS_MINIMIZEBOX;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("pppdf"),
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
        let _ = ShowWindow(hwnd, SW_SHOW);

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
                    // Swallow it; Task 9 wires this to reset-zoom.
                    if wparam.0 as u32 == HTCAPTION {
                        return LRESULT(0);
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
                WM_SIZE => {
                    self.app.set_client(self.client_size());
                    LRESULT(0)
                }
                WM_DPICHANGED => {
                    self.app.dpi = (wparam.0 & 0xFFFF) as u32;
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
                    // Temporary GDI fill; Task 7 replaces this with Direct2D.
                    let mut ps = PAINTSTRUCT::default();
                    let hdc = BeginPaint(self.hwnd, &mut ps);
                    let brush = CreateSolidBrush(COLORREF(0x001A1A1A));
                    FillRect(hdc, &ps.rcPaint, brush);
                    let _ = DeleteObject(brush.into());
                    let _ = EndPaint(self.hwnd, &ps);
                    LRESULT(0)
                }
                WM_KEYDOWN => {
                    self.on_key(wparam.0 as u16);
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
    /// (zoomed state). For now: no.
    fn wants_client_mouse(&self, _p: PointF) -> bool {
        false
    }

    fn on_key(&mut self, vk: u16) {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        match vk {
            _ if vk == VK_ESCAPE.0 => self.quit(),
            0x57 if ctrl => self.quit(), // Ctrl+W
            _ => {}
        }
    }

    fn quit(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    fn show_menu(&mut self, sx: i32, sy: i32) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, w!("Open…\tCtrl+O"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_TOC, w!("Table of Contents\tTab"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_FIT, w!("Fit to Window\t0"));
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
            // CMD_OPEN (Task 10), CMD_TOC (Task 8), CMD_FIT (Task 9)
            _ => {}
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

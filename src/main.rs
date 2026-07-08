//! pppdf — borderless native PDF viewer.
#![windows_subsystem = "windows"]

use pppdf::app::App;
use pppdf::window;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

fn main() {
    // Belt and suspenders with the embedded manifest; must precede any HWND.
    // Access-denied here just means the manifest already set it.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let initial = std::env::args().nth(1).map(std::path::PathBuf::from);
    if let Err(e) = window::run(App::new(), initial) {
        // No console in windows subsystem — surface startup failures visibly.
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
            let text: Vec<u16> =
                format!("pppdf failed to start:\n{e}\0").encode_utf16().collect();
            MessageBoxW(
                None,
                windows::core::PCWSTR(text.as_ptr()),
                windows::core::w!("pppdf"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

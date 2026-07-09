//! pppdf — borderless native PDF viewer.
#![windows_subsystem = "windows"]

use pppdf::app::App;
use pppdf::edition;
use pppdf::window;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

fn main() {
    // Any thread's panic (incl. the PDF thread) surfaces visibly, then exits —
    // no zombie chromeless window.
    std::panic::set_hook(Box::new(|info| {
        let text: Vec<u16> =
            format!("{} crashed:\n{info}\0", edition::NAME).encode_utf16().collect();
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
            MessageBoxW(
                None,
                windows::core::PCWSTR(text.as_ptr()),
                edition::NAME_W,
                MB_OK | MB_ICONERROR,
            );
        }
        std::process::exit(1);
    }));

    // File-association subcommands run without opening a window, then exit.
    let args: Vec<String> = std::env::args().collect();
    use pppdf::register::Cmd;
    match pppdf::register::classify(args.get(1).map(String::as_str)) {
        Cmd::Register => std::process::exit(pppdf::register::register()),
        Cmd::Unregister => std::process::exit(pppdf::register::unregister()),
        // Unrecognized flag or explicit --help: show usage and exit.
        Cmd::Help | Cmd::Unknown => {
            pppdf::register::show_usage();
            return;
        }
        Cmd::None => {}
    }

    // Belt and suspenders with the embedded manifest; must precede any HWND.
    // Access-denied here just means the manifest already set it.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    // argv[1] opens here; each extra path (a multi-select dropped on the exe)
    // opens in its own window, matching the one-document-per-window model.
    // Skip stray flags so a spawned child can't re-trigger a subcommand.
    let mut paths = args.into_iter().skip(1).filter(|p| !p.starts_with(['-', '/']));
    let initial = paths.next().map(std::path::PathBuf::from);
    if let Ok(exe) = std::env::current_exe() {
        for extra in paths {
            let _ = std::process::Command::new(&exe).arg(extra).spawn();
        }
    }
    if let Err(e) = window::run(App::new(), initial) {
        // No console in windows subsystem — surface startup failures visibly.
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
            let text: Vec<u16> =
                format!("{} failed to start:\n{e}\0", edition::NAME).encode_utf16().collect();
            MessageBoxW(
                None,
                windows::core::PCWSTR(text.as_ptr()),
                edition::NAME_W,
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

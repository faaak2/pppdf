//! Self-registration for Windows' "Open with" menu and Default-apps page.
//!
//! `pppdf.exe --register` writes per-user (HKCU) keys so pppdf shows up as a
//! PDF handler; `--unregister` removes them. No admin required. Windows still
//! makes the user pick pppdf as the *default* in its own UI — the `.pdf`
//! UserChoice hash is deliberately not writable by third parties — so this
//! only makes pppdf available and selectable, never silently the default.
//!
//! Registration points at wherever this exe currently lives
//! (`current_exe()`); move the exe and re-run `--register` to update it.

use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW,
    RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, KEY_WRITE, REG_OPTION_NON_VOLATILE,
    REG_SZ, REG_VALUE_TYPE, RRF_RT_REG_SZ,
};
use windows::Win32::UI::Shell::{IShellLinkW, SHChangeNotify, ShellLink, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
};

/// ProgID identifying pppdf as a PDF handler in the registry.
const PROGID: &str = "pppdf.pdf";

/// Where Windows records the user's *chosen* default handler for .pdf.
const USERCHOICE: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.pdf\UserChoice";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Which subcommand (if any) argv[1] names. Flags may be written `--x`, `-x`,
/// or `/x` in any case; anything without a flag sigil is a path (`Cmd::None`),
/// and an unrecognized flag is `Cmd::Unknown` (shows usage).
#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    Register,
    Unregister,
    Help,
    Unknown,
    None,
}

pub fn classify(arg1: Option<&str>) -> Cmd {
    let Some(a) = arg1 else { return Cmd::None };
    let Some(body) = a
        .strip_prefix("--")
        .or_else(|| a.strip_prefix('/'))
        .or_else(|| a.strip_prefix('-'))
    else {
        return Cmd::None; // no flag sigil -> it's a file path
    };
    match body.to_ascii_lowercase().as_str() {
        "register" => Cmd::Register,
        "unregister" => Cmd::Unregister,
        "help" | "h" | "?" => Cmd::Help,
        _ => Cmd::Unknown,
    }
}

/// The registry values that make up the association, as
/// `(subkey under HKCU, value name or None for the key's default, data)`.
/// Kept pure so the association's shape is unit-testable without touching the
/// real registry.
fn assoc_values(exe: &str) -> Vec<(String, Option<&'static str>, String)> {
    let cmd = format!("\"{exe}\" \"%1\"");
    let icon = format!("{exe},0");
    vec![
        // Applications\pppdf.exe — this is what populates "Open with" for .pdf.
        (r"Software\Classes\Applications\pppdf.exe".into(), Some("FriendlyAppName"), "pppdf".into()),
        (r"Software\Classes\Applications\pppdf.exe\SupportedTypes".into(), Some(".pdf"), String::new()),
        (r"Software\Classes\Applications\pppdf.exe\DefaultIcon".into(), None, icon.clone()),
        (r"Software\Classes\Applications\pppdf.exe\shell\open\command".into(), None, cmd.clone()),
        // The ProgID: pppdf's handler identity (display name, icon, open verb).
        (r"Software\Classes\pppdf.pdf".into(), None, "PDF Document".into()),
        (r"Software\Classes\pppdf.pdf\DefaultIcon".into(), None, icon),
        (r"Software\Classes\pppdf.pdf\shell\open\command".into(), None, cmd),
        // Advertise the ProgID on .pdf's Open-with list.
        (r"Software\Classes\.pdf\OpenWithProgIds".into(), Some(PROGID), String::new()),
        // Capabilities — lists pppdf on Settings > Apps > Default apps.
        (r"Software\pppdf\Capabilities".into(), Some("ApplicationName"), "pppdf".into()),
        (
            r"Software\pppdf\Capabilities".into(),
            Some("ApplicationDescription"),
            "Borderless native PDF viewer.".into(),
        ),
        (r"Software\pppdf\Capabilities\FileAssociations".into(), Some(".pdf"), PROGID.into()),
        (r"Software\RegisteredApplications".into(), Some("pppdf"), r"Software\pppdf\Capabilities".into()),
    ]
}

/// Create-or-open `HKCU\subkey` and set one REG_SZ value (`name` = None writes
/// the key's default value).
unsafe fn set_value(subkey: &str, name: Option<&str>, data: &str) -> Result<(), String> {
    let sub = wide(subkey);
    let mut hkey = HKEY::default();
    let rc = RegCreateKeyExW(
        HKEY_CURRENT_USER,
        PCWSTR(sub.as_ptr()),
        None,
        PCWSTR::null(),
        REG_OPTION_NON_VOLATILE,
        KEY_WRITE,
        None,
        &mut hkey,
        None,
    );
    if rc != ERROR_SUCCESS {
        return Err(format!("create {subkey}: error {}", rc.0));
    }
    let name_w = name.map(wide);
    let name_ptr = name_w.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr()));
    let data_w = wide(data);
    // REG_SZ wants the bytes of the wide string INCLUDING the terminating NUL.
    let bytes = std::slice::from_raw_parts(data_w.as_ptr() as *const u8, data_w.len() * 2);
    let rc = RegSetValueExW(hkey, name_ptr, None, REG_SZ, Some(bytes));
    let _ = RegCloseKey(hkey);
    if rc != ERROR_SUCCESS {
        return Err(format!("set {subkey}\\{}: error {}", name.unwrap_or("(default)"), rc.0));
    }
    Ok(())
}

/// Delete `HKCU\subkey` and everything under it (idempotent). Returns whether
/// the delete succeeded.
unsafe fn delete_tree(subkey: &str) -> bool {
    let sub = wide(subkey);
    RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr())) == ERROR_SUCCESS
}

/// Read a REG_SZ value from `HKCU\subkey`, or None if absent/other type.
unsafe fn read_reg_sz(subkey: &str, name: &str) -> Option<String> {
    let sub = wide(subkey);
    let nm = wide(name);
    let mut ty = REG_VALUE_TYPE::default();
    let mut len: u32 = 0;
    // First call sizes the value (bytes, including the terminating NUL).
    let rc = RegGetValueW(
        HKEY_CURRENT_USER,
        PCWSTR(sub.as_ptr()),
        PCWSTR(nm.as_ptr()),
        RRF_RT_REG_SZ,
        Some(&mut ty),
        None,
        Some(&mut len),
    );
    if rc != ERROR_SUCCESS || len == 0 {
        return None;
    }
    let mut buf = vec![0u16; (len as usize).div_ceil(2)];
    let rc = RegGetValueW(
        HKEY_CURRENT_USER,
        PCWSTR(sub.as_ptr()),
        PCWSTR(nm.as_ptr()),
        RRF_RT_REG_SZ,
        None,
        Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
        Some(&mut len),
    );
    if rc != ERROR_SUCCESS {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

/// True if the user's chosen .pdf default is pppdf's ProgID.
unsafe fn userchoice_is_ours() -> bool {
    read_reg_sz(USERCHOICE, "ProgId").as_deref() == Some(PROGID)
}

/// Delete a single value under a shared key (leaving the key and its other
/// values — e.g. other apps' entries — untouched).
unsafe fn delete_value(subkey: &str, name: &str) {
    let sub = wide(subkey);
    let mut hkey = HKEY::default();
    if RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr()), None, KEY_SET_VALUE, &mut hkey)
        == ERROR_SUCCESS
    {
        let n = wide(name);
        let _ = RegDeleteValueW(hkey, PCWSTR(n.as_ptr()));
        let _ = RegCloseKey(hkey);
    }
}

fn shortcut_path() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA").map(|appdata| {
        std::path::PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\pppdf.lnk")
    })
}

/// Best-effort Start-menu shortcut pointing at `exe` (via IShellLinkW).
unsafe fn create_shortcut(exe: &str) -> windows::core::Result<()> {
    let Some(lnk) = shortcut_path() else {
        return Err(windows::core::Error::empty());
    };
    // COM for this thread; a benign error if it was already initialized.
    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
    let exe_w = wide(exe);
    link.SetPath(PCWSTR(exe_w.as_ptr()))?;
    let desc = wide("Borderless native PDF viewer");
    link.SetDescription(PCWSTR(desc.as_ptr()))?;
    link.SetIconLocation(PCWSTR(exe_w.as_ptr()), 0)?;
    let pf: IPersistFile = link.cast()?;
    let lnk_w = wide(&lnk.to_string_lossy());
    pf.Save(PCWSTR(lnk_w.as_ptr()), true)?;
    Ok(())
}

fn remove_shortcut() {
    if let Some(lnk) = shortcut_path() {
        let _ = std::fs::remove_file(lnk);
    }
}

unsafe fn notify_shell() {
    SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
}

fn info(msg: &str) {
    let t = wide(msg);
    unsafe {
        MessageBoxW(None, PCWSTR(t.as_ptr()), crate::edition::NAME_W, MB_OK | MB_ICONINFORMATION);
    }
}

fn fail(msg: &str) -> i32 {
    let t = wide(msg);
    unsafe {
        MessageBoxW(None, PCWSTR(t.as_ptr()), crate::edition::NAME_W, MB_OK | MB_ICONERROR);
    }
    1
}

/// `--register`: write the association keys, drop a Start-menu shortcut, and
/// tell the user how to finish making pppdf the default. Returns a process
/// exit code.
pub fn register() -> i32 {
    let exe = match std::env::current_exe() {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(e) => return fail(&format!("Couldn't determine pppdf's own path:\n{e}")),
    };
    unsafe {
        for (subkey, name, data) in assoc_values(&exe) {
            if let Err(e) = set_value(&subkey, name, &data) {
                return fail(&format!("Registration failed:\n{e}"));
            }
        }
        let shortcut = create_shortcut(&exe);
        notify_shell();
        let mut msg = format!(
            "pppdf is registered as a PDF handler for your user account.\n\n\
             \u{2022} Open a single PDF with it: right-click the .pdf \u{2192} Open with \u{2192} pppdf.\n\
             \u{2022} Make it the default: Settings \u{2192} Apps \u{2192} Default apps, then pick pppdf \
             for .pdf. (Windows requires this last step to be done by you.)\n\n\
             Registered exe:\n{exe}\n\n\
             Moved the exe? Just run \"pppdf --register\" again."
        );
        if shortcut.is_err() {
            msg.push_str("\n\n(A Start Menu shortcut could not be created.)");
        }
        info(&msg);
    }
    0
}

/// `--unregister`: remove everything `register` added. Leaves the exe itself
/// in place. Returns a process exit code.
pub fn unregister() -> i32 {
    let mut tail = "\n\nThe pppdf.exe file was left in place.".to_string();
    unsafe {
        // If pppdf was the *chosen* default, its UserChoice still names the
        // ProgID we're about to delete — leaving that dangling would send
        // every PDF to the "how do you want to open this?" picker. Clear our
        // own dangling default first so Windows reverts to a working one.
        let was_default = userchoice_is_ours();
        let cleared = was_default && delete_tree(USERCHOICE);

        delete_tree(r"Software\Classes\Applications\pppdf.exe");
        delete_tree(r"Software\Classes\pppdf.pdf");
        delete_tree(r"Software\pppdf");
        // Shared keys: remove only pppdf's own value.
        delete_value(r"Software\Classes\.pdf\OpenWithProgIds", PROGID);
        delete_value(r"Software\RegisteredApplications", "pppdf");
        remove_shortcut();
        notify_shell();

        if was_default {
            tail = if cleared {
                "\n\npppdf was your default for PDFs; that default has been cleared. \
                 Pick a new one in Settings \u{2192} Apps \u{2192} Default apps."
            } else {
                "\n\npppdf was your default for PDFs \u{2014} please set a new default in \
                 Settings \u{2192} Apps \u{2192} Default apps."
            }
            .to_string();
        }
    }
    info(&format!("pppdf has been unregistered as a PDF handler.{tail}"));
    0
}

/// `--help` / unknown flag: usage, in a message box (no console in a windows
/// subsystem app).
pub fn show_usage() {
    info(
        "pppdf \u{2014} borderless native PDF viewer\n\n\
         Usage:\n\
         pppdf [file.pdf]      Open a PDF (or start empty)\n\
         pppdf --register      Add pppdf to the Windows \"Open with\" menu and Default apps\n\
         pppdf --unregister    Remove that registration\n\
         pppdf --help          Show this message",
    );
}

#[cfg(test)]
mod tests {
    use super::{assoc_values, classify, Cmd};

    #[test]
    fn classify_recognizes_flag_forms_and_leaves_paths_alone() {
        // Sigils --, /, - all work, case-insensitively.
        for a in ["--register", "/register", "-register", "--REGISTER", "--Register"] {
            assert_eq!(classify(Some(a)), Cmd::Register, "{a}");
        }
        assert_eq!(classify(Some("--unregister")), Cmd::Unregister);
        assert_eq!(classify(Some("/UnRegister")), Cmd::Unregister);
        for a in ["--help", "-h", "/?", "/help", "--HELP"] {
            assert_eq!(classify(Some(a)), Cmd::Help, "{a}");
        }
        // An unrecognized flag is Unknown (→ usage), never a path.
        assert_eq!(classify(Some("--frobnicate")), Cmd::Unknown);
        // Real paths (no flag sigil) and absence are not subcommands.
        assert_eq!(classify(Some(r"C:\books\rules.pdf")), Cmd::None);
        assert_eq!(classify(Some("rules.pdf")), Cmd::None);
        assert_eq!(classify(None), Cmd::None);
    }

    #[test]
    fn assoc_values_wire_open_with_and_default_apps() {
        let exe = r"C:\Program Files\pp pdf\pppdf.exe"; // spaces on purpose
        let v = assoc_values(exe);
        let has = |sub: &str, name: Option<&str>, data: &str| {
            v.iter().any(|(s, n, d)| s == sub && *n == name && d == data)
        };
        let cmd = format!("\"{exe}\" \"%1\"");

        // "Open with" for .pdf: the app declares .pdf support and an open command.
        assert!(has(r"Software\Classes\Applications\pppdf.exe\SupportedTypes", Some(".pdf"), ""));
        assert!(has(r"Software\Classes\Applications\pppdf.exe\shell\open\command", None, &cmd));

        // The ProgID is advertised for .pdf and carries the open command.
        assert!(has(r"Software\Classes\.pdf\OpenWithProgIds", Some("pppdf.pdf"), ""));
        assert!(has(r"Software\Classes\pppdf.pdf\shell\open\command", None, &cmd));

        // Default-apps capability points at the ProgID and is registered.
        assert!(has(r"Software\pppdf\Capabilities\FileAssociations", Some(".pdf"), "pppdf.pdf"));
        assert!(has(r"Software\RegisteredApplications", Some("pppdf"), r"Software\pppdf\Capabilities"));

        // Every open command must quote the exe path (it can contain spaces)
        // and pass the file as %1.
        for (sub, _, data) in &v {
            if sub.ends_with(r"\command") {
                assert!(data.starts_with('"'), "unquoted exe in {sub}: {data}");
                assert!(data.contains("\"%1\""), "missing %1 arg in {sub}: {data}");
            }
        }
    }
}

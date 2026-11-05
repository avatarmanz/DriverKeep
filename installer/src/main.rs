//! DriverKeep's setup program: one .exe holding the app. It installs for everyone on the PC,
//! adds a Start menu shortcut and appears in Windows' list of installed apps.

#![windows_subsystem = "windows"]

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, Interface, PCWSTR, w};

const APP: &str = "DriverKeep";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const PAYLOAD: &[u8] = include_bytes!("../../target/release/driverkeep.exe");
const UNINSTALL_KEY: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\DriverKeep";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const HELPER_TASK: &str = "DriverKeep Background Helper";
const NO_WINDOW: u32 = 0x0800_0000;

fn message(text: &str, kind: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(APP), kind) }
}

fn install_folder() -> PathBuf {
    let base = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
    Path::new(&base).join(APP)
}

fn shortcut_file() -> PathBuf {
    let base = std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".into());
    Path::new(&base).join("Microsoft\\Windows\\Start Menu\\Programs").join(format!("{APP}.lnk"))
}

fn quiet(program: &str, args: &[&str]) {
    let _ = Command::new(program).args(args).creation_flags(NO_WINDOW).status();
}

/// Reopens this setup with administrator rights. Windows shows its permission box.
fn elevate(args: &str) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let result = unsafe {
        ShellExecuteW(
            Some(HWND::default()),
            w!("runas"),
            &HSTRING::from(exe.as_os_str()),
            &HSTRING::from(args),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    result.0 as usize > 32
}

fn set_text(key: HKEY, name: &str, value: &str) {
    let bytes: Vec<u8> = value.encode_utf16().chain(std::iter::once(0)).flat_map(|c| c.to_le_bytes()).collect();
    unsafe {
        let _ = RegSetValueExW(key, &HSTRING::from(name), None, REG_SZ, Some(&bytes));
    }
}

fn set_number(key: HKEY, name: &str, value: u32) {
    unsafe {
        let _ = RegSetValueExW(key, &HSTRING::from(name), None, REG_DWORD, Some(&value.to_le_bytes()));
    }
}

fn register(folder: &Path, exe: &Path, uninstaller: &Path) -> Result<(), String> {
    let mut key = HKEY::default();
    let opened = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(UNINSTALL_KEY),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
    };
    if opened.is_err() {
        return Err("Windows wouldn't add DriverKeep to the installed apps list.".into());
    }
    set_text(key, "DisplayName", APP);
    set_text(key, "DisplayVersion", VERSION);
    set_text(key, "Publisher", "avatarmanz");
    set_text(key, "InstallLocation", &folder.to_string_lossy());
    set_text(key, "DisplayIcon", &exe.to_string_lossy());
    set_text(key, "UninstallString", &format!("\"{}\" --uninstall", uninstaller.display()));
    set_number(key, "NoModify", 1);
    set_number(key, "NoRepair", 1);
    set_number(key, "EstimatedSize", (PAYLOAD.len() / 512) as u32);
    unsafe {
        let _ = RegCloseKey(key);
    }
    Ok(())
}

fn make_shortcut(exe: &Path, file: &Path) -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(exe.as_os_str()))?;
        link.SetDescription(w!("Keep drivers, Windows and software up to date"))?;
        let saver: IPersistFile = link.cast()?;
        saver.Save(&HSTRING::from(file.as_os_str()), true)
    }
}

fn write_payload(folder: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(folder).map_err(|e| format!("Couldn't create the folder: {e}"))?;
    let exe = folder.join("driverkeep.exe");
    std::fs::write(&exe, PAYLOAD).map_err(|e| format!("Couldn't copy the program: {e}"))?;
    Ok(exe)
}

fn install(silent: bool) -> Result<(), String> {
    // A copy already running would hold the old file open.
    quiet("taskkill", &["/F", "/IM", "driverkeep.exe"]);
    std::thread::sleep(std::time::Duration::from_millis(500));

    let folder = install_folder();
    let exe = write_payload(&folder)?;
    let uninstaller = folder.join("uninstall.exe");
    let this = std::env::current_exe().map_err(|e| e.to_string())?;
    if this != uninstaller {
        std::fs::copy(&this, &uninstaller).map_err(|e| format!("Couldn't copy the uninstaller: {e}"))?;
    }
    make_shortcut(&exe, &shortcut_file()).map_err(|e| format!("Couldn't add the Start menu shortcut: {e}"))?;
    register(&folder, &exe, &uninstaller)?;

    if !silent {
        let answer = message("DriverKeep is installed. Open it now?", MB_YESNO | MB_ICONINFORMATION);
        if answer == IDYES {
            // Through Explorer so the app opens as a normal user, not as administrator.
            let _ = Command::new("explorer.exe").arg(&exe).spawn();
        }
    }
    Ok(())
}

fn uninstall(silent: bool) {
    if !silent {
        let question = "Remove DriverKeep from this computer?\n\nYour driver backups and settings are kept.";
        if message(question, MB_YESNO | MB_ICONQUESTION) != IDYES {
            return;
        }
    }
    quiet("taskkill", &["/F", "/IM", "driverkeep.exe"]);
    quiet("schtasks", &["/Delete", "/F", "/TN", HELPER_TASK]);
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(RUN_KEY), None, KEY_SET_VALUE, &mut key).is_ok() {
            let _ = RegDeleteValueW(key, &HSTRING::from(APP));
            let _ = RegCloseKey(key);
        }
        let _ = RegDeleteTreeW(HKEY_LOCAL_MACHINE, &HSTRING::from(UNINSTALL_KEY));
    }
    let _ = std::fs::remove_file(shortcut_file());
    // This file is running, so the folder goes a moment after it exits.
    let folder = install_folder();
    let command = format!("ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"", folder.display());
    let _ = Command::new("cmd").args(["/c", &command]).creation_flags(NO_WINDOW).spawn();
    if !silent {
        message("DriverKeep has been removed.", MB_OK | MB_ICONINFORMATION);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Writes only the program into a folder, for testing a build. Changes nothing else.
    if let Some(at) = args.iter().position(|a| a == "--extract") {
        if let Some(folder) = args.get(at + 1) {
            let _ = write_payload(Path::new(folder));
        }
        return;
    }
    let silent = args.iter().any(|a| a == "--silent");
    let removing = args.iter().any(|a| a == "--uninstall");
    if !unsafe { IsUserAnAdmin().as_bool() } {
        if !elevate(&args.join(" ")) {
            message("DriverKeep needs permission to change this computer.", MB_OK | MB_ICONWARNING);
        }
        return;
    }
    if removing {
        uninstall(silent);
        return;
    }
    if !silent {
        let question = format!("Install DriverKeep {VERSION} on this computer?");
        if message(&question, MB_YESNO | MB_ICONQUESTION) != IDYES {
            return;
        }
    }
    if let Err(error) = install(silent) {
        message(&format!("DriverKeep couldn't be installed.\n\n{error}"), MB_OK | MB_ICONERROR);
    }
}

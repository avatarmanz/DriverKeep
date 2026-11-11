//! DriverKeep's setup program: one .exe holding the app. It installs for everyone on the PC,
//! adds a Start menu shortcut and appears in Windows' list of installed apps.

#![windows_subsystem = "windows"]

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{DEFAULT_GUI_FONT, GetStockObject};
use windows::Win32::System::Com::Urlmon::URLDownloadToFileW;
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
/// Microsoft's Visual C++ files, which other programs on a new PC may also need.
const RUNTIMES: [(&str, &str); 2] = [
    ("x64", "https://aka.ms/vc14/vc_redist.x64.exe"),
    ("x86", "https://aka.ms/vc14/vc_redist.x86.exe"),
];
/// Both places Windows records them, which depends on the version installed.
const RUNTIME_KEYS: [&str; 2] = [
    "SOFTWARE\\Microsoft\\VisualStudio\\14.0\\VC\\Runtimes\\",
    "SOFTWARE\\WOW6432Node\\Microsoft\\VisualStudio\\14.0\\VC\\Runtimes\\",
];
/// Finished, finished after a restart (twice), or a newer one is already there.
const RUNTIME_OK: [i32; 4] = [0, 1641, 3010, 1638];
/// A STATIC window's centred text, across and down.
const CENTERED: u32 = 0x0001 | 0x0200;

/// A small box saying what setup is doing, for the slow steps.
struct Status(isize);

impl Status {
    fn show(text: &str) -> Self {
        let (tell, heard) = std::sync::mpsc::channel();
        let text = HSTRING::from(text);
        std::thread::spawn(move || unsafe {
            let (width, height) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
            let style = WS_POPUP | WS_BORDER | WS_VISIBLE | WINDOW_STYLE(CENTERED);
            let Ok(window) = CreateWindowExW(
                WS_EX_TOPMOST,
                w!("STATIC"),
                &text,
                style,
                (width - 420) / 2,
                (height - 90) / 2,
                420,
                90,
                None,
                None,
                None,
                None,
            ) else {
                let _ = tell.send(0);
                return;
            };
            let font = GetStockObject(DEFAULT_GUI_FONT).0 as usize;
            SendMessageW(window, WM_SETFONT, Some(WPARAM(font)), Some(LPARAM(1)));
            let _ = tell.send(window.0 as isize);
            let mut note = MSG::default();
            while GetMessageW(&mut note, None, 0, 0).as_bool() {
                DispatchMessageW(&note);
            }
        });
        Status(heard.recv().unwrap_or(0))
    }

    fn say(&self, text: &str) {
        unsafe {
            let _ = SetWindowTextW(HWND(self.0 as _), &HSTRING::from(text));
        }
    }
}

impl Drop for Status {
    fn drop(&mut self) {
        unsafe {
            let _ = PostMessageW(Some(HWND(self.0 as _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

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

fn runtime_ready(arch: &str) -> bool {
    RUNTIME_KEYS.iter().any(|base| {
        let mut value = 0u32;
        let mut size = 4u32;
        let read = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                &HSTRING::from(format!("{base}{arch}")),
                w!("Installed"),
                RRF_RT_REG_DWORD,
                None,
                Some(&mut value as *mut u32 as *mut _),
                Some(&mut size),
            )
        };
        read.is_ok() && value == 1
    })
}

/// Setup runs as administrator, so a download only runs if Microsoft really signed it.
fn signed_by_microsoft(file: &Path) -> bool {
    let path = file.display().to_string().replace('\'', "''");
    let check = format!(
        "$s=Get-AuthenticodeSignature -LiteralPath '{path}'; if($s.Status -eq 'Valid' -and $s.SignerCertificate.Subject -like '*O=Microsoft Corporation*'){{exit 0}}else{{exit 1}}"
    );
    Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &check])
        .creation_flags(NO_WINDOW)
        .status()
        .is_ok_and(|status| status.success())
}

/// Keeps a timed record of setup's steps in the temp folder, for when something goes wrong.
fn note(text: &str) {
    use std::io::Write;
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_secs());
    let path = std::env::temp_dir().join("DriverKeep-Setup.log");
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{seconds} {text}");
    }
}

fn add_runtime(arch: &str, url: &str, folder: &Path, status: Option<&Status>) -> Result<(), String> {
    let say = |text: String| {
        note(&text);
        if let Some(status) = status {
            status.say(&text);
        }
    };
    let file = folder.join(format!("vc_redist.{arch}.exe"));
    say(format!("Downloading Microsoft Visual C++ ({arch})..."));
    unsafe { URLDownloadToFileW(None, &HSTRING::from(url), &HSTRING::from(file.as_os_str()), 0, None) }
        .map_err(|e| format!("the {arch} download failed ({e})"))?;
    note("checking signature");
    if !signed_by_microsoft(&file) {
        return Err(format!("the {arch} download wasn't signed by Microsoft"));
    }
    say(format!("Installing Microsoft Visual C++ ({arch}). This can take a few minutes..."));
    let code = Command::new(&file)
        .args(["/install", "/quiet", "/norestart"])
        .creation_flags(NO_WINDOW)
        .status()
        .map_err(|e| format!("the {arch} installer couldn't start ({e})"))?
        .code()
        .unwrap_or(-1);
    note(&format!("{arch} installer finished with code {code}"));
    if RUNTIME_OK.contains(&code) {
        Ok(())
    } else {
        Err(format!("the {arch} installer stopped with code {code}"))
    }
}

/// Adds whichever Visual C++ files are missing. DriverKeep has its own copy built in, so a problem
/// here doesn't stop the install.
fn add_missing_runtimes(status: Option<&Status>) -> Vec<String> {
    let missing: Vec<_> = RUNTIMES.iter().filter(|(arch, _)| !runtime_ready(arch)).collect();
    if missing.is_empty() {
        return Vec::new();
    }
    let folder = std::env::temp_dir().join("DriverKeep-Setup");
    if let Err(error) = std::fs::create_dir_all(&folder) {
        return vec![error.to_string()];
    }
    let problems = missing
        .iter()
        .filter_map(|(arch, url)| add_runtime(arch, url, &folder, status).err())
        .collect();
    let _ = std::fs::remove_dir_all(&folder);
    problems
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

fn install_files() -> Result<PathBuf, String> {
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
    Ok(exe)
}

fn finish(exe: &Path, silent: bool) {
    if !silent {
        let answer = message("DriverKeep is installed. Open it now?", MB_YESNO | MB_ICONINFORMATION);
        if answer == IDYES {
            open_app(exe);
        }
    }
    // After a self-update the window always comes back to the front, whether it was open or
    // only the tray helper was running. The window starts the tray helper again by itself.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--helper") {
        quiet("schtasks", &["/Run", "/TN", HELPER_TASK]);
    }
    if args.iter().any(|a| a == "--reopen" || a == "--helper") {
        open_app(exe);
    }
}

/// Through Explorer so the app opens as a normal user, not as administrator.
fn open_app(exe: &Path) {
    let _ = Command::new("explorer.exe").arg(exe).spawn();
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
    // A self-update runs silently, but the person still gets a box saying what's happening, so
    // DriverKeep closing and coming back doesn't look like a crash.
    let update_run = args.iter().any(|a| a == "--reopen" || a == "--helper");
    let first_words = if update_run {
        format!("Updating DriverKeep to {VERSION}... it will reopen by itself.")
    } else {
        "Checking for Microsoft Visual C++...".to_string()
    };
    let mut status = (!silent || update_run).then(|| Status::show(&first_words));
    let problems = if update_run { Vec::new() } else { add_missing_runtimes(status.as_ref()) };
    if let Some(status) = &status {
        status.say(&if update_run {
            format!("Installing DriverKeep {VERSION}...")
        } else {
            "Installing DriverKeep...".to_string()
        });
    }
    note("installing DriverKeep");
    let installed = install_files();
    note("files done");
    if update_run && installed.is_ok() {
        write_update_marker();
        if let Some(status) = &status {
            status.say("Updated. Reopening DriverKeep...");
        }
    } else {
        status = None;
    }
    if !problems.is_empty() && !silent {
        let text = format!(
            "Microsoft Visual C++ couldn't be added: {}.\n\nDriverKeep doesn't need it, but some other programs might.",
            problems.join(", ")
        );
        message(&text, MB_OK | MB_ICONWARNING);
    }
    match installed {
        Ok(exe) => finish(&exe, silent),
        Err(error) => {
            message(&format!("DriverKeep couldn't be installed.\n\n{error}"), MB_OK | MB_ICONERROR);
        }
    }
    // Left up a moment so the "Reopening" box can be read before the window appears.
    if status.is_some() {
        std::thread::sleep(std::time::Duration::from_millis(1800));
    }
    drop(status);
}

/// Tells the new copy it was just installed over an older one, so it comes to the front.
fn write_update_marker() {
    let Ok(base) = std::env::var("LOCALAPPDATA") else { return };
    let folder = Path::new(&base).join(APP);
    let _ = std::fs::create_dir_all(&folder);
    let _ = std::fs::write(folder.join("just-updated.txt"), VERSION);
}

//! Small Windows helpers: permissions, restore points, startup, saved settings.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, GetLastError, HANDLE};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Restore::*;
use windows::Win32::System::Threading::{CreateMutexW, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{
    IsUserAnAdmin, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

pub const APP_NAME: &str = "DriverKeep";
pub const TASK_NAME: &str = "DriverKeep Background Helper";
pub const AGENT_MUTEX: &str = "Local\\DriverKeepBackgroundHelper";
pub const WINDOW_MUTEX: &str = "Local\\DriverKeepWindow";
const NO_WINDOW: u32 = 0x0800_0000;
/// Windows answers this when System Restore is switched off.
const RESTORE_TURNED_OFF: u32 = 1058;
const RESTORE_KEY: PCWSTR = w!("Software\\Microsoft\\Windows NT\\CurrentVersion\\SystemRestore");
const RESTORE_LIMIT_VALUE: PCWSTR = w!("SystemRestorePointCreationFrequency");
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

pub fn window_title() -> String {
    APP_NAME.to_string()
}

/// True when this copy runs from the place the setup program installs to.
pub fn is_installed() -> bool {
    let base = std::env::var("ProgramFiles").unwrap_or_default();
    exe_path().is_some_and(|exe| exe.starts_with(Path::new(&base).join(APP_NAME)))
}

pub fn is_admin() -> bool {
    unsafe { IsUserAnAdmin().as_bool() }
}

fn exe_path() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

/// Starts this program with administrator rights (Windows shows its permission box).
/// With `wait`, comes back once that copy has finished.
pub fn run_as_admin(args: &str, wait: bool) -> bool {
    let Some(exe) = exe_path() else { return false };
    let exe = HSTRING::from(exe.as_os_str());
    let args = HSTRING::from(args);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(args.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    if unsafe { ShellExecuteExW(&mut info) }.is_err() {
        return false;
    }
    if !info.hProcess.is_invalid() {
        unsafe {
            if wait {
                WaitForSingleObject(info.hProcess, INFINITE);
            }
            let _ = CloseHandle(info.hProcess);
        }
    }
    true
}

/// Runs a Windows tool without showing a black box. Gives back its exit code and text.
pub fn run_quiet(program: &str, args: &[&str]) -> Result<(i32, String), String> {
    let output = Command::new(program)
        .args(args)
        .creation_flags(NO_WINDOW)
        .output()
        .map_err(|e| format!("Could not start {program}: {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    Ok((output.status.code().unwrap_or(-1), text))
}

pub fn open_windows_restore() {
    // System Restore needs administrator rights. Started straight from here it would fail without a
    // word, so it goes through the Windows shell, which shows the permission box first.
    let _ = Command::new("cmd")
        .args(["/c", "start", "", "rstrui.exe"])
        .creation_flags(NO_WINDOW)
        .spawn();
}

/// `seconds` of warning, so it can still be cancelled.
pub fn restart_windows(seconds: u32, message: &str) -> bool {
    let time = seconds.to_string();
    run_quiet("shutdown", &["/r", "/t", &time, "/c", message])
        .map(|(code, _)| code == 0)
        .unwrap_or(false)
}

pub fn cancel_restart() {
    let _ = run_quiet("shutdown", &["/a"]);
}

pub fn open_folder(path: &Path) {
    let _ = Command::new("explorer.exe").arg(path).spawn();
}

/// Stops the PC sleeping while updates run, then lets it sleep again. Same values as the original.
pub fn keep_awake(on: bool) {
    use windows::Win32::System::Power::{EXECUTION_STATE, SetThreadExecutionState};
    let state = if on { 0x8000_0003 } else { 0x8000_0000 };
    unsafe {
        SetThreadExecutionState(EXECUTION_STATE(state));
    }
}

pub fn open_link(link: &str) {
    let _ = Command::new("explorer.exe").arg(link).spawn();
}

/// Whether the Microsoft Store app is on this PC. Windows Sandbox and some work PCs don't have it.
pub fn store_installed() -> bool {
    let check = "if (Get-AppxPackage -Name Microsoft.WindowsStore) { exit 0 } else { exit 1 }";
    run_quiet("powershell", &["-NoProfile", "-NonInteractive", "-Command", check])
        .map(|(code, _)| code == 0)
        .unwrap_or(false)
}

/// Opens a Microsoft Store page, or the same page on the web on a PC without the Store. Explorer
/// would open the Documents folder for a Store link nothing can handle, so the Store is checked
/// first. Gives back the link that was opened.
pub fn open_store_page(store: &'static str, web: &'static str) -> &'static str {
    let link = if store_installed() { store } else { web };
    open_link(link);
    link
}

pub fn show_window() {
    if let Some(exe) = exe_path() {
        let _ = Command::new(exe).spawn();
    }
}

/// Gives back a handle that keeps the claim, or nothing if another copy holds this name.
pub fn claim_single_instance(name: &str) -> Option<HANDLE> {
    let name = HSTRING::from(name);
    let handle = unsafe { CreateMutexW(None, true, &name) }.ok()?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return None;
    }
    Some(handle)
}

pub fn agent_running() -> bool {
    match claim_single_instance(AGENT_MUTEX) {
        Some(handle) => {
            unsafe {
                let _ = CloseHandle(handle);
            }
            false
        }
        None => true,
    }
}

pub fn start_agent() {
    if agent_running() {
        return;
    }
    if task_exists() {
        let _ = run_quiet("schtasks", &["/Run", "/TN", TASK_NAME]);
        // The saved task can point at a copy of the program that has since moved or been removed,
        // in which case it starts nothing. Give it a moment, then start the helper directly.
        for _ in 0..8 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            if agent_running() {
                return;
            }
        }
    }
    if let Some(exe) = exe_path() {
        let _ = Command::new(exe).arg("--background").creation_flags(NO_WINDOW).spawn();
    }
}

fn set_run_value(value: Option<&str>) {
    let mut key = HKEY::default();
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_SET_VALUE, &mut key) } != ERROR_SUCCESS {
        return;
    }
    let name = HSTRING::from(APP_NAME);
    unsafe {
        match value {
            Some(text) => {
                let wide: Vec<u8> = text
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .flat_map(|c| c.to_le_bytes())
                    .collect();
                let _ = RegSetValueExW(key, &name, None, REG_SZ, Some(&wide));
            }
            None => {
                let _ = RegDeleteValueW(key, &name);
            }
        }
        let _ = RegCloseKey(key);
    }
}

pub fn task_exists() -> bool {
    run_quiet("schtasks", &["/Query", "/TN", TASK_NAME])
        .map(|(code, _)| code == 0)
        .unwrap_or(false)
}

pub fn task_xml(exe: &str, arguments: &str, user: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Runs the DriverKeep background helper when you sign in.</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{exe}</Command><Arguments>{arguments}</Arguments></Exec></Actions>
</Task>"#
    )
}

/// Run by the elevated copy of this program. The task lets the helper start with
/// administrator rights at sign-in, which is what makes silent installs possible.
pub fn set_agent_task(on: bool) -> bool {
    if !on {
        return run_quiet("schtasks", &["/Delete", "/F", "/TN", TASK_NAME])
            .map(|(code, _)| code == 0)
            .unwrap_or(false);
    }
    let Some(exe) = exe_path() else { return false };
    let user = std::env::var("USERNAME").unwrap_or_default();
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    let xml = task_xml(&exe.to_string_lossy(), "--background", &format!("{domain}\\{user}"));
    let file = std::env::temp_dir().join("driverkeep-task.xml");
    let mut bytes: Vec<u8> = vec![0xFF, 0xFE];
    bytes.extend(xml.encode_utf16().flat_map(|c| c.to_le_bytes()));
    if std::fs::write(&file, bytes).is_err() {
        return false;
    }
    let done = run_quiet(
        "schtasks",
        &["/Create", "/F", "/TN", TASK_NAME, "/XML", &file.to_string_lossy()],
    )
    .map(|(code, _)| code == 0)
    .unwrap_or(false);
    let _ = std::fs::remove_file(file);
    done
}

/// Makes Windows' startup list match the settings.
pub fn apply_startup(settings: &Settings) {
    let task = task_exists();
    if settings.run_on_startup && !task {
        if let Some(exe) = exe_path() {
            let open = if settings.start_hidden { "" } else { " --open" };
            let command = format!("\"{}\" --background{open}", exe.display());
            set_run_value(Some(&command));
        }
    } else {
        set_run_value(None);
    }
    if settings.wants_agent() {
        start_agent();
    }
}

#[derive(Debug, PartialEq)]
pub enum RestorePoint {
    Created(i64),
    TurnedOff,
    Failed(u32),
}

fn read_limit() -> Option<u32> {
    let mut key = HKEY::default();
    let opened = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, RESTORE_KEY, None, KEY_READ, &mut key) };
    if opened != ERROR_SUCCESS {
        return None;
    }
    let mut value = 0u32;
    let mut size = 4u32;
    let read = unsafe {
        RegQueryValueExW(
            key,
            RESTORE_LIMIT_VALUE,
            None,
            None,
            Some(&mut value as *mut u32 as *mut u8),
            Some(&mut size),
        )
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    (read == ERROR_SUCCESS).then_some(value)
}

/// Windows normally skips a restore point if another was made in the last day. The original
/// removes that limit for good; here it is lifted only for this one restore point.
fn set_limit(value: Option<u32>) {
    let mut key = HKEY::default();
    let opened = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            RESTORE_KEY,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
    };
    if opened != ERROR_SUCCESS {
        return;
    }
    unsafe {
        match value {
            Some(v) => {
                let _ = RegSetValueExW(key, RESTORE_LIMIT_VALUE, None, REG_DWORD, Some(&v.to_le_bytes()));
            }
            None => {
                let _ = RegDeleteValueW(key, RESTORE_LIMIT_VALUE);
            }
        }
        let _ = RegCloseKey(key);
    }
}

pub fn begin_restore_point(description: &str) -> RestorePoint {
    let mut info = RESTOREPOINTINFOW {
        dwEventType: BEGIN_SYSTEM_CHANGE,
        dwRestorePtType: DEVICE_DRIVER_INSTALL,
        ..Default::default()
    };
    let text: Vec<u16> = format!("{APP_NAME}: {description}").encode_utf16().take(255).collect();
    let mut description = [0u16; 256];
    description[..text.len()].copy_from_slice(&text);
    info.szDescription = description;
    let mut status = STATEMGRSTATUS::default();

    let earlier = read_limit();
    set_limit(Some(0));
    let done = unsafe { SRSetRestorePointW(&info, &mut status) };
    set_limit(earlier);

    let (code, sequence) = (status.nStatus, status.llSequenceNumber);
    if done.as_bool() {
        RestorePoint::Created(sequence)
    } else if code.0 == RESTORE_TURNED_OFF {
        RestorePoint::TurnedOff
    } else {
        RestorePoint::Failed(code.0)
    }
}

pub fn end_restore_point(sequence: i64) {
    let info = RESTOREPOINTINFOW {
        dwEventType: END_SYSTEM_CHANGE,
        llSequenceNumber: sequence,
        ..Default::default()
    };
    let mut status = STATEMGRSTATUS::default();
    unsafe {
        let _ = SRSetRestorePointW(&info, &mut status);
    }
}

/// The accent colour chosen in Windows' own Settings, when it can be read.
pub fn windows_accent() -> Option<[u8; 3]> {
    // Stored as 0xAABBGGRR.
    let value = user_registry_number("Software\\Microsoft\\Windows\\DWM", "AccentColor")?;
    Some([(value & 0xFF) as u8, ((value >> 8) & 0xFF) as u8, ((value >> 16) & 0xFF) as u8])
}

/// A number under the current user's registry key, such as an Internet Settings switch.
pub fn user_registry_number(key_path: &str, value: &str) -> Option<u32> {
    let mut key = HKEY::default();
    let path = HSTRING::from(key_path);
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &path, None, KEY_READ, &mut key) } != ERROR_SUCCESS {
        return None;
    }
    let name = HSTRING::from(value);
    let mut number = 0u32;
    let mut size = 4u32;
    let read = unsafe {
        RegQueryValueExW(key, &name, None, None, Some(&mut number as *mut u32 as *mut u8), Some(&mut size))
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    (read == ERROR_SUCCESS).then_some(number)
}

/// Text under the current user's registry key. Empty when it isn't there.
pub fn user_registry_text(key_path: &str, value: &str) -> String {
    let mut key = HKEY::default();
    let path = HSTRING::from(key_path);
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &path, None, KEY_READ, &mut key) } != ERROR_SUCCESS {
        return String::new();
    }
    let name = HSTRING::from(value);
    let mut buffer = [0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    let read = unsafe {
        RegQueryValueExW(key, &name, None, None, Some(buffer.as_mut_ptr() as *mut u8), Some(&mut size))
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    if read != ERROR_SUCCESS {
        return String::new();
    }
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

pub fn registry_text(key_path: &str, value: &str) -> String {
    let mut key = HKEY::default();
    let path = HSTRING::from(key_path);
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, &path, None, KEY_READ, &mut key) } != ERROR_SUCCESS {
        return String::new();
    }
    let name = HSTRING::from(value);
    let mut buffer = [0u16; 512];
    let mut size = (buffer.len() * 2) as u32;
    let read = unsafe {
        RegQueryValueExW(key, &name, None, None, Some(buffer.as_mut_ptr() as *mut u8), Some(&mut size))
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    if read != ERROR_SUCCESS {
        return String::new();
    }
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// Windows' build number, such as 26100.
pub fn windows_build() -> u32 {
    registry_text("Software\\Microsoft\\Windows NT\\CurrentVersion", "CurrentBuildNumber")
        .parse()
        .unwrap_or(0)
}

pub fn is_arm_pc() -> bool {
    std::env::var("PROCESSOR_ARCHITECTURE")
        .map(|a| a.eq_ignore_ascii_case("ARM64"))
        .unwrap_or(false)
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Theme {
    System,
    Light,
    Dark,
    /// Dark with true black, for OLED screens.
    Oled,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Frequency {
    Never,
    Daily,
    Weekly,
    Monthly,
}

/// What closing the window does. The original's three choices, asking by default.
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum CloseChoice {
    Ask,
    Exit,
    Tray,
}

/// Day of the month for a monthly scan: today, or the 1st if today is after the 27th, so the
/// day exists in every month. Same formula as the original.
pub fn monthly_day(today: u32) -> u32 {
    if today <= 27 { today } else { 1 }
}

/// The original compares its daily restore point in UTC days.
pub fn restore_point_day() -> String {
    chrono::Utc::now().format("%Y%m%d").to_string()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    pub accent: [u8; 3],
    pub follow_windows_accent: bool,
    pub run_on_startup: bool,
    pub start_hidden: bool,
    pub close_choice: CloseChoice,
    pub scan_frequency: Frequency,
    pub scan_weekday: u32,
    pub scan_month_day: u32,
    pub scan_hour: u32,
    pub scan_minute: u32,
    pub backup_select_all: bool,
    pub update_all_remind: bool,
    pub tools_offered: bool,
    pub install_without_backup: bool,
    pub auto_update_app: bool,
    pub scan_drivers: bool,
    pub scan_windows: bool,
    pub scan_software: bool,
    pub scan_optional: bool,
    pub use_winget: bool,
    pub use_chocolatey: bool,
    pub pick_drivers: bool,
    pub pick_windows: bool,
    pub pick_software: bool,
    pub silent_drivers: bool,
    pub silent_windows: bool,
    pub silent_software: bool,
    pub scan_at_startup: bool,
    /// Search as soon as the window opens. Off, it opens to the Scan button.
    pub scan_on_open: bool,
    pub notify: bool,
    pub silent_install: bool,
    pub install_from: u32,
    pub install_to: u32,
    pub silent_catalog: bool,
    pub auto_restart: bool,
    pub restart_only_in_hours: bool,
    pub use_catalog: bool,
    pub make_restore_point: bool,
    pub backup_before_update: bool,
    pub backup_folder: PathBuf,
    /// Backups older than this many days are deleted. 0 keeps them all. New and existing users start at a year.
    pub backup_keep_days: u32,
    pub ignored: Vec<String>,
    pub last_scan: Option<i64>,
    pub last_restore_point_day: String,
    pub last_silent_day: String,
}

impl Default for Settings {
    fn default() -> Self {
        use chrono::{Datelike, Timelike};
        let now = chrono::Local::now();
        Self {
            theme: Theme::System,
            accent: [0x2A, 0xAE, 0x9B],
            follow_windows_accent: false,
            run_on_startup: false,
            start_hidden: true,
            close_choice: CloseChoice::Ask,
            scan_frequency: Frequency::Monthly,
            scan_weekday: 0,
            scan_month_day: monthly_day(now.day()),
            scan_hour: now.hour(),
            scan_minute: now.minute(),
            backup_select_all: true,
            update_all_remind: true,
            tools_offered: false,
            install_without_backup: false,
            auto_update_app: false,
            scan_drivers: true,
            scan_windows: true,
            scan_software: true,
            scan_optional: false,
            use_winget: true,
            use_chocolatey: true,
            pick_drivers: true,
            pick_windows: true,
            pick_software: true,
            silent_drivers: true,
            silent_windows: false,
            silent_software: false,
            scan_at_startup: false,
            scan_on_open: false,
            notify: true,
            silent_install: false,
            install_from: 1,
            install_to: 5,
            silent_catalog: false,
            auto_restart: false,
            restart_only_in_hours: true,
            use_catalog: true,
            make_restore_point: true,
            backup_before_update: true,
            backup_folder: default_backup_folder(),
            backup_keep_days: 365,
            ignored: Vec::new(),
            last_scan: None,
            last_restore_point_day: String::new(),
            last_silent_day: String::new(),
        }
    }
}

/// Documents\<app name>\MyDrivers, the original's layout.
fn default_backup_folder() -> PathBuf {
    let documents = dirs::document_dir().unwrap_or_else(|| PathBuf::from("."));
    documents.join(APP_NAME).join("MyDrivers")
}

pub fn data_dir() -> Option<PathBuf> {
    Some(dirs::data_local_dir()?.join(APP_NAME))
}

/// Setup leaves a note here after replacing DriverKeep with a newer version. Reading it removes it,
/// so the first window opened afterwards knows to come to the front and say it was updated.
pub fn take_update_marker() -> Option<String> {
    let file = data_dir()?.join("just-updated.txt");
    let text = std::fs::read_to_string(&file).ok()?;
    let _ = std::fs::remove_file(&file);
    Some(text.trim().to_string())
}

pub fn read_cache<T: DeserializeOwned>(file: &str) -> Option<T> {
    let text = std::fs::read_to_string(data_dir()?.join(file)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn write_cache<T: Serialize>(file: &str, value: &T) {
    let Some(folder) = data_dir() else { return };
    let _ = std::fs::create_dir_all(&folder);
    if let Ok(text) = serde_json::to_string_pretty(value) {
        let temporary = folder.join(format!("{file}.tmp"));
        if std::fs::write(&temporary, text).is_ok() {
            let _ = std::fs::rename(temporary, folder.join(file));
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let mut settings: Settings = read_cache("settings.json").unwrap_or_default();
        // Earlier builds used another default folder; move on from it if it was never used.
        if settings.backup_folder.ends_with("DriverKeep Backups") && !settings.backup_folder.exists() {
            settings.backup_folder = default_backup_folder();
        }
        settings
    }

    /// True when a scheduled scan's moment has passed since the last scan.
    pub fn scan_overdue(&self, now: chrono::DateTime<chrono::Local>) -> bool {
        last_due(self.scan_frequency, self.scan_weekday, self.scan_month_day, self.scan_hour, self.scan_minute, now)
            .is_some_and(|due| self.last_scan.is_none_or(|last| last < due))
    }

    pub fn save(&self) {
        write_cache("settings.json", self);
    }

    /// Anything here needs the small background helper to be running. A schedule alone doesn't:
    /// without the helper, an overdue scan simply runs the next time the app is opened.
    pub fn wants_agent(&self) -> bool {
        self.run_on_startup || self.close_choice == CloseChoice::Tray || self.silent_install
    }
}

/// True when the hour is inside the chosen window, which may cross midnight.
/// The same start and end means "any time".
pub fn in_hours(hour: u32, from: u32, to: u32) -> bool {
    if from == to {
        true
    } else if from < to {
        (from..to).contains(&hour)
    } else {
        hour >= from || hour < to
    }
}

/// The most recent moment a scheduled scan should have happened, as a Unix time.
pub fn last_due(
    frequency: Frequency,
    weekday: u32,
    month_day: u32,
    hour: u32,
    minute: u32,
    now: chrono::DateTime<chrono::Local>,
) -> Option<i64> {
    use chrono::{Datelike, Duration, TimeZone, Timelike};
    let at_hour = |date: chrono::NaiveDate| -> Option<i64> {
        let moment = date.and_hms_opt(hour.min(23), minute.min(59), 0)?;
        Some(chrono::Local.from_local_datetime(&moment).earliest()?.timestamp())
    };
    let today = now.date_naive();
    let passed = (now.hour(), now.minute()) >= (hour, minute);
    match frequency {
        Frequency::Never => None,
        Frequency::Daily => at_hour(if passed { today } else { today - Duration::days(1) }),
        Frequency::Weekly => {
            let current = now.weekday().num_days_from_monday();
            let mut back = (current + 7 - weekday.min(6)) % 7;
            if back == 0 && !passed {
                back = 7;
            }
            at_hour(today - Duration::days(back as i64))
        }
        Frequency::Monthly => {
            let day = month_day.clamp(1, 27);
            let this_month = today.with_day(day)?;
            let date = if today > this_month || (today == this_month && passed) {
                this_month
            } else {
                (today.with_day(1)? - Duration::days(1)).with_day(day)?
            };
            at_hour(date)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn hours_cross_midnight() {
        assert!(in_hours(23, 22, 5));
        assert!(in_hours(2, 22, 5));
        assert!(!in_hours(12, 22, 5));
        assert!(in_hours(3, 1, 5));
        assert!(!in_hours(5, 1, 5));
        assert!(in_hours(9, 4, 4));
    }

    #[test]
    fn schedule_catches_up_after_being_off() {
        // Wednesday 5 Nov 2025, 10:00.
        let now = chrono::Local.with_ymd_and_hms(2025, 11, 5, 10, 0, 0).unwrap();
        let at3 = chrono::Local.with_ymd_and_hms(2025, 11, 5, 3, 0, 0).unwrap().timestamp();
        assert_eq!(last_due(Frequency::Daily, 0, 1, 3, 0, now), Some(at3));
        let monday = chrono::Local.with_ymd_and_hms(2025, 11, 3, 3, 0, 0).unwrap().timestamp();
        assert_eq!(last_due(Frequency::Weekly, 0, 1, 3, 0, now), Some(monday));
        let first = chrono::Local.with_ymd_and_hms(2025, 11, 1, 3, 0, 0).unwrap().timestamp();
        assert_eq!(last_due(Frequency::Monthly, 0, 1, 3, 0, now), Some(first));
        assert_eq!(last_due(Frequency::Never, 0, 1, 3, 0, now), None);
        // The 20th hasn't come yet, so the last due moment is 20 October at 09:30.
        let october = chrono::Local.with_ymd_and_hms(2025, 10, 20, 9, 30, 0).unwrap().timestamp();
        assert_eq!(last_due(Frequency::Monthly, 0, 20, 9, 30, now), Some(october));
    }

    #[test]
    fn monthly_day_always_exists() {
        assert_eq!(monthly_day(27), 27);
        assert_eq!(monthly_day(28), 1);
        assert_eq!(monthly_day(31), 1);
    }

    #[test]
    fn helper_task_has_no_time_limit() {
        let xml = task_xml("C:\\a.exe", "--background", "PC\\me");
        assert!(xml.contains("PT0S"));
        assert!(xml.contains("HighestAvailable"));
        assert!(xml.contains("<DisallowStartIfOnBatteries>false"));
    }
}

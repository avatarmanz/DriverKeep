//! Windows System Restore: how many restore points there are, how much disk space they may use,
//! and making a new one. Windows only lets administrators read or change any of this, so the work
//! is done by an administrator copy of DriverKeep that the window starts when it's needed (or by the
//! window itself when it's already running as administrator). The two talk through small files in
//! DriverKeep's data folder, and the request can only name one of a few fixed actions.

use crate::system::{self, RestorePoint};
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

const REQUEST_FILE: &str = "restore-request.json";
const RESULT_FILE: &str = "restore-result.json";
const NO_WINDOW: u32 = 0x0800_0000;
const GB: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Action {
    /// Look at the restore points and the space they use.
    Read,
    /// Make a restore point now.
    Create,
    /// Set the most space, in gigabytes, restore points may use.
    SetLimit(u32),
    /// Turn System Restore on for the Windows drive.
    TurnOn,
    /// Delete one restore point, by its sequence number.
    Delete(u32),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub action: Action,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub number: u32,
    pub description: String,
    pub kind: u32,
    /// Unix time.
    pub created: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Info {
    pub drive: String,
    pub drive_size: u64,
    pub used: u64,
    /// The most space restore points may use. 0 means no limit has been set.
    pub limit: u64,
    pub on: bool,
    pub points: Vec<Point>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Outcome {
    pub request: u64,
    pub ok: bool,
    pub message: String,
    pub info: Option<Info>,
}

impl Info {
    /// The limit in whole gigabytes, for the text box.
    pub fn limit_gb(&self) -> u64 {
        self.limit / GB
    }

    /// What Windows calls a restore point of this type.
    pub fn kind_label(kind: u32) -> &'static str {
        match kind {
            0 => "App installed",
            1 => "App removed",
            10 => "Driver installed",
            12 => "Settings changed",
            13 => "Cancelled change",
            _ => "Restore point",
        }
    }
}

fn file(name: &str) -> Option<PathBuf> {
    let folder = system::data_dir()?;
    std::fs::create_dir_all(&folder).ok()?;
    Some(folder.join(name))
}

/// Written in one piece, so the other side never reads half a file.
fn write_whole(name: &str, text: &str) -> bool {
    let Some(path) = file(name) else { return false };
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, text).is_ok() && std::fs::rename(&temp, &path).is_ok()
}

fn system_drive() -> String {
    std::env::var("SystemDrive").ok().filter(|d| d.len() == 2).unwrap_or_else(|| "C:".to_string())
}

fn powershell(script: &str) -> Result<String, String> {
    // Sent encoded, so quotes and line breaks in the script can't be mangled on the way in.
    use base64::Engine as _;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &encoded])
        .creation_flags(NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if why.is_empty() { "Windows PowerShell reported an error.".into() } else { why })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawPoint {
    n: u32,
    d: String,
    t: u32,
    c: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Raw {
    total: String,
    used: String,
    limit: String,
    on: bool,
    points: Vec<RawPoint>,
}

/// Turns what the PowerShell script printed into an `Info`.
pub fn parse_info(json: &str, drive: &str) -> Result<Info, String> {
    let raw: Raw = serde_json::from_str(json.trim()).map_err(|e| format!("Couldn't read the restore point list ({e})."))?;
    let number = |text: &str| text.trim().parse::<u64>().unwrap_or(0);
    let drive_size = number(&raw.total);
    let limit = number(&raw.limit);
    let mut points: Vec<Point> = raw
        .points
        .into_iter()
        .map(|p| Point {
            number: p.n,
            description: p.d,
            kind: p.t,
            created: chrono::DateTime::parse_from_rfc3339(&p.c).map(|t| t.timestamp()).unwrap_or(0),
        })
        .collect();
    points.sort_by_key(|p| std::cmp::Reverse(p.created));
    Ok(Info {
        drive: drive.to_string(),
        drive_size,
        used: number(&raw.used),
        // Windows reports "no limit" as an enormous number.
        limit: if limit == 0 || limit >= drive_size { 0 } else { limit },
        on: raw.on,
        points,
    })
}

pub fn read() -> Result<Info, String> {
    let drive = system_drive();
    let script = format!(
        r#"$ErrorActionPreference = 'Stop'
$drive = '{drive}'
$vol = Get-CimInstance Win32_Volume -Filter "DriveLetter='$drive'"
$points = @(Get-ComputerRestorePoint -ErrorAction SilentlyContinue | ForEach-Object {{
  [pscustomobject]@{{ n = [int]$_.SequenceNumber; d = [string]$_.Description; t = [int]$_.RestorePointType;
    c = ([Management.ManagementDateTimeConverter]::ToDateTime($_.CreationTime)).ToUniversalTime().ToString('o') }} }})
$store = Get-CimInstance Win32_ShadowStorage | Where-Object {{ $_.Volume.DeviceID -eq $vol.DeviceID }} | Select-Object -First 1
[pscustomobject]@{{ total = [string]$vol.Capacity; used = $(if ($store) {{ [string]$store.UsedSpace }} else {{ '0' }});
  limit = $(if ($store) {{ [string]$store.MaxSpace }} else {{ '0' }}); on = [bool]$store; points = $points }} | ConvertTo-Json -Depth 4 -Compress"#
    );
    parse_info(&powershell(&script)?, &drive)
}

/// Does one action as an administrator and gives back what happened, with the fresh state.
pub fn perform(action: &Action) -> (bool, String) {
    let drive = system_drive();
    match action {
        Action::Read => (true, String::new()),
        Action::Create => match system::begin_restore_point("restore point made by hand") {
            RestorePoint::Created(number) => {
                system::end_restore_point(number);
                (true, "Restore point created.".into())
            }
            RestorePoint::TurnedOff => (false, format!("System Restore is turned off for {drive}. Turn it on first.")),
            RestorePoint::Failed(code) => (false, format!("Windows couldn't make a restore point (code {code}).")),
        },
        Action::TurnOn => match powershell(&format!("Enable-ComputerRestore -Drive '{drive}\\'")) {
            Ok(_) => (true, format!("System Restore is on for {drive}.")),
            Err(why) => (false, format!("Couldn't turn System Restore on: {why}")),
        },
        Action::Delete(number) => {
            // Windows' own call for removing a single restore point.
            let script = "Add-Type -Namespace DK -Name Sr -MemberDefinition '[DllImport(\"srclient.dll\")] public static extern int SRRemoveRestorePoint(int index);'\n[DK.Sr]::SRRemoveRestorePoint(NUMBER)"
                .replace("NUMBER", &number.to_string());
            match powershell(&script) {
                Ok(out) => match out.trim().parse::<i64>() {
                    Ok(0) => (true, "Restore point deleted.".into()),
                    Ok(code) => (false, format!("Windows couldn't delete that restore point (code {code}).")),
                    Err(_) => (false, "Windows gave an unexpected answer, so the restore point may still be there.".into()),
                },
                Err(why) => (false, format!("Couldn't delete the restore point: {why}")),
            }
        }
        Action::SetLimit(gb) => {
            let gb = (*gb).clamp(1, 1_000_000);
            // Harmless when it's already on; needed when it was off.
            let _ = powershell(&format!("Enable-ComputerRestore -Drive '{drive}\\'"));
            let run = Command::new("vssadmin")
                .args([
                    "resize".to_string(),
                    "shadowstorage".to_string(),
                    format!("/for={drive}"),
                    format!("/on={drive}"),
                    format!("/maxsize={gb}GB"),
                ])
                .creation_flags(NO_WINDOW)
                .output();
            match run {
                Ok(out) if out.status.success() => (true, format!("Applied. System Restore may now use up to {gb} GB.")),
                Ok(out) => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    let line = text.lines().rev().find(|l| l.trim_start().starts_with("Error")).unwrap_or("").trim();
                    (false, format!("Windows didn't accept that size. {line}").trim().to_string())
                }
                Err(e) => (false, format!("Couldn't run the storage tool: {e}")),
            }
        }
    }
}

/// Runs in the administrator copy (or in a thread when the window is already an administrator):
/// takes the waiting request, does it, and leaves the answer for the window.
pub fn run_job() {
    let started = std::time::Instant::now();
    let request = loop {
        if let Some(request) = take_request() {
            break request;
        }
        if started.elapsed().as_secs() > 20 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    let (ok, mut message) = perform(&request.action);
    let info = match read() {
        Ok(info) => Some(info),
        Err(why) => {
            if message.is_empty() {
                message = why;
            }
            None
        }
    };
    let outcome = Outcome { request: request.id, ok, message, info };
    if let Ok(text) = serde_json::to_string(&outcome) {
        write_whole(RESULT_FILE, &text);
    }
}

fn take_request() -> Option<Request> {
    let path = file(REQUEST_FILE)?;
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    serde_json::from_str(&text).ok()
}

/// Window side: asks for an action. As an administrator it just runs; otherwise Windows' permission
/// box appears. Gives the request's number, or None when permission wasn't given.
pub fn start(action: Action) -> Option<u64> {
    let id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |t| t.as_nanos() as u64);
    if let Some(old) = file(RESULT_FILE) {
        let _ = std::fs::remove_file(old);
    }
    let text = serde_json::to_string(&Request { id, action }).ok()?;
    if !write_whole(REQUEST_FILE, &text) {
        return None;
    }
    if system::is_admin() {
        std::thread::spawn(run_job);
        Some(id)
    } else if system::run_as_admin("--restore-job", false) {
        Some(id)
    } else {
        if let Some(path) = file(REQUEST_FILE) {
            let _ = std::fs::remove_file(path);
        }
        None
    }
}

/// Window side: the answer to this request, once there is one.
pub fn outcome(id: u64) -> Option<Outcome> {
    let text = std::fs::read_to_string(file(RESULT_FILE)?).ok()?;
    let outcome: Outcome = serde_json::from_str(&text).ok()?;
    (outcome.request == id).then_some(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_script_output() {
        let json = r#"{"total":"512110190592","used":"5368709120","limit":"21474836480","on":true,
            "points":[{"n":3,"d":"Old one","t":0,"c":"2026-11-01T10:00:00.0000000Z"},
                      {"n":4,"d":"DriverKeep: Before installing driver updates","t":10,"c":"2026-11-10T09:30:00.0000000Z"}]}"#;
        let info = parse_info(json, "C:").unwrap();
        assert!(info.on);
        assert_eq!(info.limit_gb(), 20);
        assert_eq!(info.used, 5 * GB);
        assert_eq!(info.points.len(), 2);
        // Newest first.
        assert_eq!(info.points[0].number, 4);
        assert_eq!(Info::kind_label(info.points[0].kind), "Driver installed");
    }

    #[test]
    fn no_limit_and_no_points() {
        let json = r#"{"total":"100000000000","used":"0","limit":"18446744073709551615","on":false,"points":[]}"#;
        let info = parse_info(json, "C:").unwrap();
        assert_eq!(info.limit, 0);
        assert!(!info.on);
        assert!(info.points.is_empty());
    }

    #[test]
    fn rubbish_is_an_error() {
        assert!(parse_info("not json", "C:").is_err());
    }
}

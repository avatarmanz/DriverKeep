//! Updates for other installed programs, using the Windows Package Manager (winget) that
//! comes with Windows. Only its own trusted source is used, and it does the installing.

use crate::system;
use crate::wu::{Kind, Source, Update};
use std::path::Path;

/// What the package manager says when an update needs a restart to finish.
const REBOOT_TO_FINISH: u32 = 0x8A15_0109;
pub const ID_PREFIX: &str = "winget:";
/// App Installer is the Windows Package Manager itself. Updating it takes the tool away for a
/// moment, so it goes last in any batch.
pub const PACKAGE_MANAGER_ID: &str = "winget:Microsoft.AppInstaller";
pub const CHOCO_PREFIX: &str = "choco:";
/// The Microsoft Store page for App Installer, which provides the Windows Package Manager.
pub const WINGET_STORE_LINK: &str = "ms-windows-store://pdp/?productid=9NBLGGH4NNS1";
pub const WINGET_WEB_LINK: &str = "https://apps.microsoft.com/detail/9NBLGGH4NNS1";
/// Microsoft's own steps for a PC without the Store. TLS 1.2 is added for older Windows settings.
const INSTALL_WINGET: &str = "$ProgressPreference='SilentlyContinue'; \
    [Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; \
    Install-PackageProvider -Name NuGet -Force | Out-Null; \
    Install-Module -Name Microsoft.WinGet.Client -Force -Repository PSGallery -Scope AllUsers | Out-Null; \
    Repair-WinGetPackageManager -AllUsers; \
    Add-AppxPackage -RegisterByFamilyName -MainPackage Microsoft.DesktopAppInstaller_8wekyb3d8bbwe";
/// Chocolatey's published install command.
const INSTALL_CHOCOLATEY: &str = "$ProgressPreference='SilentlyContinue'; \
    Set-ExecutionPolicy Bypass -Scope Process -Force; \
    [Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; \
    Invoke-Expression ((New-Object System.Net.WebClient).DownloadString('https://community.chocolatey.org/install.ps1'))";
const TOOLS_LOG: &str = "tools-install.log";

#[derive(Debug, PartialEq)]
pub struct Package {
    pub name: String,
    pub id: String,
    pub version: String,
    pub available: String,
}

/// Reads the table `winget upgrade` prints. Column names change with the language, so the
/// dashed line finds the table and the heading above it gives each column's position.
pub fn parse_table(text: &str) -> Vec<Package> {
    let lines: Vec<Vec<char>> = text
        .split(['\r', '\n'])
        .map(|line| line.chars().collect())
        .collect();
    let Some(rule) = lines
        .iter()
        .position(|l| l.len() > 20 && l.iter().all(|c| *c == '-'))
    else {
        return Vec::new();
    };
    let Some(heading) = lines[..rule].iter().rev().find(|l| !l.is_empty()) else {
        return Vec::new();
    };
    let mut starts = vec![0];
    for index in 1..heading.len() {
        if heading[index] != ' ' && heading[index - 1] == ' ' {
            starts.push(index);
        }
    }
    if starts.len() < 4 {
        return Vec::new();
    }
    let field = |line: &[char], column: usize| -> String {
        let from = starts[column].min(line.len());
        let to = starts.get(column + 1).copied().unwrap_or(line.len()).min(line.len());
        line[from..to].iter().collect::<String>().trim().to_string()
    };
    let mut found = Vec::new();
    for line in &lines[rule + 1..] {
        if line.is_empty() || line.iter().all(|c| *c == ' ') {
            continue;
        }
        let package = Package {
            name: field(line, 0),
            id: field(line, 1),
            version: field(line, 2),
            available: field(line, 3),
        };
        // Summary lines don't fill the columns; a cut-off id can't be used safely.
        let good_id = package.id.contains('.') && !package.id.contains(' ') && !package.id.contains('…');
        if good_id && !package.available.is_empty() {
            found.push(package);
        }
    }
    found
}

/// A tool installed a moment ago isn't on this copy's search path yet, so look where it lands.
fn tool_path(name: &str) -> String {
    let folder = match name {
        "choco" => std::env::var("ProgramData").ok().map(|base| Path::new(&base).join("chocolatey\\bin")),
        _ => dirs::data_local_dir().map(|base| base.join("Microsoft\\WindowsApps")),
    };
    folder
        .map(|folder| folder.join(format!("{name}.exe")))
        .filter(|file| std::fs::symlink_metadata(file).is_ok())
        .map_or_else(|| name.to_string(), |file| file.to_string_lossy().into_owned())
}

fn tool_ready(name: &str) -> bool {
    system::run_quiet(&tool_path(name), &["--version"]).map(|(code, _)| code == 0).unwrap_or(false)
}

/// Which package managers this PC has: (Windows Package Manager, Chocolatey).
pub fn tools_ready() -> (bool, bool) {
    (tool_ready("winget"), tool_ready("choco"))
}

fn run_script(script: &str) -> String {
    let wrapped = format!("& {{ {script} }} *>&1 | Out-String");
    let args = ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &wrapped];
    match system::run_quiet("powershell", &args) {
        Ok((code, text)) => format!("exit {code}\n{text}"),
        Err(error) => error,
    }
}

/// Run by the elevated copy of this program. Each tool installs without the other's help, and
/// what PowerShell said is saved in case one of them fails.
pub fn install_tools(which: &str) {
    let mut log = String::new();
    if which.contains("winget") && !tool_ready("winget") {
        log.push_str(&run_script(INSTALL_WINGET));
    }
    if which.contains("chocolatey") && !tool_ready("choco") {
        log.push_str(&run_script(INSTALL_CHOCOLATEY));
    }
    if let Some(folder) = system::data_dir() {
        let _ = std::fs::create_dir_all(&folder);
        let _ = std::fs::write(folder.join(TOOLS_LOG), log);
    }
}

/// Reads `choco outdated -r`: name|installed|available|pinned, one per line.
pub fn parse_chocolatey(text: &str) -> Vec<Package> {
    let mut found = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.trim().split('|').collect();
        if parts.len() < 4 || parts[3].eq_ignore_ascii_case("true") {
            continue;
        }
        found.push(Package {
            name: parts[0].to_string(),
            id: parts[0].to_string(),
            version: parts[1].to_string(),
            available: parts[2].to_string(),
        });
    }
    // "x.install" only exists to carry "x", so one line each is enough.
    let names: Vec<String> = found.iter().map(|p| p.id.to_lowercase()).collect();
    found.retain(|p| {
        let id = p.id.to_lowercase();
        let base = id.strip_suffix(".install").or(id.strip_suffix(".portable"));
        !base.is_some_and(|b| names.iter().any(|n| n == b))
    });
    found
}

fn chocolatey() -> Result<Vec<Update>, String> {
    let (_, text) = system::run_quiet(&tool_path("choco"), &["outdated", "-r", "--ignore-unfound"])
        .map_err(|_| "Chocolatey couldn't be started.".to_string())?;
    Ok(parse_chocolatey(&text)
        .into_iter()
        .map(|p| Update {
            id: format!("{CHOCO_PREFIX}{}", p.id),
            kind: Kind::Software,
            source: Source::Chocolatey,
            title: p.name,
            current: p.version,
            version: p.available,
            ..Default::default()
        })
        .collect())
}

fn plain(text: &str) -> String {
    text.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_lowercase()
}

/// A program Chocolatey installed stays with Chocolatey, so it keeps its own records right.
pub fn keep_chocolatey_copy(winget: &mut Vec<Update>, choco: &[Update]) {
    winget.retain(|update| {
        let name = plain(&update.title);
        !choco.iter().any(|c| {
            let id = plain(&c.title);
            id.len() >= 4 && name.starts_with(&id)
        })
    });
}

/// The same name with only letters and digits, and "+" spelled out so "Notepad++" matches "notepadplusplus".
fn name_key(text: &str) -> String {
    text.to_lowercase()
        .replace('+', "plus")
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

/// What Windows itself says is installed, as (name, version) from its uninstall lists. A program
/// updated by hand, or by its own updater, shows its new version here even though a package manager
/// still has the old one on record.
pub fn installed_programs() -> Vec<(String, String)> {
    let script = r#"$paths = 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*', 'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*', 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*'
Get-ItemProperty -Path $paths -ErrorAction SilentlyContinue | Where-Object { $_.DisplayName -and $_.DisplayVersion } | ForEach-Object { "$($_.DisplayName)|$($_.DisplayVersion)" }"#;
    crate::restore::powershell(script)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (name, version) = line.trim().rsplit_once('|')?;
            (!name.is_empty() && !version.is_empty()).then(|| (name.to_string(), version.to_string()))
        })
        .collect()
}

/// Drops updates for programs whose real installed version is already the offered one or newer, and
/// shows the real version as the current one for those that stay. `exact` matches only identical names;
/// otherwise a package id that starts the program's name counts too ("vlc" for "VLC media player").
pub fn reconcile(updates: Vec<Update>, installed: &[(String, String)], exact: bool) -> Vec<Update> {
    updates
        .into_iter()
        .filter_map(|mut update| {
            let id = update.id.strip_prefix(CHOCO_PREFIX).or(update.id.strip_prefix(ID_PREFIX)).unwrap_or(&update.id);
            let keys: Vec<String> = [name_key(id), name_key(&update.title)].into_iter().filter(|k| k.len() >= 3).collect();
            let mut best: Option<&str> = None;
            for (name, version) in installed {
                let name = name_key(name);
                let hit = keys.iter().any(|k| name == *k || (!exact && name.starts_with(k.as_str())));
                if hit && best.is_none_or(|b| crate::catalog::is_newer(version, b)) {
                    best = Some(version);
                }
            }
            match best {
                Some(actual) if !crate::catalog::is_newer(&update.version, actual) => None,
                Some(actual) => {
                    update.current = actual.to_string();
                    Some(update)
                }
                None => Some(update),
            }
        })
        .collect()
}

/// Asks every enabled package manager, side by side.
pub fn find(use_winget: bool, use_chocolatey: bool) -> Result<Vec<Update>, String> {
    let installed = installed_programs();
    let (first, second) = std::thread::scope(|scope| {
        let installed = &installed;
        let one = scope.spawn(move || {
            if use_winget { windows_package_manager().map(|found| reconcile(found, installed, true)) } else { Ok(Vec::new()) }
        });
        let two = scope.spawn(move || {
            if use_chocolatey && tool_ready("choco") { chocolatey().map(|found| reconcile(found, installed, false)) } else { Ok(Vec::new()) }
        });
        (one.join().unwrap_or(Ok(Vec::new())), two.join().unwrap_or(Ok(Vec::new())))
    });
    match (first, second) {
        (Err(error), Ok(apps)) if apps.is_empty() => Err(error),
        (Err(_), Ok(apps)) => Ok(apps),
        (Ok(apps), Err(_)) => Ok(apps),
        (Err(error), Err(_)) => Err(error),
        (Ok(mut winget), Ok(choco)) => {
            keep_chocolatey_copy(&mut winget, &choco);
            winget.extend(choco);
            Ok(winget)
        }
    }
}

fn windows_package_manager() -> Result<Vec<Update>, String> {
    let (code, text) = system::run_quiet(
        &tool_path("winget"),
        &["upgrade", "--source", "winget", "--accept-source-agreements", "--disable-interactivity"],
    )
    .map_err(|_| "The Windows Package Manager isn't available on this PC.".to_string())?;
    let found = parse_table(&text);
    if found.is_empty() && code != 0 && !text.contains("---") {
        return Err(format!("The Windows Package Manager couldn't check for updates ({code})."));
    }
    Ok(found
        .into_iter()
        .map(|p| Update {
            id: format!("{ID_PREFIX}{}", p.id),
            kind: Kind::Software,
            source: Source::Winget,
            title: p.name,
            current: p.version,
            version: p.available,
            ..Default::default()
        })
        .collect())
}

/// Chocolatey puts the old version back when a program's own installer gives up. That usually
/// means the program, or something using its files, was open.
fn chocolatey_failure(code: i32) -> String {
    format!(
        "The program's installer stopped (code {code}), so Chocolatey kept your current version. Close the program and anything using it, then press Try Again."
    )
}

/// Says in plain words why winget gave up. The common ones are spelled out; for the rest it asks
/// winget itself, which can describe its own error numbers.
fn winget_failure(code: u32) -> String {
    match code {
        0x8A15_0056 => "This program's installer refuses to run as an administrator, so DriverKeep can't update it. \
                        Update it from inside the program instead."
            .into(),
        0x8A15_0006 => "Windows couldn't start this program's installer. A security rule on this computer, \
                        such as AppLocker or antivirus, may be blocking it."
            .into(),
        0x8A15_008E => "A newer version exists, but it installs a different way from the copy on this computer, \
                        so the Windows Package Manager won't upgrade it. Uninstall the program and install it again."
            .into(),
        other => {
            let described = system::run_quiet(&tool_path("winget"), &["error", "--input", &format!("0x{other:08X}")])
                .ok()
                .and_then(|(_, text)| text.lines().map(str::trim).filter(|l| !l.is_empty()).nth(1).map(str::to_string));
            match described {
                Some(why) => format!("The update didn't finish: {why} (error 0x{other:08X})."),
                None => format!("The update didn't finish (error 0x{other:08X})."),
            }
        }
    }
}

/// Updates one program. Gives back true when a restart is needed to finish.
pub fn upgrade(update: &Update) -> Result<bool, String> {
    if let Some(id) = update.id.strip_prefix(CHOCO_PREFIX) {
        let (code, _) = system::run_quiet(&tool_path("choco"), &["upgrade", id, "-y", "--no-progress"])?;
        return match code {
            0 => Ok(false),
            1641 | 3010 => Ok(true),
            other => Err(chocolatey_failure(other)),
        };
    }
    let id = update.id.strip_prefix(ID_PREFIX).unwrap_or(&update.id);
    let args = [
        "upgrade",
        "--id",
        id,
        "--exact",
        "--source",
        "winget",
        "--silent",
        "--accept-package-agreements",
        "--accept-source-agreements",
        "--disable-interactivity",
    ];
    // The tool can be briefly unavailable, for example just after it updates itself.
    let mut started = None;
    for retry in 0..=crate::catalog::MAX_RETRIES {
        if retry > 0 {
            std::thread::sleep(crate::catalog::retry_wait(retry));
        }
        if let Ok(result) = system::run_quiet(&tool_path("winget"), &args) {
            started = Some(result);
            break;
        }
    }
    let (code, _) = started.ok_or("The Windows Package Manager was busy. Scan again to retry this one.")?;
    match code as u32 {
        0 => Ok(false),
        REBOOT_TO_FINISH => Ok(true),
        other => Err(winget_failure(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrade_table_is_read() {
        let text = "The source requires agreements\r\nName                  Id                     Version  Available Source\n-------------------------------------------------------------------------\nApp Installer         Microsoft.AppInstaller 1.29.379 1.29.380  winget\nUbisoft Connect       Ubisoft.Connect        < 175.1  175.1     winget\n2 upgrades available.\n1 package(s) have version numbers that cannot be determined.\n";
        let found = parse_table(text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].id, "Microsoft.AppInstaller");
        assert_eq!(found[0].available, "1.29.380");
        assert_eq!(found[1].version, "< 175.1");
    }

    #[test]
    fn chocolatey_list_is_read() {
        let text = "obs-studio|32.2.2|33.0.1|false\nobs-studio.install|32.2.2|33.0.1|false\ngit|2.0|2.1|true\n";
        let found = parse_chocolatey(text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "obs-studio");
        assert_eq!(found[0].available, "33.0.1");
    }

    #[test]
    fn same_program_stays_with_chocolatey() {
        let named = |title: &str| Update { title: title.into(), ..Default::default() };
        let mut winget = vec![named("OBS Studio 32"), named("Telegram Desktop")];
        keep_chocolatey_copy(&mut winget, &[named("obs-studio")]);
        assert_eq!(winget.len(), 1);
        assert_eq!(winget[0].title, "Telegram Desktop");
    }

    fn choco(id: &str, current: &str, available: &str) -> Update {
        Update {
            id: format!("{CHOCO_PREFIX}{id}"),
            title: id.into(),
            current: current.into(),
            version: available.into(),
            source: Source::Chocolatey,
            ..Default::default()
        }
    }

    fn installed() -> Vec<(String, String)> {
        [("OBS Studio", "33.0.1"), ("Google Chrome", "157.0.8103.79"), ("Notepad++ (64-bit x64)", "8.9.9"), ("VLC media player", "3.0.23"), ("Digital Clock", "9.9.9")]
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_program_updated_by_hand_is_no_longer_offered() {
        let left = reconcile(vec![choco("obs-studio", "32.2.2", "33.0.1"), choco("GoogleChrome", "154.0", "157.0.8103.79")], &installed(), false);
        assert!(left.is_empty());
    }

    #[test]
    fn a_program_that_is_really_older_stays_and_shows_its_real_version() {
        let left = reconcile(vec![choco("obs-studio", "32.0.0", "34.0.0")], &installed(), false);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].current, "33.0.1");
    }

    #[test]
    fn names_with_plus_signs_and_longer_names_still_match() {
        assert!(reconcile(vec![choco("notepadplusplus", "8.9.0", "8.9.9")], &installed(), false).is_empty());
        assert!(reconcile(vec![choco("vlc", "3.0.20", "3.0.23")], &installed(), false).is_empty());
    }

    #[test]
    fn short_ids_do_not_match_unrelated_programs() {
        // "git" must not be read as "Digital Clock", and a program that isn't listed is left alone.
        assert_eq!(reconcile(vec![choco("git", "2.0", "99.0")], &installed(), false).len(), 1);
        assert_eq!(reconcile(vec![choco("unknown-tool", "1.0", "2.0")], &installed(), false).len(), 1);
    }

    #[test]
    fn winget_names_must_match_exactly() {
        let winget = Update { id: "Notepad++.Notepad++".into(), title: "Notepad++ (64-bit x64)".into(), version: "8.9.9".into(), source: Source::Winget, ..Default::default() };
        assert!(reconcile(vec![winget.clone()], &installed(), true).is_empty());
        let loose = Update { title: "OBS".into(), id: "x".into(), version: "99".into(), ..winget };
        assert_eq!(reconcile(vec![loose], &installed(), true).len(), 1);
    }

    #[test]
    fn nothing_to_update_is_empty() {
        assert!(parse_table("No installed package found matching input criteria.").is_empty());
    }
}

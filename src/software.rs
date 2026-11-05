//! Updates for other installed programs, using the Windows Package Manager (winget) that
//! comes with Windows. Only its own trusted source is used, and it does the installing.

use crate::system;
use crate::wu::{Kind, Source, Update};

/// What the package manager says when an update needs a restart to finish.
const REBOOT_TO_FINISH: u32 = 0x8A15_0109;
pub const ID_PREFIX: &str = "winget:";
pub const CHOCO_PREFIX: &str = "choco:";
pub const CHOCOLATEY_PACKAGE: &str = "Chocolatey.Chocolatey";
/// The Microsoft Store page for App Installer, which provides the Windows Package Manager.
pub const WINGET_STORE_LINK: &str = "ms-windows-store://pdp/?productid=9NBLGGH4NNS1";

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

fn tool_ready(program: &str) -> bool {
    system::run_quiet(program, &["--version"]).map(|(code, _)| code == 0).unwrap_or(false)
}

/// Which package managers this PC has: (Windows Package Manager, Chocolatey).
pub fn tools_ready() -> (bool, bool) {
    (tool_ready("winget"), tool_ready("choco"))
}

/// Run by the elevated copy of this program, from the button in Settings.
pub fn install_chocolatey() {
    let _ = system::run_quiet(
        "winget",
        &[
            "install",
            "--id",
            CHOCOLATEY_PACKAGE,
            "--exact",
            "--source",
            "winget",
            "--silent",
            "--accept-package-agreements",
            "--accept-source-agreements",
            "--disable-interactivity",
        ],
    );
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
    let (_, text) = system::run_quiet("choco", &["outdated", "-r", "--ignore-unfound"])
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

/// Asks every enabled package manager, side by side.
pub fn find(use_winget: bool, use_chocolatey: bool) -> Result<Vec<Update>, String> {
    let (first, second) = std::thread::scope(|scope| {
        let one = scope.spawn(|| if use_winget { windows_package_manager() } else { Ok(Vec::new()) });
        let two = scope.spawn(|| {
            if use_chocolatey && tool_ready("choco") { chocolatey() } else { Ok(Vec::new()) }
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
        "winget",
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

/// Updates one program. Gives back true when a restart is needed to finish.
pub fn upgrade(update: &Update) -> Result<bool, String> {
    if let Some(id) = update.id.strip_prefix(CHOCO_PREFIX) {
        let (code, _) = system::run_quiet("choco", &["upgrade", id, "-y", "--no-progress"])?;
        return match code {
            0 => Ok(false),
            1641 | 3010 => Ok(true),
            other => Err(format!("The update didn't finish (error {other}).")),
        };
    }
    let id = update.id.strip_prefix(ID_PREFIX).unwrap_or(&update.id);
    let (code, _) = system::run_quiet(
        "winget",
        &[
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
        ],
    )?;
    match code as u32 {
        0 => Ok(false),
        REBOOT_TO_FINISH => Ok(true),
        other => Err(format!("The update didn't finish (error 0x{other:08X}).")),
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

    #[test]
    fn nothing_to_update_is_empty() {
        assert!(parse_table("No installed package found matching input criteria.").is_empty());
    }
}

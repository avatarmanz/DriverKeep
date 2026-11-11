//! Keeps DriverKeep itself up to date from its public GitHub releases. A new version is only
//! run if it came from this project's own release page and matches GitHub's published checksum.

use crate::catalog;
use crate::wu::{Kind, Source, Update};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use ureq::ResponseExt;

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const LATEST: &str = "https://api.github.com/repos/avatarmanz/DriverKeep/releases/latest";
const LATEST_PAGE: &str = "https://github.com/avatarmanz/DriverKeep/releases/latest";
const DOWNLOADS: &str = "https://github.com/avatarmanz/DriverKeep/releases/download/";
const SETUP_FILE: &str = "DriverKeep-Setup.exe";
const CHECKSUM_FILE: &str = "DriverKeep-Setup.sha256.txt";
const ID_PREFIX: &str = "github:";
const DETACHED: u32 = 0x0000_0008;
/// The original won't ask for a new version again until 5 minutes after its last answer.
const CHECK_WAIT: Duration = Duration::from_millis(300_000);

static LAST_CHECK: Mutex<Option<(Instant, Option<Update>)>> = Mutex::new(None);

/// Reads GitHub's release details. Gives back an update only if it's newer than this copy.
pub fn parse_release(text: &str, current: &str) -> Option<Update> {
    let release: serde_json::Value = serde_json::from_str(text).ok()?;
    if release["draft"].as_bool().unwrap_or(false) || release["prerelease"].as_bool().unwrap_or(false) {
        return None;
    }
    let version = newer_version(release["tag_name"].as_str()?, current)?;
    let asset = release["assets"]
        .as_array()?
        .iter()
        .find(|a| a["name"].as_str() == Some(SETUP_FILE))?;
    let link = asset["browser_download_url"].as_str()?;
    let checksum = asset["digest"].as_str()?.strip_prefix("sha256:")?;
    offer(version, current, link, checksum, asset["size"].as_u64().unwrap_or(0))
}

/// The version also names a folder, so it must be nothing but a number like 1.2.3.
fn newer_version(tag: &str, current: &str) -> Option<String> {
    let version = tag.trim_start_matches('v');
    let plain = !version.is_empty() && version.chars().all(|c| c.is_ascii_digit() || c == '.');
    (plain && catalog::is_newer(version, current)).then(|| version.to_string())
}

fn offer(version: String, current: &str, link: &str, checksum: &str, size: u64) -> Option<Update> {
    if !link.starts_with(DOWNLOADS) || checksum.len() != 64 {
        return None;
    }
    Some(Update {
        id: format!("{ID_PREFIX}{version}"),
        kind: Kind::Software,
        source: Source::GitHub,
        title: crate::system::APP_NAME.to_string(),
        current: current.to_string(),
        size,
        version,
        link: link.to_string(),
        checksum: checksum.to_string(),
        ..Default::default()
    })
}

/// certutil's output: the one line that's nothing but a 64 character hash.
fn hash_line(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|line| line.len() == 64 && line.chars().all(|c| c.is_ascii_hexdigit()))
}

/// GitHub answers only 60 anonymous API requests an hour for each internet connection, shared by
/// everything on it. The ordinary release page has no such limit, and it forwards to the latest
/// version, whose checksum file was published beside it.
fn read_release_page(current: &str) -> Result<Option<Update>, String> {
    let agent = catalog::client();
    let page = agent.get(LATEST_PAGE).call().map_err(|e| e.to_string())?;
    let path = page.get_uri().path();
    let tag = path.rsplit_once("/releases/tag/").ok_or("GitHub's release page has no version.")?.1;
    let Some(version) = newer_version(tag, current) else {
        return Ok(None);
    };
    let folder = format!("{DOWNLOADS}v{version}/");
    let text = agent
        .get(format!("{folder}{CHECKSUM_FILE}"))
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let checksum = hash_line(&text).ok_or("The release's checksum couldn't be read.")?;
    Ok(offer(version, current, &format!("{folder}{SETUP_FILE}"), checksum, 0))
}

fn find_release(current: &str) -> Result<Option<Update>, String> {
    match read_latest() {
        Ok(text) => Ok(parse_release(&text, current)),
        Err(_) => read_release_page(current),
    }
}

fn read_latest() -> Result<String, String> {
    catalog::client()
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())
}

/// Asks GitHub for the latest release. Being offline just means no update is offered. Scanning
/// again within 5 minutes reuses the last answer, but a failed check can be tried again at once.
pub fn check() -> Option<Update> {
    let mut last = LAST_CHECK.lock().ok()?;
    if let Some((when, found)) = last.as_ref() {
        if when.elapsed() < CHECK_WAIT {
            return found.clone();
        }
    }
    *last = None;
    let found = find_release(CURRENT).ok()?;
    *last = Some((Instant::now(), found.clone()));
    found
}

/// The Settings button: asks GitHub right now, ignoring the 5 minute wait, and says if it couldn't.
pub fn check_now() -> Result<Option<Update>, String> {
    let found = find_release(CURRENT)?;
    if let Ok(mut last) = LAST_CHECK.lock() {
        *last = Some((Instant::now(), found.clone()));
    }
    Ok(found)
}

fn checksum_bytes(text: &str) -> Option<Vec<u8>> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

fn file_matches(path: &Path, expected: &[u8]) -> bool {
    use sha2::{Digest, Sha256};
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash).is_ok() && hash.finalize().as_slice() == expected
}

/// Only the version being installed is worth keeping, like the original's update folder.
fn clear_other_versions(root: &Path, version: &str) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy() != version && entry.path().is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Gets the new setup program into its version's folder and proves it's intact. It's a .part file
/// until it's whole, and a good earlier download is used again. Nothing is run here.
fn fetch_into(update: &Update, root: &Path) -> Result<PathBuf, String> {
    if !update.link.starts_with(DOWNLOADS) {
        return Err("The download wasn't from DriverKeep's release page, so it was refused.".into());
    }
    let expected = checksum_bytes(&update.checksum).ok_or("The release's checksum couldn't be read.")?;
    clear_other_versions(root, &update.version);
    let folder = root.join(&update.version);
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let setup = folder.join(SETUP_FILE);
    if file_matches(&setup, &expected) {
        return Ok(setup);
    }
    let part = folder.join(format!("{SETUP_FILE}.part"));
    catalog::download_checked(&catalog::client(), &update.link, &part, &expected)?;
    std::fs::rename(&part, &setup).map_err(|e| e.to_string())?;
    Ok(setup)
}

/// Test: acts like a very old copy against the real release page, stopping before anything runs.
/// The second fetch should find the first one's file and return straight away.
pub fn check_download(root: &Path) -> String {
    let update = match find_release("0.0.1") {
        Ok(Some(update)) => update,
        Ok(None) => return "Failed: no release was offered.".into(),
        Err(error) => return format!("Failed: GitHub didn't answer ({error})"),
    };
    let first = fetch_into(&update, root);
    let again = Instant::now();
    match first.and_then(|path| fetch_into(&update, root).map(|same| (path, same))) {
        Ok((path, same)) if path == same => {
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            format!("Downloaded {} ({size} bytes), checksum matched, reused in {:?}.", update.version, again.elapsed())
        }
        Ok(_) => "Failed: the second fetch gave a different file.".into(),
        Err(error) => format!("Failed: {error}"),
    }
}

/// Starts the checked setup program. Setup closes this copy, replaces it, then brings back
/// whichever part was running.
pub fn install(update: &Update, from_helper: bool) -> Result<bool, String> {
    let root = crate::system::data_dir()
        .ok_or("DriverKeep's folder couldn't be found.")?
        .join("updates");
    let setup = fetch_into(update, &root)?;
    let bring_back = if from_helper { "--helper" } else { "--reopen" };
    Command::new(&setup)
        .args(["--silent", bring_back])
        .creation_flags(DETACHED)
        .spawn()
        .map_err(|e| format!("The new version couldn't be started: {e}"))?;
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "4cfd07492e93c8dd3cf710a47a9575589f665be054e7ffb5c1f58e435adfb40f";

    fn release(tag: &str, link: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","draft":false,"prerelease":false,"assets":[{{"name":"DriverKeep-Portable.exe"}},{{"name":"DriverKeep-Setup.exe","size":14495232,"digest":"sha256:{DIGEST}","browser_download_url":"{link}"}}]}}"#
        )
    }

    #[test]
    fn newer_release_is_offered() {
        let link = "https://github.com/avatarmanz/DriverKeep/releases/download/v1.0.9/DriverKeep-Setup.exe";
        let update = parse_release(&release("v1.0.9", link), "1.0.2").unwrap();
        assert_eq!(update.version, "1.0.9");
        assert_eq!(update.source, Source::GitHub);
        assert_eq!(checksum_bytes(&update.checksum).unwrap().len(), 32);
    }

    #[test]
    fn same_or_older_release_is_not() {
        let link = "https://github.com/avatarmanz/DriverKeep/releases/download/v1.0.0/DriverKeep-Setup.exe";
        assert!(parse_release(&release("v1.0.0", link), "1.0.2").is_none());
        assert!(parse_release(&release("v1.0.2", link), "1.0.2").is_none());
    }

    #[test]
    fn downloads_from_elsewhere_are_refused() {
        let link = "https://example.com/DriverKeep-Setup.exe";
        assert!(parse_release(&release("v9.0.0", link), "1.0.2").is_none());
    }

    #[test]
    fn checksum_file_is_read() {
        let text = format!("SHA256 hash of dist\\DriverKeep-Setup.exe:\r\n{DIGEST}\r\nCertUtil: -hashfile command completed successfully.\r\n");
        assert_eq!(hash_line(&text), Some(DIGEST));
        assert_eq!(hash_line("no hash here"), None);
    }

    #[test]
    fn versions_that_are_not_numbers_are_refused() {
        assert_eq!(newer_version("v1.0.9", "1.0.2").as_deref(), Some("1.0.9"));
        assert_eq!(newer_version("v1.0.0", "1.0.2"), None);
        assert_eq!(newer_version("v9.0.0-beta", "1.0.2"), None);
        let link = "https://github.com/avatarmanz/DriverKeep/releases/download/v9/DriverKeep-Setup.exe";
        assert!(parse_release(&release("v9.0.0/../evil", link), "1.0.2").is_none());
    }

    #[test]
    fn a_good_download_is_reused_and_old_versions_go() {
        use sha2::{Digest, Sha256};
        let root = std::env::temp_dir().join(format!("driverkeep-test-{}", std::process::id()));
        let keep = root.join("1.0.9");
        let old = root.join("1.0.8");
        std::fs::create_dir_all(&keep).unwrap();
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(keep.join(SETUP_FILE), b"new setup").unwrap();
        let checksum: String = Sha256::digest(b"new setup").iter().map(|b| format!("{b:02x}")).collect();
        let update = Update {
            version: "1.0.9".into(),
            link: format!("{DOWNLOADS}v1.0.9/{SETUP_FILE}"),
            checksum,
            ..Default::default()
        };
        assert_eq!(fetch_into(&update, &root).unwrap(), keep.join(SETUP_FILE));
        assert!(!old.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}

//! Keeps DriverKeep itself up to date from its public GitHub releases. A new version is only
//! run if it came from this project's own release page and matches GitHub's published checksum.

use crate::catalog;
use crate::wu::{Kind, Source, Update};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const LATEST: &str = "https://api.github.com/repos/avatarmanz/DriverKeep/releases/latest";
const DOWNLOADS: &str = "https://github.com/avatarmanz/DriverKeep/releases/download/";
const SETUP_FILE: &str = "DriverKeep-Setup.exe";
const ID_PREFIX: &str = "github:";
const DETACHED: u32 = 0x0000_0008;

/// Reads GitHub's release details. Gives back an update only if it's newer than this copy.
pub fn parse_release(text: &str, current: &str) -> Option<Update> {
    let release: serde_json::Value = serde_json::from_str(text).ok()?;
    if release["draft"].as_bool().unwrap_or(false) || release["prerelease"].as_bool().unwrap_or(false) {
        return None;
    }
    let version = release["tag_name"].as_str()?.trim_start_matches('v').to_string();
    if !catalog::is_newer(&version, current) {
        return None;
    }
    let asset = release["assets"]
        .as_array()?
        .iter()
        .find(|a| a["name"].as_str() == Some(SETUP_FILE))?;
    let link = asset["browser_download_url"].as_str()?.to_string();
    let checksum = asset["digest"].as_str()?.strip_prefix("sha256:")?.to_string();
    if !link.starts_with(DOWNLOADS) || checksum.len() != 64 {
        return None;
    }
    Some(Update {
        id: format!("{ID_PREFIX}{version}"),
        kind: Kind::Software,
        source: Source::GitHub,
        title: crate::system::APP_NAME.to_string(),
        current: current.to_string(),
        size: asset["size"].as_u64().unwrap_or(0),
        version,
        link,
        checksum,
        ..Default::default()
    })
}

/// Asks GitHub for the latest release. Being offline just means no update is offered.
pub fn check() -> Option<Update> {
    let agent = catalog::client();
    let text = agent
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()?;
    parse_release(&text, CURRENT)
}

fn checksum_bytes(text: &str) -> Option<Vec<u8>> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Downloads the new setup program and proves it's intact. Nothing is run here.
pub fn fetch(update: &Update) -> Result<PathBuf, String> {
    if !update.link.starts_with(DOWNLOADS) {
        return Err("The download wasn't from DriverKeep's release page, so it was refused.".into());
    }
    let expected = checksum_bytes(&update.checksum).ok_or("The release's checksum couldn't be read.")?;
    let folder = std::env::temp_dir().join("DriverKeep-Update");
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let setup = folder.join(SETUP_FILE);
    catalog::download_checked(&catalog::client(), &update.link, &setup, &expected)?;
    Ok(setup)
}

/// Test: acts like a very old copy against the real release page, stopping before anything runs.
pub fn check_download() -> String {
    let release = catalog::client()
        .get(LATEST)
        .call()
        .and_then(|mut r| r.body_mut().read_to_string());
    let Some(update) = release.ok().and_then(|text| parse_release(&text, "0.0.1")) else {
        return "No release could be read.".into();
    };
    match fetch(&update) {
        Ok(path) => {
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let _ = std::fs::remove_file(&path);
            format!("Downloaded {} ({size} bytes), checksum matched.", update.version)
        }
        Err(error) => format!("Failed: {error}"),
    }
}

/// Starts the checked setup program. Setup closes this copy, replaces it, then brings back
/// whichever part was running.
pub fn install(update: &Update, from_helper: bool) -> Result<bool, String> {
    let setup = fetch(update)?;
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
}

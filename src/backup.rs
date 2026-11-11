//! Saves driver packages to one backup file and puts them back.
//! The steps follow app/MECHANICS.md ("Backup" and "Restore").

use crate::devices::{self, Device, InstallOutcome};
use crate::system;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;

pub const FILE_PREFIX: &str = "mydrivers-";
pub const FILE_EXTENSION: &str = "drvbak";
const INFO_FILE: &str = "BackupInfo.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackupEntry {
    pub instance_id: String,
    pub device_name: String,
    pub class_title: String,
    pub class_guid: String,
    pub hardware_ids: Vec<String>,
    pub matching_id: String,
    pub inf_name: String,
    /// Path of the driver's setup file inside the backup.
    pub inf_in_backup: String,
    pub provider: String,
    pub version: String,
    pub date: String,
    pub third_party: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackupInfo {
    pub format: u32,
    pub created: String,
    pub windows: String,
    pub bits: u32,
    pub reason: String,
    pub automatic: bool,
    pub entries: Vec<BackupEntry>,
}

#[derive(Clone, Debug)]
pub struct BackupFile {
    pub path: PathBuf,
    pub size: u64,
    pub info: BackupInfo,
}

pub struct BackupDone {
    pub path: PathBuf,
    pub saved: usize,
    pub bytes: u64,
    pub skipped: Vec<String>,
}

fn walk(folder: &Path, found: &mut Vec<PathBuf>) -> io::Result<()> {
    for item in fs::read_dir(folder)? {
        let path = item?.path();
        if path.is_dir() {
            walk(&path, found)?;
        } else {
            found.push(path);
        }
    }
    Ok(())
}

pub fn folder_size(folder: &Path) -> u64 {
    let mut files = Vec::new();
    let _ = walk(folder, &mut files);
    files.iter().filter_map(|f| f.metadata().ok()).map(|m| m.len()).sum()
}

fn windows_version() -> String {
    system::run_quiet("cmd", &["/c", "ver"])
        .map(|(_, t)| t.trim().to_string())
        .unwrap_or_default()
}

/// Backs up each device's whole driver package. A package shared by several devices is stored
/// once. Nothing is left behind if it stops partway: the file only gets its name at the end.
pub fn create(
    devices: &[Device],
    folder: &Path,
    reason: &str,
    automatic: bool,
    mut progress: impl FnMut(u32, &str),
) -> Result<BackupDone, String> {
    fs::create_dir_all(folder).map_err(|e| format!("Could not open the backup folder: {e}"))?;
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    let target = folder.join(format!("{FILE_PREFIX}{stamp}.{FILE_EXTENSION}"));
    let partial = target.with_extension("partial");

    struct Package {
        root: PathBuf,
        name: String,
        files: Vec<PathBuf>,
    }
    let mut packages: Vec<Package> = Vec::new();
    let mut entries = Vec::new();
    let mut skipped = Vec::new();
    let mut seen = HashSet::new();

    for device in devices {
        let Some(driver) = &device.driver else { continue };
        let Some(inf) = devices::package_folder(&driver.inf_name) else {
            skipped.push(device.name.clone());
            continue;
        };
        let Some(root) = inf.parent().map(Path::to_path_buf) else {
            skipped.push(device.name.clone());
            continue;
        };
        let name = root.file_name().unwrap_or_default().to_string_lossy().to_string();
        if seen.insert(name.clone()) {
            let mut files = Vec::new();
            if walk(&root, &mut files).is_err() {
                skipped.push(device.name.clone());
                continue;
            }
            packages.push(Package { root: root.clone(), name: name.clone(), files });
        }
        // Windows can report a name in a different case from the file itself, and names
        // inside a backup are case-sensitive, so use the name as it really is on disk.
        let reported = inf.file_name().unwrap_or_default().to_string_lossy().to_string();
        let inf_file = packages
            .iter()
            .find(|p| p.name == name)
            .and_then(|p| {
                p.files.iter().find_map(|file| {
                    let found = file.file_name()?.to_string_lossy().to_string();
                    let beside_root = file.parent() == Some(p.root.as_path());
                    (beside_root && found.eq_ignore_ascii_case(&reported)).then_some(found)
                })
            })
            .unwrap_or(reported);
        entries.push(BackupEntry {
            instance_id: device.instance_id.clone(),
            device_name: device.name.clone(),
            class_title: device.class_title.clone(),
            class_guid: device.class_guid.clone(),
            hardware_ids: device.hardware_ids.clone(),
            matching_id: driver.matching_id.clone(),
            inf_name: driver.inf_name.clone(),
            inf_in_backup: format!("packages/{name}/{inf_file}"),
            provider: driver.provider.clone(),
            version: driver.version.clone(),
            date: driver.date.clone(),
            third_party: driver.is_third_party(),
        });
    }
    if entries.is_empty() {
        return Err("None of the chosen devices has a driver that can be saved.".into());
    }

    let total: u64 = packages
        .iter()
        .flat_map(|p| &p.files)
        .filter_map(|f| f.metadata().ok())
        .map(|m| m.len())
        .sum();

    let info = BackupInfo {
        format: 1,
        created: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        windows: windows_version(),
        bits: if cfg!(target_pointer_width = "64") { 64 } else { 32 },
        reason: reason.to_string(),
        automatic,
        entries,
    };

    let result = (|| -> Result<(), String> {
        let file = File::create(&partial).map_err(|e| format!("Could not write the backup: {e}"))?;
        let mut zip = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true);
        let text = serde_json::to_vec_pretty(&info).map_err(|e| e.to_string())?;
        zip.start_file(INFO_FILE, options).map_err(|e| e.to_string())?;
        zip.write_all(&text).map_err(|e| e.to_string())?;

        let mut done = 0u64;
        let mut buffer = vec![0u8; 1 << 20];
        for package in &packages {
            for path in &package.files {
                let relative = path.strip_prefix(&package.root).map_err(|e| e.to_string())?;
                let inner = relative.to_string_lossy().replace('\\', "/");
                zip.start_file(format!("packages/{}/{}", package.name, inner), options)
                    .map_err(|e| e.to_string())?;
                let mut source = File::open(path)
                    .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
                loop {
                    let read = source.read(&mut buffer).map_err(|e| e.to_string())?;
                    if read == 0 {
                        break;
                    }
                    zip.write_all(&buffer[..read])
                        .map_err(|e| format!("Could not write the backup: {e}"))?;
                    done += read as u64;
                }
                let percent = if total == 0 { 0 } else { done * 85 / total };
                progress(percent as u32, &package.name);
            }
        }
        zip.finish().map_err(|e| e.to_string())?;
        progress(86, "Checking the backup");
        verify(&partial, &info)?;
        Ok(())
    })();

    if let Err(error) = result {
        let _ = fs::remove_file(&partial);
        return Err(error);
    }
    fs::rename(&partial, &target).map_err(|e| format!("Could not finish the backup: {e}"))?;
    let bytes = fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
    progress(100, "Backup finished");
    Ok(BackupDone { path: target, saved: info.entries.len(), bytes, skipped })
}

/// Reads every stored file back so a damaged backup is caught now, not on the day it's needed.
fn verify(path: &Path, info: &BackupInfo) -> Result<(), String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    for entry in &info.entries {
        archive
            .by_name(&entry.inf_in_backup)
            .map_err(|_| format!("The backup is missing {}", entry.inf_in_backup))?;
    }
    for index in 0..archive.len() {
        let mut item = archive.by_index(index).map_err(|e| e.to_string())?;
        io::copy(&mut item, &mut io::sink())
            .map_err(|e| format!("The backup failed its check: {e}"))?;
    }
    Ok(())
}

pub fn read_info(path: &Path) -> Result<BackupInfo, String> {
    let file = File::open(path).map_err(|e| format!("Could not open the backup: {e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|_| "That file isn't a driver backup.".to_string())?;
    let item = archive
        .by_name(INFO_FILE)
        .map_err(|_| "That file isn't a driver backup.".to_string())?;
    serde_json::from_reader(item).map_err(|_| "That backup's contents list is damaged.".to_string())
}

pub fn list(folder: &Path) -> Vec<BackupFile> {
    let mut found = Vec::new();
    let Ok(items) = fs::read_dir(folder) else {
        return found;
    };
    for item in items.flatten() {
        let path = item.path();
        if path.extension().and_then(|e| e.to_str()) != Some(FILE_EXTENSION) {
            continue;
        }
        if let Ok(info) = read_info(&path) {
            let size = item.metadata().map(|m| m.len()).unwrap_or(0);
            found.push(BackupFile { path, size, info });
        }
    }
    found.sort_by(|a, b| b.info.created.cmp(&a.info.created).then(b.path.cmp(&a.path)));
    found
}

/// Deletes backup files older than `days` days from the backup folder. 0 keeps everything.
/// Only DriverKeep's own backup files are touched. Gives back how many were removed.
pub fn prune(folder: &Path, days: u32) -> usize {
    if days == 0 {
        return 0;
    }
    let age = std::time::Duration::from_secs(u64::from(days) * 86_400);
    let Some(limit) = std::time::SystemTime::now().checked_sub(age) else { return 0 };
    let Ok(items) = fs::read_dir(folder) else { return 0 };
    let mut removed = 0;
    for item in items.flatten() {
        let path = item.path();
        if path.extension().and_then(|e| e.to_str()) != Some(FILE_EXTENSION) {
            continue;
        }
        let old = item.metadata().and_then(|m| m.modified()).is_ok_and(|when| when < limit);
        if old && fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod prune_tests {
    use super::*;

    fn touch(folder: &Path, name: &str, days_old: u64) {
        let path = folder.join(name);
        let file = fs::File::create(&path).unwrap();
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(days_old * 86_400);
        file.set_modified(when).unwrap();
    }

    #[test]
    fn only_old_backup_files_are_removed() {
        let folder = std::env::temp_dir().join(format!("driverkeep-prune-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        touch(&folder, "old.drvbak", 40);
        touch(&folder, "new.drvbak", 2);
        touch(&folder, "notes.txt", 400);
        assert_eq!(prune(&folder, 30), 1);
        assert!(!folder.join("old.drvbak").exists());
        assert!(folder.join("new.drvbak").exists());
        assert!(folder.join("notes.txt").exists());
        // Zero keeps everything.
        assert_eq!(prune(&folder, 0), 0);
        let _ = fs::remove_dir_all(&folder);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RestoreState {
    Restored { restart: bool, now_version: String },
    /// Windows accepted it but the device still reports another version.
    Unconfirmed { now_version: String },
    /// The device isn't plugged in; Windows has the driver ready for when it is.
    WaitingForDevice,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct RestoreResult {
    pub device_name: String,
    pub target_version: String,
    pub state: RestoreState,
}

/// Unpacks only the chosen packages, refusing any path that would land outside the folder.
fn extract(path: &Path, wanted: &HashSet<String>, into: &Path) -> Result<(), String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    for index in 0..archive.len() {
        let mut item = archive.by_index(index).map_err(|e| e.to_string())?;
        let Some(relative) = item.enclosed_name() else {
            return Err("That backup contains an unsafe file path.".into());
        };
        let mut parts = relative.components();
        parts.next();
        let package = parts
            .next()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .unwrap_or_default();
        if !wanted.contains(&package) {
            continue;
        }
        let out = into.join(&relative);
        if item.is_dir() {
            fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut target = File::create(&out).map_err(|e| e.to_string())?;
        io::copy(&mut item, &mut target).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn package_of(entry: &BackupEntry) -> String {
    entry.inf_in_backup.split('/').nth(1).unwrap_or("").to_string()
}

fn version_now(instance_id: &str) -> String {
    devices::enumerate(|_, _| {})
        .ok()
        .and_then(|all| {
            all.into_iter()
                .find(|d| d.instance_id.eq_ignore_ascii_case(instance_id))
        })
        .and_then(|d| d.driver)
        .map(|d| d.version)
        .unwrap_or_default()
}

/// Restores the chosen drivers one at a time, then re-reads the device to confirm.
pub fn restore(
    backup: &Path,
    entries: &[BackupEntry],
    mut progress: impl FnMut(u32, &str),
) -> Result<Vec<RestoreResult>, String> {
    let stamp = chrono::Local::now().format("%Y%m%d%H%M%S");
    let temp = std::env::temp_dir().join(format!("DriverKeepRestore-{stamp}"));
    let wanted: HashSet<String> = entries.iter().map(package_of).collect();
    progress(5, "Unpacking the backup");
    extract(backup, &wanted, &temp).map_err(|e| format!("Could not unpack the backup: {e}"))?;

    let mut results = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let percent = 10 + (index * 85 / entries.len().max(1)) as u32;
        progress(percent, &entry.device_name);
        let inf = temp.join(entry.inf_in_backup.replace('/', "\\"));
        let inf_text = inf.to_string_lossy().to_string();
        let state = if !devices::is_present(&entry.instance_id) {
            stage_for_later(&inf_text)
        } else {
            match devices::install_inf(&entry.matching_id, &inf_text, true) {
                Ok(InstallOutcome::Installed { restart }) => {
                    let now = version_now(&entry.instance_id);
                    if now == entry.version {
                        RestoreState::Restored { restart, now_version: now }
                    } else if restart {
                        RestoreState::Restored { restart, now_version: entry.version.clone() }
                    } else {
                        RestoreState::Unconfirmed { now_version: now }
                    }
                }
                Ok(InstallOutcome::Unplugged) => stage_for_later(&inf_text),
                Ok(InstallOutcome::NotBetter) => {
                    RestoreState::Failed("Windows kept the current driver.".into())
                }
                Err(error) => RestoreState::Failed(error),
            }
        };
        results.push(RestoreResult {
            device_name: entry.device_name.clone(),
            target_version: entry.version.clone(),
            state,
        });
    }
    let _ = fs::remove_dir_all(&temp);
    progress(100, "Restore finished");
    Ok(results)
}

/// Hands the driver to Windows so it's used the next time the device is plugged in.
fn stage_for_later(inf: &str) -> RestoreState {
    match system::run_quiet("pnputil", &["/add-driver", inf]) {
        Ok((0, _)) | Ok((259, _)) | Ok((3010, _)) => RestoreState::WaitingForDevice,
        Ok((code, text)) => {
            let reason = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
            RestoreState::Failed(format!("Windows could not store the driver ({code}). {reason}"))
        }
        Err(error) => RestoreState::Failed(error),
    }
}

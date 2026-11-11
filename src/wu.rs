//! The driver engine: scans, Windows Update, and installing with a safety net first.
//! It runs on its own thread because Windows Update objects must stay where they were made.
//! The window and the background helper both drive it through the same commands.

use crate::backup;
use crate::catalog;
use crate::devices::{self, Device};
use crate::history::{self, Record};
use crate::system::{self, RestorePoint};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use windows::Win32::System::Com::*;
use windows::Win32::System::UpdateAgent::*;
use windows::core::{BSTR, Interface};

pub const SEARCH_RULE: &str = "IsInstalled=0 and IsHidden=0";
pub const SCAN_CACHE: &str = "last_scan.json";
const CLIENT_NAME: &str = "DriverKeep";

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Source {
    #[default]
    WindowsUpdate,
    Catalog,
    Winget,
    Chocolatey,
    GitHub,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::WindowsUpdate => "Windows Update",
            Source::Catalog => "Microsoft Catalog",
            Source::Winget => "Windows Package Manager",
            Source::Chocolatey => "Chocolatey",
            Source::GitHub => "GitHub",
        }
    }
}

/// The three groups the person sees.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    #[default]
    Driver,
    Windows,
    Software,
}

/// Which checks a scan runs.
#[derive(Clone, Copy, Debug)]
pub struct ScanPlan {
    pub drivers: bool,
    pub catalog: bool,
    pub windows: bool,
    pub optional: bool,
    pub winget: bool,
    pub chocolatey: bool,
}

impl ScanPlan {
    pub fn from_settings(settings: &system::Settings) -> Self {
        Self {
            drivers: settings.scan_drivers,
            catalog: settings.scan_drivers && settings.use_catalog,
            windows: settings.scan_windows,
            optional: settings.scan_optional,
            winget: settings.scan_software && settings.use_winget,
            chocolatey: settings.scan_software && settings.use_chocolatey,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Update {
    pub id: String,
    pub kind: Kind,
    pub source: Source,
    pub title: String,
    /// The version in use now, when the source says.
    pub current: String,
    /// Where a new version of this app downloads from, with its SHA-256.
    pub link: String,
    pub checksum: String,
    pub model: String,
    pub provider: String,
    pub class: String,
    pub hardware_id: String,
    pub version: String,
    pub date: String,
    pub size: u64,
    /// The devices this update is for.
    pub devices: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemState {
    Waiting,
    SavingBackup,
    Downloading,
    Installing,
    Installed,
    NeedsRestart,
    Skipped(String),
    Failed(String),
}

#[derive(Clone)]
pub struct InstallItem {
    pub update: Update,
    pub devices: Vec<Device>,
}

pub struct InstallPlan {
    pub items: Vec<InstallItem>,
    pub restore_point: bool,
    pub backup_folder: Option<PathBuf>,
    /// When true, a driver whose backup fails is skipped rather than installed.
    pub require_backup: bool,
    pub automatic: bool,
}

pub enum Command {
    Scan(ScanPlan),
    Install(InstallPlan),
}

pub enum Event {
    Devices(Result<Vec<Device>, String>),
    CatalogProgress { done: usize, total: usize, text: String },
    CatalogDone(Result<Vec<Update>, String>),
    WindowsUpdateDone(Result<Vec<Update>, String>),
    SoftwareDone(Result<Vec<Update>, String>),
    Status { percent: u32, text: String },
    Item { id: String, state: ItemState },
    /// How far along the current download or install of one item is.
    ItemProgress { id: String, percent: u32 },
    Note(String),
    InstallFinished { restart: bool, installed: usize, failed: usize },
}

#[derive(Clone)]
pub struct Engine {
    pub commands: Sender<Command>,
    pub stop: Arc<AtomicBool>,
}

pub fn start(events: Sender<Event>, wake: impl Fn() + Send + Sync + Clone + 'static) -> Engine {
    let (commands, inbox) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || worker(inbox, events, wake, flag));
    Engine { commands, stop }
}

/// Everything found, one best update per device, ignored ones left out.
pub fn merge(mut lists: Vec<Update>, ignored: &[String]) -> Vec<Update> {
    lists.retain(|u| !ignored.contains(&u.id));
    // Windows Update first, then newest version: the first offer for a device wins.
    lists.sort_by(|a, b| {
        let rank = |u: &Update| u.source != Source::WindowsUpdate;
        rank(a).cmp(&rank(b)).then_with(|| b.date.cmp(&a.date))
    });
    let mut kept: Vec<Update> = Vec::new();
    for mut update in lists {
        update.devices.retain(|d| {
            !kept.iter().any(|k| {
                k.devices.contains(d)
                    && (k.source == Source::WindowsUpdate && update.source == Source::WindowsUpdate
                        || !catalog::is_newer(&update.version, &k.version))
            })
        });
        for earlier in &mut kept {
            if catalog::is_newer(&update.version, &earlier.version) {
                earlier.devices.retain(|d| !update.devices.contains(d));
            }
        }
        if !update.devices.is_empty() || update.hardware_id.is_empty() {
            kept.push(update);
        }
    }
    kept.retain(|u| !u.devices.is_empty() || u.hardware_id.is_empty());
    kept
}

/// Calendar date from the day-count Windows uses here (days since 30 Dec 1899).
fn ole_date(value: f64) -> String {
    if value <= 0.0 {
        return String::new();
    }
    devices::short_date(((value - 25569.0) * 86400.0) as i64)
}

/// "Intel - System - 10.1.19.1" ends with its version.
fn version_from_title(title: &str) -> String {
    let last = title.rsplit(['-', ' ']).next().unwrap_or("").trim();
    let last = last.trim_matches(['(', ')']);
    if last.contains('.') && last.chars().all(|c| c.is_ascii_digit() || c == '.') {
        last.to_string()
    } else {
        String::new()
    }
}

type Found = (IUpdateSession, IUpdateCollection, Vec<Update>);

fn worker(
    inbox: Receiver<Command>,
    events: Sender<Event>,
    wake: impl Fn() + Send + Sync + Clone + 'static,
    stop: Arc<AtomicBool>,
) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED).ok();
    }
    let mut last: Option<(IUpdateSession, IUpdateCollection)> = None;
    while let Ok(command) = inbox.recv() {
        match command {
            Command::Scan(plan) => {
                stop.store(false, Ordering::SeqCst);
                // Devices, the catalog and software run beside the slower Windows Update search.
                let side = {
                    let events = events.clone();
                    let wake = wake.clone();
                    let stop = stop.clone();
                    std::thread::spawn(move || device_scan(plan.catalog, events, wake, stop))
                };
                let apps = {
                    let events = events.clone();
                    let wake = wake.clone();
                    std::thread::spawn(move || {
                        let found = crate::software::find(plan.winget, plan.chocolatey);
                        // Every scan also asks whether DriverKeep itself has a later release.
                        let own = crate::selfupdate::check();
                        let found = match (found, own) {
                            (Ok(mut apps), Some(own)) => {
                                apps.push(own);
                                Ok(apps)
                            }
                            (Err(error), Some(own)) => {
                                let _ = events.send(Event::Note(error));
                                Ok(vec![own])
                            }
                            (found, None) => found,
                        };
                        let _ = events.send(Event::SoftwareDone(found));
                        wake();
                    })
                };
                if plan.drivers || plan.windows {
                    match search(&plan) {
                        Ok((session, updates, items)) => {
                            last = Some((session, updates));
                            let _ = events.send(Event::WindowsUpdateDone(Ok(items)));
                        }
                        Err(error) => {
                            let _ = events.send(Event::WindowsUpdateDone(Err(explain(&error))));
                        }
                    }
                } else {
                    let _ = events.send(Event::WindowsUpdateDone(Ok(Vec::new())));
                }
                let _ = side.join();
                let _ = apps.join();
            }
            Command::Install(plan) => {
                if last.is_none() && plan.items.iter().any(|i| i.update.source == Source::WindowsUpdate) {
                    let everything = ScanPlan {
                        drivers: true,
                        catalog: false,
                        windows: true,
                        optional: true,
                        winget: false,
                        chocolatey: false,
                    };
                    if let Ok((session, updates, _)) = search(&everything) {
                        last = Some((session, updates));
                    }
                }
                let outcome = install(last.as_ref(), plan, &events, &wake);
                let _ = events.send(Event::InstallFinished {
                    restart: outcome.0,
                    installed: outcome.1,
                    failed: outcome.2,
                });
            }
        }
        wake();
    }
}

fn device_scan(
    use_catalog: bool,
    events: Sender<Event>,
    wake: impl Fn() + Send + Sync,
    stop: Arc<AtomicBool>,
) {
    let found = devices::enumerate(|_, _| {});
    let list = found.as_ref().ok().cloned();
    let _ = events.send(Event::Devices(found));
    wake();
    let Some(list) = list else {
        let _ = events.send(Event::CatalogDone(Ok(Vec::new())));
        return;
    };
    if !use_catalog {
        let _ = events.send(Event::CatalogDone(Ok(Vec::new())));
        wake();
        return;
    }
    let sender = Mutex::new(events.clone());
    let result = catalog::find_updates(&list, &stop, |p| {
        if let Ok(tx) = sender.lock() {
            let _ = tx.send(Event::CatalogProgress { done: p.done, total: p.total, text: p.text });
        }
        wake();
    });
    let missed = catalog::MISSED.load(Ordering::SeqCst);
    if result.is_ok() && missed > 0 {
        let note = format!("{missed} devices couldn't be checked online this time. Scan again to retry them.");
        let _ = events.send(Event::Note(note));
    }
    let _ = events.send(Event::CatalogDone(result));
    wake();
}

use std::sync::Mutex;

fn explain(error: &windows::core::Error) -> String {
    match error.code().0 as u32 {
        0x8024402C | 0x80072EE7 | 0x80072EFD | 0x80072EE2 => {
            "Windows Update couldn't be reached.".into()
        }
        0x8024001E | 0x80070422 => "The Windows Update service is turned off on this PC.".into(),
        _ => format!("Windows Update couldn't finish its search ({error})."),
    }
}

fn search(plan: &ScanPlan) -> windows::core::Result<Found> {
    unsafe {
        let session: IUpdateSession = CoCreateInstance(&UpdateSession, None, CLSCTX_ALL)?;
        session.SetClientApplicationID(&BSTR::from(CLIENT_NAME))?;
        let searcher = session.CreateUpdateSearcher()?;
        searcher.SetServerSelection(ssWindowsUpdate)?;
        searcher.SetOnline(true.into())?;
        let found = searcher.Search(&BSTR::from(SEARCH_RULE))?;
        let updates = found.Updates()?;
        let mut items = Vec::new();
        for index in 0..updates.Count()? {
            let update = updates.get_Item(index)?;
            let is_driver = update.Type().map(|t| t == utDriver).unwrap_or(false);
            let wanted = if is_driver { plan.drivers } else { plan.windows };
            if !wanted {
                continue;
            }
            // "Optional" updates are previews Windows doesn't push by itself.
            if !is_driver && !plan.optional {
                let optional = update.cast::<IUpdate3>().and_then(|u| u.BrowseOnly());
                if optional.map(|v| v.as_bool()).unwrap_or(false) {
                    continue;
                }
            }
            let title = update.Title()?.to_string();
            let mut item = Update {
                id: update.Identity()?.UpdateID()?.to_string(),
                kind: if is_driver { Kind::Driver } else { Kind::Windows },
                source: Source::WindowsUpdate,
                version: version_from_title(&title),
                date: update.LastDeploymentChangeTime().map(ole_date).unwrap_or_default(),
                title,
                size: update.MaxDownloadSize().map(|d| d.Anonymous2.Lo64).unwrap_or(0),
                ..Default::default()
            };
            if !is_driver {
                items.push(item);
                continue;
            }
            if let Ok(driver) = update.cast::<IWindowsDriverUpdate>() {
                item.model = driver.DriverModel().map(|s| s.to_string()).unwrap_or_default();
                item.provider = driver.DriverProvider().map(|s| s.to_string()).unwrap_or_default();
                item.class = driver.DriverClass().map(|s| s.to_string()).unwrap_or_default();
                item.hardware_id = driver.DriverHardwareID().map(|s| s.to_string()).unwrap_or_default();
                item.date = driver.DriverVerDate().map(ole_date).unwrap_or_default();
            }
            items.push(item);
        }
        Ok((session, updates, items))
    }
}

fn single(update: &IUpdate) -> windows::core::Result<IUpdateCollection> {
    unsafe {
        let list: IUpdateCollection = CoCreateInstance(&UpdateCollection, None, CLSCTX_ALL)?;
        list.Add(update)?;
        Ok(list)
    }
}

fn find_update(updates: &IUpdateCollection, id: &str) -> Option<IUpdate> {
    let total = unsafe { updates.Count() }.unwrap_or(0);
    (0..total).find_map(|index| {
        let update = unsafe { updates.get_Item(index) }.ok()?;
        let same = unsafe { update.Identity().and_then(|u| u.UpdateID()) }.ok()?.to_string() == id;
        same.then_some(update)
    })
}

/// Download one update and install it. Gives back true when Windows needs a restart.
fn windows_update_install(
    session: &IUpdateSession,
    update: &IUpdate,
    note: &impl Fn(ItemState),
    progress: &impl Fn(u32),
) -> Result<bool, String> {
    use windows::Win32::System::Variant::VARIANT;
    let none = None::<&windows::core::IUnknown>;
    let wait = std::time::Duration::from_millis(300);
    unsafe {
        let list = single(update).map_err(|e| e.to_string())?;
        if !update.EulaAccepted().map(|v| v.as_bool()).unwrap_or(true) {
            update.AcceptEula().map_err(|e| e.to_string())?;
        }
        if !update.IsDownloaded().map(|v| v.as_bool()).unwrap_or(false) {
            note(ItemState::Downloading);
            progress(0);
            let downloader = session.CreateUpdateDownloader().map_err(|e| e.to_string())?;
            downloader.SetUpdates(&list).map_err(|e| e.to_string())?;
            // Started without callbacks, then asked how far along it is until it's done.
            let job = downloader
                .BeginDownload(none, none, &VARIANT::default())
                .map_err(|e| format!("Download failed ({e})"))?;
            while !job.IsCompleted().map(|v| v.as_bool()).unwrap_or(true) {
                if let Ok(done) = job.GetProgress().and_then(|p| p.PercentComplete()) {
                    progress(done.clamp(0, 100) as u32);
                }
                std::thread::sleep(wait);
            }
            let result = downloader.EndDownload(&job).map_err(|e| format!("Download failed ({e})"))?;
            let code = result.ResultCode().map_err(|e| e.to_string())?;
            if code != orcSucceeded && code != orcSucceededWithErrors {
                return Err(format!("Download failed (code {})", code.0));
            }
        }
        note(ItemState::Installing);
        progress(0);
        let installer = session.CreateUpdateInstaller().map_err(|e| e.to_string())?;
        installer.SetUpdates(&list).map_err(|e| e.to_string())?;
        let job = installer
            .BeginInstall(none, none, &VARIANT::default())
            .map_err(|e| format!("Install failed ({e})"))?;
        while !job.IsCompleted().map(|v| v.as_bool()).unwrap_or(true) {
            if let Ok(done) = job.GetProgress().and_then(|p| p.PercentComplete()) {
                progress(done.clamp(0, 100) as u32);
            }
            std::thread::sleep(wait);
        }
        let result = installer.EndInstall(&job).map_err(|e| format!("Install failed ({e})"))?;
        let code = result.ResultCode().map_err(|e| e.to_string())?;
        if code != orcSucceeded && code != orcSucceededWithErrors {
            let detail = result.HResult().unwrap_or(0);
            return Err(format!("Install failed (error 0x{detail:08X})"));
        }
        Ok(result.RebootRequired().map(|v| v.as_bool()).unwrap_or(false))
    }
}

fn install(
    found: Option<&(IUpdateSession, IUpdateCollection)>,
    mut plan: InstallPlan,
    events: &Sender<Event>,
    wake: &impl Fn(),
) -> (bool, usize, usize) {
    // The package manager goes near the end, and DriverKeep last because updating it closes this copy.
    plan.items.sort_by_key(|item| {
        (item.update.source == Source::GitHub, item.update.id == crate::software::PACKAGE_MANAGER_ID)
    });
    let status = |percent: u32, text: String| {
        let _ = events.send(Event::Status { percent, text });
        wake();
    };

    system::keep_awake(true);

    // Step 0: a Windows restore point, same as the original.
    let mut sequence = None;
    if plan.restore_point {
        status(2, "Creating a Windows restore point".into());
        let note = match system::begin_restore_point("Before installing driver updates") {
            RestorePoint::Created(number) => {
                sequence = Some(number);
                "Windows restore point created.".to_string()
            }
            RestorePoint::TurnedOff => "System Restore is off on this PC, so no restore point was made.".to_string(),
            RestorePoint::Failed(code) => format!("Windows couldn't make a restore point (code {code})."),
        };
        let _ = events.send(Event::Note(note));
    }

    let mut restart = false;
    let mut installed = 0;
    let mut failed = 0;
    let mut records = Vec::new();
    let count = plan.items.len().max(1) as u32;
    for (position, item) in plan.items.iter().enumerate() {
        let update = &item.update;
        let id = update.id.clone();
        let note = |state| {
            let _ = events.send(Event::Item { id: id.clone(), state });
            wake();
        };
        let progress = |percent: u32| {
            let _ = events.send(Event::ItemProgress { id: id.clone(), percent });
            wake();
        };
        status(5 + (position as u32) * 95 / count, update.title.clone());

        // A device whose own add-on driver can't be saved is left alone.
        let mut saved_to = None;
        if let Some(folder) = &plan.backup_folder {
            if item.devices.iter().any(|d| d.driver.is_some()) {
                note(ItemState::SavingBackup);
                let reason = format!("Before updating {}", item.devices[0].name);
                let saved = backup::create(&item.devices, folder, &reason, true, |_, _| {});
                let must_have = item
                    .devices
                    .iter()
                    .any(|d| d.driver.as_ref().is_some_and(|x| x.is_third_party()));
                let problem = match &saved {
                    Ok(done) if done.skipped.is_empty() => None,
                    Ok(done) => Some(format!("no driver file for {}", done.skipped.join(", "))),
                    Err(error) => Some(error.clone()),
                };
                if let Some(problem) = problem.filter(|_| must_have && plan.require_backup) {
                    failed += 1;
                    note(ItemState::Skipped(format!(
                        "The current driver couldn't be backed up ({problem}), so this was skipped. Use ··· to install it anyway."
                    )));
                    continue;
                }
                saved_to = saved.ok().map(|d| d.path.to_string_lossy().to_string());
            }
        }

        let result = match update.source {
            Source::WindowsUpdate => match found.and_then(|(s, u)| Some((s, find_update(u, &id)?))) {
                Some((session, chosen)) => windows_update_install(session, &chosen, &note, &progress),
                None => Err("Windows Update no longer offers this driver.".to_string()),
            },
            Source::Catalog => {
                note(ItemState::Downloading);
                catalog::install(update, &item.devices)
            }
            Source::Winget | Source::Chocolatey => {
                note(ItemState::Installing);
                crate::software::upgrade(update)
            }
            Source::GitHub => {
                note(ItemState::Downloading);
                // Saved first: setup closes this copy before the loop can finish.
                history::add(std::mem::take(&mut records));
                crate::selfupdate::install(update, plan.automatic, || {
                    note(ItemState::Installing);
                    status(
                        99,
                        "Installing the new version. Windows may ask for permission, then DriverKeep closes and reopens by itself.".into(),
                    );
                })
            }
        };
        match result {
            Ok(needs_restart) => {
                installed += 1;
                restart |= needs_restart;
                note(if needs_restart { ItemState::NeedsRestart } else { ItemState::Installed });
                let now = chrono::Local::now().timestamp();
                if item.devices.is_empty() {
                    records.push(Record {
                        when: now,
                        device: update.title.clone(),
                        instance_id: String::new(),
                        from_version: update.current.clone(),
                        to_version: update.version.clone(),
                        source: update.source.label().to_string(),
                        backup: None,
                        automatic: plan.automatic,
                    });
                }
                for device in &item.devices {
                    records.push(Record {
                        when: now,
                        device: device.name.clone(),
                        instance_id: device.instance_id.clone(),
                        from_version: device.driver.as_ref().map(|d| d.version.clone()).unwrap_or_default(),
                        to_version: update.version.clone(),
                        source: update.source.label().to_string(),
                        backup: saved_to.clone(),
                        automatic: plan.automatic,
                    });
                }
                // Saved straight away, so a run that gets interrupted keeps its record.
                history::add(std::mem::take(&mut records));
            }
            Err(error) => {
                failed += 1;
                note(ItemState::Failed(error));
            }
        }
    }
    if let Some(number) = sequence {
        system::end_restore_point(number);
    }
    history::add(records);
    system::keep_awake(false);
    status(100, "Finished".into());
    (restart, installed, failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(id: &str, source: Source, version: &str, devices: &[&str]) -> Update {
        Update {
            id: id.into(),
            source,
            version: version.into(),
            hardware_id: "x".into(),
            devices: devices.iter().map(|d| d.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn same_version_prefers_windows_update() {
        let merged = merge(
            vec![offer("c", Source::Catalog, "1.2", &["a"]), offer("w", Source::WindowsUpdate, "1.2", &["a"])],
            &[],
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id, "w");
    }

    #[test]
    fn newer_catalog_version_wins() {
        let merged = merge(
            vec![offer("w", Source::WindowsUpdate, "1.2", &["a"]), offer("c", Source::Catalog, "1.3", &["a"])],
            &[],
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id, "c");
    }

    #[test]
    fn ignored_updates_stay_hidden() {
        let merged = merge(vec![offer("w", Source::WindowsUpdate, "1.2", &["a"])], &["w".to_string()]);
        assert!(merged.is_empty());
    }

    #[test]
    fn title_gives_version() {
        assert_eq!(version_from_title("Intel - System - 10.1.19.1"), "10.1.19.1");
        assert_eq!(version_from_title("Maker Audio Driver Update (1.0.4.55794)"), "1.0.4.55794");
        assert_eq!(version_from_title("Some Driver Update"), "");
    }
}

//! The window. All real work happens in the other files, on background threads.

mod backups_page;
mod drivers_page;
mod look;
mod settings_page;
mod system_page;

pub use look::{bring_to_front, tray_icon, window_icon, write_icon};
use look::*;

use crate::Options;
use crate::agent;
use crate::backup::{self, BackupEntry, BackupFile, RestoreResult, RestoreState};
use crate::devices::{self, Device};
use crate::handoff;
use crate::history::{self, Record};
use crate::sysinfo::{self, SystemInfo};
use crate::system::{self, CloseChoice, Settings, Theme};
use crate::wu::{self, Command, Event, InstallItem, InstallPlan, ItemState, Kind, SCAN_CACHE, ScanPlan, Source, Update};
use egui::{Align, Align2, Color32, FontId, Frame, Layout, Margin, RichText, Sense, Vec2};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

/// An install the window handed to the background helper, while it's still going.
struct HelperJob {
    request: u64,
    started: std::time::Instant,
    ids: Vec<String>,
    skip_backup: bool,
    /// Updates ignored while the helper works, and whether the person asked to stop.
    left_out: Vec<String>,
    /// Done by a copy started through Windows' permission box, not the background helper.
    worker: bool,
}

/// What the Check for Updates button found out.
#[derive(Clone, Copy, PartialEq)]
enum AppCheck {
    Latest,
    Available,
    Failed,
}

/// The calendar day, in this computer's time zone, that a saved time falls on.
fn day_of(unix: i64) -> chrono::NaiveDate {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(unix, 0)
        .single()
        .map(|t| t.date_naive())
        .unwrap_or_else(|| chrono::Local::now().date_naive())
}

/// The first day of the month a date is in.
fn month_of(day: chrono::NaiveDate) -> chrono::NaiveDate {
    use chrono::Datelike;
    day.with_day(1).unwrap_or(day)
}

/// The widest the page content gets, in points. Wider windows get empty sides.
const MAX_CONTENT_WIDTH: f32 = 1000.0;

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Drivers,
    System,
    Backups,
    Settings,
}

#[derive(Clone, Copy, PartialEq)]
enum Show {
    All,
    Connected,
    Past,
    NeedsDriver,
}

enum Work {
    /// The answer to Settings > About > Check for Updates.
    AppUpdate(Result<Option<Update>, String>),
    Devices(Vec<Device>),
    System(SystemInfo),
    Progress(u32, String),
    BackupDone(Result<backup::BackupDone, String>),
    RestoreDone(Result<Vec<RestoreResult>, String>),
    RollbackDone(Result<bool, String>, String),
    HelperTask(bool, bool),
    Tools(bool, bool),
    ToolsInstalled(bool, bool, bool),
}

#[derive(PartialEq, Clone, Copy)]
enum Busy {
    Idle,
    Scanning,
    Installing,
    BackingUp,
    Restoring,
}

struct Row {
    update: Update,
    selected: bool,
    state: ItemState,
    /// Download or install progress, when the source reports it.
    percent: Option<u32>,
}

enum Ask {
    Update(Vec<String>),
    UpdateNoBackup(String),
    Restore(PathBuf, Vec<BackupEntry>),
    Delete(PathBuf),
    Rollback(String, String),
    HelperTask(bool),
    Close,
    CloseWhileWorking,
    /// Whether to install the Windows Package Manager, and Chocolatey.
    InstallTools(bool, bool),
}

pub struct App {
    options: Options,
    settings: Settings,
    page: Page,
    admin: bool,
    busy: Busy,
    percent: u32,
    status: String,
    message: Option<(String, bool)>,

    devices: Vec<Device>,
    system: Option<SystemInfo>,
    raw: Vec<Update>,
    rows: Vec<Row>,
    searched: bool,
    catalog_pending: bool,
    windows_update_pending: bool,
    software_pending: bool,
    flash_app_update: bool,
    discard: u32,
    notes: Vec<String>,
    show_uptodate: bool,
    collapsed: HashSet<Kind>,
    tools: Option<(bool, bool)>,
    tools_installing: bool,
    settings_tab: settings_page::SettingsTab,

    filter: String,
    show: Show,
    show_system_items: bool,
    picked_device: Option<String>,

    backup_picks: HashSet<String>,
    backup_picks_ready: bool,
    choosing: bool,
    show_hidden_backups: bool,
    backups: Vec<BackupFile>,
    open_backup: Option<PathBuf>,
    restore_picks: HashSet<String>,
    results: Vec<RestoreResult>,
    history: Vec<Record>,
    restart_needed: bool,
    ask: Option<Ask>,
    closing: bool,
    remember_close: bool,
    stop_reminding: bool,

    tx: Sender<Work>,
    rx: Receiver<Work>,
    engine: wu::Engine,
    events: Receiver<Event>,

    windows_accent: Option<[u8; 3]>,
    windows_accent_read: Option<std::time::Instant>,
    helper_job: Option<HelperJob>,
    /// Cancel Updates was pressed; the current update is finishing or being dropped.
    cancelling: bool,
    /// The Backups page was asked to scroll down to the saved backups.
    jump_to_backups: bool,
    /// What the running job was asked to install, so the sidebar can count down what's left.
    job_ids: Vec<String>,
    /// Update Everything is offered after a scan, and goes away once a job has run.
    updates_ready: bool,
    checking_app: bool,
    app_check: Option<(String, AppCheck)>,
    preview_percent: f32,
    preview_running: bool,
    preview_on: bool,
    bring_forward: bool,
    calendar_month: chrono::NaiveDate,
    selected_day: Option<chrono::NaiveDate>,
    spin: f32,
    frames: u32,
    shot_sent: bool,
    shot_done: Arc<AtomicBool>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, options: Options) -> Self {
        install_look(&cc.egui_ctx);
        let (tx, rx) = channel();
        let (event_tx, events) = channel();
        let wake = cc.egui_ctx.clone();
        let engine = wu::start(event_tx, move || wake.request_repaint());
        let settings = Settings::load();
        let page = match options.page.as_deref() {
            Some("system") | Some("hardware") => Page::System,
            Some("backups") | Some("backup") | Some("restore") => Page::Backups,
            Some(page) if page.starts_with("settings") || page == "about" => Page::Settings,
            _ => Page::Drivers,
        };
        let backups = backup::list(&settings.backup_folder);
        let raw: Vec<Update> = system::read_cache(SCAN_CACHE).unwrap_or_default();
        let searched = settings.last_scan.is_some();
        let past_changes = history::load();
        let latest_change_day = past_changes.iter().map(|r| day_of(r.when)).max();
        let mut app = Self {
            settings,
            page,
            admin: system::is_admin(),
            busy: Busy::Idle,
            percent: 0,
            status: String::new(),
            message: None,
            devices: Vec::new(),
            system: system::read_cache(sysinfo::CACHE_FILE),
            raw,
            rows: Vec::new(),
            searched,
            catalog_pending: false,
            windows_update_pending: false,
            software_pending: false,
            flash_app_update: false,
            discard: 0,
            notes: Vec::new(),
            show_uptodate: false,
            collapsed: HashSet::new(),
            tools: None,
            tools_installing: false,
            settings_tab: settings_page::SettingsTab::from_page(options.page.as_deref()),
            filter: String::new(),
            show: Show::All,
            show_system_items: false,
            picked_device: None,
            backup_picks: HashSet::new(),
            backup_picks_ready: false,
            choosing: false,
            show_hidden_backups: false,
            backups,
            open_backup: None,
            restore_picks: HashSet::new(),
            results: Vec::new(),
            history: past_changes,
            restart_needed: false,
            ask: None,
            closing: false,
            remember_close: false,
            stop_reminding: false,
            tx,
            rx,
            engine,
            events,
            windows_accent: None,
            windows_accent_read: None,
            helper_job: None,
            cancelling: false,
            jump_to_backups: false,
            job_ids: Vec::new(),
            updates_ready: true,
            checking_app: false,
            app_check: None,
            preview_percent: 60.0,
            preview_running: false,
            preview_on: true,
            bring_forward: false,
            calendar_month: month_of(latest_change_day.unwrap_or_else(|| chrono::Local::now().date_naive())),
            selected_day: latest_change_day,
            spin: 0.0,
            frames: 0,
            shot_sent: false,
            shot_done: Arc::new(AtomicBool::new(false)),
            options,
        };
        // After a self-update the window always comes back to the front, even if it was in the tray.
        if app.options.screenshot.is_none() {
            if let Some(version) = system::take_update_marker() {
                let text = if version.is_empty() {
                    "DriverKeep was updated.".to_string()
                } else {
                    format!("DriverKeep was updated to version {version}.")
                };
                app.message = Some((text, false));
                app.bring_forward = true;
            }
        }
        // Show the last results straight away. Without this the window starts on the empty "Scan"
        // page until the hardware list has loaded, then jumps to the updates.
        app.rebuild_rows();
        app.refresh_devices(&cc.egui_ctx);
        app.refresh_system(&cc.egui_ctx);
        app.refresh_tools(&cc.egui_ctx);
        // Scheduled scans belong to the background helper; the window only searches on open if asked.
        let on_open = app.options.screenshot.is_none() && app.settings.scan_on_open;
        if app.options.full_scan || app.options.resume.is_some() || on_open {
            app.start_scan();
        }
        if app.options.page.as_deref() == Some("closing") {
            app.ask = Some(Ask::CloseWhileWorking);
        }
        if app.options.page.as_deref() == Some("tools") {
            app.ask = Some(Ask::InstallTools(true, true));
        }
        if app.options.page.as_deref() == Some("no-backup") {
            app.ask = Some(Ask::UpdateNoBackup(String::new()));
        }
        // Keeps Windows' startup entry pointing at this copy, for example after an upgrade.
        if app.settings.wants_agent() && app.options.screenshot.is_none() {
            let settings = app.settings.clone();
            std::thread::spawn(move || system::apply_startup(&settings));
        }
        app
    }

    fn check_app_update(&mut self, ctx: &egui::Context) {
        if self.checking_app {
            return;
        }
        self.checking_app = true;
        self.app_check = None;
        let tx = self.tx.clone();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Work::AppUpdate(crate::selfupdate::check_now()));
            wake.request_repaint();
        });
    }

    fn refresh_devices(&self, ctx: &egui::Context) {
        let tx = self.tx.clone();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            if let Ok(list) = devices::enumerate(|_, _| {}) {
                let _ = tx.send(Work::Devices(list));
                wake.request_repaint();
            }
        });
    }

    fn refresh_tools(&self, ctx: &egui::Context) {
        let tx = self.tx.clone();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let (winget, chocolatey) = crate::software::tools_ready();
            let _ = tx.send(Work::Tools(winget, chocolatey));
            wake.request_repaint();
        });
    }

    fn refresh_system(&self, ctx: &egui::Context) {
        let tx = self.tx.clone();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            if let Ok(info) = sysinfo::gather() {
                system::write_cache(sysinfo::CACHE_FILE, &info);
                let _ = tx.send(Work::System(info));
                wake.request_repaint();
            }
        });
    }

    fn start_scan(&mut self) {
        // Searching while something installs (or another search runs) breaks both.
        if self.busy != Busy::Idle {
            return;
        }
        self.busy = Busy::Scanning;
        self.percent = 2;
        self.status = "Looking through your hardware".into();
        self.message = None;
        self.notes.clear();
        self.catalog_pending = true;
        self.windows_update_pending = true;
        self.software_pending = true;
        self.raw.clear();
        self.rebuild_rows();
        self.page = Page::Drivers;
        let _ = self.engine.commands.send(Command::Scan(ScanPlan::from_settings(&self.settings)));
    }

    fn stop_scan(&mut self) {
        self.engine.stop.store(true, Ordering::SeqCst);
        self.discard = self.catalog_pending as u32
            + self.windows_update_pending as u32
            + self.software_pending as u32;
        self.catalog_pending = false;
        self.windows_update_pending = false;
        self.software_pending = false;
        self.busy = Busy::Idle;
    }

    fn picked_by_default(&self, kind: Kind) -> bool {
        match kind {
            Kind::Driver => self.settings.pick_drivers,
            Kind::Windows => self.settings.pick_windows,
            Kind::Software => self.settings.pick_software,
        }
    }

    fn rebuild_rows(&mut self) {
        let old = std::mem::take(&mut self.rows);
        let merged = agent::attach(self.raw.clone(), &self.devices, &self.settings);
        self.rows = merged
            .into_iter()
            .map(|update| {
                let before = old.iter().find(|r| r.update.id == update.id);
                Row {
                    selected: before.map(|r| r.selected).unwrap_or(self.picked_by_default(update.kind)),
                    state: before.map(|r| r.state.clone()).unwrap_or(ItemState::Waiting),
                    percent: before.and_then(|r| r.percent),
                    update,
                }
            })
            .collect();
    }

    fn scan_part_done(&mut self, ctx: &egui::Context) {
        self.rebuild_rows();
        if !self.catalog_pending && !self.windows_update_pending && !self.software_pending {
            self.searched = true;
            self.updates_ready = true;
            if self.busy == Busy::Scanning {
                self.busy = Busy::Idle;
            }
            self.settings.last_scan = Some(chrono::Local::now().timestamp());
            self.settings.save();
            let updates: Vec<Update> = self.rows.iter().map(|r| r.update.clone()).collect();
            system::write_cache(SCAN_CACHE, &updates);
            self.take_resume(ctx);
            let own = self.rows.iter().find(|r| r.update.source == Source::GitHub).map(|r| r.update.id.clone());
            if let Some(id) = own {
                if self.settings.auto_update_app && system::is_installed() {
                    if self.busy == Busy::Idle {
                        self.run_update(vec![id], false);
                    }
                } else {
                    // A new DriverKeep can't be missed: the taskbar button flashes once.
                    self.flash_app_update = true;
                }
            }
        }
        // Nothing is shown until every part of the search has finished.
    }

    fn counts(&self) -> (usize, usize, usize) {
        let mut now = 0;
        let mut past = 0;
        let mut needs = 0;
        for d in self.devices.iter().filter(|d| !d.system_item) {
            if d.present {
                now += 1;
            } else {
                past += 1;
            }
            if d.needs_driver() {
                needs += 1;
            }
        }
        (now, past, needs)
    }

    fn open_rows(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| !matches!(r.state, ItemState::Installed | ItemState::NeedsRestart))
            .count()
    }

    fn pick_default_backups(&mut self) {
        if self.backup_picks_ready {
            return;
        }
        self.backup_picks_ready = true;
        if !self.settings.backup_select_all {
            return;
        }
        let picks: Vec<String> = self
            .devices
            .iter()
            .filter(|d| self.backup_visible(d))
            .map(|d| d.instance_id.clone())
            .collect();
        self.backup_picks.extend(picks);
    }

    /// The original hides unplugged devices and Windows' own drivers from Backup until asked.
    fn backup_visible(&self, device: &Device) -> bool {
        let hidden = !device.present || !device.driver.as_ref().is_some_and(|d| d.is_third_party());
        !device.system_item && device.driver.is_some() && (self.show_hidden_backups || !hidden)
    }

    fn reload_backups(&mut self) {
        self.backups = backup::list(&self.settings.backup_folder);
        self.history = history::load();
    }

    fn apply_settings(&self) {
        self.settings.save();
        let settings = self.settings.clone();
        std::thread::spawn(move || system::apply_startup(&settings));
    }

    fn receive(&mut self, ctx: &egui::Context) {
        self.poll_helper_job(ctx);
        while let Ok(work) = self.rx.try_recv() {
            match work {
                Work::AppUpdate(result) => {
                    self.checking_app = false;
                    // Shown beside the Check for Updates button in Settings > About.
                    self.app_check = Some(match result {
                        Ok(Some(update)) => {
                            let text = format!("Version {} is available", update.version);
                            self.raw.retain(|u| u.source != Source::GitHub);
                            self.raw.push(update);
                            self.rebuild_rows();
                            self.flash_app_update = true;
                            (text, AppCheck::Available)
                        }
                        Ok(None) => (
                            format!("You have the latest version ({})", crate::selfupdate::CURRENT),
                            AppCheck::Latest,
                        ),
                        Err(_) => ("Couldn't check right now".to_string(), AppCheck::Failed),
                    });
                }
                Work::Devices(list) => {
                    self.devices = list;
                    self.pick_default_backups();
                    self.rebuild_rows();
                }
                Work::System(info) => self.system = Some(info),
                Work::Tools(winget, chocolatey) => {
                    self.tools = Some((winget, chocolatey));
                    // Offered once, on first launch, when something's missing.
                    let missing = !winget || !chocolatey;
                    let quiet = self.options.screenshot.is_some() || self.options.resume.is_some();
                    if missing && !self.settings.tools_offered && !quiet && self.ask.is_none() {
                        self.ask = Some(Ask::InstallTools(!winget, !chocolatey));
                    }
                }
                Work::ToolsInstalled(ran, winget, chocolatey) => {
                    self.tools = Some((winget, chocolatey));
                    self.tools_installing = false;
                    self.message = Some(match (ran, winget, chocolatey) {
                        (false, ..) => ("Windows didn't give permission, so nothing was changed.".into(), true),
                        (_, true, true) => ("Package managers are ready. Scan Again to see updates for your programs.".into(), false),
                        (_, false, _) => ("The Windows Package Manager couldn't be installed. You can get it from the Microsoft Store in Settings.".into(), true),
                        (_, true, false) => ("Chocolatey couldn't be installed. You can try again in Settings.".into(), true),
                    });
                }
                Work::Progress(percent, text) => {
                    self.percent = percent;
                    self.status = text;
                }
                Work::BackupDone(result) => {
                    self.busy = Busy::Idle;
                    self.message = Some(match result {
                        Ok(done) => {
                            let mut text = format!(
                                "Saved {} driver{} ({}).",
                                done.saved,
                                plural(done.saved),
                                size_text(done.bytes)
                            );
                            if !done.skipped.is_empty() {
                                text.push_str(&format!(" Couldn't save: {}.", done.skipped.join(", ")));
                            }
                            (text, !done.skipped.is_empty())
                        }
                        Err(error) => (error, true),
                    });
                    self.reload_backups();
                }
                Work::RestoreDone(result) => {
                    self.busy = Busy::Idle;
                    match result {
                        Ok(items) => {
                            self.restart_needed |= items
                                .iter()
                                .any(|r| matches!(r.state, RestoreState::Restored { restart: true, .. }));
                            self.results = items;
                        }
                        Err(error) => self.message = Some((error, true)),
                    }
                    self.refresh_devices(ctx);
                }
                Work::RollbackDone(result, name) => {
                    self.busy = Busy::Idle;
                    self.message = Some(match result {
                        Ok(restart) => {
                            self.restart_needed |= restart;
                            (format!("{name} is back on its previous driver."), false)
                        }
                        Err(error) => (error, true),
                    });
                    self.refresh_devices(ctx);
                }
                Work::HelperTask(wanted, worked) => {
                    self.busy = Busy::Idle;
                    self.settings.silent_install = wanted && worked;
                    if wanted && !worked {
                        self.message = Some((
                            "Silent installs stayed off because Windows didn't give permission.".into(),
                            true,
                        ));
                    }
                    self.apply_settings();
                }
            }
        }

        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Devices(Ok(list)) => {
                    self.devices = list;
                    self.pick_default_backups();
                    self.rebuild_rows();
                    if self.busy == Busy::Scanning {
                        self.percent = 10;
                        self.status = "Searching for newer drivers".into();
                    }
                }
                Event::Devices(Err(error)) => self.notes.push(error),
                Event::CatalogProgress { done, total, text } => {
                    if self.busy == Busy::Scanning && self.discard == 0 {
                        self.percent = 10 + (done * 70 / total.max(1)) as u32;
                        self.status = text;
                    }
                }
                Event::CatalogDone(result) => {
                    if self.discard > 0 {
                        self.discard -= 1;
                        continue;
                    }
                    self.catalog_pending = false;
                    match result {
                        Ok(found) => self.raw.extend(found),
                        Err(error) => self.notes.push(error),
                    }
                    if self.busy == Busy::Scanning {
                        self.percent = self.percent.max(82);
                        self.status = "Asking Windows Update".into();
                    }
                    self.scan_part_done(ctx);
                }
                Event::WindowsUpdateDone(result) => {
                    if self.discard > 0 {
                        self.discard -= 1;
                        continue;
                    }
                    self.windows_update_pending = false;
                    match result {
                        Ok(found) => self.raw.extend(found),
                        Err(error) => self.notes.push(error),
                    }
                    self.scan_part_done(ctx);
                }
                Event::SoftwareDone(result) => {
                    if self.discard > 0 {
                        self.discard -= 1;
                        continue;
                    }
                    self.software_pending = false;
                    match result {
                        Ok(found) => self.raw.extend(found),
                        Err(error) => self.notes.push(error),
                    }
                    self.scan_part_done(ctx);
                }
                Event::Status { percent, text } => {
                    self.percent = percent;
                    self.status = text;
                }
                Event::Item { id, state } => {
                    if let Some(row) = self.rows.iter_mut().find(|r| r.update.id == id) {
                        row.state = state;
                        row.percent = None;
                    }
                }
                Event::ItemProgress { id, percent } => {
                    if let Some(row) = self.rows.iter_mut().find(|r| r.update.id == id) {
                        row.percent = Some(percent);
                    }
                }
                Event::Note(text) => self.notes.push(text),
                Event::InstallFinished { restart, .. } => {
                    self.busy = Busy::Idle;
                    self.updates_ready = false;
                    self.cancelling = false;
                    self.restart_needed |= restart;
                    self.reload_backups();
                    self.refresh_devices(ctx);
                }
            }
        }
    }

    /// Carries on with what was asked for before Windows gave administrator rights.
    fn take_resume(&mut self, ctx: &egui::Context) {
        let Some(job) = self.options.resume.take() else { return };
        if let Some(ids) = job.strip_prefix("update:") {
            self.page = Page::Drivers;
            self.run_update(ids.split(',').map(str::to_string).collect(), false);
        } else if let Some(id) = job.strip_prefix("update-no-backup:") {
            self.page = Page::Drivers;
            self.run_update(vec![id.to_string()], true);
        } else if let Some(rest) = job.strip_prefix("restore:") {
            self.page = Page::Backups;
            if let Some((path, ids)) = rest.split_once('|') {
                let path = PathBuf::from(path);
                let wanted: HashSet<&str> = ids.split('\t').collect();
                if let Ok(info) = backup::read_info(&path) {
                    let entries = info
                        .entries
                        .into_iter()
                        .filter(|e| wanted.contains(e.instance_id.as_str()))
                        .collect();
                    self.run_restore(path, entries, ctx);
                }
            }
        } else if let Some(id) = job.strip_prefix("rollback:") {
            self.page = Page::System;
            let name = self
                .devices
                .iter()
                .find(|d| d.instance_id == id)
                .map(|d| d.name.clone())
                .unwrap_or_else(|| "The device".into());
            self.run_rollback(id.to_string(), name, ctx);
        }
    }

    fn run_update(&mut self, ids: Vec<String>, skip_backup: bool) {
        if self.busy != Busy::Idle {
            return;
        }
        self.notes.clear();
        self.engine.reset_install();
        self.cancelling = false;
        self.job_ids = ids.clone();
        self.busy = Busy::Installing;
        self.percent = 1;
        self.status = "Getting ready".into();
        let items = self
            .rows
            .iter()
            .filter(|r| ids.contains(&r.update.id))
            .map(|r| InstallItem {
                update: r.update.clone(),
                devices: self
                    .devices
                    .iter()
                    .filter(|d| r.update.devices.contains(&d.instance_id))
                    .cloned()
                    .collect(),
            })
            .collect();
        let today = system::restore_point_day();
        // Like the original: one restore point a day is enough.
        let only_this_app = self
            .rows
            .iter()
            .filter(|r| ids.contains(&r.update.id))
            .all(|r| r.update.source == Source::GitHub);
        let restore_point = self.settings.make_restore_point
            && self.settings.last_restore_point_day != today
            && !only_this_app;
        if restore_point {
            self.settings.last_restore_point_day = today;
            self.settings.save();
        }
        let _ = self.engine.commands.send(Command::Install(InstallPlan {
            items,
            restore_point,
            backup_folder: (self.settings.backup_before_update && !skip_backup)
                .then(|| self.settings.backup_folder.clone()),
            require_backup: !self.settings.install_without_backup,
            automatic: false,
        }));
    }

    fn run_backup(&mut self, ctx: &egui::Context) {
        let chosen: Vec<Device> = self
            .devices
            .iter()
            .filter(|d| self.backup_picks.contains(&d.instance_id))
            .cloned()
            .collect();
        self.message = None;
        self.busy = Busy::BackingUp;
        self.percent = 0;
        self.status = "Gathering drivers".into();
        let folder = self.settings.backup_folder.clone();
        let tx = self.tx.clone();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let update = tx.clone();
            let tick = wake.clone();
            let result = backup::create(&chosen, &folder, "Backup you asked for", false, |p, t| {
                let _ = update.send(Work::Progress(p, t.to_string()));
                tick.request_repaint();
            });
            let _ = tx.send(Work::BackupDone(result));
            wake.request_repaint();
        });
    }

    fn run_restore(&mut self, path: PathBuf, entries: Vec<BackupEntry>, ctx: &egui::Context) {
        self.results.clear();
        self.message = None;
        self.busy = Busy::Restoring;
        self.percent = 0;
        self.status = "Getting ready".into();
        let tx = self.tx.clone();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let update = tx.clone();
            let tick = wake.clone();
            let result = backup::restore(&path, &entries, |p, t| {
                let _ = update.send(Work::Progress(p, t.to_string()));
                tick.request_repaint();
            });
            let _ = tx.send(Work::RestoreDone(result));
            wake.request_repaint();
        });
    }

    fn run_rollback(&mut self, id: String, name: String, ctx: &egui::Context) {
        self.busy = Busy::Restoring;
        self.status = format!("Rolling back {name}");
        let tx = self.tx.clone();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Work::RollbackDone(devices::roll_back(&id), name));
            wake.request_repaint();
        });
    }

    /// Installs now when this window is already an administrator. Otherwise the background helper
    /// does it, if it's switched on in Settings, so nothing restarts. Only when neither is
    /// possible does Windows show its permission box and the program reopens.
    fn install_or_elevate(&mut self, ids: Vec<String>, skip_backup: bool, ctx: &egui::Context) {
        if self.admin {
            self.run_update(ids, skip_backup);
        } else if !self.hand_to_helper(&ids, skip_backup, true) {
            // Windows' permission box starts an administrator copy; progress is read in poll_helper_job.
            self.message = Some(("Windows didn't give permission, so nothing was changed.".into(), true));
        }
        let _ = ctx;
    }

    /// Hands the install to something that already has (or is about to get) administrator rights,
    /// and follows its progress. With `worker` set, Windows' permission box appears and a short-lived
    /// administrator copy does the work while this window stays open. Gives false if that was refused.
    fn hand_to_helper(&mut self, ids: &[String], skip_backup: bool, worker: bool) -> bool {
        let request = handoff::Request { id: handoff::new_request_id(), ids: ids.to_vec(), skip_backup, worker };
        if !handoff::send(&request) {
            return false;
        }
        if worker {
            // The permission box. If it's declined, nothing was started.
            if !system::run_as_admin("--install-job", false) {
                handoff::withdraw();
                return false;
            }
        } else {
            // Wakes the helper if it isn't running yet; it picks the request up when it starts.
            system::start_agent();
        }
        self.notes.clear();
        self.job_ids = ids.to_vec();
        self.busy = Busy::Installing;
        self.percent = 1;
        self.status = if worker {
            "Starting the installer with administrator rights".into()
        } else {
            "Asking the background helper to install. No restart is needed.".into()
        };
        self.helper_job = Some(HelperJob {
            request: request.id,
            started: std::time::Instant::now(),
            ids: ids.to_vec(),
            skip_backup,
            left_out: Vec::new(),
            worker,
        });
        self.cancelling = false;
        true
    }

    /// Stops the job: the update being downloaded is dropped, one already installing is allowed
    /// to finish, and nothing after it starts. The buttons come back when it has stopped.
    fn cancel_updates(&mut self) {
        if self.busy != Busy::Installing || self.cancelling {
            return;
        }
        self.cancelling = true;
        self.status = "Cancelling. Waiting for the current update to stop or finish.".into();
        if let Some(job) = &self.helper_job {
            handoff::send_control(&handoff::Control { request: job.request, cancel: true, skip: job.left_out.clone() });
        } else {
            self.engine.cancel_install();
        }
    }

    /// Hides an update for good. If it's part of the running job it's left out of that too.
    fn ignore_update(&mut self, id: String) {
        if self.busy == Busy::Installing {
            self.engine.skip_update(&id);
            if let Some(job) = &mut self.helper_job {
                job.left_out.push(id.clone());
                handoff::send_control(&handoff::Control {
                    request: job.request,
                    cancel: self.cancelling,
                    skip: job.left_out.clone(),
                });
            }
        }
        self.settings.ignored.push(id);
        self.settings.save();
        self.rebuild_rows();
    }

    /// Follows the helper's progress file while it installs, and falls back to the permission
    /// box if the helper never answers or says it can't.
    fn poll_helper_job(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.helper_job else { return };
        ctx.request_repaint_after(std::time::Duration::from_millis(400));
        let (request, waited) = (job.request, job.started.elapsed());
        let Some(progress) = handoff::read_progress(request) else {
            if waited.as_secs() >= 30 {
                self.give_up_on_helper(ctx, "The background helper didn't answer.");
            }
            return;
        };
        if progress.refused {
            self.give_up_on_helper(ctx, &progress.text);
            return;
        }
        for item in &progress.items {
            if let Some(row) = self.rows.iter_mut().find(|r| r.update.id == item.id) {
                row.state = item.to_state();
                row.percent = item.percent;
            }
        }
        self.percent = progress.percent.max(1);
        if !progress.text.is_empty() {
            self.status = progress.text.clone();
        }
        self.notes = progress.notes.clone();
        if progress.finished {
            self.helper_job = None;
            self.busy = Busy::Idle;
            self.updates_ready = false;
            self.cancelling = false;
            self.restart_needed |= progress.restart;
            self.reload_backups();
            self.refresh_devices(ctx);
        } else if chrono::Local::now().timestamp() - progress.updated > 120 {
            // Nothing for two minutes: the helper has stopped.
            self.helper_job = None;
            self.busy = Busy::Idle;
            self.cancelling = false;
            self.message = Some(("The background helper stopped responding. Try the update again.".into(), true));
        }
    }

    fn give_up_on_helper(&mut self, ctx: &egui::Context, reason: &str) {
        let Some(job) = self.helper_job.take() else { return };
        self.busy = Busy::Idle;
        let _ = ctx;
        handoff::withdraw();
        if job.worker {
            self.message = Some((format!("{reason} Nothing was installed. Try again."), true));
        } else {
            // The background helper isn't available, so Windows' permission box is used instead.
            self.notes.push(format!("{reason} Using Windows' permission box instead."));
            if !self.hand_to_helper(&job.ids, job.skip_backup, true) {
                self.message = Some(("Windows didn't give permission, so nothing was changed.".into(), true));
            }
        }
    }

    fn elevate(&mut self, job: String, ctx: &egui::Context) {
        if system::run_as_admin(&format!("--resume \"{job}\""), false) {
            self.closing = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            self.message = Some(("Windows didn't give permission, so nothing was changed.".into(), true));
        }
    }

    fn rail(&mut self, ui: &mut egui::Ui) {
        ui.add_space(14.0);
        let installing = self.busy == Busy::Installing;
        // During a job the number counts down from what it started with to what's left.
        let badge = if installing {
            self.rows
                .iter()
                .filter(|r| {
                    self.job_ids.contains(&r.update.id)
                        && matches!(
                            r.state,
                            ItemState::Waiting | ItemState::SavingBackup | ItemState::Downloading | ItemState::Installing
                        )
                })
                .count()
        } else {
            self.open_rows()
        };
        self.rail_item(ui, Page::Drivers, ICON_DRIVERS, "Updates", badge, installing);
        self.rail_item(ui, Page::Backups, ICON_BACKUP, "Backups", 0, false);
        self.rail_item(ui, Page::System, ICON_SYSTEM, "System Info", 0, false);
        let space = ui.available_height() - 80.0;
        ui.add_space(space.max(0.0));
        self.rail_item(ui, Page::Settings, ICON_SETTINGS, "Settings", 0, false);
    }

    fn rail_item(&mut self, ui: &mut egui::Ui, page: Page, glyph: &str, label: &str, count: usize, spinning: bool) {
        let width = ui.available_width();
        // Tall enough that the count badge sits fully inside the highlighted box.
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 72.0), Sense::click());
        let inner = rect.shrink2(Vec2::new(6.0, 3.0));
        let chosen = self.page == page;
        let p = pal();
        if chosen {
            // The page you're on is the same solid accent color as the buttons.
            ui.painter().rect_filled(inner, 10.0, teal());
        } else if response.hovered() {
            ui.painter().rect_filled(inner, 10.0, p.hover);
        }
        let tint = if chosen { Color32::WHITE } else { p.soft };
        let glyph_at = inner.center() - Vec2::new(0.0, 9.0);
        if spinning {
            // The two arrows turn around the middle while a job runs, and sit still otherwise.
            let galley = ui.painter().layout_no_wrap(glyph.to_string(), icon_font(19.0), tint);
            let angle = ui.input(|i| i.time) as f32 * 2.4;
            let shape = egui::epaint::TextShape::new(glyph_at - galley.size() / 2.0, galley, tint)
                .with_angle_and_anchor(angle, Align2::CENTER_CENTER);
            ui.painter().add(shape);
            ui.ctx().request_repaint();
        } else {
            ui.painter().text(glyph_at, Align2::CENTER_CENTER, glyph, icon_font(19.0), tint);
        }
        ui.painter().text(
            inner.center() + Vec2::new(0.0, 13.0),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(11.5),
            if chosen { Color32::WHITE } else { p.soft },
        );
        if count > 0 {
            // The count sits inside the circle of arrows and stays upright while they turn. It's the
            // accent color like the "4 available" in the header, or white on the selected tab.
            let ink = if chosen { Color32::WHITE } else { teal() };
            let size = if count >= 10 { 8.5 } else { 10.0 };
            ui.painter().text(glyph_at + Vec2::new(0.5, -1.0), Align2::CENTER_CENTER, count.to_string(), bold(size), ink);
        }
        if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            self.page = page;
        }
    }

    fn banners(&mut self, ui: &mut egui::Ui) {
        if self.restart_needed {
            card().fill(CAUTION.gamma_multiply(0.16)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    beside(ui, 190.0, |ui| {
                        ui.label("Restart your computer to finish the driver changes.");
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if secondary(ui, "Restart in 1 minute", true) {
                            system::restart_windows(60, "DriverKeep is finishing driver updates.");
                        }
                    });
                });
            });
            ui.add_space(8.0);
        }
        if let Some((text, bad)) = self.message.clone() {
            let color = if bad { CRITICAL } else { teal() };
            let mut close = false;
            card().fill(color.gamma_multiply(0.16)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    beside(ui, 90.0, |ui| {
                        ui.label(text);
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        close = ui.small_button("Dismiss").clicked();
                    });
                });
            });
            if close {
                self.message = None;
            }
            ui.add_space(8.0);
        }
    }

    fn working_card(&self, ui: &mut egui::Ui) {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(15.0).color(teal()));
                ui.add(egui::Label::new(self.status.clone()).truncate());
            });
            ui.add_space(2.0);
            progress_bar(ui, self.percent);
        });
        ui.add_space(8.0);
    }

    fn working(&self) -> bool {
        self.busy != Busy::Idle
            || self.windows_update_pending
            || self.catalog_pending
            || self.software_pending
    }

    fn close_while_working(&mut self, ctx: &egui::Context) {
        let installing = matches!(self.busy, Busy::Installing | Busy::Restoring);
        let (title, body) = if installing {
            (
                "Updates are still installing",
                "Closing now could leave an update half finished. It's safest to wait until it's done.",
            )
        } else {
            ("A search or backup is still running", "Closing now will stop it.")
        };
        let mut wait = false;
        let mut close = false;
        egui::Modal::new(egui::Id::unique("close_working")).show(ctx, |ui| {
            ui.set_width(390.0);
            ui.label(RichText::new(title).font(bold(17.0)));
            ui.add_space(6.0);
            ui.label(body);
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                wait = primary(ui, "Keep Working", true);
                close = secondary(ui, "Close Anyway", true);
            });
        });
        if wait {
            self.ask = None;
        }
        if close {
            self.ask = None;
            self.closing = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Closing the window follows the original's three choices.
    fn handle_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.closing {
            return;
        }
        if self.options.screenshot.is_some() {
            return;
        }
        // Work in progress always gets a question first, whatever the close setting says.
        if self.working() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.ask = Some(Ask::CloseWhileWorking);
            return;
        }
        match self.settings.close_choice {
            CloseChoice::Exit => self.closing = true,
            CloseChoice::Tray => {
                system::start_agent();
                self.closing = true;
            }
            CloseChoice::Ask => {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.ask = Some(Ask::Close);
            }
        }
    }

    fn close_dialog(&mut self, ctx: &egui::Context) {
        let mut choice = None;
        let mut stop = false;
        egui::Modal::new(egui::Id::unique("close")).show(ctx, |ui| {
            ui.set_width(390.0);
            ui.label(RichText::new("Close DriverKeep?").font(bold(17.0)));
            ui.add_space(6.0);
            ui.label("Stay in the tray to keep scheduled scans working, or exit completely.");
            ui.add_space(6.0);
            ui.checkbox(&mut self.remember_close, "Don't ask again");
            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if primary(ui, "Stay in Tray", true) {
                    choice = Some(CloseChoice::Tray);
                }
                if secondary(ui, "Exit", true) {
                    choice = Some(CloseChoice::Exit);
                }
                stop = secondary(ui, "Cancel", true);
            });
        });
        if stop {
            self.ask = None;
        }
        if let Some(choice) = choice {
            self.ask = None;
            if self.remember_close {
                self.settings.close_choice = choice;
                self.apply_settings();
            }
            if choice == CloseChoice::Tray {
                system::start_agent();
            }
            self.closing = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(ask) = &self.ask else { return };
        if matches!(ask, Ask::Close) {
            self.close_dialog(ctx);
            return;
        }
        if matches!(ask, Ask::CloseWhileWorking) {
            self.close_while_working(ctx);
            return;
        }
        // The original only asks first when drivers are included and the reminder is on.
        if let Ask::Update(ids) = ask {
            let drivers = self
                .rows
                .iter()
                .any(|r| r.update.kind == Kind::Driver && ids.contains(&r.update.id));
            if !(self.settings.update_all_remind && drivers) {
                let ids = ids.clone();
                self.ask = None;
                // DriverKeep's own setup asks Windows for permission by itself.
                let only_this_app = self
                    .rows
                    .iter()
                    .filter(|r| ids.contains(&r.update.id))
                    .all(|r| r.update.source == Source::GitHub);
                if only_this_app {
                    self.run_update(ids, false);
                } else {
                    self.install_or_elevate(ids, false, ctx);
                }
                return;
            }
        }
        let is_update = matches!(ask, Ask::Update(_));
        let permission = if self.admin {
            ""
        } else {
            "\n\nWindows will ask for permission, then this app reopens and carries on."
        };
        let (title, body, button): (String, String, String) = match ask {
            Ask::Update(ids) => (
                format!("Install {} update{}?", ids.len(), plural(ids.len())),
                format!(
                    "Your current drivers are saved first, so you can put them back if something goes wrong.{permission}\n\nThe screen may flicker, programs being updated may close, and a restart may be needed."
                ),
                "Install".into(),
            ),
            Ask::UpdateNoBackup(_) => (
                "Install without a backup?".into(),
                format!(
                    "DriverKeep won't be able to put your current driver back if something goes wrong. A Windows restore point is still made if today doesn't have one.{permission}"
                ),
                "Install Anyway".into(),
            ),
            Ask::Restore(_, entries) => (
                format!("Put {} driver{} back?", entries.len(), plural(entries.len())),
                format!("Each device goes back to the version in this backup. A device that isn't plugged in gets it next time.{permission}"),
                "Restore".into(),
            ),
            Ask::Delete(path) => (
                "Delete this backup?".into(),
                format!("{}\n\nThis can't be undone.", path.file_name().unwrap_or_default().to_string_lossy()),
                "Delete".into(),
            ),
            Ask::Rollback(_, name) => (
                "Roll back this driver?".into(),
                format!("{name} goes back to the driver it used before.{permission}"),
                "Roll Back".into(),
            ),
            Ask::HelperTask(true) => (
                "Allow silent installs?".into(),
                "Windows will ask for permission once. After that, the background helper starts when you sign in and can install drivers in your chosen hours without asking.".into(),
                "Allow".into(),
            ),
            Ask::HelperTask(false) => (
                "Turn off silent installs?".into(),
                "Windows will ask for permission once to remove the helper's administrator access.".into(),
                "Turn Off".into(),
            ),
            Ask::Close | Ask::CloseWhileWorking => return,
            Ask::InstallTools(winget, chocolatey) => {
                let (missing, button) = match (*winget, *chocolatey) {
                    (true, true) => ("The Windows Package Manager and Chocolatey aren't", "Install Both"),
                    (true, false) => ("The Windows Package Manager isn't", "Install"),
                    _ => ("Chocolatey isn't", "Install"),
                };
                (
                    "Update more of your software?".into(),
                    format!(
                        "Package managers let DriverKeep find and update your other programs. {missing} on this PC yet.\n\nThey're free. Windows will ask for permission once, then they install in the background. You can change this later in Settings."
                    ),
                    button.into(),
                )
            }
        };
        let mut go = false;
        let mut stop = false;
        egui::Modal::new(egui::Id::unique("ask")).show(ctx, |ui| {
            ui.set_width(390.0);
            ui.label(RichText::new(title).font(bold(17.0)));
            ui.add_space(6.0);
            ui.label(body);
            if is_update {
                ui.add_space(6.0);
                ui.checkbox(&mut self.stop_reminding, "Don't ask me again");
            }
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                go = primary(ui, &button, true);
                stop = secondary(ui, "Cancel", true);
            });
        });
        if (stop || go) && matches!(self.ask, Some(Ask::InstallTools(..))) {
            self.settings.tools_offered = true;
            self.settings.save();
        }
        if stop {
            self.ask = None;
        }
        if go {
            if is_update && self.stop_reminding {
                self.settings.update_all_remind = false;
                self.settings.save();
            }
            let Some(ask) = self.ask.take() else { return };
            match ask {
                Ask::Update(ids) => self.install_or_elevate(ids, false, ctx),
                Ask::UpdateNoBackup(id) => self.install_or_elevate(vec![id], true, ctx),
                Ask::Restore(path, entries) if self.admin => self.run_restore(path, entries, ctx),
                Ask::Restore(path, entries) => {
                    let ids: Vec<&str> = entries.iter().map(|e| e.instance_id.as_str()).collect();
                    self.elevate(format!("restore:{}|{}", path.display(), ids.join("\t")), ctx);
                }
                Ask::Delete(path) => {
                    if let Err(error) = std::fs::remove_file(&path) {
                        self.message = Some((format!("Couldn't delete it: {error}"), true));
                    }
                    self.open_backup = None;
                    self.reload_backups();
                }
                Ask::Rollback(id, name) if self.admin => self.run_rollback(id, name, ctx),
                Ask::Rollback(id, _) => self.elevate(format!("rollback:{id}"), ctx),
                Ask::Close | Ask::CloseWhileWorking => {}
                Ask::InstallTools(winget, chocolatey) => {
                    self.tools_installing = true;
                    self.message = Some(("Installing in the background. This can take a few minutes.".into(), false));
                    let tx = self.tx.clone();
                    let wake = ctx.clone();
                    std::thread::spawn(move || {
                        let which = match (winget, chocolatey) {
                            (true, true) => "winget,chocolatey",
                            (true, false) => "winget",
                            _ => "chocolatey",
                        };
                        let ran = system::run_as_admin(&format!("--install-tools {which}"), true);
                        let (winget, chocolatey) = crate::software::tools_ready();
                        let _ = tx.send(Work::ToolsInstalled(ran, winget, chocolatey));
                        wake.request_repaint();
                    });
                }
                Ask::HelperTask(on) => {
                    let tx = self.tx.clone();
                    let wake = ctx.clone();
                    std::thread::spawn(move || {
                        let state = if on { "on" } else { "off" };
                        let ran = system::run_as_admin(&format!("--helper-task {state}"), true);
                        let worked = ran && system::task_exists();
                        let _ = tx.send(Work::HelperTask(on, worked));
                        wake.request_repaint();
                    });
                }
            }
        }
    }

    /// `--screenshot`: draw about 120 frames with the first reads finished, then save.
    fn screenshot_step(&mut self, ctx: &egui::Context) {
        let Some(path) = self.options.screenshot.clone() else { return };
        self.frames += 1;
        ctx.request_repaint();
        if self.shot_done.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let settled = self.busy == Busy::Idle && !self.devices.is_empty();
        let ready = match self.options.page.as_deref() {
            Some("scanning") => self.percent >= 30 || settled,
            _ if self.options.full_scan => {
                settled
                    && !self.windows_update_pending
                    && !self.catalog_pending
                    && !self.software_pending
            }
            _ => settled || self.frames > 3000,
        };
        if !self.shot_sent && self.frames >= 120 && ready {
            self.shot_sent = true;
            let done = self.shot_done.clone();
            let wake = ctx.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(
                move |image| {
                    let size = [image.size[0] as u32, image.size[1] as u32];
                    let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                    if let Some(picture) = image::RgbaImage::from_raw(size[0], size[1], bytes) {
                        if let Some(folder) = path.parent() {
                            let _ = std::fs::create_dir_all(folder);
                        }
                        let _ = picture.save(&path);
                    }
                    done.store(true, Ordering::SeqCst);
                    wake.request_repaint();
                },
            )));
        }
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        let choice = match self.options.theme.as_deref() {
            Some("dark") => Theme::Dark,
            Some("light") => Theme::Light,
            Some("oled") => Theme::Oled,
            _ => self.settings.theme,
        };
        // Windows' own accent colour is looked at again every couple of seconds.
        let mut accent = self.settings.accent;
        if self.settings.follow_windows_accent {
            let stale = self.windows_accent_read.is_none_or(|at| at.elapsed().as_secs() >= 2);
            if stale {
                self.windows_accent = system::windows_accent();
                self.windows_accent_read = Some(std::time::Instant::now());
            }
            accent = self.windows_accent.unwrap_or(accent);
            ctx.request_repaint_after(std::time::Duration::from_secs(2));
        }
        let mut changed = set_oled(choice == Theme::Oled);
        changed |= set_accent(accent);
        if changed {
            apply_style(ctx);
        }
        ctx.set_theme(match choice {
            Theme::Dark | Theme::Oled => egui::ThemePreference::Dark,
            Theme::Light => egui::ThemePreference::Light,
            Theme::System => egui::ThemePreference::System,
        });
        set_dark(ctx.theme() == egui::Theme::Dark);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.apply_theme(&ctx);
        self.handle_close(&ctx);
        self.receive(&ctx);
        self.screenshot_step(&ctx);
        if std::mem::take(&mut self.bring_forward) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if std::mem::take(&mut self.flash_app_update) && self.options.screenshot.is_none() {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Critical,
            ));
        }
        if self.busy != Busy::Idle || self.windows_update_pending {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }

        egui::Panel::left("rail")
            .resizable(false)
            .exact_size(82.0)
            .show_separator_line(false)
            .frame(Frame::new().fill(pal().rail))
            .show(ui, |ui| self.rail(ui));

        egui::CentralPanel::default()
            .frame(Frame::new().fill(pal().bg).inner_margin(Margin::symmetric(24, 16)))
            .show(ui, |ui| {
                // On a wide screen the content stays in a centred column, so names and
                // their buttons are close enough to read across.
                let full = ui.max_rect();
                let width = full.width().min(MAX_CONTENT_WIDTH);
                let left = full.center().x - width / 2.0;
                let column = egui::Rect::from_min_max(
                    egui::pos2(left, full.top()),
                    egui::pos2(left + width, full.bottom()),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(column), |ui| {
                    self.banners(ui);
                    match self.page {
                        Page::Drivers => self.drivers_page(ui),
                        Page::System => self.system_page(ui),
                        Page::Backups => self.backups_page(ui),
                        Page::Settings => self.settings_page(ui),
                    }
                });
            });

        self.dialogs(&ctx);
    }
}

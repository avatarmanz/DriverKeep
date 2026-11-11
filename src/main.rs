#![windows_subsystem = "windows"]

mod agent;
mod backup;
mod catalog;
mod devices;
mod handoff;
mod history;
mod restore;
mod selfupdate;
mod software;
mod sysinfo;
mod system;
mod ui;
mod wu;

use std::path::PathBuf;

/// Things the program can be told when it starts.
#[derive(Default, Clone)]
pub struct Options {
    pub screenshot: Option<PathBuf>,
    pub page: Option<String>,
    pub theme: Option<String>,
    pub full_scan: bool,
    pub resume: Option<String>,
    pub report: Option<PathBuf>,
    pub check_update: Option<PathBuf>,
    pub check_backup: Option<PathBuf>,
    pub only: Option<String>,
    pub background: bool,
    pub install_job: bool,
    pub restore_job: bool,
    pub plain: bool,
    pub open: bool,
    pub helper_task: Option<String>,
    pub install_tools: Option<String>,
    pub open_store: Option<PathBuf>,
    pub write_icon: Option<PathBuf>,
}

fn read_options() -> Options {
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--screenshot" => options.screenshot = args.next().map(PathBuf::from),
            "--page" => options.page = args.next(),
            "--theme" => options.theme = args.next(),
            "--full-scan" => options.full_scan = true,
            "--resume" => options.resume = args.next(),
            "--report" => options.report = args.next().map(PathBuf::from),
            "--check-update" => options.check_update = args.next().map(PathBuf::from),
            "--check-backup" => options.check_backup = args.next().map(PathBuf::from),
            "--only" => options.only = args.next(),
            "--background" => options.background = true,
            "--install-job" => options.install_job = true,
            "--restore-job" => options.restore_job = true,
            "--plain" => options.plain = true,
            "--open" => options.open = true,
            "--helper-task" => options.helper_task = args.next(),
            "--install-tools" => options.install_tools = args.next(),
            "--open-store" => options.open_store = args.next().map(PathBuf::from),
            "--write-icon" => options.write_icon = args.next().map(PathBuf::from),
            _ => {}
        }
    }
    options
}

/// Writes what a scan finds to a text file, without opening a window.
fn write_report(path: &std::path::Path) -> Result<(), String> {
    let started = std::time::Instant::now();
    let list = devices::enumerate(|_, _| {})?;
    let listed = started.elapsed();
    let mut lines = vec![format!("{} devices in {listed:?}", list.len())];
    for d in &list {
        let driver = d.driver.as_ref();
        lines.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            if d.present { "connected" } else { "past" },
            d.class_title,
            d.name,
            driver.map(|x| x.version.as_str()).unwrap_or("-"),
            driver.map(|x| x.inf_name.as_str()).unwrap_or("-"),
            d.hardware_ids.first().map(String::as_str).unwrap_or("-"),
            d.last_connected.map(devices::short_date).unwrap_or_default(),
        ));
    }
    let stop = std::sync::atomic::AtomicBool::new(false);
    let online = std::time::Instant::now();
    match catalog::find_updates(&list, &stop, |_| {}) {
        Ok(found) => {
            lines.push(format!("catalog: {} updates in {:?}", found.len(), online.elapsed()));
            for u in found {
                lines.push(format!("UPDATE\t{}\t{}\t{}\t{}", u.title, u.version, u.date, u.devices.join(" | ")));
            }
        }
        Err(e) => lines.push(format!("catalog error: {e}")),
    }
    lines.push(format!("this copy: {}, newer release: {:?}", selfupdate::CURRENT, selfupdate::check().map(|u| u.version)));
    let update_test = path.with_file_name("update-test");
    lines.push(format!("self-update download: {}", selfupdate::check_download(&update_test)));
    match software::find(true, true) {
        Ok(found) => {
            lines.push(format!("software: {} updates", found.len()));
            lines.extend(found.iter().map(|u| format!("APP\t{}\t{}\t{}", u.title, u.current, u.version)));
        }
        Err(e) => lines.push(format!("software error: {e}")),
    }
    let info = sysinfo::gather()?;
    for s in info.sections {
        lines.push(format!("[{}]", s.title));
        lines.extend(s.facts.into_iter().map(|(k, v)| format!("{k}: {v}")));
    }
    std::fs::write(path, lines.join("\n")).map_err(|e| e.to_string())
}

/// Runs a catalog update through every step except the install itself.
fn check_update(path: &std::path::Path) -> Result<(), String> {
    let list = devices::enumerate(|_, _| {})?;
    let stop = std::sync::atomic::AtomicBool::new(false);
    let found = catalog::find_updates(&list, &stop, |_| {})?;
    let mut lines = vec![format!("{} updates found", found.len())];
    for update in found.iter().take(3) {
        let chosen: Vec<devices::Device> = list
            .iter()
            .filter(|d| update.devices.contains(&d.instance_id))
            .cloned()
            .collect();
        match catalog::prepare(update, &chosen) {
            Ok(ready) => {
                lines.push(format!("OK {} -> {} for {}", update.title, ready.inf.display(), ready.hardware_id));
                let _ = std::fs::remove_dir_all(&ready.folder);
            }
            Err(e) => lines.push(format!("FAILED {}: {e}", update.title)),
        }
    }
    std::fs::write(path, lines.join("\n")).map_err(|e| e.to_string())
}

/// Backs up a small add-on driver for real, then reads the backup back.
fn check_backup(folder: &std::path::Path, only: Option<&str>) -> Result<(), String> {
    let list = devices::enumerate(|_, _| {})?;
    let chosen: Vec<devices::Device> = list
        .iter()
        .filter(|d| d.driver.as_ref().is_some_and(|x| x.is_third_party()) && !d.system_item)
        .filter(|d| {
            // Name a device to test that one; otherwise a few small drivers.
            if let Some(name) = only {
                return d.name.to_lowercase().contains(&name.to_lowercase());
            }
            let size = d.driver.as_ref().and_then(|x| devices::package_folder(&x.inf_name));
            size.and_then(|p| p.parent().map(|f| backup::folder_size(f))).is_some_and(|s| s < 8 << 20)
        })
        .take(3)
        .cloned()
        .collect();
    let done = backup::create(&chosen, folder, "Self test", false, |_, _| {})?;
    let info = backup::read_info(&done.path)?;
    let text = format!(
        "saved {} of {} ({} bytes, skipped {:?})\n{:#?}",
        done.saved, chosen.len(), done.bytes, done.skipped, info.entries
    );
    std::fs::write(folder.join("report.txt"), text).map_err(|e| e.to_string())
}

fn main() -> eframe::Result {
    let options = read_options();
    if let Some(path) = &options.write_icon {
        let _ = ui::write_icon(path);
        return Ok(());
    }
    if let Some(path) = &options.report {
        if let Err(e) = write_report(path) {
            let _ = std::fs::write(path, e);
        }
        return Ok(());
    }
    if let Some(path) = &options.check_update {
        if let Err(e) = check_update(path) {
            let _ = std::fs::write(path, e);
        }
        return Ok(());
    }
    if let Some(path) = &options.check_backup {
        let _ = std::fs::create_dir_all(path);
        if let Err(e) = check_backup(path, options.only.as_deref()) {
            let _ = std::fs::write(path.join("report.txt"), e);
        }
        return Ok(());
    }
    if let Some(which) = &options.install_tools {
        software::install_tools(which);
        return Ok(());
    }
    if let Some(path) = &options.open_store {
        let opened = system::open_store_page(software::WINGET_STORE_LINK, software::WINGET_WEB_LINK);
        let _ = std::fs::write(path, opened);
        return Ok(());
    }
    if let Some(state) = &options.helper_task {
        system::set_agent_task(state == "on");
        return Ok(());
    }
    if options.restore_job {
        restore::run_job();
        return Ok(());
    }
    if options.install_job {
        agent::install_job();
        return Ok(());
    }
    if options.background {
        agent::run(options.open);
        return Ok(());
    }
    if options.screenshot.is_none() && options.resume.is_none() {
        // Opening the app twice brings the first window forward.
        if system::claim_single_instance(system::WINDOW_MUTEX).is_none() {
            ui::bring_to_front();
            return Ok(());
        }
    }
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(system::window_title())
            .with_inner_size([1000.0, 660.0])
            .with_min_inner_size([800.0, 480.0])
            .with_icon(ui::window_icon()),
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        system::APP_NAME,
        native,
        Box::new(move |cc| Ok(Box::new(ui::App::new(cc, options)))),
    )
}

//! The small background helper: a tray icon, scheduled scans, silent installs in the chosen
//! hours and automatic restarts. It has no window of its own, so it stays light.

use crate::devices::Device;
use crate::system::{self, Settings, in_hours};
use crate::wu::{self, Command, Event, InstallItem, InstallPlan, Kind, SCAN_CACHE, ScanPlan, Source, Update};
use chrono::Timelike;
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const TICK: Duration = Duration::from_secs(30);
const WM_TRAY: u32 = WM_APP + 1;
const WM_BALLOON: u32 = WM_APP + 2;
const MENU_OPEN: usize = 1;
const MENU_SCAN: usize = 2;
const MENU_CANCEL_RESTART: usize = 3;
const MENU_EXIT: usize = 4;
pub const RESTART_WARNING_SECONDS: u32 = 300;

enum Ask {
    ScanNow,
    CancelRestart,
    Quit,
    /// The window asked this helper (which can run as administrator) to install updates.
    Install(crate::handoff::Request),
}

static TO_WORKER: OnceLock<Mutex<Sender<Ask>>> = OnceLock::new();
static BALLOON: Mutex<Option<(String, String)>> = Mutex::new(None);
static WINDOW: Mutex<isize> = Mutex::new(0);
static RESTART_PENDING: Mutex<bool> = Mutex::new(false);

fn ask(request: Ask) {
    if let Some(tx) = TO_WORKER.get().and_then(|m| m.lock().ok()) {
        let _ = tx.send(request);
    }
}

fn tell(title: &str, text: &str) {
    if let Ok(mut slot) = BALLOON.lock() {
        *slot = Some((title.to_string(), text.to_string()));
    }
    let raw = WINDOW.lock().map(|w| *w).unwrap_or(0);
    if raw != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(raw as *mut _)), WM_BALLOON, WPARAM(0), LPARAM(0));
        }
    }
}

fn copy_text(target: &mut [u16], text: &str) {
    let room = target.len() - 1;
    let wide: Vec<u16> = text.encode_utf16().take(room).collect();
    target[..wide.len()].copy_from_slice(&wide);
    target[wide.len()] = 0;
}

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    };
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = crate::ui::tray_icon().unwrap_or_default();
    copy_text(&mut data.szTip, system::APP_NAME);
    data
}

fn show_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("Open DriverKeep"));
        let _ = AppendMenuW(menu, MF_STRING, MENU_SCAN, w!("Scan now"));
        if RESTART_PENDING.lock().map(|r| *r).unwrap_or(false) {
            let _ = AppendMenuW(menu, MF_STRING, MENU_CANCEL_RESTART, w!("Cancel restart"));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Exit background helper"));
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, point.x, point.y, None, hwnd, None);
        let _ = DestroyMenu(menu);
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_TRAY => {
            let event = (lparam.0 & 0xFFFF) as u32;
            if event == WM_LBUTTONUP {
                system::show_window();
            } else if event == WM_RBUTTONUP {
                show_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_BALLOON => {
            let pending = BALLOON.lock().ok().and_then(|mut slot| slot.take());
            if let Some((title, text)) = pending {
                let mut data = tray_data(hwnd);
                data.uFlags = NIF_INFO;
                data.dwInfoFlags = NIIF_INFO;
                copy_text(&mut data.szInfoTitle, &title);
                copy_text(&mut data.szInfo, &text);
                unsafe {
                    let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            match (wparam.0 & 0xFFFF) as usize {
                MENU_OPEN => system::show_window(),
                MENU_SCAN => ask(Ask::ScanNow),
                MENU_CANCEL_RESTART => ask(Ask::CancelRestart),
                MENU_EXIT => ask(Ask::Quit),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let data = tray_data(hwnd);
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn quit_window() {
    let raw = WINDOW.lock().map(|w| *w).unwrap_or(0);
    if raw != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(raw as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Blocks until the running work has finished, gathering what it found.
fn wait_for_scan(events: &std::sync::mpsc::Receiver<Event>, settings: &Settings) -> (Vec<Device>, Vec<Update>) {
    let mut devices = Vec::new();
    let mut found = Vec::new();
    let mut waiting = 3;
    while waiting > 0 {
        match events.recv() {
            Ok(Event::Devices(Ok(list))) => devices = list,
            Ok(Event::CatalogDone(result))
            | Ok(Event::WindowsUpdateDone(result))
            | Ok(Event::SoftwareDone(result)) => {
                waiting -= 1;
                found.extend(result.unwrap_or_default());
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let updates = attach(found, &devices, settings);
    (devices, updates)
}

/// Connects each Windows Update offer to the devices it's for.
pub fn attach(updates: Vec<Update>, devices: &[Device], settings: &Settings) -> Vec<Update> {
    let updates = updates
        .into_iter()
        // A saved list may offer a version that's already been installed.
        .filter(|u| u.source != Source::GitHub || crate::catalog::is_newer(&u.version, crate::selfupdate::CURRENT))
        .map(|mut update| {
            if update.devices.is_empty() && !update.hardware_id.is_empty() {
                // The original's rule: only a later driver date counts as an update.
                update.devices = devices
                    .iter()
                    .filter(|d| d.answers_to(&update.hardware_id))
                    .filter(|d| {
                        d.driver
                            .as_ref()
                            .is_none_or(|current| crate::catalog::date_is_newer(&update.date, &current.date))
                    })
                    .map(|d| d.instance_id.clone())
                    .collect();
            }
            // A saved list can be out of date: drop devices that already got this driver.
            if update.kind == Kind::Driver && !devices.is_empty() {
                update.devices.retain(|id| {
                    devices
                        .iter()
                        .find(|d| d.instance_id == *id)
                        .and_then(|d| d.driver.as_ref())
                        .is_none_or(|current| current.version != update.version)
                });
            }
            update
        })
        .collect();
    wu::merge(updates, &settings.ignored)
}

/// One scheduled pass. Returns true when a restart is needed.
fn scan_and_maybe_install(
    engine: &wu::Engine,
    events: &std::sync::mpsc::Receiver<Event>,
    settings: &mut Settings,
    by_hand: bool,
) -> bool {
    let _ = engine.commands.send(Command::Scan(ScanPlan::from_settings(settings)));
    let (devices, updates) = wait_for_scan(events, settings);
    settings.last_scan = Some(chrono::Local::now().timestamp());
    settings.save();
    system::write_cache(SCAN_CACHE, &updates);

    let count = updates.len();
    if count == 0 {
        if by_hand {
            tell(system::APP_NAME, "Your drivers are up to date.");
        }
        return false;
    }
    let hour = chrono::Local::now().hour();
    let allowed = settings.silent_install
        && system::is_admin()
        && in_hours(hour, settings.install_from, settings.install_to);
    if !allowed {
        if settings.notify || by_hand {
            let text = format!("{count} driver update{} ready.", if count == 1 { "" } else { "s" });
            tell(system::APP_NAME, &text);
        }
        return false;
    }

    let items: Vec<InstallItem> = updates
        .into_iter()
        .filter(|u| match u.kind {
            Kind::Driver => {
                settings.silent_drivers && (u.source == Source::WindowsUpdate || settings.silent_catalog)
            }
            Kind::Windows => settings.silent_windows,
            Kind::Software => settings.silent_software && u.source != Source::GitHub,
        })
        .map(|update| InstallItem {
            devices: devices.iter().filter(|d| update.devices.contains(&d.instance_id)).cloned().collect(),
            update,
        })
        .collect();
    if items.is_empty() {
        return false;
    }
    let kinds: Vec<(Kind, bool)> = items.iter().map(|i| (i.update.kind, i.update.source == Source::GitHub)).collect();
    let restore_point = settings.wants_restore_point(&kinds);
    if restore_point {
        settings.last_restore_point_at = chrono::Local::now().timestamp();
        settings.save();
    }
    let _ = engine.commands.send(Command::Install(InstallPlan {
        items,
        restore_point,
        backup_folder: settings.backup_before_update.then(|| settings.backup_folder.clone()),
        require_backup: !settings.install_without_backup,
        automatic: true,
    }));
    loop {
        match events.recv() {
            Ok(Event::InstallFinished { restart, installed, failed }) => {
                if settings.notify && installed + failed > 0 {
                    let mut text = format!("{installed} driver{} updated.", if installed == 1 { "" } else { "s" });
                    if failed > 0 {
                        text.push_str(&format!(" {failed} didn't install."));
                    }
                    tell(system::APP_NAME, &text);
                }
                return restart;
            }
            Ok(_) => {}
            Err(_) => return false,
        }
    }
}

/// Installs what the window asked for, and reports progress in a file the window reads. The helper
/// does its own scan and only installs updates it finds itself that match the request. Returns
/// true when a restart is needed.
fn install_requested(
    engine: &wu::Engine,
    events: &std::sync::mpsc::Receiver<Event>,
    settings: &mut Settings,
    request: crate::handoff::Request,
) -> bool {
    use crate::handoff::{self, Progress};
    let mut progress = Progress::new(request.id);
    // Without administrator rights nothing here could install, so the window uses the other way.
    if !system::is_admin() {
        progress.refused = true;
        progress.finished = true;
        progress.text = "The background helper isn't running as administrator.".into();
        handoff::write_progress(&mut progress);
        return false;
    }
    progress.percent = 2;
    progress.text = "Checking what's available".into();
    handoff::write_progress(&mut progress);

    let _ = engine.commands.send(Command::Scan(ScanPlan::from_settings(settings)));
    let (devices, updates) = wait_for_scan(events, settings);
    let items: Vec<InstallItem> = updates
        .into_iter()
        .filter(|u| request.ids.contains(&u.id) && u.source != Source::GitHub)
        .map(|update| InstallItem {
            devices: devices.iter().filter(|d| update.devices.contains(&d.instance_id)).cloned().collect(),
            update,
        })
        .collect();
    if items.is_empty() {
        progress.finished = true;
        progress.percent = 100;
        progress.text = "Finished".into();
        progress.notes.push("Those updates aren't offered any more, so nothing was installed.".into());
        handoff::write_progress(&mut progress);
        return false;
    }

    let kinds: Vec<(Kind, bool)> = items.iter().map(|i| (i.update.kind, i.update.source == Source::GitHub)).collect();
    let restore_point = settings.wants_restore_point(&kinds);
    if restore_point {
        settings.last_restore_point_at = chrono::Local::now().timestamp();
        settings.save();
    }
    engine.reset_install();
    let _ = engine.commands.send(Command::Install(InstallPlan {
        items,
        restore_point,
        backup_folder: (settings.backup_before_update && !request.skip_backup)
            .then(|| settings.backup_folder.clone()),
        require_backup: !settings.install_without_backup,
        automatic: false,
    }));
    let mut skipped = 0;
    loop {
        // The window may ask to stop, or to leave an update out, while this runs.
        if let Some(control) = handoff::read_control(request.id) {
            if control.cancel {
                engine.cancel_install();
            }
            if control.skip.len() > skipped {
                control.skip.iter().for_each(|id| engine.skip_update(id));
                skipped = control.skip.len();
            }
        }
        let event = match events.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(event) => event,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => return false,
        };
        match Ok::<Event, ()>(event) {
            Ok(Event::Status { percent, text }) => {
                progress.percent = percent;
                progress.text = text;
            }
            Ok(Event::Item { id, state }) => progress.set_state(&id, &state),
            Ok(Event::ItemProgress { id, percent }) => progress.set_percent(&id, percent),
            Ok(Event::Note(text)) => progress.notes.push(text),
            Ok(Event::InstallFinished { restart, .. }) => {
                progress.finished = true;
                progress.restart = restart;
                progress.percent = 100;
                handoff::write_progress(&mut progress);
                return restart;
            }
            Ok(_) => continue,
            Err(_) => return false,
        }
        handoff::write_progress(&mut progress);
    }
}

/// The short-lived administrator copy that Windows' permission box starts while the window stays
/// open. It does the one install the window asked for, reports progress, and exits.
pub fn install_job() {
    let (event_tx, events) = channel();
    let engine = wu::start(event_tx, || {});
    let started = std::time::Instant::now();
    let request = loop {
        if let Some(request) = crate::handoff::take_request(true) {
            break request;
        }
        if started.elapsed().as_secs() > 20 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    let mut settings = Settings::load();
    install_requested(&engine, &events, &mut settings, request);
}

/// With the setting on, installs a later release of DriverKeep found by the last scan.
/// Setup closes this helper and starts it again, so nothing after this runs if it works.
fn update_self(engine: &wu::Engine, events: &std::sync::mpsc::Receiver<Event>, settings: &Settings) {
    if !settings.auto_update_app || !system::is_admin() || !system::is_installed() {
        return;
    }
    let saved: Vec<Update> = system::read_cache(SCAN_CACHE).unwrap_or_default();
    let newer = saved
        .into_iter()
        .find(|u| u.source == Source::GitHub && crate::catalog::is_newer(&u.version, crate::selfupdate::CURRENT))
        .filter(|u| !settings.ignored.contains(&u.id));
    let Some(update) = newer else { return };
    let _ = engine.commands.send(Command::Install(InstallPlan {
        items: vec![InstallItem { update, devices: Vec::new() }],
        restore_point: false,
        backup_folder: None,
        require_backup: true,
        automatic: true,
    }));
    loop {
        match events.recv() {
            Ok(Event::InstallFinished { .. }) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

fn worker(inbox: std::sync::mpsc::Receiver<Ask>, first_run: bool) {
    let (event_tx, events) = channel();
    let engine = wu::start(event_tx, || {});
    let mut restart_due = false;
    let mut startup_scan = first_run;
    loop {
        let mut settings = Settings::load();
        if !settings.wants_agent() {
            break;
        }
        crate::backup::prune(&settings.backup_folder, settings.backup_keep_days);
        let now = chrono::Local::now();
        if settings.scan_overdue(now) || (startup_scan && settings.scan_at_startup) {
            restart_due |= scan_and_maybe_install(&engine, &events, &mut settings, false);
        }
        startup_scan = false;

        // Updates found earlier in the day wait here for the allowed hours.
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let hour_now = chrono::Local::now().hour();
        let waiting: Vec<Update> = system::read_cache(SCAN_CACHE).unwrap_or_default();
        if settings.silent_install
            && system::is_admin()
            && !waiting.is_empty()
            && settings.last_silent_day != today
            && in_hours(hour_now, settings.install_from, settings.install_to)
        {
            settings.last_silent_day = today;
            settings.save();
            restart_due |= scan_and_maybe_install(&engine, &events, &mut settings, false);
        }

        // A restart waiting comes first: updating closes this helper, which would forget it.
        if !restart_due {
            update_self(&engine, &events, &settings);
        }

        if restart_due && settings.auto_restart {
            let hour = chrono::Local::now().hour();
            let fits = !settings.restart_only_in_hours
                || in_hours(hour, settings.install_from, settings.install_to);
            if fits && system::restart_windows(RESTART_WARNING_SECONDS, "DriverKeep is finishing driver updates.") {
                if let Ok(mut flag) = RESTART_PENDING.lock() {
                    *flag = true;
                }
                tell(system::APP_NAME, "Restarting in 5 minutes to finish driver updates. Right-click this icon to cancel.");
                restart_due = false;
            }
        }

        match inbox.recv_timeout(TICK) {
            Ok(Ask::ScanNow) => {
                restart_due |= scan_and_maybe_install(&engine, &events, &mut settings, true);
            }
            Ok(Ask::CancelRestart) => {
                system::cancel_restart();
                if let Ok(mut flag) = RESTART_PENDING.lock() {
                    *flag = false;
                }
                tell(system::APP_NAME, "Restart cancelled.");
            }
            Ok(Ask::Install(request)) => {
                restart_due |= install_requested(&engine, &events, &mut settings, request);
            }
            Ok(Ask::Quit) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
    quit_window();
}

pub fn run(open_window: bool) {
    let Some(_claim) = system::claim_single_instance(system::AGENT_MUTEX) else {
        if open_window {
            system::show_window();
        }
        return;
    };
    if open_window {
        system::show_window();
    }
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let class = w!("DriverKeepHelper");
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: module.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&window_class);
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("DriverKeep helper"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(module.into()),
            None,
        ) else {
            return;
        };
        if let Ok(mut slot) = WINDOW.lock() {
            *slot = hwnd.0 as isize;
        }
        let _ = Shell_NotifyIconW(NIM_ADD, &tray_data(hwnd));

        let (tx, inbox) = channel();
        let _ = TO_WORKER.set(Mutex::new(tx));
        std::thread::spawn(move || worker(inbox, true));
        // Looks for an install request from the window, once a second.
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_secs(1));
            if let Some(request) = crate::handoff::take_request(false) {
                ask(Ask::Install(request));
            }
        });

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

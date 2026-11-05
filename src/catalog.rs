//! Looks up newer drivers on the public Microsoft Update Catalog, by hardware id.
//! Every result is checked against the exact ids the catalog says it supports, and
//! every download is checked against the checksum Microsoft publishes for it.

use crate::devices::{self, Device, InstallOutcome};
use crate::system;
use crate::wu::{Source, Update};
use base64::Engine;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

const SITE: &str = "https://www.catalog.update.microsoft.com";
pub const WORKERS: usize = 8;
pub const CANDIDATES_CHECKED: usize = 4;
pub const MAX_RETRIES: usize = 2;
const TEMPORARY_CODES: [u16; 5] = [408, 500, 502, 503, 504];
/// How many devices the last search couldn't check, so the app can say so.
pub static MISSED: AtomicUsize = AtomicUsize::new(0);
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) DriverKeep/0.2";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub id: String,
    pub title: String,
    pub products: String,
    pub classification: String,
    pub version: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Details {
    pub hardware_ids: Vec<String>,
    pub provider: String,
    pub model: String,
    pub class: String,
    pub date: String,
}

fn client() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .timeout_connect(Some(Duration::from_secs(15)))
        .user_agent(USER_AGENT)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .build(),
        )
        .build()
        .into()
}

fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"_-.".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn fetch(agent: &ureq::Agent, url: &str) -> Result<String, ureq::Error> {
    agent.get(url).call()?.body_mut().with_config().limit(4 << 20).read_to_string()
}

/// The original's list of errors worth trying again: time-outs, dropped connections and
/// these server answers. Anything else won't get better by waiting.
pub fn is_temporary(error: &ureq::Error) -> bool {
    match error {
        ureq::Error::StatusCode(code) => TEMPORARY_CODES.contains(code),
        ureq::Error::Io(_) | ureq::Error::Timeout(_) | ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => true,
        _ => false,
    }
}

/// 2 seconds before the first retry, 5 before any later one.
pub fn retry_wait(retry: usize) -> Duration {
    Duration::from_millis(if retry > 1 { 5000 } else { 2000 })
}

/// The catalog sometimes drops a request when asked many things at once, so try again.
fn fetch_with_retry(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    let mut last = String::new();
    for retry in 0..=MAX_RETRIES {
        if retry > 0 {
            std::thread::sleep(retry_wait(retry));
        }
        match fetch(agent, url) {
            Ok(html) if html.contains("tableContainer") || html.contains("ScopedViewHandler") => return Ok(html),
            Ok(_) => last = "The catalog sent back an unexpected page.".into(),
            Err(error) => {
                last = error.to_string();
                if !is_temporary(&error) {
                    break;
                }
            }
        }
    }
    Err(last)
}

fn clean(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&nbsp;", " ")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn cell(html: &str, id: &str, column: u32, row: &str) -> String {
    let start = format!("id=\"{id}_C{column}_R{row}\">");
    let Some(from) = html.find(&start) else { return String::new() };
    let rest = &html[from + start.len()..];
    let end = rest.find("</td>").unwrap_or(rest.len());
    clean(&rest[..end])
}

/// Reads the result table on a catalog search page.
pub fn parse_results(html: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut from = 0;
    while let Some(found) = html[from..].find("_C1_R") {
        let at = from + found;
        from = at + 5;
        if at < 36 || !html.is_char_boundary(at - 36) {
            continue;
        }
        let id = &html[at - 36..at];
        if !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            continue;
        }
        let number: String = html[at + 5..].chars().take_while(char::is_ascii_digit).collect();
        let size_mark = format!("id=\"{id}_originalSize\">");
        let size = html
            .find(&size_mark)
            .map(|p| {
                html[p + size_mark.len()..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
            })
            .and_then(|t| t.parse().ok())
            .unwrap_or(0);
        rows.push(Row {
            id: id.to_string(),
            title: cell(html, id, 1, &number),
            products: cell(html, id, 2, &number),
            classification: cell(html, id, 3, &number),
            version: cell(html, id, 5, &number),
            size,
        });
    }
    rows
}

fn span(html: &str, name: &str) -> String {
    let mark = format!("id=\"{name}\">");
    let Some(at) = html.find(&mark) else { return String::new() };
    let rest = &html[at + mark.len()..];
    clean(&rest[..rest.find("</span>").unwrap_or(rest.len())])
}

/// Reads a catalog detail page.
pub fn parse_details(html: &str) -> Details {
    let mut hardware_ids = Vec::new();
    if let Some(start) = html.find("id=\"driverhwIDs\"") {
        let rest = &html[start + 16..];
        let end = rest.find("</table>").or(rest.find("</tbody>")).unwrap_or(rest.len());
        let text = rest[..end].replace("&amp;", "&");
        for piece in text.split(['<', '>', '\n', '\r', ' ', '\t']) {
            if piece.contains('\\') && !piece.contains('=') {
                hardware_ids.push(piece.to_lowercase());
            }
        }
    }
    Details {
        hardware_ids,
        provider: span(html, "ScopedViewHandler_driverProvider"),
        model: span(html, "ScopedViewHandler_driverModel"),
        class: span(html, "ScopedViewHandler_driverClass"),
        date: us_date(&span(html, "ScopedViewHandler_versionDate")),
    }
}

/// 7/3/2025 becomes 2025-07-03.
fn us_date(text: &str) -> String {
    let parts: Vec<&str> = text.split('/').collect();
    if parts.len() != 3 {
        return String::new();
    }
    format!("{}-{:0>2}-{:0>2}", parts[2], parts[0], parts[1])
}

/// The part of a hardware id worth searching for: maker and model, no revision details.
pub fn search_key(hardware_id: &str) -> String {
    let mut out = Vec::new();
    for piece in hardware_id.split('&') {
        out.push(piece);
        let name = piece.rsplit('\\').next().unwrap_or(piece).to_uppercase();
        if name.starts_with("DEV_") || name.starts_with("PID_") {
            break;
        }
    }
    out.join("&")
}

fn searchable(device: &Device) -> bool {
    let prefix = device.instance_id.split('\\').next().unwrap_or("").to_uppercase();
    device.present
        && !device.system_item
        && ["PCI", "USB", "HDAUDIO", "ACPI", "INTELAUDIO", "BTH", "BTHENUM"].contains(&prefix.as_str())
        && !device.hardware_ids.is_empty()
}

/// Windows' own drivers are only replaced when a device has no driver at all.
fn wants_update(device: &Device) -> bool {
    device.needs_driver() || device.driver.as_ref().is_some_and(|d| d.is_third_party())
}

fn numbers(version: &str) -> Vec<u64> {
    version.split('.').map(|p| p.trim().parse().unwrap_or(0)).collect()
}

/// The original's rule: a driver only counts as newer if its calendar date is later than the
/// installed one. A driver with no date on record passes. Dates are yyyy-mm-dd, so plain
/// comparison is calendar order.
pub fn date_is_newer(candidate: &str, installed: &str) -> bool {
    installed.is_empty() || candidate > installed
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    let mut a = numbers(candidate);
    let mut b = numbers(current);
    a.resize(a.len().max(b.len()), 0);
    b.resize(a.len(), 0);
    a > b
}

/// Windows 10 build each "version" name needs.
fn windows10_build(name: &str) -> Option<u32> {
    Some(match name.to_uppercase().as_str() {
        "1903" => 18362,
        "1909" => 18363,
        "2004" => 19041,
        "20H2" => 19042,
        "21H1" => 19043,
        "21H2" => 19044,
        "22H2" => 19045,
        _ => return None,
    })
}

/// Windows 11 build each "version" name needs.
fn windows11_build(name: &str) -> Option<u32> {
    Some(match name.to_uppercase().as_str() {
        "21H2" => 22000,
        "22H2" => 22621,
        "23H2" => 22631,
        "24H2" => 26100,
        "25H2" => 26200,
        _ => return None,
    })
}

/// Reads the catalog's "Products" text and says whether this PC is covered.
pub fn fits_this_pc(products: &str, build: u32, arm: bool) -> bool {
    let lower = products.to_lowercase();
    if lower.contains("arm64") != arm {
        return false;
    }
    let windows11 = build >= 22000;
    // The text repeats "<product>, version X and later, <kind>".
    let mut pieces = lower.split(',').map(str::trim).peekable();
    while let Some(piece) = pieces.next() {
        if piece.contains("server") {
            continue;
        }
        if piece.contains("windows 10 and later") {
            return true;
        }
        let is_11 = piece.starts_with("windows 11");
        let is_10 = piece.starts_with("windows 10");
        if (!is_11 && !is_10) || (is_11 && !windows11) || piece.ends_with(" s") {
            continue;
        }
        let Some(rest) = pieces.peek().and_then(|n| n.strip_prefix("version ")) else {
            continue;
        };
        let name = rest.split_whitespace().next().unwrap_or("");
        let needed = if is_11 { windows11_build(name) } else { windows10_build(name) };
        let Some(needed) = needed else { continue };
        if rest.contains("and later") {
            if build >= needed {
                return true;
            }
        } else if (needed..needed + 100).contains(&build) {
            return true;
        }
    }
    false
}

pub fn usable_kind(row: &Row) -> bool {
    let text = format!("{} {}", row.title, row.classification).to_lowercase();
    !text.contains("firmware") && !text.contains("bios") && !text.contains("extension")
}

pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub text: String,
}

/// Searches the catalog for every connected device. Runs several lookups at once so a
/// full pass takes seconds.
pub fn find_updates(
    devices: &[Device],
    stop: &AtomicBool,
    report: impl Fn(Progress) + Send + Sync,
) -> Result<Vec<Update>, String> {
    let build = system::windows_build();
    let arm = system::is_arm_pc();
    let mut by_key: HashMap<String, Vec<&Device>> = HashMap::new();
    for device in devices.iter().filter(|d| searchable(d) && wants_update(d)) {
        let key = search_key(&device.hardware_ids[0]).to_lowercase();
        by_key.entry(key).or_default().push(device);
    }
    let keys: Vec<String> = by_key.keys().cloned().collect();
    let total = keys.len();
    let next = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);
    let failures = AtomicUsize::new(0);
    let found: Mutex<Vec<(String, Vec<Row>)>> = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            scope.spawn(|| {
                let agent = client();
                loop {
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    let Some(key) = keys.get(index) else { break };
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let url = format!("{SITE}/Search.aspx?q={}", encode(key));
                    match fetch_with_retry(&agent, &url) {
                        Ok(html) => {
                            let rows = parse_results(&html)
                                .into_iter()
                                .filter(|r| usable_kind(r) && fits_this_pc(&r.products, build, arm))
                                .collect();
                            found.lock().unwrap().push((key.clone(), rows));
                        }
                        Err(_) => {
                            failures.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                    let done = finished.fetch_add(1, Ordering::SeqCst) + 1;
                    report(Progress { done, total, text: by_key[key][0].name.clone() });
                }
            });
        }
    });
    let missed = failures.load(Ordering::SeqCst);
    MISSED.store(missed, Ordering::SeqCst);
    if total > 0 && missed == total {
        return Err("The Microsoft driver catalog couldn't be reached.".into());
    }

    // Keep only versions newer than what each device has, newest first.
    let found = found.into_inner().unwrap_or_default();
    let mut tries: Vec<(&Device, Vec<Row>)> = Vec::new();
    for (key, mut rows) in found {
        rows.sort_by(|a, b| numbers(&b.version).cmp(&numbers(&a.version)));
        for device in &by_key[&key] {
            let current = device.driver.as_ref().map(|d| d.version.clone()).unwrap_or_default();
            let newer: Vec<Row> = rows
                .iter()
                .filter(|r| device.driver.is_none() || is_newer(&r.version, &current))
                .take(CANDIDATES_CHECKED)
                .cloned()
                .collect();
            if !newer.is_empty() {
                tries.push((device, newer));
            }
        }
    }
    let wanted_ids: HashSet<String> =
        tries.iter().flat_map(|(_, rows)| rows.iter().map(|r| r.id.clone())).collect();
    let details = load_details(wanted_ids.into_iter().collect(), stop);

    let mut wanted: HashMap<String, (Row, Vec<&Device>)> = HashMap::new();
    for (device, rows) in tries {
        let ids: Vec<String> = device.hardware_ids.iter().map(|h| h.to_lowercase()).collect();
        let installed = device.driver.as_ref().map(|d| d.date.clone()).unwrap_or_default();
        for row in rows {
            let Some(info) = details.get(&row.id) else { continue };
            let fits = info.hardware_ids.iter().any(|h| ids.contains(h));
            if fits && date_is_newer(&info.date, &installed) {
                wanted.entry(row.id.clone()).or_insert_with(|| (row.clone(), Vec::new())).1.push(device);
                break;
            }
        }
    }

    Ok(wanted
        .into_values()
        .map(|(row, list)| {
            let info = details.get(&row.id).cloned().unwrap_or_default();
            Update {
                id: row.id.clone(),
                source: Source::Catalog,
                title: row.title.clone(),
                model: info.model.split(',').next().unwrap_or("").to_string(),
                provider: info.provider.clone(),
                class: info.class.clone(),
                hardware_id: list[0].hardware_ids[0].clone(),
                version: row.version.clone(),
                date: info.date.clone(),
                size: row.size,
                devices: list.iter().map(|d| d.instance_id.clone()).collect(),
                ..Default::default()
            }
        })
        .collect())
}

fn load_details(ids: Vec<String>, stop: &AtomicBool) -> HashMap<String, Details> {
    let next = AtomicUsize::new(0);
    let out = Mutex::new(HashMap::new());
    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            scope.spawn(|| {
                let agent = client();
                loop {
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    let Some(id) = ids.get(index) else { break };
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let url = format!("{SITE}/ScopedViewInline.aspx?updateid={id}");
                    if let Ok(html) = fetch_with_retry(&agent, &url) {
                        out.lock().unwrap().insert(id.clone(), parse_details(&html));
                    }
                }
            });
        }
    });
    out.into_inner().unwrap_or_default()
}

/// Where a catalog update can be downloaded, with its checksum.
fn download_link(agent: &ureq::Agent, id: &str) -> Result<(String, String), String> {
    let body = format!("[{{\"size\":0,\"languages\":\"\",\"uidInfo\":\"{id}\",\"updateID\":\"{id}\"}}]");
    let html = agent
        .post(format!("{SITE}/DownloadDialog.aspx"))
        .send_form([("updateIDs", body.as_str())])
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(4 << 20)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let value = |mark: &str| -> Option<String> {
        let at = html.find(mark)?;
        let rest = &html[at + mark.len()..];
        let open = rest.find('\'')?;
        let rest = &rest[open + 1..];
        Some(rest[..rest.find('\'')?].to_string())
    };
    let url = value(".files[0].url").ok_or("The catalog gave no download link.")?;
    let sha = value(".files[0].sha256").ok_or("The catalog gave no checksum.")?;
    Ok((url, sha))
}

pub fn safe_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split('/').next().unwrap_or("").to_lowercase();
    host.ends_with(".windowsupdate.com") || host.ends_with(".microsoft.com")
}

fn walk(folder: &Path, found: &mut Vec<PathBuf>) {
    let Ok(items) = std::fs::read_dir(folder) else { return };
    for item in items.flatten() {
        let path = item.path();
        if path.is_dir() {
            walk(&path, found);
        } else {
            found.push(path);
        }
    }
}

fn read_inf(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let words: Vec<u16> =
            bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&words)
    } else {
        String::from_utf8_lossy(&bytes).to_string()
    }
}

fn download_once(agent: &ureq::Agent, url: &str, target: &Path) -> Result<Vec<u8>, ureq::Error> {
    let mut response = agent.get(url).call()?;
    let mut reader = response.body_mut().as_reader();
    let mut file = std::fs::File::create(target)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
        file.write_all(&buffer[..read])?;
    }
    Ok(hash.finalize().to_vec())
}

/// Same retry rule as the original. A damaged file is downloaded again too.
fn download_checked(agent: &ureq::Agent, url: &str, target: &Path, expected: &[u8]) -> Result<(), String> {
    let mut problem = String::new();
    for retry in 0..=MAX_RETRIES {
        if retry > 0 {
            std::thread::sleep(retry_wait(retry));
        }
        match download_once(agent, url, target) {
            Ok(hash) if hash == expected => return Ok(()),
            Ok(_) => {
                problem = "The download was damaged (its checksum didn't match), so it was refused.".into();
            }
            Err(error) => {
                problem = format!("Download failed ({error})");
                if !is_temporary(&error) {
                    break;
                }
            }
        }
    }
    Err(problem)
}

pub struct Prepared {
    pub folder: PathBuf,
    pub inf: PathBuf,
    pub hardware_id: String,
}

/// Downloads a catalog driver, proves it's intact, unpacks it and finds the exact setup
/// file for the device. Nothing on the computer changes in this step.
pub fn prepare(update: &Update, devices: &[Device]) -> Result<Prepared, String> {
    let agent = client();
    let (url, sha) = download_link(&agent, &update.id)?;
    if !safe_url(&url) {
        return Err("The download wasn't from a Microsoft address, so it was refused.".into());
    }
    let expected = base64::engine::general_purpose::STANDARD
        .decode(&sha)
        .map_err(|_| "The catalog's checksum couldn't be read.")?;

    let folder = std::env::temp_dir().join(format!("DriverKeep-{}", update.id));
    let _ = std::fs::remove_dir_all(&folder);
    let unpacked = folder.join("driver");
    std::fs::create_dir_all(&unpacked).map_err(|e| e.to_string())?;
    let cab = folder.join("driver.cab");

    if let Err(error) = download_checked(&agent, &url, &cab, &expected) {
        let _ = std::fs::remove_dir_all(&folder);
        return Err(error);
    }

    let (code, _) = system::run_quiet(
        "expand.exe",
        &[&cab.to_string_lossy(), "-F:*", &unpacked.to_string_lossy()],
    )?;
    if code != 0 {
        return Err(format!("The driver package couldn't be unpacked (code {code})."));
    }

    let mut files = Vec::new();
    walk(&unpacked, &mut files);
    let infs: Vec<PathBuf> = files
        .into_iter()
        .filter(|f| f.extension().is_some_and(|e| e.eq_ignore_ascii_case("inf")))
        .collect();
    let ids: Vec<&String> = devices
        .iter()
        .flat_map(|d| d.hardware_ids.iter().chain(d.compatible_ids.iter()))
        .collect();
    let mut matches = Vec::new();
    for inf in &infs {
        let text = read_inf(inf);
        if text.to_lowercase().contains("extensionid") {
            continue;
        }
        let declared = inf_ids(&text);
        if let Some(id) = ids.iter().find(|id| declared.contains(&id.to_lowercase())) {
            matches.push((inf.clone(), (*id).clone()));
        }
    }
    // Same rule as the original: exactly one setup file must name the device, or it's refused.
    if matches.len() != 1 {
        let _ = std::fs::remove_dir_all(&folder);
        return Err(if matches.is_empty() {
            "The package didn't contain a driver for this exact device.".into()
        } else {
            "The package had more than one possible driver for this device, so it was left alone.".into()
        });
    }
    let (inf, hardware_id) = matches.remove(0);
    Ok(Prepared { folder, inf, hardware_id })
}

/// Every device id a setup file declares, in lower case.
pub fn inf_ids(text: &str) -> HashSet<String> {
    let mut ids = HashSet::new();
    for line in text.lines() {
        let line = line.split(';').next().unwrap_or("");
        for piece in line.split(',') {
            let piece = piece.trim().trim_matches('"');
            if piece.contains('\\') && !piece.contains(' ') && !piece.contains('%') && !piece.contains('=') {
                ids.insert(piece.to_lowercase());
            }
        }
    }
    ids
}

/// Installs a catalog driver. Windows checks its signature before accepting it.
pub fn install(update: &Update, devices: &[Device]) -> Result<bool, String> {
    let ready = prepare(update, devices)?;
    let result = devices::install_inf(&ready.hardware_id, &ready.inf.to_string_lossy(), false);
    let _ = std::fs::remove_dir_all(&ready.folder);
    match result? {
        InstallOutcome::Installed { restart } => Ok(restart),
        InstallOutcome::Unplugged => Err("The device isn't plugged in.".into()),
        InstallOutcome::NotBetter => {
            Err("Windows decided your current driver is the better match.".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_drop_revision_details() {
        assert_eq!(search_key("PCI\\VEN_8086&DEV_7778&SUBSYS_14F11025&REV_20"), "PCI\\VEN_8086&DEV_7778");
        assert_eq!(search_key("USB\\VID_046D&PID_085E&REV_0317"), "USB\\VID_046D&PID_085E");
        assert_eq!(
            search_key("HDAUDIO\\FUNC_01&VEN_10EC&DEV_0295&SUBSYS_1"),
            "HDAUDIO\\FUNC_01&VEN_10EC&DEV_0295"
        );
        assert_eq!(search_key("ACPI\\INT33A0"), "ACPI\\INT33A0");
    }

    #[test]
    fn versions_compare_as_numbers() {
        assert!(is_newer("30.100.2527.40", "30.100.2417.1"));
        assert!(is_newer("10.0.1", "9.9.9"));
        assert!(!is_newer("1.2.3", "1.2.3.0"));
        assert!(!is_newer("1.2", "1.10"));
    }

    #[test]
    fn retries_follow_the_original() {
        assert_eq!(retry_wait(1), Duration::from_millis(2000));
        assert_eq!(retry_wait(2), Duration::from_millis(5000));
        assert!(is_temporary(&ureq::Error::StatusCode(503)));
        assert!(!is_temporary(&ureq::Error::StatusCode(404)));
    }

    #[test]
    fn only_a_later_date_counts_as_newer() {
        assert!(date_is_newer("2026-07-02", "2026-07-01"));
        assert!(!date_is_newer("2026-07-01", "2026-07-01"));
        assert!(!date_is_newer("2025-01-01", "2026-07-01"));
        assert!(date_is_newer("2025-01-01", ""));
    }

    #[test]
    fn setup_file_ids_are_exact() {
        let text = "[Models]\n%Name% = Install, PCI\\VEN_8086&DEV_7728 ; note\n%Other% = Install, PCI\\VEN_8086&DEV_77281\n";
        let ids = inf_ids(text);
        assert!(ids.contains("pci\\ven_8086&dev_7728"));
        assert!(ids.contains("pci\\ven_8086&dev_77281"));
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn product_text_is_understood() {
        let text = "Windows 11 Client, version 24H2 and later, Servicing Drivers, Windows 11 Client, version 24H2 and later, Upgrade & Servicing Drivers";
        assert!(fits_this_pc(text, 26100, false));
        assert!(fits_this_pc(text, 26300, false));
        assert!(!fits_this_pc(text, 22631, false));
        assert!(!fits_this_pc(text, 19045, false));
        assert!(fits_this_pc("Windows 10 and later drivers", 26100, false));
        assert!(!fits_this_pc("Windows Server 2022, Servicing Drivers", 26100, false));
        assert!(!fits_this_pc(text, 26100, true));
        assert!(fits_this_pc("Windows 10, version 1903 and later, Servicing Drivers", 26100, false));
    }

    #[test]
    fn firmware_is_left_alone() {
        let row = Row { title: "Intel Firmware 1.2".into(), ..Default::default() };
        assert!(!usable_kind(&row));
        let row = Row {
            title: "Intel System".into(),
            classification: "Drivers (Other Hardware)".into(),
            ..Default::default()
        };
        assert!(usable_kind(&row));
    }

    #[test]
    fn only_microsoft_downloads_are_allowed() {
        assert!(safe_url("https://catalog.s.download.windowsupdate.com/c/a.cab"));
        assert!(safe_url("https://download.microsoft.com/a.cab"));
        assert!(!safe_url("http://catalog.s.download.windowsupdate.com/c/a.cab"));
        assert!(!safe_url("https://windowsupdate.com.evil.example/a.cab"));
    }

    #[test]
    fn result_rows_are_read() {
        let id = "acb748d4-e5fd-4d80-916a-9d0988ad77ce";
        let html = format!(
            "<td id=\"{id}_C1_R0\"><a>\n  Intel System (1.2)\n</a></td><td id=\"{id}_C2_R0\">Windows 11 Client, version 24H2 and later, Servicing Drivers</td><td id=\"{id}_C3_R0\">Drivers (Other Hardware)</td><td id=\"{id}_C4_R0\">7/2/2025</td><td id=\"{id}_C5_R0\">30.100.2527.40</td><span id=\"{id}_originalSize\">302436</span>"
        );
        let rows = parse_results(&html);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Intel System (1.2)");
        assert_eq!(rows[0].version, "30.100.2527.40");
        assert_eq!(rows[0].size, 302436);
    }

    #[test]
    fn detail_page_gives_exact_ids() {
        let html = "<div id=\"driverhwIDs\" TABINDEX=\"1\"><table><tr><td>\n pci\\ven_8086&amp;dev_7778\n</td></tr><tr><td> pci\\ven_8086&amp;dev_777b </td></tr></table></div><span id=\"ScopedViewHandler_versionDate\">7/3/2025</span>";
        let info = parse_details(html);
        assert_eq!(info.hardware_ids, vec!["pci\\ven_8086&dev_7778", "pci\\ven_8086&dev_777b"]);
        assert_eq!(info.date, "2025-07-03");
    }
}

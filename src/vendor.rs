//! Drivers straight from the computer's maker (Dell) and the graphics chip's maker (NVIDIA), for
//! the cases the Microsoft catalog doesn't cover. Both publish their lists openly. Nothing from
//! them is trusted blindly: downloads only come from a short list of their own sites, Dell's
//! checksum has to match, and the file's digital signature has to be valid and from the right company
//! before it is run.

use crate::catalog;
use crate::devices::Device;
use crate::restore;
use crate::system;
use crate::wu::{Kind, Source, Update};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

const NO_WINDOW: u32 = 0x0800_0000;
const DELL_SITE: &str = "https://downloads.dell.com/";
const DELL_INDEX: &str = "https://downloads.dell.com/catalog/CatalogIndexPC.cab";
// The GeForce Game Ready driver is one download for every current GeForce card, so one lookup does.
const NVIDIA_LOOKUP: &str = "https://gfwsl.geforce.com/services_toolkit/services/com/nvidia/services/AjaxDriverService.php?func=DriverManualLookup&psid=127&pfid=999&osID=57&languageCode=1033&beta=0&isWHQL=1&dch=1&upCRD=0&qnf=0&sort1=0&numberOfResults=1";
const NVIDIA_TITLE: &str = "NVIDIA GeForce Game Ready Driver";
const HOSTS: [&str; 4] = ["downloads.dell.com", "dellupdater.dell.com", "us.download.nvidia.com", "download.nvidia.com"];

/// Downloads are only taken from these companies' own servers.
pub fn allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split('/').next().unwrap_or("").to_lowercase();
    HOSTS.contains(&host.as_str())
}

fn decode(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let words: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&words)
    } else {
        String::from_utf8_lossy(bytes).trim_start_matches('\u{feff}').to_string()
    }
}

fn get_bytes(url: &str) -> Result<Vec<u8>, String> {
    if !allowed(url) && url != NVIDIA_LOOKUP {
        return Err("That address isn't one of the manufacturer's own sites, so it was refused.".into());
    }
    catalog::client()
        .get(url)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_vec()
        .map_err(|e| e.to_string())
}

/// Opens a .cab with the expand tool that comes with Windows and gives back the one file inside.
fn read_cab(bytes: &[u8]) -> Result<String, String> {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |t| t.as_nanos());
    let folder = std::env::temp_dir().join(format!("driverkeep-cab-{stamp}"));
    let out = folder.join("out");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let cab = folder.join("in.cab");
    let result = (|| {
        std::fs::write(&cab, bytes).map_err(|e| e.to_string())?;
        let run = Command::new("expand.exe")
            .arg(&cab)
            .arg("-F:*")
            .arg(&out)
            .creation_flags(NO_WINDOW)
            .output()
            .map_err(|e| format!("Couldn't open the list: {e}"))?;
        if !run.status.success() {
            return Err("Windows couldn't open the manufacturer's list.".to_string());
        }
        let file = std::fs::read_dir(&out)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.is_file())
            .ok_or("The manufacturer's list was empty.")?;
        Ok(decode(&std::fs::read(file).map_err(|e| e.to_string())?))
    })();
    let _ = std::fs::remove_dir_all(&folder);
    result
}

fn child<'a, 'b>(node: roxmltree::Node<'a, 'b>, name: &str) -> Option<roxmltree::Node<'a, 'b>> {
    node.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

fn element_text(node: roxmltree::Node, name: &str) -> String {
    node.descendants()
        .find(|c| c.is_element() && c.tag_name().name() == name)
        .and_then(|c| c.text())
        .unwrap_or("")
        .trim()
        .to_string()
}

// ---------------------------------------------------------------- Dell

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DellPackage {
    pub id: String,
    pub name: String,
    pub version: String,
    pub date: String,
    pub path: String,
    pub size: u64,
    pub sha256: String,
    /// Hardware id starts (lowercase) the package is for.
    pub pci: Vec<String>,
}

/// Which list Dell keeps for a computer model. `system_id` is the model's number in the BIOS.
pub fn dell_model_path(index: &str, system_id: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(index).ok()?;
    for group in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "GroupManifest") {
        let mine = group
            .descendants()
            .any(|n| n.is_element() && n.tag_name().name() == "Model" && n.attribute("systemID").is_some_and(|id| id.eq_ignore_ascii_case(system_id)));
        if !mine {
            continue;
        }
        let info = child(group, "ManifestInformation")?;
        return info.attribute("path").map(str::to_string);
    }
    None
}

/// Reads a model's list: the driver packages that have a checksum, a Windows installer and PCI ids.
pub fn parse_dell_catalog(xml: &str) -> Vec<DellPackage> {
    let Ok(doc) = roxmltree::Document::parse(xml) else { return Vec::new() };
    let mut found = Vec::new();
    for part in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "SoftwareComponent") {
        let is_driver = child(part, "ComponentType").and_then(|c| c.attribute("value")) == Some("DRVR");
        let path = part.attribute("path").unwrap_or("");
        if !is_driver || !path.to_lowercase().ends_with(".exe") {
            continue;
        }
        let sha256 = part
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "Hash" && n.attribute("algorithm") == Some("SHA256"))
            .and_then(|n| n.text())
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let pci: Vec<String> = part
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "PCIInfo")
            .filter_map(|n| {
                let vendor = n.attribute("vendorID")?.to_lowercase();
                let device = n.attribute("deviceID")?.to_lowercase();
                let sub_device = n.attribute("subDeviceID").unwrap_or("").to_lowercase();
                let sub_vendor = n.attribute("subVendorID").unwrap_or("").to_lowercase();
                Some(if sub_device.is_empty() || sub_vendor.is_empty() {
                    format!("pci\\ven_{vendor}&dev_{device}")
                } else {
                    format!("pci\\ven_{vendor}&dev_{device}&subsys_{sub_device}{sub_vendor}")
                })
            })
            .collect();
        if sha256.len() != 64 || pci.is_empty() {
            continue;
        }
        found.push(DellPackage {
            id: part.attribute("packageID").unwrap_or(path).to_string(),
            name: child(part, "Name").map(|n| element_text(n, "Display")).unwrap_or_default(),
            version: part.attribute("vendorVersion").unwrap_or("").to_string(),
            date: part.attribute("dateTime").unwrap_or("").chars().take(10).collect(),
            path: path.to_string(),
            size: part.attribute("size").and_then(|s| s.parse().ok()).unwrap_or(0),
            sha256,
            pci,
        });
    }
    found
}

fn numbers(version: &str) -> Vec<u64> {
    version.split('.').map(|p| p.trim().parse().unwrap_or(0)).collect()
}

/// The packages that are newer than what the matching devices run, newest first, each device once.
pub fn dell_updates(devices: &[Device], mut packages: Vec<DellPackage>, builtin: bool) -> Vec<Update> {
    packages.sort_by(|a, b| numbers(&b.version).cmp(&numbers(&a.version)));
    let mut taken: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for package in &packages {
        let mut matched: Vec<&Device> = Vec::new();
        for device in devices.iter().filter(|d| catalog::searchable(d) && catalog::wants_update(d, builtin)) {
            if taken.contains(&device.instance_id) {
                continue;
            }
            let fits = device.hardware_ids.iter().any(|id| {
                let id = id.to_lowercase();
                package.pci.iter().any(|start| id.starts_with(start.as_str()))
            });
            let newer = match &device.driver {
                None => true,
                Some(driver) => catalog::is_newer(&package.version, &driver.version) && catalog::date_is_newer(&package.date, &driver.date),
            };
            if fits && newer {
                matched.push(device);
            }
        }
        if matched.is_empty() {
            continue;
        }
        taken.extend(matched.iter().map(|d| d.instance_id.clone()));
        out.push(Update {
            id: format!("dell:{}", package.id),
            kind: Kind::Driver,
            source: Source::Dell,
            title: package.name.clone(),
            model: matched[0].name.clone(),
            provider: "Dell".into(),
            hardware_id: matched[0].hardware_ids.first().cloned().unwrap_or_default(),
            version: package.version.clone(),
            date: package.date.clone(),
            size: package.size,
            link: format!("{DELL_SITE}{}", package.path),
            checksum: package.sha256.clone(),
            devices: matched.iter().map(|d| d.instance_id.clone()).collect(),
            ..Default::default()
        });
    }
    out
}

/// "Dell Inc.|0A5B": the maker and the model number in the BIOS.
fn computer_identity() -> (String, String) {
    let text = restore::powershell(
        "$c = Get-CimInstance Win32_ComputerSystem; \"$($c.Manufacturer)|$($c.SystemSKUNumber)\"",
    )
    .unwrap_or_default();
    let mut parts = text.trim().splitn(2, '|');
    (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").trim().to_string())
}

fn dell(devices: &[Device], system_id: &str, builtin: bool) -> Result<Vec<Update>, String> {
    if system_id.is_empty() {
        return Err("Dell's model number couldn't be read from the BIOS.".into());
    }
    let index = read_cab(&get_bytes(DELL_INDEX)?)?;
    let Some(path) = dell_model_path(&index, system_id) else {
        return Ok(Vec::new());
    };
    let xml = read_cab(&get_bytes(&format!("{DELL_SITE}{path}"))?)?;
    Ok(dell_updates(devices, parse_dell_catalog(&xml), builtin))
}

// ---------------------------------------------------------------- NVIDIA

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NvidiaDriver {
    pub version: String,
    pub date: String,
    pub link: String,
    pub size: u64,
}

/// What Windows calls the driver version, "32.0.15.7270", is NVIDIA's "572.70".
pub fn nvidia_version(windows_version: &str) -> Option<String> {
    let parts: Vec<&str> = windows_version.split('.').collect();
    if parts.len() < 4 {
        return None;
    }
    let digits = format!("{}{}", parts[2].chars().last()?, parts[3]);
    if digits.len() < 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (major, minor) = digits.split_at(digits.len() - 2);
    Some(format!("{major}.{minor}"))
}

fn size_from_text(text: &str) -> u64 {
    let mut pieces = text.split_whitespace();
    let number: f64 = pieces.next().and_then(|n| n.parse().ok()).unwrap_or(0.0);
    let unit = match pieces.next().unwrap_or("").to_uppercase().as_str() {
        "KB" => 1024.0,
        "MB" => 1024.0 * 1024.0,
        "GB" => 1024.0 * 1024.0 * 1024.0,
        _ => 0.0,
    };
    (number * unit) as u64
}

pub fn parse_nvidia(json: &str) -> Option<NvidiaDriver> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let info = value.get("IDS")?.get(0)?.get("downloadInfo")?;
    let text = |key: &str| info.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let link = text("DownloadURL");
    let version = text("Version");
    if link.is_empty() || version.is_empty() || !allowed(&link) {
        return None;
    }
    let date = chrono::NaiveDate::parse_from_str(&text("ReleaseDateTime"), "%a %b %d, %Y")
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    Some(NvidiaDriver { version, date, link, size: size_from_text(&text("DownloadURLFileSize")) })
}

/// Cards the Game Ready driver still supports: the RTX range and the GTX 16 series.
fn current_geforce(device: &Device) -> bool {
    let name = device.name.to_uppercase();
    device.present
        && device.instance_id.to_uppercase().starts_with("PCI\\VEN_10DE")
        && name.contains("GEFORCE")
        && (name.contains("RTX") || name.contains("GTX 16"))
}

pub fn nvidia_updates(devices: &[Device], driver: &NvidiaDriver) -> Vec<Update> {
    let mut mine: Vec<&Device> = Vec::new();
    let mut current = String::new();
    for device in devices.iter().filter(|d| current_geforce(d)) {
        let installed = device.driver.as_ref().and_then(|d| nvidia_version(&d.version));
        let newer = installed.as_ref().is_none_or(|v| catalog::is_newer(&driver.version, v));
        if newer {
            if current.is_empty() {
                current = installed.unwrap_or_default();
            }
            mine.push(device);
        }
    }
    if mine.is_empty() {
        return Vec::new();
    }
    vec![Update {
        id: format!("nvidia:{}", driver.version),
        kind: Kind::Driver,
        source: Source::Nvidia,
        title: NVIDIA_TITLE.into(),
        model: mine[0].name.clone(),
        provider: "NVIDIA".into(),
        class: "Display".into(),
        hardware_id: mine[0].hardware_ids.first().cloned().unwrap_or_default(),
        version: driver.version.clone(),
        date: driver.date.clone(),
        size: driver.size,
        link: driver.link.clone(),
        devices: mine.iter().map(|d| d.instance_id.clone()).collect(),
        ..Default::default()
    }]
}

fn nvidia(devices: &[Device]) -> Result<Vec<Update>, String> {
    if !devices.iter().any(current_geforce) {
        return Ok(Vec::new());
    }
    let text = decode(&get_bytes(NVIDIA_LOOKUP)?);
    let driver = parse_nvidia(&text).ok_or("NVIDIA's driver list couldn't be read.")?;
    Ok(nvidia_updates(devices, &driver))
}

// ---------------------------------------------------------------- finding and installing

/// Asks each maker that applies to this computer. Gives the offers and any notes about what
/// couldn't be checked. A maker being unreachable never stops the rest of the scan.
pub fn find(devices: &[Device], builtin: bool) -> (Vec<Update>, Vec<String>) {
    let mut updates = Vec::new();
    let mut notes = Vec::new();
    let (maker, system_id) = computer_identity();
    if maker.to_lowercase().contains("dell") {
        match dell(devices, &system_id, builtin) {
            Ok(found) => updates.extend(found),
            Err(why) => notes.push(format!("Dell's driver list couldn't be checked ({why}).")),
        }
    }
    match nvidia(devices) {
        Ok(found) => updates.extend(found),
        Err(why) => notes.push(format!("NVIDIA's driver list couldn't be checked ({why}).")),
    }
    (updates, notes)
}

fn file_name(url: &str) -> String {
    let last = url.rsplit('/').next().unwrap_or("driver.exe");
    let clean: String = last.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).collect();
    if clean.to_lowercase().ends_with(".exe") { clean } else { "driver.exe".to_string() }
}

/// Valid signature, and from the company the file is meant to come from.
fn signed_by(path: &Path, company: &str) -> Result<(), String> {
    let quoted = path.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$s = Get-AuthenticodeSignature -LiteralPath '{quoted}'; if ($s.Status -eq 'Valid') {{ $s.SignerCertificate.Subject }} else {{ 'INVALID ' + $s.Status }}"
    );
    let answer = restore::powershell(&script)?;
    if answer.trim().to_lowercase().contains(&company.to_lowercase()) && !answer.contains("INVALID") {
        Ok(())
    } else {
        Err(format!("The download wasn't signed by {company}, so it wasn't run."))
    }
}

/// Downloads the installer (checksum-checked when the maker gives one), checks who signed it, and
/// runs it quietly. Gives back true when a restart is needed.
pub fn install(update: &Update) -> Result<bool, String> {
    if !allowed(&update.link) {
        return Err("The download wasn't from the manufacturer's own site, so it was refused.".into());
    }
    let (company, silent): (&str, &[&str]) = match update.source {
        Source::Dell => ("Dell", &["/s"]),
        Source::Nvidia => ("NVIDIA", &["-s", "-noreboot", "-noeula"]),
        _ => return Err("This isn't a manufacturer download.".into()),
    };
    let folder = system::data_dir().ok_or("DriverKeep's data folder couldn't be found.")?.join("vendor");
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let target = folder.join(file_name(&update.link));
    let part = target.with_extension("part");

    let hash = download(&update.link, &part)?;
    let expected = update.checksum.to_lowercase();
    if !expected.is_empty() && hex(&hash) != expected {
        let _ = std::fs::remove_file(&part);
        return Err("The download was damaged (its checksum didn't match), so it was refused.".into());
    }
    std::fs::rename(&part, &target).map_err(|e| e.to_string())?;
    let outcome = signed_by(&target, company).and_then(|()| {
        Command::new(&target)
            .args(silent)
            .creation_flags(NO_WINDOW)
            .status()
            .map_err(|e| format!("The installer couldn't start: {e}"))
    });
    let _ = std::fs::remove_file(&target);
    let status = outcome?;
    match (update.source, status.code().unwrap_or(-1)) {
        (_, 0) => Ok(false),
        (Source::Dell, 2 | 6) | (_, 3010) => Ok(true),
        (Source::Dell, 3) => Err("Dell's installer says this package isn't needed on this computer.".into()),
        (_, code) => Err(format!("{company}'s installer stopped (code {code}). Nothing was changed if it didn't finish.")),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn download(url: &str, target: &Path) -> Result<Vec<u8>, String> {
    let mut last = String::new();
    for retry in 0..=catalog::MAX_RETRIES {
        if retry > 0 {
            std::thread::sleep(catalog::retry_wait(retry));
        }
        let attempt = (|| -> Result<Vec<u8>, ureq::Error> {
            let mut response = catalog::client().get(url).call()?;
            let mut reader = response.body_mut().with_config().limit(8 * 1024 * 1024 * 1024).reader();
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
        })();
        match attempt {
            Ok(hash) => return Ok(hash),
            Err(error) => {
                last = format!("Download failed ({error})");
                if !catalog::is_temporary(&error) {
                    break;
                }
            }
        }
    }
    let _ = std::fs::remove_file(target);
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::DriverInfo;

    const INDEX: &str = r#"<?xml version="1.0"?>
<ManifestIndex xmlns="openmanage/cm/dm">
  <GroupManifest type="MTPDK" id="a"><SupportedSystems><Brand key="1"><Model systemID="07E8" systemIDType="BIOS"/></Brand></SupportedSystems>
    <ManifestInformation id="x" path="FOLDER1/1/Inspiron_07E8.cab"/></GroupManifest>
  <GroupManifest type="MTPDK" id="b"><SupportedSystems><Brand key="2"><Model systemID="0A5B" systemIDType="BIOS"/></Brand></SupportedSystems>
    <ManifestInformation id="y" path="FOLDER2/1/Latitude_0A5B.cab"/></GroupManifest>
</ManifestIndex>"#;

    fn catalog_xml(version: &str) -> String {
        format!(
            r#"<Manifest xmlns="openmanage/cm/dm"><SoftwareComponent schemaVersion="3.0" vendorVersion="{version}" packageID="ABC12" path="FOLDER9/6/Graphics_ABC12_WIN_{version}.EXE" dateTime="2026-05-01T10:00:00-05:00" size="1000">
<Name><Display lang="en"><![CDATA[Intel Graphics Driver]]></Display></Name>
<ComponentType value="DRVR"><Display lang="en"><![CDATA[Driver]]></Display></ComponentType>
<SupportedDevices><Device componentID="1"><PCIInfo deviceID="7D55" vendorID="8086" subDeviceID="0C2B" subVendorID="1028"/></Device></SupportedDevices>
<Cryptography><Hash algorithm="MD5">aa</Hash><Hash algorithm="SHA256">{hash}</Hash></Cryptography>
</SoftwareComponent>
<SoftwareComponent vendorVersion="1.0" packageID="BIOS1" path="FOLDER9/1/bios.exe"><ComponentType value="BIOS"/></SoftwareComponent></Manifest>"#,
            hash = "ab".repeat(32)
        )
    }

    fn graphics_card(driver_version: &str, driver_date: &str) -> Device {
        Device {
            name: "Intel(R) Arc(TM) Graphics".into(),
            instance_id: "PCI\\VEN_8086&DEV_7D55&SUBSYS_0C2B1028&REV_08\\3&1&0".into(),
            hardware_ids: vec!["PCI\\VEN_8086&DEV_7D55&SUBSYS_0C2B1028&REV_08".into(), "PCI\\VEN_8086&DEV_7D55".into()],
            present: true,
            driver: Some(DriverInfo { version: driver_version.into(), date: driver_date.into(), ..Default::default() }),
            ..Default::default()
        }
    }

    #[test]
    fn finds_the_models_list_by_its_bios_number() {
        assert_eq!(dell_model_path(INDEX, "0a5b").as_deref(), Some("FOLDER2/1/Latitude_0A5B.cab"));
        assert_eq!(dell_model_path(INDEX, "07E8").as_deref(), Some("FOLDER1/1/Inspiron_07E8.cab"));
        assert_eq!(dell_model_path(INDEX, "FFFF"), None);
    }

    #[test]
    fn reads_driver_packages_only() {
        let found = parse_dell_catalog(&catalog_xml("31.0.101.5000"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Intel Graphics Driver");
        assert_eq!(found[0].date, "2026-05-01");
        assert_eq!(found[0].pci, vec!["pci\\ven_8086&dev_7d55&subsys_0c2b1028"]);
        assert_eq!(found[0].sha256.len(), 64);
    }

    #[test]
    fn a_newer_dell_package_is_offered_for_the_matching_card() {
        let found = parse_dell_catalog(&catalog_xml("31.0.101.5000"));
        let updates = dell_updates(&[graphics_card("31.0.101.4000", "2025-01-01")], found, true);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].source, Source::Dell);
        assert!(updates[0].link.starts_with("https://downloads.dell.com/FOLDER9/"));
        assert_eq!(updates[0].devices.len(), 1);
    }

    #[test]
    fn an_equal_or_older_dell_package_is_not() {
        let found = parse_dell_catalog(&catalog_xml("31.0.101.5000"));
        assert!(dell_updates(&[graphics_card("31.0.101.5000", "2025-01-01")], found.clone(), true).is_empty());
        assert!(dell_updates(&[graphics_card("31.0.101.6000", "2025-01-01")], found, true).is_empty());
    }

    #[test]
    fn nvidia_versions_are_converted() {
        assert_eq!(nvidia_version("32.0.15.7270").as_deref(), Some("572.70"));
        assert_eq!(nvidia_version("32.0.15.6081").as_deref(), Some("560.81"));
        assert_eq!(nvidia_version("10.0"), None);
    }

    const LOOKUP: &str = r#"{ "Success" : "1", "IDS" : [{ "downloadInfo": { "Success" : "1", "Name" : "GeForce%20Game%20Ready%20Driver", "Version" : "621.24", "ReleaseDateTime" : "Thu Nov 12, 2026", "DownloadURL" : "https://us.download.nvidia.com/Windows/621.24/621.24-desktop-win10-win11-64bit-international-dch-whql.exe", "DownloadURLFileSize" : "1.04 GB" } }] }"#;

    #[test]
    fn reads_nvidias_answer() {
        let driver = parse_nvidia(LOOKUP).unwrap();
        assert_eq!(driver.version, "621.24");
        assert_eq!(driver.date, "2026-11-12");
        assert_eq!(driver.size, (1.04 * 1024.0 * 1024.0 * 1024.0) as u64);
        assert!(parse_nvidia("{}").is_none());
    }

    #[test]
    fn a_link_to_another_site_is_refused() {
        assert!(parse_nvidia(&LOOKUP.replace("us.download.nvidia.com", "evil.example.com")).is_none());
        assert!(allowed("https://downloads.dell.com/x.exe"));
        assert!(!allowed("http://downloads.dell.com/x.exe"));
        assert!(!allowed("https://downloads.dell.com.evil.com/x.exe"));
    }

    #[test]
    fn an_older_geforce_driver_gets_the_newer_one() {
        let card = Device {
            name: "NVIDIA GeForce RTX 3080 Ti".into(),
            instance_id: "PCI\\VEN_10DE&DEV_2208&SUBSYS_1&REV_A1\\4&1".into(),
            hardware_ids: vec!["PCI\\VEN_10DE&DEV_2208".into()],
            present: true,
            driver: Some(DriverInfo { version: "32.0.15.7270".into(), ..Default::default() }),
            ..Default::default()
        };
        let driver = parse_nvidia(LOOKUP).unwrap();
        let offers = nvidia_updates(std::slice::from_ref(&card), &driver);
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].source, Source::Nvidia);
        let current = Device { driver: Some(DriverInfo { version: "32.0.16.2124".into(), ..Default::default() }), ..card };
        assert!(nvidia_updates(&[current], &driver).is_empty());
    }

    /// Real network check (run with `cargo test -- --ignored`): Dell's actual lists read correctly.
    #[test]
    #[ignore]
    fn dell_and_nvidia_live_lists_are_understood() {
        let index = read_cab(&get_bytes(DELL_INDEX).unwrap()).unwrap();
        let path = dell_model_path(&index, "07E8").expect("model found");
        let xml = read_cab(&get_bytes(&format!("{DELL_SITE}{path}")).unwrap()).unwrap();
        let packages = parse_dell_catalog(&xml);
        assert!(packages.len() > 3, "only {} packages", packages.len());
        assert!(packages.iter().all(|p| p.sha256.len() == 64 && !p.pci.is_empty()));
        let nvidia = parse_nvidia(&decode(&get_bytes(NVIDIA_LOOKUP).unwrap())).expect("NVIDIA answer");
        assert!(allowed(&nvidia.link) && nvidia.size > 100_000_000);
        println!("{} Dell packages; NVIDIA {} ({})", packages.len(), nvidia.version, nvidia.date);
    }

    #[test]
    fn old_geforce_cards_are_left_alone() {
        let old = Device {
            name: "NVIDIA GeForce GTX 970".into(),
            instance_id: "PCI\\VEN_10DE&DEV_13C2\\4&1".into(),
            present: true,
            ..Default::default()
        };
        assert!(nvidia_updates(&[old], &parse_nvidia(LOOKUP).unwrap()).is_empty());
    }
}

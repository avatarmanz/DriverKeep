//! Lenovo drivers, from the catalog Lenovo publishes for each machine type (the one its own System
//! Update and Thin Installer read). Each package has a small description file that says which devices
//! it is for, how to tell it is already installed, its SHA-256 and the silent command.
//!
//! Only packages that can be judged safely are offered: those whose "already installed" test is made
//! purely of driver-version rules. Anything involving firmware, BIOS, the embedded controller, the
//! registry or files is left alone, since DriverKeep can't tell whether it is needed.

use crate::catalog;
use crate::devices::Device;
use crate::system;
use crate::vendor;
use crate::wu::{Kind, Source, Update};
use std::os::windows::process::CommandExt;
use std::process::Command;

const CATALOG: &str = "https://download.lenovo.com/catalog/";

#[derive(Clone, Debug, PartialEq)]
pub enum Rule {
    /// True when every present device with one of these ids has a driver at least this version
    /// (and when there is no such device at all).
    Driver { ids: Vec<String>, at_least: String },
    And(Vec<Rule>),
    Or(Vec<Rule>),
    Not(Box<Rule>),
    /// Something DriverKeep can't check.
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Package {
    pub id: String,
    pub title: String,
    pub version: String,
    pub date: String,
    pub descriptor: String,
    /// Hardware id starts (lowercase) the package is for.
    pub devices: Vec<String>,
    pub installed_when: Rule,
    pub installer: String,
    pub sha256: String,
    pub size: u64,
    pub extract: Option<String>,
    pub install: String,
    pub ok_codes: Vec<i32>,
    pub reboot_type: u32,
}

fn named<'a, 'b, 'n>(node: roxmltree::Node<'a, 'b>, name: &'n str) -> impl Iterator<Item = roxmltree::Node<'a, 'b>> + use<'a, 'b, 'n> {
    node.children().filter(move |c| c.is_element() && c.tag_name().name() == name)
}

fn first<'a, 'b>(node: roxmltree::Node<'a, 'b>, name: &str) -> Option<roxmltree::Node<'a, 'b>> {
    named(node, name).next()
}

fn text_of(node: roxmltree::Node) -> String {
    node.text().unwrap_or("").trim().to_string()
}

fn deep<'a, 'b>(node: roxmltree::Node<'a, 'b>, name: &str) -> Option<roxmltree::Node<'a, 'b>> {
    node.descendants().find(|c| c.is_element() && c.tag_name().name() == name)
}

/// "10.1.24.6^" means that version or newer.
fn version_floor(text: &str) -> String {
    text.trim().split('^').next().unwrap_or("").to_string()
}

fn parse_rule(node: roxmltree::Node) -> Rule {
    let kids = |n: roxmltree::Node| n.children().filter(|c| c.is_element()).map(parse_rule).collect::<Vec<_>>();
    match node.tag_name().name() {
        "And" => Rule::And(kids(node)),
        "Or" => Rule::Or(kids(node)),
        "Not" => Rule::Not(Box::new(kids(node).into_iter().next().unwrap_or(Rule::Unknown))),
        "_Driver" => {
            let ids: Vec<String> = named(node, "HardwareID").map(|n| text_of(n).to_lowercase()).filter(|s| !s.is_empty()).collect();
            let at_least = first(node, "Version").map(|v| version_floor(&text_of(v))).unwrap_or_default();
            if ids.is_empty() || at_least.is_empty() { Rule::Unknown } else { Rule::Driver { ids, at_least } }
        }
        _ => Rule::Unknown,
    }
}

fn only_driver_rules(rule: &Rule) -> bool {
    match rule {
        Rule::Driver { .. } => true,
        Rule::And(v) | Rule::Or(v) => !v.is_empty() && v.iter().all(only_driver_rules),
        Rule::Not(r) => only_driver_rules(r),
        Rule::Unknown => false,
    }
}

/// The machine catalog's entries that could be drivers: location of each package's description.
pub fn parse_catalog(xml: &str) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(xml.trim_start_matches('\u{feff}')) else { return Vec::new() };
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "package")
        .filter_map(|p| {
            let category = deep(p, "category").map(text_of).unwrap_or_default().to_lowercase();
            let location = deep(p, "location").map(text_of)?;
            // Firmware and utilities are never judged here.
            (!category.contains("firmware") && !category.contains("bios") && !category.contains("software and utilities"))
                .then_some(location)
        })
        .collect()
}

/// Reads one package's description. Gives nothing for packages that can't be judged safely.
pub fn parse_package(xml: &str, descriptor_url: &str) -> Option<Package> {
    let doc = roxmltree::Document::parse(xml.trim_start_matches('\u{feff}')).ok()?;
    let root = doc.root_element();
    if root.tag_name().name() != "Package" {
        return None;
    }
    let title = deep(root, "Desc").map(text_of).filter(|t| !t.is_empty()).or_else(|| root.attribute("name").map(str::to_string))?;
    let lower = title.to_lowercase();
    if lower.contains("firmware") || lower.contains("bios") || lower.contains("uefi") {
        return None;
    }
    let devices: Vec<String> = first(root, "DetectVersion")
        .map(|d| named(d, "_PnPID").map(|n| text_of(n).to_lowercase()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    let detect = first(root, "DetectInstall")?.children().find(|c| c.is_element()).map(parse_rule)?;
    if devices.is_empty() || !only_driver_rules(&detect) {
        return None;
    }
    let installers: Vec<_> = first(root, "Files").and_then(|f| first(f, "Installer")).map(|i| named(i, "File").collect()).unwrap_or_default();
    if installers.len() != 1 {
        return None;
    }
    let file = installers[0];
    let installer = first(file, "Name").map(text_of)?;
    let sha256 = first(file, "CRC").map(|c| text_of(c).to_lowercase())?;
    if sha256.len() != 64 || installer.is_empty() || installer.contains(['/', '\\']) {
        return None;
    }
    let install_node = first(root, "Install")?;
    let install = deep(install_node, "Cmdline").map(text_of)?;
    let ok_codes: Vec<i32> = install_node
        .attribute("rc")
        .unwrap_or("0")
        .split(',')
        .filter_map(|c| c.trim().parse().ok())
        .collect();
    Some(Package {
        id: root.attribute("id")?.to_string(),
        title,
        version: root.attribute("version").unwrap_or("").to_string(),
        date: first(root, "ReleaseDate").map(text_of).unwrap_or_default(),
        descriptor: descriptor_url.to_string(),
        devices,
        installed_when: detect,
        installer,
        sha256,
        size: first(file, "Size").and_then(|s| text_of(s).parse().ok()).unwrap_or(0),
        extract: first(root, "ExtractCommand").map(text_of).filter(|c| !c.is_empty()),
        install,
        ok_codes,
        reboot_type: first(root, "Reboot").and_then(|r| r.attribute("type")).and_then(|t| t.parse().ok()).unwrap_or(0),
    })
}

fn is_present_for(device: &Device, starts: &[String]) -> bool {
    device.present
        && device.hardware_ids.iter().chain(device.compatible_ids.iter()).any(|id| {
            let id = id.to_lowercase();
            starts.iter().any(|s| id.starts_with(s.as_str()))
        })
}

/// Does the machine already satisfy this rule? None when it can't be told.
pub fn satisfied(rule: &Rule, devices: &[Device]) -> Option<bool> {
    match rule {
        Rule::Driver { ids, at_least } => {
            let mine: Vec<&Device> = devices.iter().filter(|d| is_present_for(d, ids)).collect();
            // No such device here: nothing to update, so the rule holds.
            Some(mine.iter().all(|d| d.driver.as_ref().is_some_and(|driver| !catalog::is_newer(at_least, &driver.version))))
        }
        Rule::And(v) => v.iter().map(|r| satisfied(r, devices)).try_fold(true, |all, r| Some(all && r?)),
        Rule::Or(v) => v.iter().map(|r| satisfied(r, devices)).try_fold(false, |any, r| Some(any || r?)),
        Rule::Not(r) => satisfied(r, devices).map(|b| !b),
        Rule::Unknown => None,
    }
}

/// The packages that are for devices in this computer and aren't installed yet.
pub fn lenovo_updates(devices: &[Device], packages: &[Package]) -> Vec<Update> {
    let mut out = Vec::new();
    for package in packages {
        let mine: Vec<&Device> = devices.iter().filter(|d| is_present_for(d, &package.devices)).collect();
        if mine.is_empty() || satisfied(&package.installed_when, devices) != Some(false) {
            continue;
        }
        out.push(Update {
            id: format!("lenovo:{}", package.id),
            kind: Kind::Driver,
            source: Source::Lenovo,
            title: package.title.clone(),
            model: mine[0].name.clone(),
            provider: "Lenovo".into(),
            hardware_id: mine[0].hardware_ids.first().cloned().unwrap_or_default(),
            version: package.version.clone(),
            date: package.date.clone(),
            size: package.size,
            link: package.descriptor.clone(),
            checksum: package.sha256.clone(),
            devices: mine.iter().map(|d| d.instance_id.clone()).collect(),
            ..Default::default()
        });
    }
    out
}

/// The four-character machine type at the start of Lenovo's model number, "20XW" from "20XWCTO1WW".
pub fn machine_type(product_name: &str) -> Option<String> {
    let mt: String = product_name.trim().chars().take(4).collect();
    (mt.len() == 4 && mt.chars().all(|c| c.is_ascii_alphanumeric())).then(|| mt.to_uppercase())
}

pub fn find(devices: &[Device], product_name: &str) -> Result<Vec<Update>, String> {
    let machine = machine_type(product_name).ok_or("Lenovo's machine type couldn't be read.")?;
    let os = if system::windows_build() >= 22000 { "Win11" } else { "Win10" };
    let url = format!("{CATALOG}{machine}_{os}.xml");
    let listing = match vendor::get_text(&url) {
        Ok(text) => text,
        // No list for this model and Windows version.
        Err(_) => return Ok(Vec::new()),
    };
    let locations: Vec<String> = parse_catalog(&listing).into_iter().filter(|l| vendor::allowed(l)).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let found = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Some(location) = locations.get(index) else { break };
                    if let Some(package) = vendor::get_text(location).ok().and_then(|xml| parse_package(&xml, location)) {
                        found.lock().unwrap().push(package);
                    }
                }
            });
        }
    });
    let packages = found.into_inner().unwrap_or_default();
    Ok(lenovo_updates(devices, &packages))
}

fn fill_path(command: &str, folder: &std::path::Path) -> String {
    let path = folder.to_string_lossy();
    let lower = command.to_lowercase();
    let mut out = String::new();
    let mut at = 0;
    while let Some(found) = lower[at..].find("%packagepath%") {
        out.push_str(&command[at..at + found]);
        out.push_str(&path);
        at += found + "%packagepath%".len();
    }
    out.push_str(&command[at..]);
    out
}

fn run(command: &str, folder: &std::path::Path) -> Result<i32, String> {
    Command::new("cmd.exe")
        .current_dir(folder)
        .raw_arg(format!("/S /C \"{}\"", fill_path(command, folder)))
        .creation_flags(0x0800_0000)
        .status()
        .map(|s| s.code().unwrap_or(-1))
        .map_err(|e| format!("The installer couldn't start: {e}"))
}

/// Gets the package's description again (so the commands come from Lenovo, not from a saved copy),
/// downloads and checks the installer, then runs Lenovo's own silent command.
pub fn install(update: &Update) -> Result<bool, String> {
    if !vendor::allowed(&update.link) || !update.link.starts_with("https://download.lenovo.com/") {
        return Err("The download wasn't from Lenovo's own site, so it was refused.".into());
    }
    let package = parse_package(&vendor::get_text(&update.link)?, &update.link)
        .ok_or("Lenovo's package description couldn't be read, or it is no longer a plain driver package.")?;
    let base = update.link.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
    let folder = system::data_dir()
        .ok_or("DriverKeep's data folder couldn't be found.")?
        .join("vendor")
        .join(format!("lenovo-{}", package.id));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let result = (|| {
        let target = folder.join(&package.installer);
        let hash = vendor::download(&format!("{base}/{}", package.installer), &target.with_extension("part"))?;
        if vendor::hex(&hash) != package.sha256 {
            return Err("The download was damaged (its checksum didn't match), so it was refused.".to_string());
        }
        std::fs::rename(target.with_extension("part"), &target).map_err(|e| e.to_string())?;
        vendor::signed_by(&target, "Lenovo")?;
        if let Some(extract) = &package.extract {
            let code = run(extract, &folder)?;
            if !package.ok_codes.contains(&code) && code != 0 {
                return Err(format!("Lenovo's package couldn't unpack (code {code})."));
            }
        }
        let code = run(&package.install, &folder)?;
        if !package.ok_codes.contains(&code) {
            return Err(format!("Lenovo's installer stopped (code {code}). Nothing was changed if it didn't finish."));
        }
        Ok(matches!(code, 3010 | 1641) || matches!(package.reboot_type, 1 | 5))
    })();
    let _ = std::fs::remove_dir_all(&folder);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::DriverInfo;

    const CATALOG_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?><packages count="3">
<package><location>https://download.lenovo.com/pccbbs/mobiles/a_2_.xml</location><category>Motherboard Devices Backplanes core chipset onboard video PCIe switches</category></package>
<package><location>https://download.lenovo.com/pccbbs/mobiles/b_2_.xml</location><category>Advanced Firmware</category></package>
<package><location>https://download.lenovo.com/pccbbs/mobiles/c_2_.xml</location><category>Software and Utilities</category></package></packages>"#;

    fn package_xml(title: &str, detect: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?><?pcdDescriptor version="0230"?>
<Package name="CHIP" id="n32ic03w" version="10.1.18800.8277" hide="False">
  <Title default="EN"><Desc id="EN">{title}</Desc></Title>
  <DetectVersion type="Inf"><_PnPID><![CDATA[PCI\VEN_8086&DEV_A082]]></_PnPID></DetectVersion>
  <ExtractCommand>n32ic03w.exe /VERYSILENT /DIR=%PACKAGEPATH% /EXTRACT="YES"</ExtractCommand>
  <ReleaseDate>2021-11-09</ReleaseDate>
  <Reboot type="3" />
  <Install rc="0,259,3010" type="cmd" default="EN"><Cmdline id="EN">%PACKAGEPATH%\n32ic03w.exe /VERYSILENT /DIR=%PACKAGEPATH%\TMP</Cmdline></Install>
  <DetectInstall>{detect}</DetectInstall>
  <Files><Installer><File><Name>n32ic03w.exe</Name><CRC>{hash}</CRC><Size>1049112</Size></File></Installer></Files>
</Package>"#,
            hash = "AB".repeat(32)
        )
    }

    const DRIVER_RULE: &str = r#"<_Driver><HardwareID><![CDATA[PCI\VEN_8086&DEV_A082]]></HardwareID><Version>10.1.24.6^</Version></_Driver>"#;

    fn device(version: Option<&str>) -> Device {
        Device {
            name: "Intel Chipset".into(),
            instance_id: "PCI\\VEN_8086&DEV_A082&SUBSYS_1\\3&1".into(),
            hardware_ids: vec!["PCI\\VEN_8086&DEV_A082&SUBSYS_1&REV_01".into()],
            present: true,
            driver: version.map(|v| DriverInfo { version: v.into(), ..Default::default() }),
            ..Default::default()
        }
    }

    #[test]
    fn firmware_and_utility_categories_are_skipped() {
        assert_eq!(parse_catalog(CATALOG_XML), vec!["https://download.lenovo.com/pccbbs/mobiles/a_2_.xml"]);
    }

    #[test]
    fn reads_a_driver_package() {
        let package = parse_package(&package_xml("Intel Chipset Device Software", DRIVER_RULE), "https://download.lenovo.com/pccbbs/mobiles/n32ic03w_2_.xml").unwrap();
        assert_eq!(package.version, "10.1.18800.8277");
        assert_eq!(package.devices, vec!["pci\\ven_8086&dev_a082"]);
        assert_eq!(package.ok_codes, vec![0, 259, 3010]);
        assert_eq!(package.installer, "n32ic03w.exe");
        assert_eq!(package.sha256.len(), 64);
        assert!(package.extract.is_some());
    }

    #[test]
    fn firmware_titles_and_unjudgeable_rules_are_refused() {
        assert!(parse_package(&package_xml("System BIOS Update", DRIVER_RULE), "u").is_none());
        let firmware = r#"<_Firmware><Version>1.2^</Version></_Firmware>"#;
        assert!(parse_package(&package_xml("Some Driver", firmware), "u").is_none());
        let mixed = format!("<And>{DRIVER_RULE}{firmware}</And>");
        assert!(parse_package(&package_xml("Some Driver", &mixed), "u").is_none());
    }

    #[test]
    fn an_older_driver_gets_the_package_and_a_current_one_does_not() {
        let package = parse_package(&package_xml("Intel Chipset Device Software", DRIVER_RULE), "https://download.lenovo.com/pccbbs/mobiles/n32ic03w_2_.xml").unwrap();
        let old = lenovo_updates(&[device(Some("10.1.18.1"))], std::slice::from_ref(&package));
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].source, Source::Lenovo);
        assert!(lenovo_updates(&[device(Some("10.1.24.6"))], std::slice::from_ref(&package)).is_empty());
        assert!(lenovo_updates(&[device(Some("10.1.30.0"))], std::slice::from_ref(&package)).is_empty());
        // No driver at all counts as not installed.
        assert_eq!(lenovo_updates(&[device(None)], std::slice::from_ref(&package)).len(), 1);
    }

    #[test]
    fn a_package_for_hardware_this_pc_lacks_is_not_offered() {
        let package = parse_package(&package_xml("Intel Chipset Device Software", DRIVER_RULE), "u").unwrap();
        let other = Device { hardware_ids: vec!["PCI\\VEN_10EC&DEV_8168".into()], present: true, ..Default::default() };
        assert!(lenovo_updates(&[other], &[package]).is_empty());
    }

    #[test]
    fn machine_types_come_from_the_model_number() {
        assert_eq!(machine_type("20XWCTO1WW").as_deref(), Some("20XW"));
        assert_eq!(machine_type("21k3002kus").as_deref(), Some("21K3"));
        assert_eq!(machine_type(""), None);
    }

    #[test]
    fn the_package_path_is_filled_in_whatever_its_case() {
        let filled = fill_path("%PACKAGEPATH%\\a.exe /DIR=%packagepath%\\TMP", std::path::Path::new("C:\\x y"));
        assert_eq!(filled, "C:\\x y\\a.exe /DIR=C:\\x y\\TMP");
    }

    /// Real network check (run with `cargo test -- --ignored`): a real Lenovo catalog is read.
    #[test]
    #[ignore]
    fn lenovo_live_catalog_is_understood() {
        let listing = vendor::get_text(&format!("{CATALOG}20XW_Win11.xml")).unwrap();
        let locations = parse_catalog(&listing);
        assert!(!locations.is_empty());
        let mut judged = 0;
        for location in &locations {
            if let Some(package) = vendor::get_text(location).ok().and_then(|xml| parse_package(&xml, location)) {
                judged += 1;
                println!("{} {} {}", package.id, package.version, package.title);
            }
        }
        println!("{judged} of {} packages can be judged", locations.len());
        assert!(judged > 0);
    }
}

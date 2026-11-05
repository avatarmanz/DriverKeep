//! Everything about the PC, gathered for the one-page System view.
//! Uses the same Windows tables the original reads, so no bundled native library is needed.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use windows::Win32::System::Com::*;
use windows::Win32::System::Variant::{VARIANT, VariantClear, VariantToString};
use windows::Win32::System::Wmi::*;
use windows::core::{BSTR, PCWSTR, w};

pub const CACHE_FILE: &str = "system_info.json";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Section {
    pub title: String,
    pub icon: String,
    pub facts: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SystemInfo {
    pub computer: String,
    pub sections: Vec<Section>,
}

type Row = HashMap<String, String>;

struct Wmi(IWbemServices);

impl Wmi {
    fn connect() -> windows::core::Result<Self> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
            let services = locator.ConnectServer(
                &BSTR::from("ROOT\\CIMV2"),
                &BSTR::new(),
                &BSTR::new(),
                &BSTR::new(),
                0,
                &BSTR::new(),
                None,
            )?;
            // Without this, Windows answers every question with "access denied".
            CoSetProxyBlanket(
                &services,
                10, // Windows sign-in
                0,
                PCWSTR::null(),
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )?;
            Ok(Wmi(services))
        }
    }

    fn query(&self, wql: &str, fields: &[&str]) -> Vec<Row> {
        let mut rows = Vec::new();
        unsafe {
            let Ok(list) = self.0.ExecQuery(
                &BSTR::from("WQL"),
                &BSTR::from(wql),
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None,
            ) else {
                return rows;
            };
            loop {
                let mut items = [None];
                let mut returned = 0u32;
                if list.Next(WBEM_INFINITE, &mut items, &mut returned).is_err() || returned == 0 {
                    break;
                }
                let Some(item) = items[0].take() else { break };
                let mut row = Row::new();
                for field in fields {
                    let name = windows::core::HSTRING::from(*field);
                    let mut value = VARIANT::default();
                    if item.Get(&name, 0, &mut value, None, None).is_ok() {
                        let mut text = vec![0u16; 1024];
                        if VariantToString(&value, &mut text).is_ok() {
                            let end = text.iter().position(|c| *c == 0).unwrap_or(text.len());
                            row.insert(field.to_string(), String::from_utf16_lossy(&text[..end]).trim().to_string());
                        }
                        let _ = VariantClear(&mut value);
                    }
                }
                rows.push(row);
            }
        }
        rows
    }
}

fn get<'a>(row: &'a Row, field: &str) -> &'a str {
    row.get(field).map(String::as_str).unwrap_or("")
}

fn gigabytes(bytes: f64) -> String {
    let gb = bytes / (1024.0 * 1024.0 * 1024.0);
    if gb >= 1000.0 {
        format!("{:.1} TB", gb / 1024.0)
    } else if gb >= 10.0 {
        format!("{gb:.0} GB")
    } else {
        format!("{gb:.1} GB")
    }
}

fn number(text: &str) -> f64 {
    text.parse().unwrap_or(0.0)
}

/// Windows' SMBIOS code for each kind of memory.
pub fn memory_kind(code: &str) -> &'static str {
    match code {
        "20" => "DDR",
        "21" => "DDR2",
        "24" => "DDR3",
        "26" => "DDR4",
        "27" => "LPDDR",
        "28" => "LPDDR2",
        "29" => "LPDDR3",
        "30" => "LPDDR4",
        "34" => "DDR5",
        "35" => "LPDDR5",
        _ => "",
    }
}

fn add(section: &mut Section, label: &str, value: impl Into<String>) {
    let value = value.into();
    let value = value.trim();
    if !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("default string") {
        section.facts.push((label.to_string(), value.to_string()));
    }
}

fn section(title: &str, icon: &str) -> Section {
    Section { title: title.into(), icon: icon.into(), facts: Vec::new() }
}

/// 20250509000000.000000+000 becomes 2025-05-09.
fn wmi_date(text: &str) -> String {
    if text.len() >= 8 {
        format!("{}-{}-{}", &text[..4], &text[4..6], &text[6..8])
    } else {
        String::new()
    }
}

/// Real video memory: the older field stops at 4 GB, so the registry is asked first.
fn video_memory(adapter: &str, fallback: f64) -> f64 {
    use windows::Win32::System::Registry::*;
    const CLASS: &str = "SYSTEM\\CurrentControlSet\\Control\\Class\\{4d36e968-e325-11ce-bfc1-08002be10318}";
    for index in 0..16 {
        let key = format!("{CLASS}\\{index:04}");
        if crate::system::registry_text(&key, "DriverDesc") != adapter {
            continue;
        }
        let mut handle = HKEY::default();
        let path = windows::core::HSTRING::from(key);
        unsafe {
            if RegOpenKeyExW(HKEY_LOCAL_MACHINE, &path, None, KEY_READ, &mut handle).is_err() {
                continue;
            }
            let mut value = 0u64;
            let mut size = 8u32;
            let name: PCWSTR = w!("HardwareInformation.qwMemorySize");
            let read =
                RegQueryValueExW(handle, name, None, None, Some(&mut value as *mut u64 as *mut u8), Some(&mut size));
            let _ = RegCloseKey(handle);
            if read.is_ok() && value > 0 {
                return value as f64;
            }
        }
    }
    fallback
}

pub fn gather() -> Result<SystemInfo, String> {
    let wmi = Wmi::connect().map_err(|e| format!("Windows wouldn't share system details ({e})"))?;
    let mut info = SystemInfo::default();

    let machine = wmi.query("SELECT Manufacturer,Model FROM Win32_ComputerSystem", &["Manufacturer", "Model"]);
    let os = wmi.query(
        "SELECT Caption,OSArchitecture,Version,TotalVisibleMemorySize FROM Win32_OperatingSystem",
        &["Caption", "OSArchitecture", "Version", "TotalVisibleMemorySize"],
    );
    let cpu = wmi.query(
        "SELECT Name,Manufacturer,NumberOfCores,NumberOfLogicalProcessors,MaxClockSpeed,L2CacheSize,L3CacheSize,SocketDesignation,Description FROM Win32_Processor",
        &["Name", "Manufacturer", "NumberOfCores", "NumberOfLogicalProcessors", "MaxClockSpeed", "L2CacheSize", "L3CacheSize", "SocketDesignation", "Description"],
    );
    let board = wmi.query("SELECT Manufacturer,Product,Version FROM Win32_BaseBoard", &["Manufacturer", "Product", "Version"]);
    let bios = wmi.query(
        "SELECT Manufacturer,SMBIOSBIOSVersion,ReleaseDate FROM Win32_BIOS",
        &["Manufacturer", "SMBIOSBIOSVersion", "ReleaseDate"],
    );
    let sticks = wmi.query(
        "SELECT Capacity,Speed,ConfiguredClockSpeed,SMBIOSMemoryType,Manufacturer,PartNumber,DeviceLocator FROM Win32_PhysicalMemory",
        &["Capacity", "Speed", "ConfiguredClockSpeed", "SMBIOSMemoryType", "Manufacturer", "PartNumber", "DeviceLocator"],
    );
    let slots = wmi.query("SELECT MemoryDevices FROM Win32_PhysicalMemoryArray", &["MemoryDevices"]);
    let video = wmi.query(
        "SELECT Name,PNPDeviceID,AdapterRAM,VideoProcessor,DriverVersion,DriverDate,Status,CurrentHorizontalResolution,CurrentVerticalResolution,CurrentRefreshRate,AdapterCompatibility FROM Win32_VideoController",
        &["Name", "PNPDeviceID", "AdapterRAM", "VideoProcessor", "DriverVersion", "DriverDate", "Status", "CurrentHorizontalResolution", "CurrentVerticalResolution", "CurrentRefreshRate", "AdapterCompatibility"],
    );
    let disks = wmi.query("SELECT Model,Size,InterfaceType,MediaType FROM Win32_DiskDrive", &["Model", "Size", "InterfaceType", "MediaType"]);
    let network = wmi.query(
        "SELECT Name,MACAddress,Speed,NetEnabled FROM Win32_NetworkAdapter WHERE PhysicalAdapter=True",
        &["Name", "MACAddress", "Speed", "NetEnabled"],
    );
    let audio = wmi.query("SELECT Name FROM Win32_SoundDevice", &["Name"]);

    let mut computer = section("Computer", "\u{E7F4}");
    if let Some(row) = machine.first() {
        info.computer = format!("{} {}", get(row, "Manufacturer"), get(row, "Model")).trim().to_string();
        add(&mut computer, "Machine", info.computer.clone());
    }
    if let Some(row) = os.first() {
        add(&mut computer, "Windows", format!("{} ({})", get(row, "Caption"), get(row, "OSArchitecture")));
        add(&mut computer, "Build", get(row, "Version"));
    }
    let total_memory = os.first().map(|r| number(get(r, "TotalVisibleMemorySize")) * 1024.0).unwrap_or(0.0);
    if let Some(row) = board.first() {
        add(&mut computer, "Motherboard", format!("{} {}", get(row, "Manufacturer"), get(row, "Product")));
    }
    if let Some(row) = bios.first() {
        add(&mut computer, "BIOS", format!("{} {}", get(row, "Manufacturer"), get(row, "SMBIOSBIOSVersion")));
        add(&mut computer, "BIOS date", wmi_date(get(row, "ReleaseDate")));
    }
    info.sections.push(computer);

    let mut processor = section("Processor", "\u{E950}");
    for (index, row) in cpu.iter().enumerate() {
        let label = |text: &str| if cpu.len() > 1 { format!("{text} {}", index + 1) } else { text.to_string() };
        add(&mut processor, &label("Name"), get(row, "Name"));
        add(&mut processor, &label("Maker"), get(row, "Manufacturer").replace("GenuineIntel", "Intel").replace("AuthenticAMD", "AMD"));
        add(&mut processor, &label("Cores"), format!("{} cores, {} threads", get(row, "NumberOfCores"), get(row, "NumberOfLogicalProcessors")));
        add(&mut processor, &label("Base speed"), format!("{:.2} GHz", number(get(row, "MaxClockSpeed")) / 1000.0));
        let l2 = number(get(row, "L2CacheSize")) / 1024.0;
        let l3 = number(get(row, "L3CacheSize")) / 1024.0;
        if l2 > 0.0 || l3 > 0.0 {
            add(&mut processor, &label("Cache"), format!("L2 {l2:.0} MB, L3 {l3:.0} MB"));
        }
        add(&mut processor, &label("Socket"), get(row, "SocketDesignation"));
    }
    info.sections.push(processor);

    let mut memory = section("Memory", "\u{E964}");
    let installed: f64 = sticks.iter().map(|s| number(get(s, "Capacity"))).sum();
    add(&mut memory, "Total", gigabytes(if installed > 0.0 { installed } else { total_memory }));
    if let Some(stick) = sticks.first() {
        add(&mut memory, "Type", memory_kind(get(stick, "SMBIOSMemoryType")));
        let speed = get(stick, "ConfiguredClockSpeed");
        let speed = if speed.is_empty() || speed == "0" { get(stick, "Speed") } else { speed };
        if !speed.is_empty() && speed != "0" {
            add(&mut memory, "Speed", format!("{speed} MT/s"));
        }
    }
    let total_slots: f64 = slots.iter().map(|s| number(get(s, "MemoryDevices"))).sum();
    if total_slots > 0.0 {
        add(&mut memory, "Slots", format!("{} used of {total_slots:.0}", sticks.len()));
    }
    // Identical modules share one line.
    let mut kinds: Vec<(String, usize)> = Vec::new();
    for stick in &sticks {
        let text = format!(
            "{} {} {}",
            gigabytes(number(get(stick, "Capacity"))),
            get(stick, "Manufacturer"),
            get(stick, "PartNumber")
        );
        match kinds.iter_mut().find(|(kind, _)| *kind == text) {
            Some((_, count)) => *count += 1,
            None => kinds.push((text, 1)),
        }
    }
    for (text, count) in kinds {
        add(&mut memory, "Modules", format!("{count} × {text}"));
    }
    info.sections.push(memory);

    for row in &video {
        let mut card = section("Graphics", "\u{E7F8}");
        add(&mut card, "Name", get(row, "Name"));
        add(&mut card, "Maker", get(row, "AdapterCompatibility"));
        let memory = video_memory(get(row, "Name"), number(get(row, "AdapterRAM")));
        // USB display adapters report shared system memory here, which would mislead.
        if memory > 0.0 && !get(row, "Name").contains("DisplayLink") {
            add(&mut card, "Video memory", gigabytes(memory));
        }
        add(&mut card, "Driver", get(row, "DriverVersion"));
        add(&mut card, "Driver date", wmi_date(get(row, "DriverDate")));
        let width = get(row, "CurrentHorizontalResolution");
        if !width.is_empty() && width != "0" {
            add(&mut card, "Screen", format!("{width} × {} at {} Hz", get(row, "CurrentVerticalResolution"), get(row, "CurrentRefreshRate")));
        }
        add(&mut card, "Status", get(row, "Status"));
        info.sections.push(card);
    }

    let mut storage = section("Storage", "\u{EDA2}");
    for disk in &disks {
        add(
            &mut storage,
            get(disk, "Model"),
            format!("{}, {}", gigabytes(number(get(disk, "Size"))), get(disk, "InterfaceType")),
        );
    }
    info.sections.push(storage);

    let mut net = section("Network", "\u{E968}");
    for adapter in &network {
        let speed = number(get(adapter, "Speed")) / 1_000_000.0;
        let on = matches!(get(adapter, "NetEnabled").to_lowercase().as_str(), "true" | "-1" | "1");
        let state = if on && speed > 0.0 {
            if speed >= 1000.0 { format!("Connected, {:.1} Gbps", speed / 1000.0) } else { format!("Connected, {speed:.0} Mbps") }
        } else {
            "Not connected".to_string()
        };
        add(&mut net, get(adapter, "Name"), state);
    }
    info.sections.push(net);

    let mut sound = section("Audio", "\u{E767}");
    for (index, device) in audio.iter().enumerate() {
        add(&mut sound, &format!("Device {}", index + 1), get(device, "Name"));
    }
    info.sections.push(sound);

    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_sizes_read_nicely() {
        assert_eq!(wmi_date("20250509000000.000000+000"), "2025-05-09");
        assert_eq!(gigabytes(32.0 * 1024.0 * 1024.0 * 1024.0), "32 GB");
        assert_eq!(gigabytes(2048.0 * 1024.0 * 1024.0 * 1024.0), "2.0 TB");
        assert_eq!(memory_kind("34"), "DDR5");
    }
}

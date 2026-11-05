//! Finds every device Windows knows about, connected now or not.
//! The rules here follow app/MECHANICS.md ("Finding hardware").

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::Properties::*;
use windows::Win32::Foundation::{DEVPROPKEY, ERROR_NO_MORE_ITEMS};
use windows::core::{GUID, HSTRING, PCWSTR};

/// Device paths the original leaves out of its list.
pub const ORIGINAL_HIDDEN_PREFIXES: [&str; 7] =
    ["ROOT", "SW", "ISAPNP", "HTREE", "STORAGE", "USBSTOR", "UMB"];
/// USB sticks and drives sit under this prefix. The person asked for hardware that used to be
/// plugged in, so unlike the original we keep them in the main list.
pub const KEPT_FOR_PAST_HARDWARE: &str = "USBSTOR";
/// "Other devices": used when Windows gives no device type.
pub const OTHER_DEVICES_CLASS: &str = "{4D36E97E-E325-11CE-BFC1-08002BE10318}";

const INSTALL_DATES: GUID = GUID::from_u128(0x83da6326_97a6_4088_9453_a1923f573b29);
const ERROR_NO_SUCH_DEVINST: u32 = 0xE000_020B;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DriverInfo {
    pub inf_name: String,
    pub inf_section: String,
    pub matching_id: String,
    pub provider: String,
    pub version: String,
    pub date: String,
    pub description: String,
}

impl DriverInfo {
    /// Third-party packages are the ones Windows names oem<number>.inf.
    pub fn is_third_party(&self) -> bool {
        let n = self.inf_name.to_lowercase();
        n.starts_with("oem")
            && n.ends_with(".inf")
            && n[3..n.len() - 4].chars().all(|c| c.is_ascii_digit())
            && n.len() > 7
    }
}

#[derive(Clone, Debug, Default)]
pub struct Device {
    pub instance_id: String,
    pub name: String,
    pub class_guid: String,
    pub class_name: String,
    pub class_title: String,
    pub manufacturer: String,
    pub location: String,
    pub hardware_ids: Vec<String>,
    pub compatible_ids: Vec<String>,
    pub present: bool,
    pub system_item: bool,
    pub problem_code: u32,
    pub no_driver: bool,
    pub driver: Option<DriverInfo>,
    pub first_installed: Option<i64>,
    pub last_connected: Option<i64>,
    pub last_removed: Option<i64>,
}

impl Device {
    pub fn needs_driver(&self) -> bool {
        self.present && self.no_driver
    }
    pub fn has_problem(&self) -> bool {
        self.present && self.problem_code != 0
    }
    pub fn answers_to(&self, id: &str) -> bool {
        self.hardware_ids
            .iter()
            .chain(self.compatible_ids.iter())
            .any(|h| h.eq_ignore_ascii_case(id))
    }
}

struct DeviceList(HDEVINFO);
impl Drop for DeviceList {
    fn drop(&mut self) {
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

fn blank_data() -> SP_DEVINFO_DATA {
    SP_DEVINFO_DATA {
        cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
        ..Default::default()
    }
}

fn wide_to_string(words: &[u16]) -> String {
    let end = words.iter().position(|c| *c == 0).unwrap_or(words.len());
    String::from_utf16_lossy(&words[..end])
}

fn bytes_to_words(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect()
}

fn registry_bytes(
    set: HDEVINFO,
    data: &SP_DEVINFO_DATA,
    property: SETUP_DI_REGISTRY_PROPERTY,
) -> Option<Vec<u8>> {
    let mut needed = 0u32;
    unsafe {
        let _ = SetupDiGetDeviceRegistryPropertyW(
            set,
            data,
            property,
            None,
            None,
            Some(&mut needed),
        );
    }
    if needed == 0 {
        return None;
    }
    let mut buffer = vec![0u8; needed as usize];
    unsafe {
        SetupDiGetDeviceRegistryPropertyW(set, data, property, None, Some(&mut buffer), None)
            .ok()?;
    }
    Some(buffer)
}

fn registry_text(set: HDEVINFO, data: &SP_DEVINFO_DATA, id: u32) -> String {
    registry_bytes(set, data, SETUP_DI_REGISTRY_PROPERTY(id))
        .map(|b| wide_to_string(&bytes_to_words(&b)))
        .unwrap_or_default()
}

fn registry_list(set: HDEVINFO, data: &SP_DEVINFO_DATA, id: u32) -> Vec<String> {
    let Some(bytes) = registry_bytes(set, data, SETUP_DI_REGISTRY_PROPERTY(id)) else {
        return Vec::new();
    };
    bytes_to_words(&bytes)
        .split(|c| *c == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

fn property_bytes(set: HDEVINFO, data: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<Vec<u8>> {
    let mut kind = DEVPROPTYPE::default();
    let mut needed = 0u32;
    unsafe {
        let _ = SetupDiGetDevicePropertyW(set, data, key, &mut kind, None, Some(&mut needed), 0);
    }
    if needed == 0 {
        return None;
    }
    let mut buffer = vec![0u8; needed as usize];
    unsafe {
        SetupDiGetDevicePropertyW(set, data, key, &mut kind, Some(&mut buffer), None, 0).ok()?;
    }
    Some(buffer)
}

fn property_text(set: HDEVINFO, data: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> String {
    property_bytes(set, data, key)
        .map(|b| wide_to_string(&bytes_to_words(&b)))
        .unwrap_or_default()
}

/// Windows stores these dates as 100-nanosecond steps since the year 1601.
fn filetime_to_unix(bytes: &[u8]) -> Option<i64> {
    if bytes.len() < 8 {
        return None;
    }
    let ticks = u64::from_le_bytes(bytes[..8].try_into().ok()?);
    if ticks == 0 {
        return None;
    }
    Some((ticks / 10_000_000) as i64 - 11_644_473_600)
}

fn property_time(set: HDEVINFO, data: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<i64> {
    filetime_to_unix(&property_bytes(set, data, key)?)
}

fn property_u32(set: HDEVINFO, data: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> u32 {
    property_bytes(set, data, key)
        .filter(|b| b.len() >= 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .unwrap_or(0)
}

fn format_guid(g: &GUID) -> String {
    format!("{{{:?}}}", g).to_uppercase()
}

/// A device counts as gone only when Windows answers "no such device".
pub fn is_present(instance_id: &str) -> bool {
    let mut node = 0u32;
    let id = HSTRING::from(instance_id);
    let result = unsafe { CM_Locate_DevNodeW(&mut node, &id, CM_LOCATE_DEVNODE_NORMAL) };
    result != CR_NO_SUCH_DEVNODE
}

fn class_title(guid: &GUID, cache: &mut HashMap<String, String>, key: &str) -> String {
    if let Some(found) = cache.get(key) {
        return found.clone();
    }
    let mut text = vec![0u16; 256];
    let title = unsafe {
        SetupDiGetClassDescriptionW(guid, &mut text, None)
            .map(|_| wide_to_string(&text))
            .unwrap_or_default()
    };
    cache.insert(key.to_string(), title.clone());
    title
}

fn prefix_of(instance_id: &str) -> &str {
    instance_id.split('\\').next().unwrap_or("")
}

fn is_system_item(instance_id: &str) -> bool {
    let prefix = prefix_of(instance_id);
    if prefix.eq_ignore_ascii_case(KEPT_FOR_PAST_HARDWARE) {
        return false;
    }
    ORIGINAL_HIDDEN_PREFIXES
        .iter()
        .any(|p| p.eq_ignore_ascii_case(prefix))
}

/// Lists all devices Windows has on record. No "present only" filter is passed, which is what
/// brings in hardware that used to be plugged in.
pub fn enumerate(mut progress: impl FnMut(usize, &str)) -> Result<Vec<Device>, String> {
    let handle = unsafe { SetupDiGetClassDevsW(None, PCWSTR::null(), None, DIGCF_ALLCLASSES) }
        .map_err(|e| format!("Windows would not list devices: {e}"))?;
    let list = DeviceList(handle);
    let set = list.0;
    let mut out = Vec::new();
    let mut titles = HashMap::new();
    let mut index = 0u32;
    loop {
        let mut data = blank_data();
        match unsafe { SetupDiEnumDeviceInfo(set, index, &mut data) } {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_NO_MORE_ITEMS.to_hresult() => break,
            Err(_) => break,
        }
        index += 1;

        let mut id = vec![0u16; 1024];
        if unsafe { SetupDiGetDeviceInstanceIdW(set, &data, Some(&mut id), None) }.is_err() {
            continue;
        }
        let instance_id = wide_to_string(&id);
        if instance_id.is_empty() {
            continue;
        }

        let mut device = Device {
            present: is_present(&instance_id),
            system_item: is_system_item(&instance_id),
            hardware_ids: registry_list(set, &data, SPDRP_HARDWAREID.0),
            compatible_ids: registry_list(set, &data, SPDRP_COMPATIBLEIDS.0),
            manufacturer: registry_text(set, &data, SPDRP_MFG.0),
            location: registry_text(set, &data, SPDRP_LOCATION_INFORMATION.0),
            instance_id: instance_id.clone(),
            ..Default::default()
        };

        // Name order: friendly name, then description, then the first hardware id.
        device.name = registry_text(set, &data, SPDRP_FRIENDLYNAME.0);
        if device.name.is_empty() {
            device.name = registry_text(set, &data, SPDRP_DEVICEDESC.0);
        }
        if device.name.is_empty() {
            device.name = device
                .hardware_ids
                .first()
                .cloned()
                .unwrap_or_else(|| instance_id.clone());
        }

        let class = registry_text(set, &data, SPDRP_CLASS.0);
        let has_class = data.ClassGuid != GUID::zeroed();
        device.class_guid = if has_class {
            format_guid(&data.ClassGuid)
        } else {
            OTHER_DEVICES_CLASS.to_string()
        };
        device.class_name = if class.is_empty() { "Unknown".into() } else { class };
        let title = if has_class {
            class_title(&data.ClassGuid, &mut titles, &device.class_guid)
        } else {
            String::new()
        };
        device.class_title = if title.is_empty() {
            device.class_name.clone()
        } else {
            title
        };

        device.first_installed = property_time(
            set,
            &data,
            &DEVPROPKEY { fmtid: INSTALL_DATES, pid: 101 },
        );
        device.last_connected = property_time(
            set,
            &data,
            &DEVPROPKEY { fmtid: INSTALL_DATES, pid: 102 },
        );
        device.last_removed = property_time(
            set,
            &data,
            &DEVPROPKEY { fmtid: INSTALL_DATES, pid: 103 },
        );
        device.problem_code = property_u32(set, &data, &DEVPKEY_Device_ProblemCode);

        let inf_name = property_text(set, &data, &DEVPKEY_Device_DriverInfPath);
        if inf_name.is_empty() {
            // Same test the original uses before it calls a device driverless:
            // connected, no driver file on record and no driver key either.
            device.no_driver =
                device.present && registry_text(set, &data, SPDRP_DRIVER.0).is_empty();
        } else {
            let section = property_text(set, &data, &DEVPKEY_Device_DriverInfSection);
            let extra = property_text(set, &data, &DEVPKEY_Device_DriverInfSectionExt);
            let date = property_time(set, &data, &DEVPKEY_Device_DriverDate)
                .map(short_date)
                .unwrap_or_default();
            device.driver = Some(DriverInfo {
                inf_name,
                inf_section: format!("{section}{extra}"),
                matching_id: property_text(set, &data, &DEVPKEY_Device_MatchingDeviceId),
                provider: property_text(set, &data, &DEVPKEY_Device_DriverProvider),
                version: property_text(set, &data, &DEVPKEY_Device_DriverVersion),
                description: property_text(set, &data, &DEVPKEY_Device_DriverDesc),
                date,
            });
        }

        progress(index as usize, &device.name);
        out.push(device);
    }
    out.sort_by(|a, b| {
        (a.class_title.to_lowercase(), a.name.to_lowercase())
            .cmp(&(b.class_title.to_lowercase(), b.name.to_lowercase()))
    });
    Ok(out)
}

pub fn short_date(unix: i64) -> String {
    chrono::DateTime::from_timestamp(unix, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// Where Windows keeps the complete driver package for an installed driver.
pub fn package_folder(inf_name: &str) -> Option<PathBuf> {
    let windows_dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let path = HSTRING::from(format!("{windows_dir}\\INF\\{inf_name}"));
    let mut text = vec![0u16; 1024];
    unsafe {
        SetupGetInfDriverStoreLocationW(&path, None, PCWSTR::null(), &mut text, None).ok()?;
    }
    let inf = PathBuf::from(wide_to_string(&text));
    inf.is_file().then_some(inf)
}

#[derive(Debug, PartialEq)]
pub enum InstallOutcome {
    Installed { restart: bool },
    Unplugged,
    NotBetter,
}

/// Puts a driver package on the device. A plain install is tried first. When Windows says
/// "nothing better", `force` pushes it on, which is what lets an older driver go back.
/// Updates never force: Windows gets the final say on what's better.
pub fn install_inf(matching_id: &str, inf_path: &str, force: bool) -> Result<InstallOutcome, String> {
    let id = HSTRING::from(matching_id);
    let inf = HSTRING::from(inf_path);
    let mut restart = windows_core::BOOL(0);
    let first = unsafe {
        UpdateDriverForPlugAndPlayDevicesW(None, &id, &inf, Default::default(), Some(&mut restart))
    };
    let error = match first {
        Ok(()) => return Ok(InstallOutcome::Installed { restart: restart.as_bool() }),
        Err(e) => e,
    };
    let code = (error.code().0 as u32) & 0xFFFF;
    let raw = error.code().0 as u32;
    if code == ERROR_NO_MORE_ITEMS.0 {
        if !force {
            return Ok(InstallOutcome::NotBetter);
        }
        return unsafe {
            UpdateDriverForPlugAndPlayDevicesW(None, &id, &inf, INSTALLFLAG_FORCE, Some(&mut restart))
        }
        .map(|_| InstallOutcome::Installed { restart: restart.as_bool() })
        .map_err(|e| format!("Windows refused the driver ({e})"));
    }
    if raw == ERROR_NO_SUCH_DEVINST {
        return Ok(InstallOutcome::Unplugged);
    }
    Err(format!("Windows refused the driver ({error})"))
}

fn open_one(instance_id: &str) -> Option<(DeviceList, SP_DEVINFO_DATA)> {
    let list = DeviceList(unsafe { SetupDiCreateDeviceInfoList(None, None) }.ok()?);
    let mut data = blank_data();
    let id = HSTRING::from(instance_id);
    unsafe { SetupDiOpenDeviceInfoW(list.0, &id, None, 0, Some(&mut data)) }.ok()?;
    Some((list, data))
}

/// Windows keeps a note of the previous driver when one was replaced.
pub fn can_roll_back(instance_id: &str) -> bool {
    let Some((list, mut data)) = open_one(instance_id) else {
        return false;
    };
    if unsafe { SetupDiBuildDriverInfoList(list.0, Some(&mut data), SPDIT_COMPATDRIVER) }.is_err() {
        return false;
    }
    !property_text(list.0, &data, &DEVPROPKEY { fmtid: INSTALL_DATES, pid: 4 }).is_empty()
}

/// Asks Windows to put the previous driver back. Returns true when a restart is needed.
pub fn roll_back(instance_id: &str) -> Result<bool, String> {
    let (list, mut data) = open_one(instance_id).ok_or("That device could not be opened.")?;
    unsafe { SetupDiBuildDriverInfoList(list.0, Some(&mut data), SPDIT_COMPATDRIVER) }
        .map_err(|e| format!("Windows could not read this device's drivers ({e})"))?;
    let mut restart = windows_core::BOOL(0);
    unsafe { DiRollbackDriver(list.0, &data, None, ROLLBACK_FLAG_NO_UI, Some(&mut restart)) }
        .map_err(|e| format!("Windows could not roll back this driver ({e})"))?;
    Ok(restart.as_bool())
}

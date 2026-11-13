//! Find NVIDIA GPUs (vendor 0x10de) via /sys, with lspci for the marketing name.

use serde::Serialize;
use std::fs;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Gpu {
    /// "10de:2208"
    pub pci_id: String,
    pub name: String,
    /// Can use NVIDIA's open kernel modules (Turing and newer). None if the id could not be read.
    pub supports_open: Option<bool>,
}

/// Turing (2018) is the first generation the open kernel modules support. Device ids of Turing
/// and every later generation (Ampere 0x20xx/0x22xx, Hopper 0x23xx, Ada 0x26xx+, Blackwell 0x29xx+)
/// are >= 0x1e00, while Maxwell/Pascal/Volta are below it.
pub fn supports_open(device_id: u16) -> bool {
    device_id >= 0x1e00
}

/// Parse lines from `lspci -nn -d 10de:`; keeps display controllers (class 03xx) only.
/// Example: `01:00.0 VGA compatible controller [0300]: NVIDIA Corporation GA102 [GeForce RTX 3080 Ti] [10de:2208] (rev a1)`
pub fn parse_lspci(text: &str) -> Vec<Gpu> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(idpos) = line.rfind("[10de:") else { continue };
        let id_end = match line[idpos..].find(']') {
            Some(e) => idpos + e,
            None => continue,
        };
        let dev_hex = &line[idpos + 6..id_end];
        let Ok(dev) = u16::from_str_radix(dev_hex, 16) else { continue };
        // class "[03xx]:" appears right before the description
        let Some(colon) = line.find("]: ") else { continue };
        let before = &line[..colon];
        let Some(cls_start) = before.rfind('[') else { continue };
        let class = &before[cls_start + 1..];
        if !class.starts_with("03") {
            continue;
        }
        let desc = line[colon + 3..idpos].trim();
        let desc = desc.strip_prefix("NVIDIA Corporation").unwrap_or(desc).trim();
        // Prefer the bracketed marketing name: "GA102 [GeForce RTX 3080 Ti]".
        let name = match (desc.rfind('['), desc.rfind(']')) {
            (Some(a), Some(b)) if b > a => format!("NVIDIA {}", &desc[a + 1..b]),
            _ => format!("NVIDIA {}", desc),
        };
        out.push(Gpu {
            pci_id: format!("10de:{}", dev_hex.to_lowercase()),
            name,
            supports_open: Some(supports_open(dev)),
        });
    }
    out
}

/// Scan /sys/bus/pci/devices for NVIDIA display controllers.
fn sysfs_devices() -> Vec<u16> {
    let mut ids = Vec::new();
    let Ok(dir) = fs::read_dir("/sys/bus/pci/devices") else { return ids };
    for e in dir.flatten() {
        let p = e.path();
        let read = |f: &str| fs::read_to_string(p.join(f)).unwrap_or_default().trim().to_lowercase();
        if read("vendor") != "0x10de" {
            continue;
        }
        if !read("class").starts_with("0x03") {
            continue;
        }
        if let Ok(d) = u16::from_str_radix(read("device").trim_start_matches("0x"), 16) {
            ids.push(d);
        }
    }
    ids
}

pub fn detect() -> Vec<Gpu> {
    // lspci gives names; fall back to sysfs ids alone if lspci is missing (minimal servers).
    let from_lspci = crate::exec::stdout_of("lspci", &["-nn", "-d", "10de:"])
        .map(|t| parse_lspci(&t))
        .unwrap_or_default();
    if !from_lspci.is_empty() {
        return from_lspci;
    }
    sysfs_devices()
        .into_iter()
        .map(|d| Gpu {
            pci_id: format!("10de:{:04x}", d),
            name: "NVIDIA GPU (install pciutils for the model name)".to_string(),
            supports_open: Some(supports_open(d)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LSPCI: &str = "\
01:00.0 VGA compatible controller [0300]: NVIDIA Corporation GA102 [GeForce RTX 3080 Ti] [10de:2208] (rev a1)
01:00.1 Audio device [0403]: NVIDIA Corporation GA102 High Definition Audio Controller [10de:1aef] (rev a1)
";

    #[test]
    fn parses_3080ti_and_skips_audio() {
        let g = parse_lspci(LSPCI);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].pci_id, "10de:2208");
        assert_eq!(g[0].name, "NVIDIA GeForce RTX 3080 Ti");
        assert_eq!(g[0].supports_open, Some(true));
    }

    #[test]
    fn pascal_is_not_open_capable() {
        let t = "03:00.0 VGA compatible controller [0300]: NVIDIA Corporation GP104 [GeForce GTX 1080] [10de:1b80] (rev a1)\n";
        let g = parse_lspci(t);
        assert_eq!(g[0].supports_open, Some(false));
    }

    #[test]
    fn three_d_controller_class_counts() {
        let t = "3b:00.0 3D controller [0302]: NVIDIA Corporation GA100 [A100 PCIe 40GB] [10de:20f1] (rev a1)\n";
        let g = parse_lspci(t);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].supports_open, Some(true));
    }

    #[test]
    fn open_threshold() {
        assert!(!supports_open(0x1d81)); // Titan V (Volta)
        assert!(supports_open(0x1e04)); // RTX 2080 Ti (Turing)
        assert!(supports_open(0x2684)); // RTX 4090 (Ada)
    }

    #[test]
    fn ignores_other_vendors() {
        let t = "00:02.0 VGA compatible controller [0300]: Intel Corporation UHD Graphics [8086:3e92]\n";
        assert!(parse_lspci(t).is_empty());
    }
}

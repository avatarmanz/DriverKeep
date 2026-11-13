//! What driver is installed / loaded right now: nvidia-smi, modinfo, /proc/driver/nvidia/version.

use crate::exec;
use crate::version;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Flavor {
    Proprietary,
    Open,
}

impl Flavor {
    pub fn label(self) -> &'static str {
        match self {
            Flavor::Proprietary => "proprietary",
            Flavor::Open => "open",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmiGpu {
    pub driver_version: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmiResult {
    Gpus(Vec<SmiGpu>),
    /// nvidia-smi ran but could not talk to the driver (not loaded, mismatch, Secure Boot...).
    Failed(String),
}

/// Parse `nvidia-smi --query-gpu=driver_version,name --format=csv,noheader`.
pub fn parse_smi_query(text: &str) -> SmiResult {
    let t = text.trim();
    if t.is_empty() {
        return SmiResult::Failed("nvidia-smi printed nothing".into());
    }
    let mut gpus = Vec::new();
    for line in t.lines() {
        if let Some((v, n)) = line.split_once(',') {
            if version::upstream(v.trim()).is_some() {
                gpus.push(SmiGpu {
                    driver_version: v.trim().to_string(),
                    name: n.trim().to_string(),
                });
                continue;
            }
        }
        // Anything else is an error message ("NVIDIA-SMI has failed because ...",
        // "Failed to initialize NVML: Driver/library version mismatch").
        return SmiResult::Failed(line.trim().to_string());
    }
    SmiResult::Gpus(gpus)
}

/// Fallback: pull "Driver Version: 550.54.14" out of the plain `nvidia-smi` banner.
pub fn parse_smi_banner(text: &str) -> Option<String> {
    let i = text.find("Driver Version:")?;
    let rest = text[i + "Driver Version:".len()..].trim_start();
    let v: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    version::upstream(&v)
}

/// `/proc/driver/nvidia/version`: "NVRM version: NVIDIA UNIX x86_64 Kernel Module  550.54.14  Thu ..."
/// (open modules print "NVIDIA UNIX Open Kernel Module for x86_64  550.54.14 ...").
pub fn parse_proc_version(text: &str) -> Option<(String, Flavor)> {
    let line = text.lines().find(|l| l.contains("Kernel Module"))?;
    let flavor = if line.contains("Open Kernel Module") { Flavor::Open } else { Flavor::Proprietary };
    let idx = line.find("Kernel Module")? + "Kernel Module".len();
    let tail = &line[idx..];
    for tok in tail.split_whitespace() {
        if tok.chars().next().map_or(false, |c| c.is_ascii_digit()) && tok.contains('.') {
            return version::upstream(tok).map(|v| (v, flavor));
        }
    }
    None
}

/// `modinfo -F license nvidia`: open modules are "Dual MIT/GPL", proprietary is "NVIDIA".
pub fn parse_modinfo_license(text: &str) -> Option<Flavor> {
    let t = text.trim();
    if t.is_empty() {
        None
    } else if t.contains("GPL") || t.contains("MIT") {
        Some(Flavor::Open)
    } else {
        Some(Flavor::Proprietary)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Installed {
    /// Version of the kernel module on disk (what a reboot would load), or from nvidia-smi.
    pub version: Option<String>,
    /// Version of the module currently loaded in the running kernel.
    pub loaded_version: Option<String>,
    pub flavor: Option<Flavor>,
    pub smi_ok: bool,
    pub smi_message: Option<String>,
    pub gpu_name_from_smi: Option<String>,
}

pub fn detect() -> Installed {
    let mut inst = Installed::default();

    if let Some(o) = exec::capture(
        "nvidia-smi",
        &["--query-gpu=driver_version,name", "--format=csv,noheader"],
    ) {
        let text = if o.ok { o.stdout.clone() } else { format!("{}{}", o.stdout, o.stderr) };
        match parse_smi_query(&text) {
            SmiResult::Gpus(g) if !g.is_empty() => {
                inst.smi_ok = true;
                inst.version = version::upstream(&g[0].driver_version);
                inst.loaded_version = inst.version.clone();
                inst.gpu_name_from_smi = Some(g[0].name.clone());
            }
            SmiResult::Gpus(_) => {}
            SmiResult::Failed(m) => {
                // Very old nvidia-smi may not support --query-gpu; try the plain banner.
                if let Some(v) = exec::stdout_of("nvidia-smi", &[]).and_then(|t| parse_smi_banner(&t)) {
                    inst.smi_ok = true;
                    inst.version = Some(v.clone());
                    inst.loaded_version = Some(v);
                } else {
                    inst.smi_message = Some(m);
                }
            }
        }
    } else {
        inst.smi_message = Some("nvidia-smi is not installed".into());
    }

    if let Ok(t) = std::fs::read_to_string("/proc/driver/nvidia/version") {
        if let Some((v, f)) = parse_proc_version(&t) {
            inst.loaded_version = Some(v);
            inst.flavor = Some(f);
        }
    }

    if let Some(v) = exec::stdout_of("modinfo", &["-F", "version", "nvidia"]) {
        if let Some(v) = version::upstream(v.trim()) {
            inst.version = Some(v);
        }
    }
    if inst.flavor.is_none() {
        if let Some(l) = exec::stdout_of("modinfo", &["-F", "license", "nvidia"]) {
            inst.flavor = parse_modinfo_license(&l);
        }
    }
    inst
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_query_output() {
        let r = parse_smi_query("550.54.14, NVIDIA GeForce RTX 3080 Ti\n");
        assert_eq!(
            r,
            SmiResult::Gpus(vec![SmiGpu {
                driver_version: "550.54.14".into(),
                name: "NVIDIA GeForce RTX 3080 Ti".into()
            }])
        );
    }

    #[test]
    fn parses_multi_gpu() {
        let r = parse_smi_query("570.144, NVIDIA A100\n570.144, NVIDIA A100\n");
        match r {
            SmiResult::Gpus(g) => assert_eq!(g.len(), 2),
            _ => panic!(),
        }
    }

    #[test]
    fn detects_failure_messages() {
        let r = parse_smi_query(
            "NVIDIA-SMI has failed because it couldn't communicate with the NVIDIA driver. Make sure that the latest NVIDIA driver is installed and running.\n",
        );
        assert!(matches!(r, SmiResult::Failed(m) if m.starts_with("NVIDIA-SMI has failed")));
        let r = parse_smi_query("Failed to initialize NVML: Driver/library version mismatch\n");
        assert!(matches!(r, SmiResult::Failed(_)));
        assert!(matches!(parse_smi_query(""), SmiResult::Failed(_)));
    }

    #[test]
    fn parses_banner() {
        let b = "| NVIDIA-SMI 550.54.14              Driver Version: 550.54.14      CUDA Version: 12.4     |";
        assert_eq!(parse_smi_banner(b).as_deref(), Some("550.54.14"));
        assert_eq!(parse_smi_banner("nothing"), None);
    }

    #[test]
    fn parses_proc_version() {
        let p = "NVRM version: NVIDIA UNIX x86_64 Kernel Module  550.54.14  Thu Feb  1 20:49:53 UTC 2024\nGCC version:  gcc version 12.2.0\n";
        assert_eq!(parse_proc_version(p), Some(("550.54.14".to_string(), Flavor::Proprietary)));
        let o = "NVRM version: NVIDIA UNIX Open Kernel Module for x86_64  570.144  Release Build  (dvs-builder@U16-I1-N08-12-1)  Tue Apr  1 2025\n";
        assert_eq!(parse_proc_version(o), Some(("570.144".to_string(), Flavor::Open)));
    }

    #[test]
    fn modinfo_license_flavor() {
        assert_eq!(parse_modinfo_license("Dual MIT/GPL\n"), Some(Flavor::Open));
        assert_eq!(parse_modinfo_license("NVIDIA\n"), Some(Flavor::Proprietary));
        assert_eq!(parse_modinfo_license(""), None);
    }
}

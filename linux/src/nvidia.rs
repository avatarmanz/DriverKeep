//! NVIDIA's own download site: latest published version (reference) and the `.run` installer
//! fallback. The ONLY host this program ever downloads from (apart from the distro's own
//! package manager) is download.nvidia.com, over HTTPS, with redirects disabled.

use crate::smi::Flavor;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const HOST_PREFIX: &str = "https://download.nvidia.com/";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Latest {
    pub version: String,
    pub run_url: String,
    pub sha256_url: String,
    pub file_name: String,
}

pub fn is_allowed_url(u: &str) -> bool {
    u.starts_with(HOST_PREFIX) && !u.contains("..") && !u[HOST_PREFIX.len()..].contains('@')
}

/// "x86_64" -> ("Linux-x86_64", "x86_64"). Only these two are published on this path.
pub fn arch_dir(arch: &str) -> Option<(&'static str, &'static str)> {
    match arch {
        "x86_64" => Some(("Linux-x86_64", "x86_64")),
        "aarch64" => Some(("Linux-aarch64", "aarch64")),
        _ => None,
    }
}

/// Parse `latest.txt`: `620.70.06 620.70.06/NVIDIA-Linux-x86_64-620.70.06.run`.
/// Strict: the file name must be exactly what we expect, so a tampered response can't
/// point us at some other path.
pub fn parse_latest(text: &str, arch: &str) -> Option<Latest> {
    let (dir, a) = arch_dir(arch)?;
    let mut it = text.split_whitespace();
    let version = it.next()?;
    let path = it.next()?;
    if it.next().is_some() {
        return None;
    }
    if version.is_empty() || !version.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let file = format!("NVIDIA-Linux-{a}-{version}.run");
    if path != format!("{version}/{file}") {
        return None;
    }
    let base = format!("{HOST_PREFIX}XFree86/{dir}/{version}/{file}");
    Some(Latest {
        version: version.to_string(),
        sha256_url: format!("{base}.sha256sum"),
        run_url: base,
        file_name: file,
    })
}

/// Parse a `.sha256sum` file: `<64 hex>  <filename>`.
pub fn parse_sha256_file(text: &str, expect_name: &str) -> Option<String> {
    let line = text.lines().find(|l| !l.trim().is_empty())?;
    let mut it = line.split_whitespace();
    let hash = it.next()?.to_lowercase();
    let name = it.next()?.trim_start_matches('*');
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) || name != expect_name {
        return None;
    }
    Some(hash)
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0) // never follow a redirect off NVIDIA's host
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(60))
        .user_agent(concat!("driverkeep-linux/", env!("CARGO_PKG_VERSION")))
        .build()
}

fn get_text(url: &str) -> Result<String, String> {
    if !is_allowed_url(url) {
        return Err(format!("refusing to fetch {url}: not on download.nvidia.com"));
    }
    let r = agent().get(url).call().map_err(|e| format!("{url}: {e}"))?;
    if r.status() != 200 {
        return Err(format!("{url}: HTTP {}", r.status()));
    }
    r.into_string().map_err(|e| format!("{url}: {e}"))
}

pub fn fetch_latest(arch: &str) -> Result<Latest, String> {
    let (dir, _) = arch_dir(arch).ok_or_else(|| format!("NVIDIA .run installers are only supported for x86_64/aarch64 here, not {arch}"))?;
    let url = format!("{HOST_PREFIX}XFree86/{dir}/latest.txt");
    let text = get_text(&url)?;
    parse_latest(&text, arch).ok_or_else(|| format!("unexpected content in {url}"))
}

/// Download the `.run` file into `dir` and verify its SHA-256 against NVIDIA's published
/// checksum. The file is deleted if the checksum doesn't match.
pub fn download_verified(l: &Latest, dir: &Path) -> Result<PathBuf, String> {
    if !is_allowed_url(&l.run_url) || !is_allowed_url(&l.sha256_url) {
        return Err("refusing to download from a non-NVIDIA URL".into());
    }
    let expected = parse_sha256_file(&get_text(&l.sha256_url)?, &l.file_name)
        .ok_or("could not read NVIDIA's checksum file")?;
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(&l.file_name);
    let r = agent().get(&l.run_url).call().map_err(|e| format!("{}: {e}", l.run_url))?;
    if r.status() != 200 {
        return Err(format!("{}: HTTP {}", l.run_url, r.status()));
    }
    let total: Option<u64> = r.header("Content-Length").and_then(|v| v.parse().ok());
    let mut reader = r.into_reader();
    let mut out = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let (mut got, mut last_pct) = (0u64, 101u64);
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("download failed: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| format!("write failed (disk full?): {e}"))?;
        got += n as u64;
        if let Some(t) = total.filter(|t| *t > 0) {
            let pct = got * 100 / t;
            if pct / 5 != last_pct / 5 {
                eprintln!("  downloaded {pct}%");
                last_pct = pct;
            }
        }
    }
    drop(out);
    if total.map_or(false, |t| t != got) {
        let _ = fs::remove_file(&path);
        return Err("download was cut short; file removed".into());
    }
    let actual = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>();
    if actual != expected {
        let _ = fs::remove_file(&path);
        return Err(format!("CHECKSUM MISMATCH (expected {expected}, got {actual}); file removed, nothing was installed"));
    }
    eprintln!("  checksum OK ({actual})");
    Ok(path)
}

/// The installer command: DKMS so the module is rebuilt on kernel updates.
pub fn installer_argv(path: &Path, flavor: Flavor) -> Vec<String> {
    vec![
        "sh".into(),
        path.to_string_lossy().into_owned(),
        "--silent".into(),
        "--dkms".into(),
        format!("--kernel-module-type={}", match flavor {
            Flavor::Open => "open",
            Flavor::Proprietary => "proprietary",
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_latest_txt() {
        let l = parse_latest("620.70.06 620.70.06/NVIDIA-Linux-x86_64-620.70.06.run\n", "x86_64").unwrap();
        assert_eq!(l.version, "620.70.06");
        assert_eq!(
            l.run_url,
            "https://download.nvidia.com/XFree86/Linux-x86_64/620.70.06/NVIDIA-Linux-x86_64-620.70.06.run"
        );
        assert!(l.sha256_url.ends_with(".run.sha256sum"));
    }

    #[test]
    fn rejects_odd_latest_txt() {
        assert!(parse_latest("620.70.06 ../../evil.run", "x86_64").is_none());
        assert!(parse_latest("620.70.06 620.70.06/other.run", "x86_64").is_none());
        assert!(parse_latest("abc abc/NVIDIA-Linux-x86_64-abc.run", "x86_64").is_none());
        assert!(parse_latest("", "x86_64").is_none());
        assert!(parse_latest("1.0 1.0/NVIDIA-Linux-x86_64-1.0.run", "riscv64").is_none());
    }

    #[test]
    fn parses_checksum_file() {
        let t = "29118e3abd08c07559bcca3d9c76238d039490af0362a2e5bcfcbe7204afd4b1  NVIDIA-Linux-x86_64-620.70.06.run\n";
        assert_eq!(
            parse_sha256_file(t, "NVIDIA-Linux-x86_64-620.70.06.run").as_deref(),
            Some("29118e3abd08c07559bcca3d9c76238d039490af0362a2e5bcfcbe7204afd4b1")
        );
        assert!(parse_sha256_file(t, "other.run").is_none());
        assert!(parse_sha256_file("short  x.run", "x.run").is_none());
    }

    #[test]
    fn url_allow_list() {
        assert!(is_allowed_url("https://download.nvidia.com/XFree86/Linux-x86_64/latest.txt"));
        assert!(!is_allowed_url("http://download.nvidia.com/x"));
        assert!(!is_allowed_url("https://download.nvidia.com.evil.com/x"));
        assert!(!is_allowed_url("https://download.nvidia.com/../x"));
        assert!(!is_allowed_url("https://example.com/https://download.nvidia.com/"));
    }

    #[test]
    fn installer_command() {
        let v = installer_argv(Path::new("/var/tmp/a.run"), Flavor::Open);
        assert_eq!(v, vec!["sh", "/var/tmp/a.run", "--silent", "--dkms", "--kernel-module-type=open"]);
    }
}

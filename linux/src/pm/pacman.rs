//! Arch Linux (and Arch derivatives), plus a conservative Manjaro path.

use super::{Env, Offer, Probe};
use crate::exec;
use crate::smi::Flavor;
use crate::version;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Kernel {
    Arch,
    Lts,
    Zen,
    Hardened,
    Other,
}

/// Work out the kernel package family from `uname -r` ("6.9.7-arch1-1", "6.6.40-1-lts", "6.9.7-zen1-1-zen").
pub fn kernel_kind(uname: &str) -> Kernel {
    let segs: Vec<&str> = uname.split('-').collect();
    if segs.contains(&"lts") {
        Kernel::Lts
    } else if segs.iter().any(|s| s.starts_with("zen")) {
        Kernel::Zen
    } else if segs.contains(&"hardened") {
        Kernel::Hardened
    } else if segs.iter().any(|s| s.starts_with("arch")) {
        Kernel::Arch
    } else {
        Kernel::Other
    }
}

/// Parse one `pacman -Si <pkg>` block into (name, version).
pub fn parse_si(text: &str) -> Option<(String, String)> {
    let mut name = None;
    let mut ver = None;
    for l in text.lines() {
        if let Some((k, v)) = l.split_once(':') {
            match k.trim() {
                "Name" if name.is_none() => name = Some(v.trim().to_string()),
                "Version" if ver.is_none() => ver = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }
    Some((name?, ver?))
}

/// Parse `pacman -Si` "Repository" line.
fn parse_repo(text: &str) -> String {
    text.lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == "Repository").then(|| v.trim().to_string())
        })
        .unwrap_or_else(|| "pacman".into())
}

fn query(pkg: &str) -> Option<(String, String)> {
    let o = exec::capture("pacman", &["-Si", pkg])?;
    if !o.ok {
        return None;
    }
    let (_, v) = parse_si(&o.stdout)?;
    Some((v, parse_repo(&o.stdout)))
}

/// Manjaro's own kernel-matched packages, e.g. `linux66-nvidia`, `linux612-nvidia-open`.
pub fn manjaro_driver_packages(installed: &str) -> Vec<String> {
    installed
        .lines()
        .map(str::trim)
        .filter(|p| p.starts_with("linux") && p.contains("-nvidia") && !p.contains("utils") && !p.contains("-headers"))
        .map(String::from)
        .collect()
}

pub fn probe(manjaro: bool, env: &Env) -> Probe {
    let mut p = Probe { full_upgrade: true, ..Probe::default() };
    if manjaro {
        let installed = exec::stdout_of("pacman", &["-Qq"]).unwrap_or_default();
        let pkgs = manjaro_driver_packages(&installed);
        if pkgs.is_empty() {
            p.notes.push(
                "Manjaro manages NVIDIA drivers with its own tool; this program won't install one. \
                 Run `sudo mhwd -a pci nonfree 0300` (see the Manjaro wiki), then use this tool to check for updates."
                    .into(),
            );
        }
        for pkg in pkgs {
            if let Some((v, repo)) = query(&pkg) {
                if let Some(up) = version::upstream(&v) {
                    let flavor = if pkg.ends_with("-open") { Flavor::Open } else { Flavor::Proprietary };
                    p.offers.push(Offer { package: pkg, extra: vec![], flavor, version: v, upstream: up, source: repo });
                }
            }
        }
        p.notes.push("Manjaro support is minimal: updates run a full `pacman -Syu`.".into());
        return p;
    }

    let kind = kernel_kind(&env.kernel);
    // (package, flavor, header package for dkms variants)
    let (prop, open): ((&str, Option<&str>), (&str, Option<&str>)) = match kind {
        Kernel::Arch => (("nvidia", None), ("nvidia-open", None)),
        Kernel::Lts => (("nvidia-lts", None), ("nvidia-open-dkms", Some("linux-lts-headers"))),
        Kernel::Zen => (("nvidia-dkms", Some("linux-zen-headers")), ("nvidia-open-dkms", Some("linux-zen-headers"))),
        Kernel::Hardened => (("nvidia-dkms", Some("linux-hardened-headers")), ("nvidia-open-dkms", Some("linux-hardened-headers"))),
        Kernel::Other => (("nvidia-dkms", None), ("nvidia-open-dkms", None)),
    };
    if kind == Kernel::Other {
        p.notes.push(
            "Unrecognised kernel: using the DKMS packages. Make sure the headers package for your kernel is installed."
                .into(),
        );
    }
    for (pkg, hdr, flavor) in [(prop.0, prop.1, Flavor::Proprietary), (open.0, open.1, Flavor::Open)] {
        if let Some((v, repo)) = query(pkg) {
            if let Some(up) = version::upstream(&v) {
                p.offers.push(Offer {
                    package: pkg.into(),
                    extra: hdr.map(|h| vec![h.to_string()]).unwrap_or_default(),
                    flavor,
                    version: v,
                    upstream: up,
                    source: repo,
                });
            }
        }
    }
    p.notes.push(
        "Arch: the version shown comes from your last `pacman -Sy` sync; it may be stale until you sync. \
         Updating runs a full `pacman -Syu` (Arch doesn't support partial upgrades)."
            .into(),
    );
    p
}

pub fn steps(offer: &Offer, full_upgrade: bool) -> Vec<Vec<String>> {
    let mut v = vec!["pacman".to_string(), if full_upgrade { "-Syu" } else { "-S" }.to_string(), "--noconfirm".into(), "--needed".into()];
    v.extend(offer.extra.iter().cloned());
    v.push(offer.package.clone());
    vec![v]
}

#[cfg(test)]
mod tests {
    use super::*;

    const SI: &str = "\
Repository      : extra
Name            : nvidia-open
Version         : 570.144-1
Description     : NVIDIA open kernel modules
Architecture    : x86_64
URL             : https://github.com/NVIDIA/open-gpu-kernel-modules/
Licenses        : custom:NVIDIA
Depends On      : linux  nvidia-utils=570.144
";

    #[test]
    fn parses_si() {
        let (n, v) = parse_si(SI).unwrap();
        assert_eq!(n, "nvidia-open");
        assert_eq!(v, "570.144-1");
        assert_eq!(parse_repo(SI), "extra");
        assert!(parse_si("error: package 'x' was not found").is_none());
    }

    #[test]
    fn kernel_kinds() {
        assert_eq!(kernel_kind("6.9.7-arch1-1"), Kernel::Arch);
        assert_eq!(kernel_kind("6.6.40-1-lts"), Kernel::Lts);
        assert_eq!(kernel_kind("6.9.7-zen1-1-zen"), Kernel::Zen);
        assert_eq!(kernel_kind("6.9.7-hardened1-1-hardened"), Kernel::Hardened);
        assert_eq!(kernel_kind("6.1.0-cachyos"), Kernel::Other);
    }

    #[test]
    fn manjaro_packages() {
        let q = "bash\nlinux66\nlinux66-nvidia\nlinux66-headers\nnvidia-utils\nlinux612-nvidia-open\n";
        assert_eq!(manjaro_driver_packages(q), vec!["linux66-nvidia", "linux612-nvidia-open"]);
    }

    #[test]
    fn steps_use_full_upgrade() {
        let o = Offer {
            package: "nvidia-open".into(),
            extra: vec![],
            flavor: Flavor::Open,
            version: "570.144-1".into(),
            upstream: "570.144".into(),
            source: "extra".into(),
        };
        assert_eq!(steps(&o, true)[0], vec!["pacman", "-Syu", "--noconfirm", "--needed", "nvidia-open"]);
    }
}

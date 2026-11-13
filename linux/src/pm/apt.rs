//! Debian / Ubuntu (apt).

use super::{Env, Offer, Probe};
use crate::exec;
use crate::smi::Flavor;
use crate::version;

#[derive(Debug, PartialEq, Eq)]
pub struct Policy {
    pub installed: Option<String>,
    pub candidate: Option<String>,
}

/// Parse `apt-cache policy <pkg>`; "(none)" and unknown packages give `None`.
pub fn parse_policy(text: &str) -> Policy {
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.trim().strip_prefix(key).map(|v| v.trim().to_string()))
            .filter(|v| !v.is_empty() && v != "(none)")
    };
    Policy { installed: field("Installed:"), candidate: field("Candidate:") }
}

/// Parse `ubuntu-drivers list` (or `apt-cache pkgnames nvidia-driver-`): keeps desktop/compute
/// driver packages `nvidia-driver-NNN` and `nvidia-driver-NNN-open`, drops `-server` branches.
/// Lines look like: `nvidia-driver-570-open, (kernel modules provided by linux-modules-nvidia-570-open-generic)`.
pub fn parse_driver_names(text: &str) -> Vec<(String, Flavor)> {
    let mut out: Vec<(String, Flavor)> = Vec::new();
    for line in text.lines() {
        let name = line.split(',').next().unwrap_or("").trim();
        let Some(rest) = name.strip_prefix("nvidia-driver-") else { continue };
        if rest.contains("server") {
            continue;
        }
        let (digits, open) = match rest.strip_suffix("-open") {
            Some(d) => (d, true),
            None => (rest, false),
        };
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if !out.iter().any(|(n, _)| n == name) {
            out.push((name.to_string(), if open { Flavor::Open } else { Flavor::Proprietary }));
        }
    }
    out
}

fn headers_pkg(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" => Some("linux-headers-amd64"),
        "aarch64" => Some("linux-headers-arm64"),
        _ => None,
    }
}

pub fn probe(ubuntu: bool, env: &Env) -> Probe {
    let mut p = Probe::default();
    if !exec::which("apt-cache") {
        p.notes.push("apt-cache not found".into());
        return p;
    }
    let mut names: Vec<(String, Flavor)> = if ubuntu {
        let from_ud = exec::stdout_of("ubuntu-drivers", &["list"])
            .map(|t| parse_driver_names(&t))
            .unwrap_or_default();
        if from_ud.is_empty() {
            exec::stdout_of("apt-cache", &["pkgnames", "nvidia-driver-"])
                .map(|t| parse_driver_names(&t))
                .unwrap_or_default()
        } else {
            from_ud
        }
    } else {
        vec![
            ("nvidia-driver".to_string(), Flavor::Proprietary),
            // NVIDIA's own CUDA repo metapackage, only present if that repo is configured.
            ("nvidia-open".to_string(), Flavor::Open),
        ]
    };
    names.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, flavor) in names {
        let Some(o) = exec::capture("apt-cache", &["policy", &name]) else { continue };
        let pol = parse_policy(&o.stdout);
        let Some(cand) = pol.candidate else { continue };
        let Some(up) = version::upstream(&cand) else { continue };
        let mut extra = Vec::new();
        if !ubuntu {
            if let Some(h) = headers_pkg(&env.arch) {
                extra.push(h.to_string());
            }
        }
        p.offers.push(Offer { package: name, extra, flavor, version: cand, upstream: up, source: "apt".into() });
    }
    if !ubuntu && !p.offers.iter().any(|o| o.package == "nvidia-driver") {
        p.notes.push(
            "Debian's `nvidia-driver` package was not found. It lives in the `non-free` component: \
             add `non-free` (and `non-free-firmware`) to your apt sources, run `apt update`, then try again."
                .into(),
        );
    }
    p
}

pub fn steps(offer: &Offer) -> Vec<Vec<String>> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let mut install = s(&["apt-get", "install", "-y"]);
    install.extend(offer.extra.iter().cloned());
    install.push(offer.package.clone());
    vec![s(&["apt-get", "update"]), install]
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY_DEBIAN: &str = "\
nvidia-driver:
  Installed: 535.183.01-1~deb12u1
  Candidate: 550.163.01-2
  Version table:
     550.163.01-2 500
        500 http://deb.debian.org/debian bookworm-backports/non-free amd64 Packages
 *** 535.183.01-1~deb12u1 500
        500 http://deb.debian.org/debian bookworm/non-free amd64 Packages
        100 /var/lib/dpkg/status
";

    const POLICY_NOT_INSTALLED: &str = "\
nvidia-driver:
  Installed: (none)
  Candidate: 535.183.01-1~deb12u1
  Version table:
     535.183.01-1~deb12u1 500
";

    const UBUNTU_DRIVERS: &str = "\
nvidia-driver-535, (kernel modules provided by linux-modules-nvidia-535-generic)
nvidia-driver-570-open, (kernel modules provided by linux-modules-nvidia-570-open-generic)
nvidia-driver-570, (kernel modules provided by linux-modules-nvidia-570-generic)
nvidia-driver-535-server, (kernel modules provided by linux-modules-nvidia-535-server-generic)
nvidia-driver-570-server-open, (kernel modules provided by linux-modules-nvidia-570-server-open-generic)
xserver-xorg-video-nouveau, (distro non-free)
";

    #[test]
    fn parses_policy_with_update() {
        let p = parse_policy(POLICY_DEBIAN);
        assert_eq!(p.installed.as_deref(), Some("535.183.01-1~deb12u1"));
        assert_eq!(p.candidate.as_deref(), Some("550.163.01-2"));
    }

    #[test]
    fn parses_policy_not_installed() {
        let p = parse_policy(POLICY_NOT_INSTALLED);
        assert_eq!(p.installed, None);
        assert!(p.candidate.is_some());
    }

    #[test]
    fn parses_policy_unknown_package() {
        assert_eq!(parse_policy(""), Policy { installed: None, candidate: None });
        assert_eq!(parse_policy("N: Unable to locate package foo").candidate, None);
    }

    #[test]
    fn parses_ubuntu_drivers_list() {
        let n = parse_driver_names(UBUNTU_DRIVERS);
        assert_eq!(
            n,
            vec![
                ("nvidia-driver-535".to_string(), Flavor::Proprietary),
                ("nvidia-driver-570-open".to_string(), Flavor::Open),
                ("nvidia-driver-570".to_string(), Flavor::Proprietary),
            ]
        );
    }

    #[test]
    fn parses_bare_package_names() {
        let n = parse_driver_names("nvidia-driver-550\nnvidia-driver-550-open\nnvidia-driver-\n");
        assert_eq!(n.len(), 2);
    }

    #[test]
    fn install_steps_refresh_then_install() {
        let o = Offer {
            package: "nvidia-driver".into(),
            extra: vec!["linux-headers-amd64".into()],
            flavor: Flavor::Proprietary,
            version: "550.163.01-2".into(),
            upstream: "550.163.01".into(),
            source: "apt".into(),
        };
        let s = steps(&o);
        assert_eq!(s[0], vec!["apt-get", "update"]);
        assert_eq!(s[1], vec!["apt-get", "install", "-y", "linux-headers-amd64", "nvidia-driver"]);
    }
}

//! Fedora / RHEL family (dnf). Repositories are never added automatically.
//! Fedora: RPM Fusion nonfree (`akmod-nvidia`) or NVIDIA's CUDA repo. RHEL clones: CUDA repo.

use super::{Env, Offer, Probe};
use crate::exec;
use crate::smi::Flavor;
use crate::version;

/// Parse `dnf list --available <pkg>` into (name, version, repo) rows.
/// Row: `akmod-nvidia.x86_64    3:570.144-1.fc41    rpmfusion-nonfree-updates`
pub fn parse_list(text: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() != 3 {
            continue; // headers ("Available Packages"), errors, wrapped lines
        }
        let Some((name, arch)) = cols[0].rsplit_once('.') else { continue };
        if arch.is_empty() || !(arch == "noarch" || arch.starts_with("x86") || arch.starts_with("aarch") || arch.starts_with("i686")) {
            continue;
        }
        if version::upstream(cols[1]).is_none() {
            continue;
        }
        out.push((name.to_string(), cols[1].to_string(), cols[2].to_string()));
    }
    out
}

/// Parse `dnf repolist` into repo ids (first column, header skipped).
pub fn parse_repolist(text: &str) -> Vec<String> {
    text.lines()
        .skip_while(|l| !l.trim_start().to_lowercase().starts_with("repo id"))
        .skip(1)
        .filter_map(|l| l.split_whitespace().next().map(String::from))
        .collect()
}

fn newest(pkg: &str) -> Option<(String, String)> {
    let o = exec::capture("dnf", &["--quiet", "list", "--available", pkg])?;
    parse_list(&o.stdout)
        .into_iter()
        .filter(|(n, _, _)| n == pkg)
        .max_by(|a, b| {
            version::compare(&version::upstream(&a.1).unwrap_or_default(), &version::upstream(&b.1).unwrap_or_default())
        })
        .map(|(_, v, repo)| (v, repo))
}

pub fn probe(fedora: bool, _env: &Env) -> Probe {
    let mut p = Probe::default();
    let repos = exec::stdout_of("dnf", &["--quiet", "repolist"]).map(|t| parse_repolist(&t)).unwrap_or_default();
    let rpmfusion = fedora && repos.iter().any(|r| r.starts_with("rpmfusion-nonfree"));
    let cuda = repos.iter().any(|r| r.starts_with("cuda-"));

    // (package, flavor, extra packages)
    let mut wanted: Vec<(&str, Flavor, Vec<&str>)> = Vec::new();
    if rpmfusion {
        // xorg-x11-drv-nvidia-cuda provides nvidia-smi and CUDA libraries (works headless too).
        wanted.push(("akmod-nvidia", Flavor::Proprietary, vec!["xorg-x11-drv-nvidia-cuda"]));
    }
    if cuda {
        wanted.push(("nvidia-open", Flavor::Open, vec![]));
        wanted.push(("cuda-drivers", Flavor::Proprietary, vec![]));
    }
    if wanted.is_empty() {
        p.notes.push(if fedora {
            "No NVIDIA-capable repository is configured. Enable RPM Fusion (free + nonfree, see rpmfusion.org/Configuration) \
             or NVIDIA's CUDA repository (developer.nvidia.com/cuda-downloads), then run this again. \
             This tool does not add repositories for you."
                .into()
        } else {
            "No NVIDIA CUDA repository is configured. Add it from developer.nvidia.com/cuda-downloads, \
             then run this again. This tool does not add repositories for you."
                .into()
        });
    }
    for (pkg, flavor, extra) in wanted {
        if let Some((v, repo)) = newest(pkg) {
            if let Some(up) = version::upstream(&v) {
                p.offers.push(Offer {
                    package: pkg.into(),
                    extra: extra.into_iter().map(String::from).collect(),
                    flavor,
                    version: v,
                    upstream: up,
                    source: repo,
                });
            }
        }
    }
    if rpmfusion {
        p.notes.push(
            "RPM Fusion's akmod builds the kernel module on first boot after install; \
             with Secure Boot on it must be signed (see rpmfusion.org/Howto/Secure Boot)."
                .into(),
        );
    }
    if !fedora {
        p.notes.push("RHEL-family support is best-effort and has not been verified on real systems.".into());
    }
    p
}

pub fn steps(offer: &Offer) -> Vec<Vec<String>> {
    let mut v = vec!["dnf".to_string(), "install".into(), "-y".into()];
    v.extend(offer.extra.iter().cloned());
    v.push(offer.package.clone());
    vec![v]
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "\
Available Packages
akmod-nvidia.x86_64                  3:570.133.07-1.fc41             rpmfusion-nonfree
akmod-nvidia.x86_64                  3:570.144-1.fc41                rpmfusion-nonfree-updates
";

    const REPOLIST: &str = "\
repo id                          repo name
cuda-fedora41-x86_64             CUDA Repository
fedora                           Fedora 41 - x86_64
rpmfusion-nonfree                RPM Fusion for Fedora 41 - Nonfree
rpmfusion-nonfree-updates        RPM Fusion for Fedora 41 - Nonfree - Updates
";

    #[test]
    fn parses_list() {
        let rows = parse_list(LIST);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].0, "akmod-nvidia");
        assert_eq!(rows[1].1, "3:570.144-1.fc41");
        assert_eq!(rows[1].2, "rpmfusion-nonfree-updates");
    }

    #[test]
    fn list_error_output_is_empty() {
        assert!(parse_list("Error: No matching Packages to list\n").is_empty());
    }

    #[test]
    fn parses_repolist() {
        let r = parse_repolist(REPOLIST);
        assert_eq!(r.len(), 4);
        assert!(r.iter().any(|x| x == "rpmfusion-nonfree-updates"));
        assert!(r.iter().any(|x| x.starts_with("cuda-")));
    }

    #[test]
    fn install_step() {
        let o = Offer {
            package: "akmod-nvidia".into(),
            extra: vec!["xorg-x11-drv-nvidia-cuda".into()],
            flavor: Flavor::Proprietary,
            version: "3:570.144-1.fc41".into(),
            upstream: "570.144".into(),
            source: "x".into(),
        };
        assert_eq!(steps(&o)[0], vec!["dnf", "install", "-y", "xorg-x11-drv-nvidia-cuda", "akmod-nvidia"]);
    }
}

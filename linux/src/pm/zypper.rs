//! openSUSE Leap / Tumbleweed (zypper). Uses NVIDIA's repository for openSUSE; never adds it for you.
//! Package names are from the openSUSE NVIDIA driver documentation and have not been verified on
//! a real system by this project.

use super::{Env, Offer, Probe};
use crate::exec;
use crate::smi::Flavor;
use crate::version;

/// Parse `zypper search -s` table rows: `   | name | package | 570.144-1.1 | x86_64 | NVIDIA`.
/// Returns (name, version, repo).
pub fn parse_search(text: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split('|').map(str::trim).collect();
        if cols.len() < 6 || cols[1] == "Name" {
            continue;
        }
        if version::upstream(cols[3]).is_none() {
            continue;
        }
        out.push((cols[1].to_string(), cols[3].to_string(), cols[5].to_string()));
    }
    out
}

fn newest(pkg: &str) -> Option<(String, String)> {
    let o = exec::capture("zypper", &["--non-interactive", "--quiet", "search", "-s", "--match-exact", pkg])?;
    parse_search(&o.stdout)
        .into_iter()
        .filter(|(n, _, _)| n == pkg)
        .max_by(|a, b| {
            version::compare(&version::upstream(&a.1).unwrap_or_default(), &version::upstream(&b.1).unwrap_or_default())
        })
        .map(|(_, v, repo)| (v, repo))
}

/// `uname -r` ends with the kernel flavor: "6.4.0-150600.23.25-default" -> "default".
pub fn kernel_flavor(uname: &str) -> String {
    uname.rsplit('-').next().filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphabetic())).unwrap_or("default").to_string()
}

pub fn probe(env: &Env) -> Probe {
    let mut p = Probe::default();
    let kf = kernel_flavor(&env.kernel);
    let mut common = vec!["nvidia-compute-utils-G06".to_string()];
    if !env.headless {
        common.push("nvidia-video-G06".into());
        common.push("nvidia-gl-G06".into());
    }
    let wanted = [
        (format!("nvidia-open-driver-G06-signed-kmp-{kf}"), Flavor::Open),
        (format!("nvidia-driver-G06-kmp-{kf}"), Flavor::Proprietary),
    ];
    for (pkg, flavor) in wanted {
        if let Some((v, repo)) = newest(&pkg) {
            if let Some(up) = version::upstream(&v) {
                p.offers.push(Offer { package: pkg, extra: common.clone(), flavor, version: v, upstream: up, source: repo });
            }
        }
    }
    if p.offers.is_empty() {
        p.notes.push(
            "No NVIDIA driver packages found. Add NVIDIA's openSUSE repository (see the openSUSE wiki page \
             \"SDB:NVIDIA drivers\"), then run this again. This tool does not add repositories for you."
                .into(),
        );
    } else {
        p.notes.push(
            "openSUSE's signed open-driver package is the easiest route with Secure Boot (enroll the key with `mokutil`)."
                .into(),
        );
    }
    p
}

pub fn steps(offer: &Offer) -> Vec<Vec<String>> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let mut install = s(&["zypper", "--non-interactive", "install"]);
    install.extend(offer.extra.iter().cloned());
    install.push(offer.package.clone());
    vec![s(&["zypper", "--non-interactive", "refresh"]), install]
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH: &str = "\
Loading repository data...
Reading installed packages...

S  | Name                                       | Type    | Version      | Arch   | Repository
---+--------------------------------------------+---------+--------------+--------+------------
   | nvidia-open-driver-G06-signed-kmp-default  | package | 570.133.07-1 | x86_64 | NVIDIA
i  | nvidia-open-driver-G06-signed-kmp-default  | package | 565.57.01-1  | x86_64 | @System
";

    #[test]
    fn parses_search_table() {
        let r = parse_search(SEARCH);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "nvidia-open-driver-G06-signed-kmp-default");
        assert_eq!(r[0].1, "570.133.07-1");
        assert_eq!(r[0].2, "NVIDIA");
    }

    #[test]
    fn kernel_flavors() {
        assert_eq!(kernel_flavor("6.4.0-150600.23.25-default"), "default");
        assert_eq!(kernel_flavor("6.8.0-azure"), "azure");
        assert_eq!(kernel_flavor("weird"), "weird");
    }
}

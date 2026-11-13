//! Distro package managers: find the newest NVIDIA driver each one offers, and build the
//! install commands. Parsers live next to each backend and are unit-tested with captured text.

pub mod apt;
pub mod dnf;
pub mod pacman;
pub mod zypper;

use crate::osrelease::Family;
use crate::smi::Flavor;
use crate::version;
use serde::Serialize;

pub struct Env {
    pub headless: bool,
    /// `uname -r`
    pub kernel: String,
    /// `uname -m`
    pub arch: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Offer {
    /// Main package; its version is the driver version on offer.
    pub package: String,
    /// Extra packages installed together with it (headers, nvidia-smi tools...).
    pub extra: Vec<String>,
    pub flavor: Flavor,
    /// Raw package version as the package manager prints it.
    pub version: String,
    /// Upstream NVIDIA driver version, e.g. "550.163.01".
    pub upstream: String,
    /// Where it comes from (repo name), for display.
    pub source: String,
}

#[derive(Default)]
pub struct Probe {
    pub offers: Vec<Offer>,
    pub notes: Vec<String>,
    /// Set when the distro's normal route is to upgrade the whole system (Arch/Manjaro).
    pub full_upgrade: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    Apt { ubuntu: bool },
    Pacman { manjaro: bool },
    Dnf { fedora: bool },
    Zypper,
}

pub fn manager_for(f: Family) -> Option<Manager> {
    Some(match f {
        Family::Debian => Manager::Apt { ubuntu: false },
        Family::Ubuntu => Manager::Apt { ubuntu: true },
        Family::Arch => Manager::Pacman { manjaro: false },
        Family::Manjaro => Manager::Pacman { manjaro: true },
        Family::Fedora => Manager::Dnf { fedora: true },
        Family::Rhel => Manager::Dnf { fedora: false },
        Family::Suse => Manager::Zypper,
        Family::Unknown => return None,
    })
}

impl Manager {
    pub fn name(self) -> &'static str {
        match self {
            Manager::Apt { .. } => "apt",
            Manager::Pacman { .. } => "pacman",
            Manager::Dnf { .. } => "dnf",
            Manager::Zypper => "zypper",
        }
    }

    pub fn probe(self, env: &Env) -> Probe {
        match self {
            Manager::Apt { ubuntu } => apt::probe(ubuntu, env),
            Manager::Pacman { manjaro } => pacman::probe(manjaro, env),
            Manager::Dnf { fedora } => dnf::probe(fedora, env),
            Manager::Zypper => zypper::probe(env),
        }
    }

    /// Commands to run (as argv lists, never via a shell). `refresh` first syncs repo metadata.
    pub fn steps(self, offer: &Offer, full_upgrade: bool) -> Vec<Vec<String>> {
        match self {
            Manager::Apt { .. } => apt::steps(offer),
            Manager::Pacman { .. } => pacman::steps(offer, full_upgrade),
            Manager::Dnf { .. } => dnf::steps(offer),
            Manager::Zypper => zypper::steps(offer),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlavorChoice {
    Auto,
    Open,
    Proprietary,
}

/// Pick which offer to use. Never silently switches an installed driver between open and
/// proprietary: with `Auto` we stay on the installed kind.
pub fn choose(
    offers: &[Offer],
    choice: FlavorChoice,
    installed: Option<Flavor>,
    gpu_open: Option<bool>,
) -> Result<(Offer, Option<String>), String> {
    let has = |f: Flavor| offers.iter().any(|o| o.flavor == f);
    let wanted = match choice {
        FlavorChoice::Open => Flavor::Open,
        FlavorChoice::Proprietary => Flavor::Proprietary,
        FlavorChoice::Auto => match installed {
            Some(f) => f,
            None if gpu_open == Some(true) && has(Flavor::Open) => Flavor::Open,
            None => Flavor::Proprietary,
        },
    };
    let best = |f: Flavor| {
        offers
            .iter()
            .filter(|o| o.flavor == f)
            .max_by(|a, b| version::compare(&a.upstream, &b.upstream))
            .cloned()
    };
    if let Some(o) = best(wanted) {
        let mut note = None;
        if choice == FlavorChoice::Auto
            && wanted == Flavor::Proprietary
            && gpu_open == Some(true)
            && has(Flavor::Open)
        {
            note = Some(
                "Your GPU also supports NVIDIA's open kernel modules and this distro offers them; \
                 pass --flavor open to switch (not done automatically)."
                    .to_string(),
            );
        }
        return Ok((o, note));
    }
    // Wanted kind unavailable.
    if choice == FlavorChoice::Auto && installed.is_none() {
        let other = if wanted == Flavor::Open { Flavor::Proprietary } else { Flavor::Open };
        let blocked = other == Flavor::Open && gpu_open == Some(false);
        if let Some(o) = best(other).filter(|_| !blocked) {
            let msg = format!("No {} driver package found; using the {} one.", wanted.label(), other.label());
            return Ok((o, Some(msg)));
        }
    }
    Err(format!(
        "this distro's repositories don't offer a {} NVIDIA driver package",
        wanted.label()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(pkg: &str, f: Flavor, up: &str) -> Offer {
        Offer {
            package: pkg.into(),
            extra: vec![],
            flavor: f,
            version: up.into(),
            upstream: up.into(),
            source: "test".into(),
        }
    }

    #[test]
    fn auto_prefers_open_on_fresh_install_for_turing_plus() {
        let o = vec![offer("a", Flavor::Proprietary, "570.1"), offer("b", Flavor::Open, "570.1")];
        let (c, _) = choose(&o, FlavorChoice::Auto, None, Some(true)).unwrap();
        assert_eq!(c.package, "b");
    }

    #[test]
    fn auto_keeps_installed_kind_and_hints() {
        let o = vec![offer("a", Flavor::Proprietary, "570.1"), offer("b", Flavor::Open, "575.1")];
        let (c, note) = choose(&o, FlavorChoice::Auto, Some(Flavor::Proprietary), Some(true)).unwrap();
        assert_eq!(c.package, "a");
        assert!(note.unwrap().contains("--flavor open"));
    }

    #[test]
    fn picks_highest_version_within_kind() {
        let o = vec![offer("a", Flavor::Proprietary, "535.9"), offer("b", Flavor::Proprietary, "550.1")];
        let (c, _) = choose(&o, FlavorChoice::Auto, None, Some(false)).unwrap();
        assert_eq!(c.package, "b");
    }

    #[test]
    fn explicit_choice_is_not_overridden() {
        let o = vec![offer("a", Flavor::Proprietary, "570.1")];
        assert!(choose(&o, FlavorChoice::Open, None, Some(true)).is_err());
    }

    #[test]
    fn falls_back_on_fresh_install_only() {
        let o = vec![offer("a", Flavor::Proprietary, "570.1")];
        let (c, note) = choose(&o, FlavorChoice::Auto, None, Some(true)).unwrap();
        assert_eq!(c.package, "a");
        assert!(note.is_none());
        // Installed open modules are never silently swapped for proprietary.
        assert!(choose(&o, FlavorChoice::Auto, Some(Flavor::Open), Some(true)).is_err());
        // Old GPU + only an open package on offer: don't pick it.
        let only_open = vec![offer("b", Flavor::Open, "570.1")];
        assert!(choose(&only_open, FlavorChoice::Auto, None, Some(false)).is_err());
    }

    #[test]
    fn manager_mapping() {
        assert_eq!(manager_for(Family::Debian), Some(Manager::Apt { ubuntu: false }));
        assert_eq!(manager_for(Family::Unknown), None);
    }
}

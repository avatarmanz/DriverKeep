//! DriverKeep for Linux: check for and install NVIDIA GPU driver updates.
//! Separate from the Windows app in the repo's top-level `src/`.

mod exec;
mod gpu;
mod nvidia;
mod osrelease;
mod pm;
mod smi;
mod system;
mod version;

use clap::{Parser, Subcommand, ValueEnum};
use osrelease::Family;
use pm::{FlavorChoice, Manager, Offer};
use serde::Serialize;
use smi::Flavor;
use std::cmp::Ordering;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use system::SecureBoot;

const EXIT_ERROR: i32 = 1;
const EXIT_NEEDS_ROOT: i32 = 3;
const EXIT_DECLINED: i32 = 4;
const EXIT_UPDATE_AVAILABLE: i32 = 10;

#[derive(Parser)]
#[command(name = "driverkeep-linux", version, about = "Check for and install NVIDIA GPU driver updates on Linux")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    /// Machine-readable output (for `check` and `update --dry-run`)
    #[arg(long, global = true)]
    json: bool,
    /// Open (Turing and newer) or proprietary kernel modules. `auto` keeps what is installed.
    #[arg(long, global = true, value_enum, default_value = "auto")]
    flavor: FlavorArg,
    /// `package` = the distro's package manager (default where supported); `run` = NVIDIA's .run installer
    #[arg(long, global = true, value_enum, default_value = "auto")]
    method: MethodArg,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show installed vs available driver versions. Read-only.
    Check {
        /// Exit with status 10 when an update is available (handy in scripts)
        #[arg(long)]
        exit_code: bool,
        /// Don't contact NVIDIA's website for the reference "latest" version
        #[arg(long)]
        offline: bool,
    },
    /// Install the newest driver the distro offers (asks first). Needs root.
    Update {
        /// Don't ask for confirmation
        #[arg(short, long)]
        yes: bool,
        /// Show what would be done and stop
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum FlavorArg {
    Auto,
    Open,
    Proprietary,
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum MethodArg {
    Auto,
    Package,
    Run,
}

impl From<FlavorArg> for FlavorChoice {
    fn from(f: FlavorArg) -> Self {
        match f {
            FlavorArg::Auto => FlavorChoice::Auto,
            FlavorArg::Open => FlavorChoice::Open,
            FlavorArg::Proprietary => FlavorChoice::Proprietary,
        }
    }
}

// ---------------------------------------------------------------- status logic

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    NoGpu,
    NotInstalled,
    UpdateAvailable,
    UpToDate,
    /// Installed is newer than what the repositories offer (e.g. installed from NVIDIA directly).
    AheadOfRepo,
    /// No version to compare against.
    Unknown,
}

pub fn status(has_gpu: bool, installed: Option<&str>, target: Option<&str>) -> Status {
    if !has_gpu {
        return Status::NoGpu;
    }
    match (installed, target) {
        (None, _) => Status::NotInstalled,
        (Some(_), None) => Status::Unknown,
        (Some(i), Some(t)) => match version::compare(i, t) {
            Ordering::Less => Status::UpdateAvailable,
            Ordering::Equal => Status::UpToDate,
            Ordering::Greater => Status::AheadOfRepo,
        },
    }
}

// ---------------------------------------------------------------- gathering

struct Ctx {
    os: osrelease::OsRelease,
    family: Family,
    manager: Option<Manager>,
    gpus: Vec<gpu::Gpu>,
    installed: smi::Installed,
    secure_boot: SecureBoot,
    headless: bool,
    env: pm::Env,
}

fn uname(flag: &str) -> String {
    exec::stdout_of("uname", &[flag]).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn gather() -> Ctx {
    let os = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .map(|t| osrelease::parse(&t))
        .unwrap_or_default();
    let family = os.family();
    let headless = system::is_headless();
    Ctx {
        manager: pm::manager_for(family),
        family,
        os,
        gpus: gpu::detect(),
        installed: smi::detect(),
        secure_boot: system::secure_boot(),
        headless,
        env: pm::Env { headless, kernel: uname("-r"), arch: uname("-m") },
    }
}

fn gpu_open(ctx: &Ctx) -> Option<bool> {
    ctx.gpus.first().and_then(|g| g.supports_open)
}

fn secure_boot_warning(ctx: &Ctx) -> String {
    let how = match ctx.family {
        Family::Debian => "On Debian, DKMS signs modules with its own key; enroll it with `sudo mokutil --import /var/lib/dkms/mok.pub` and reboot to approve it in the blue MOK screen.",
        Family::Ubuntu => "On Ubuntu, the prebuilt signed modules (linux-modules-nvidia-*) work with Secure Boot; DKMS builds need a MOK key enrolled (Ubuntu offers to do this during install).",
        Family::Arch | Family::Manjaro => "On Arch-based systems you must sign the modules yourself (e.g. with sbctl) or disable Secure Boot.",
        Family::Fedora | Family::Rhel => "With akmods, generate and enroll a key (`sudo kmodgenca`, then `sudo mokutil --import /etc/pki/akmods/certs/public_key.der`) and reboot.",
        Family::Suse => "openSUSE's signed open-driver package works with Secure Boot after enrolling NVIDIA's key via `mokutil`.",
        Family::Unknown => "Modules built by the .run installer must be signed and enrolled by you (`--module-signing-*` options), or Secure Boot disabled.",
    };
    format!("Secure Boot is ON. Unsigned NVIDIA kernel modules will be refused at boot, leaving you without a working driver. {how}")
}

// ---------------------------------------------------------------- check

#[derive(Serialize)]
struct Report {
    tool_version: &'static str,
    distro: DistroInfo,
    gpus: Vec<gpu::Gpu>,
    driver: smi::Installed,
    package_manager: Option<&'static str>,
    distro_offer: Option<Offer>,
    all_offers: Vec<Offer>,
    nvidia_latest: Option<nvidia::Latest>,
    nvidia_latest_error: Option<String>,
    status: Status,
    update_available: bool,
    reboot_required: bool,
    secure_boot: &'static str,
    headless: bool,
    warnings: Vec<String>,
    notes: Vec<String>,
}

#[derive(Serialize)]
struct DistroInfo {
    id: String,
    name: String,
    version_id: String,
    family: &'static str,
}

fn family_name(f: Family) -> &'static str {
    match f {
        Family::Debian => "debian",
        Family::Ubuntu => "ubuntu",
        Family::Arch => "arch",
        Family::Manjaro => "manjaro",
        Family::Fedora => "fedora",
        Family::Rhel => "rhel",
        Family::Suse => "suse",
        Family::Unknown => "unknown",
    }
}

fn build_report(cli: &Cli, offline: bool) -> Report {
    let ctx = gather();
    let mut warnings = Vec::new();
    let mut notes = Vec::new();

    if ctx.gpus.is_empty() {
        warnings.push("No NVIDIA GPU was found on the PCI bus.".to_string());
    }
    if ctx.secure_boot == SecureBoot::Enabled {
        warnings.push(secure_boot_warning(&ctx));
    }
    if ctx.headless {
        notes.push("No display detected (headless/server). That's fine: the driver is used for compute (CUDA) and nvidia-smi. Distro driver packages may still pull in some desktop libraries.".into());
    }

    let (mut offers, mut distro_offer) = (Vec::new(), None);
    if let Some(m) = ctx.manager {
        let probe = m.probe(&ctx.env);
        notes.extend(probe.notes);
        if !probe.offers.is_empty() {
            match pm::choose(&probe.offers, cli.flavor.into(), ctx.installed.flavor, gpu_open(&ctx)) {
                Ok((o, note)) => {
                    notes.extend(note);
                    distro_offer = Some(o);
                }
                Err(e) => notes.push(format!("Could not pick a package: {e}.")),
            }
        }
        offers = probe.offers;
    } else {
        notes.push("This distro isn't one of the supported package-manager families; use `update --method run` for NVIDIA's .run installer.".into());
    }

    // NVIDIA's own latest (reference only).
    let (mut latest, mut latest_err) = (None, None);
    if !offline {
        match nvidia::fetch_latest(&ctx.env.arch) {
            Ok(l) => latest = Some(l),
            Err(e) => latest_err = Some(e),
        }
    }
    if let (Some(l), Some(o)) = (&latest, &distro_offer) {
        if version::compare(&l.version, &o.upstream) == Ordering::Greater {
            notes.push("NVIDIA's newest release is ahead of your distro's package. That's normal: distros hold back for stability. Prefer the distro package so DKMS and kernel updates keep working.".into());
        }
    }
    if ctx.gpus.iter().any(|g| g.supports_open == Some(false)) {
        notes.push("Your GPU is older than Turing, so NVIDIA's newest driver branch may not support it and the open kernel modules don't apply; use the distro's package (it picks a supported branch).".into());
    }

    // Which version are we comparing against?
    let target_ver = match (cli.method, &distro_offer, &latest) {
        (MethodArg::Run, _, Some(l)) => Some(l.version.clone()),
        (_, Some(o), _) => Some(o.upstream.clone()),
        (MethodArg::Auto, None, Some(l)) if ctx.manager.is_none() => Some(l.version.clone()),
        _ => None,
    };
    let st = status(!ctx.gpus.is_empty(), ctx.installed.version.as_deref(), target_ver.as_deref());

    if let (Some(a), Some(b)) = (&ctx.installed.version, &ctx.installed.loaded_version) {
        if version::compare(a, b) != Ordering::Equal {
            notes.push(format!("Driver {a} is installed on disk but {b} is still loaded: reboot to switch."));
        }
    }
    let reboot_required = match (&ctx.installed.version, &ctx.installed.loaded_version) {
        (Some(a), Some(b)) => version::compare(a, b) != Ordering::Equal,
        _ => false,
    };
    if ctx.installed.version.is_some() && !ctx.installed.smi_ok {
        if let Some(m) = &ctx.installed.smi_message {
            warnings.push(format!("Driver files are installed but nvidia-smi can't use them: \"{m}\". A reboot is usually needed{}.",
                if ctx.secure_boot == SecureBoot::Enabled { ", and with Secure Boot on the module may be unsigned" } else { "" }));
        }
    }
    if ctx.installed.version.is_none() && std::path::Path::new("/sys/module/nouveau").exists() {
        notes.push("The open-source nouveau driver is currently in use; it's replaced after installing the NVIDIA driver and rebooting.".into());
    }

    Report {
        tool_version: env!("CARGO_PKG_VERSION"),
        distro: DistroInfo {
            id: ctx.os.id.clone(),
            name: ctx.os.display(),
            version_id: ctx.os.version_id.clone(),
            family: family_name(ctx.family),
        },
        gpus: ctx.gpus.clone(),
        driver: ctx.installed.clone(),
        package_manager: ctx.manager.map(|m| m.name()),
        distro_offer,
        all_offers: offers,
        nvidia_latest: latest,
        nvidia_latest_error: latest_err,
        status: st,
        update_available: matches!(st, Status::UpdateAvailable | Status::NotInstalled) && target_ver.is_some(),
        reboot_required,
        secure_boot: match ctx.secure_boot {
            SecureBoot::Enabled => "enabled",
            SecureBoot::Disabled => "disabled",
            SecureBoot::Unknown => "unknown",
        },
        headless: ctx.headless,
        warnings,
        notes,
    }
}

fn print_report(r: &Report) {
    println!("DriverKeep for Linux {}", r.tool_version);
    println!();
    println!("System:        {}{}", r.distro.name, r.package_manager.map(|m| format!("  [{m}]")).unwrap_or_default());
    if r.gpus.is_empty() {
        println!("GPU:           none found");
    }
    for g in &r.gpus {
        let open = match g.supports_open {
            Some(true) => "supports open kernel modules",
            Some(false) => "proprietary driver only",
            None => "",
        };
        println!("GPU:           {} ({}) {}", g.name, g.pci_id, if open.is_empty() { String::new() } else { format!("- {open}") });
    }
    println!("Display:       {}", if r.headless { "none (headless server)" } else { "present" });
    println!("Secure Boot:   {}", r.secure_boot);
    println!();
    match &r.driver.version {
        Some(v) => {
            let kind = r.driver.flavor.map(|f| format!(", {} modules", f.label())).unwrap_or_default();
            println!("Installed:     {v}{kind}");
            if let Some(l) = &r.driver.loaded_version {
                if l != v {
                    println!("  (running:    {l} - reboot needed)");
                }
            }
        }
        None => println!("Installed:     no NVIDIA driver found"),
    }
    match &r.distro_offer {
        Some(o) => println!("Distro offers: {}  ({}, {} modules, from {})", o.upstream, o.package, o.flavor.label(), o.source),
        None => println!("Distro offers: nothing found"),
    }
    if r.all_offers.len() > 1 {
        let others: Vec<String> = r.all_offers.iter().map(|o| format!("{} {}", o.package, o.upstream)).collect();
        println!("  (all found:  {})", others.join(", "));
    }
    match (&r.nvidia_latest, &r.nvidia_latest_error) {
        (Some(l), _) => println!("NVIDIA latest: {}  (reference only, from download.nvidia.com)", l.version),
        (None, Some(e)) => println!("NVIDIA latest: unavailable ({e})"),
        _ => println!("NVIDIA latest: not checked (--offline)"),
    }
    println!();
    println!(
        "Status:        {}",
        match r.status {
            Status::NoGpu => "no NVIDIA GPU found",
            Status::NotInstalled => "driver not installed - run `update` to install it",
            Status::UpdateAvailable => "UPDATE AVAILABLE - run `sudo driverkeep-linux update`",
            Status::UpToDate => "up to date",
            Status::AheadOfRepo => "newer than the distro's package (nothing to do)",
            Status::Unknown => "can't compare (no version to compare against)",
        }
    );
    for w in &r.warnings {
        println!("\nWARNING: {w}");
    }
    for n in &r.notes {
        println!("\nNote: {n}");
    }
}

fn cmd_check(cli: &Cli, exit_code: bool, offline: bool) -> i32 {
    let r = build_report(cli, offline);
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&r).unwrap_or_else(|_| "{}".into()));
    } else {
        print_report(&r);
    }
    if exit_code && r.status == Status::UpdateAvailable {
        EXIT_UPDATE_AVAILABLE
    } else {
        0
    }
}

// ---------------------------------------------------------------- update

fn fail(msg: &str) -> i32 {
    eprintln!("error: {msg}");
    EXIT_ERROR
}

fn confirm(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s).is_ok() && matches!(s.trim().to_lowercase().as_str(), "y" | "yes")
}

#[derive(Serialize)]
struct Plan {
    method: &'static str,
    target_version: String,
    installed_version: Option<String>,
    flavor: Flavor,
    steps: Vec<String>,
    needs_root: bool,
    reboot_required_after: bool,
}

fn cmd_update(cli: &Cli, yes: bool, dry_run: bool) -> i32 {
    let ctx = gather();
    if ctx.gpus.is_empty() {
        return fail("no NVIDIA GPU found on the PCI bus; nothing to do.");
    }
    if cli.json && !dry_run {
        return fail("--json is only supported with `check` and `update --dry-run`.");
    }

    let use_run = match cli.method {
        MethodArg::Run => true,
        MethodArg::Package => false,
        MethodArg::Auto => ctx.manager.is_none(),
    };
    let mut notes: Vec<String> = Vec::new();
    let installed_ver = ctx.installed.version.clone();

    // Decide target and the list of steps. Download (for .run) happens after confirmation.
    let (target, flavor, mut steps, latest): (String, Flavor, Vec<Vec<String>>, Option<nvidia::Latest>);
    if !use_run {
        let Some(m) = ctx.manager else {
            return fail("this distro isn't supported for package installs; use `--method run` for NVIDIA's installer.");
        };
        let probe = m.probe(&ctx.env);
        notes.extend(probe.notes.clone());
        if probe.offers.is_empty() {
            for n in &notes {
                eprintln!("{n}");
            }
            return fail("the package manager offers no NVIDIA driver (see above). Nothing was changed.");
        }
        let (offer, note) = match pm::choose(&probe.offers, cli.flavor.into(), ctx.installed.flavor, gpu_open(&ctx)) {
            Ok(x) => x,
            Err(e) => return fail(&e),
        };
        notes.extend(note);
        if offer.flavor == Flavor::Open && gpu_open(&ctx) == Some(false) {
            return fail("your GPU is older than Turing; the open kernel modules don't support it. Use --flavor proprietary.");
        }
        match status(true, installed_ver.as_deref(), Some(&offer.upstream)) {
            Status::UpToDate | Status::AheadOfRepo => {
                println!(
                    "Nothing to do: installed {} is {} the newest the distro offers ({}).",
                    installed_ver.as_deref().unwrap_or("?"),
                    if status(true, installed_ver.as_deref(), Some(&offer.upstream)) == Status::UpToDate { "already" } else { "newer than" },
                    offer.upstream
                );
                return 0;
            }
            _ => {}
        }
        if ctx.installed.flavor.is_some() && ctx.installed.flavor != Some(offer.flavor) {
            notes.push(format!(
                "This switches the kernel modules from {} to {}.",
                ctx.installed.flavor.map(|f| f.label()).unwrap_or("?"),
                offer.flavor.label()
            ));
        }
        target = offer.upstream.clone();
        flavor = offer.flavor;
        steps = m.steps(&offer, probe.full_upgrade);
        if probe.full_upgrade {
            notes.push("This distro updates drivers as part of a full system upgrade (`pacman -Syu`): every outdated package will be upgraded, not just the driver.".into());
        }
        latest = None;
    } else {
        if gpu_open(&ctx) == Some(false) {
            return fail("your GPU is older than Turing; NVIDIA's latest .run installer may not support it. Use your distro's package, which picks a supported legacy branch.");
        }
        let l = match nvidia::fetch_latest(&ctx.env.arch) {
            Ok(l) => l,
            Err(e) => return fail(&e),
        };
        let f = match FlavorChoice::from(cli.flavor) {
            FlavorChoice::Open => Flavor::Open,
            FlavorChoice::Proprietary => Flavor::Proprietary,
            FlavorChoice::Auto => ctx.installed.flavor.unwrap_or(if gpu_open(&ctx) == Some(true) { Flavor::Open } else { Flavor::Proprietary }),
        };
        if matches!(status(true, installed_ver.as_deref(), Some(&l.version)), Status::UpToDate | Status::AheadOfRepo) {
            println!("Nothing to do: installed {} is not older than NVIDIA's latest ({}).", installed_ver.as_deref().unwrap_or("?"), l.version);
            return 0;
        }
        let missing: Vec<&str> = ["gcc", "make", "dkms"].into_iter().filter(|t| !exec::which(t)).collect();
        if !missing.is_empty() {
            return fail(&format!("the .run installer needs these tools first: {}. Install them with your package manager and try again.", missing.join(", ")));
        }
        if ctx.manager.is_some() {
            notes.push("You chose NVIDIA's .run installer on a distro with its own driver packages. The two don't mix: if the distro's nvidia packages are installed, remove them first, and kernel/driver updates won't be tracked by your package manager. The distro package is the recommended route.".into());
        }
        if std::path::Path::new("/sys/module/nouveau").exists() {
            notes.push("The nouveau driver is loaded; the installer may need it blacklisted, then a reboot.".into());
        }
        target = l.version.clone();
        flavor = f;
        let dir = PathBuf::from("/var/tmp/driverkeep");
        steps = vec![
            vec![format!("(download {} into {} and verify its SHA-256 against NVIDIA's published checksum)", l.run_url, dir.display())],
            nvidia::installer_argv(&dir.join(&l.file_name), f),
        ];
        latest = Some(l);
    }

    let plan_text: Vec<String> = steps.iter().map(|s| if s[0].starts_with('(') { s[0].clone() } else { exec::shell_join(s) }).collect();
    if cli.json {
        let plan = Plan {
            method: if use_run { "run" } else { "package" },
            target_version: target,
            installed_version: installed_ver,
            flavor,
            steps: plan_text,
            needs_root: true,
            reboot_required_after: true,
        };
        println!("{}", serde_json::to_string_pretty(&plan).unwrap_or_default());
        return 0;
    }

    println!("Plan: {} -> {}  ({} modules, via {})",
        installed_ver.as_deref().unwrap_or("not installed"), target, flavor.label(), if use_run { "NVIDIA .run installer" } else { "package manager" });
    for s in &plan_text {
        println!("  $ {s}");
    }
    for n in &notes {
        println!("Note: {n}");
    }
    if ctx.secure_boot == SecureBoot::Enabled {
        println!("\nWARNING: {}", secure_boot_warning(&ctx));
    }
    if ctx.headless {
        println!("(Headless server detected; no display needed.)");
    }
    println!("A reboot will be needed afterwards to load the new driver.");

    if dry_run {
        println!("\nDry run: nothing was changed.");
        return 0;
    }
    if !exec::is_root() {
        eprintln!("\nerror: installing drivers needs root. Re-run with: sudo driverkeep-linux update");
        return EXIT_NEEDS_ROOT;
    }
    if !yes && !confirm("\nProceed?") {
        eprintln!("{}", if std::io::stdin().is_terminal() { "Cancelled. Nothing was changed." } else { "Not a terminal and no --yes given: nothing was changed." });
        return EXIT_DECLINED;
    }

    // Execute.
    if let Some(l) = latest {
        eprintln!("Downloading {} ...", l.file_name);
        let dir = PathBuf::from("/var/tmp/driverkeep");
        match nvidia::download_verified(&l, &dir) {
            Ok(path) => steps = vec![nvidia::installer_argv(&path, flavor)],
            Err(e) => return fail(&e),
        }
    }
    for s in &steps {
        eprintln!("\n$ {}", exec::shell_join(s));
        if !exec::run_visible(s) {
            return fail(&format!("`{}` failed. Stopping; later steps were not run. Check the output above.", exec::shell_join(s)));
        }
    }
    println!("\nDone. REBOOT to load the new driver (`sudo reboot`), then run `driverkeep-linux check`.");
    if ctx.secure_boot == SecureBoot::Enabled {
        println!("Secure Boot is on: if the driver doesn't load after reboot, the module needs signing (see warning above).");
    }
    0
}

fn main() {
    let cli = Cli::parse();
    let code = match &cli.cmd {
        Cmd::Check { exit_code, offline } => cmd_check(&cli, *exit_code, *offline),
        Cmd::Update { yes, dry_run } => cmd_update(&cli, *yes, *dry_run),
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_cases() {
        assert_eq!(status(false, None, None), Status::NoGpu);
        assert_eq!(status(true, None, Some("550.1")), Status::NotInstalled);
        assert_eq!(status(true, Some("535.183.01"), Some("550.163.01")), Status::UpdateAvailable);
        assert_eq!(status(true, Some("550.163.01"), Some("550.163.01")), Status::UpToDate);
        assert_eq!(status(true, Some("570.1"), Some("550.163.01")), Status::AheadOfRepo);
        assert_eq!(status(true, Some("570.1"), None), Status::Unknown);
    }
}

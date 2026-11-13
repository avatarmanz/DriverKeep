# DriverKeep for Linux (NVIDIA, command line)

Checks whether a newer NVIDIA GPU driver is available and installs it using your distro's own package manager.
Built for a headless Debian server with an RTX 3080 Ti first, but it also handles Ubuntu, Arch, Fedora/RHEL and
openSUSE. This is a separate program from the Windows app: it lives in its own crate here and is not built by
`make-installer.cmd`.

## What has actually been tested

**Early test version. Please read this.**

| What | Status |
|---|---|
| Compiles | Yes, on Windows (host build) |
| Unit tests (47) | Pass. They cover parsing of `os-release`, `lspci`, `nvidia-smi`, `/proc/driver/nvidia/version`, `apt-cache policy`, `ubuntu-drivers`, `pacman -Si`, `dnf list`/`repolist`, `zypper search`, `mokutil`, and NVIDIA's `latest.txt` and checksum files, using **sample text I wrote from memory of the tools' output**, not captures from real machines |
| Fetching NVIDIA's latest version | Run for real against download.nvidia.com (returned 620.70.06, and the `.run` checksum file exists) |
| Running on Linux (any distro) | **Not done.** No Linux machine was available |
| Real `update` (installing a driver) | **Not done on any distro** |
| `.run` download + checksum path | Not run end to end (the file is about 550 MB). The URL layout was checked with HTTP requests |
| Static musl build | **Not built.** Instructions below are untested |
| Debian, Ubuntu, Arch | Most likely to work (package names are well known) but unverified |
| Fedora, RHEL clones, openSUSE, Manjaro | Package names and repo handling are best effort and **more likely to need fixes** |

Package names such as Debian's `nvidia-open`, openSUSE's `nvidia-open-driver-G06-signed-kmp-default`, and the
`.run` option `--kernel-module-type` come from documentation I remember, not from testing. The tool looks each
package up in the repositories first and only offers what actually exists, so a wrong name shows up as "nothing
found" rather than a broken install, but please report it.

## Use

```bash
driverkeep-linux check                 # read-only: installed vs available
driverkeep-linux check --json          # same, for scripts
driverkeep-linux check --exit-code     # exit 10 if an update is available
sudo driverkeep-linux update --dry-run # show the plan, change nothing
sudo driverkeep-linux update           # ask, then install
sudo driverkeep-linux update --yes     # no question (for automation)
```

Options (all commands): `--flavor auto|open|proprietary`, `--method auto|package|run`.
`check --offline` skips the call to NVIDIA's website.

Exit codes: 0 ok, 1 error, 3 needs root, 4 not confirmed, 10 update available (with `--exit-code`).

## What it does

- Finds the GPU through `lspci` / `/sys` (vendor `10de`) and the installed driver through `nvidia-smi`,
  `modinfo nvidia` and `/proc/driver/nvidia/version`. It reports both the installed version and the one loaded in
  the running kernel, and tells you a reboot is needed when they differ.
- Reads `/etc/os-release` and uses the distro's package manager: `apt` (Debian, Ubuntu and derivatives such as
  Mint), `pacman` (Arch family; Manjaro minimal), `dnf` (Fedora, RHEL clones), `zypper` (openSUSE). It works out
  the newest driver package the repositories offer (`apt-cache policy` + `ubuntu-drivers`, `pacman -Si`,
  `dnf list`, `zypper search`) and compares it with what is installed.
- Shows NVIDIA's latest published version for reference, from
  `https://download.nvidia.com/XFree86/Linux-x86_64/latest.txt`. It is only a reference: distros hold back for
  stability, and a new branch may not support older cards.
- **Open vs proprietary kernel modules.** GPUs from Turing (2018) on, including the 3080 Ti, support NVIDIA's open
  modules. `auto` keeps whatever is installed, and on a fresh install picks open when the distro offers it. It
  never switches an installed driver between the two unless you pass `--flavor`.
- **Secure Boot.** Warns when it is on, because unsigned modules won't load, with a hint for your distro.
- **Headless servers.** Detected and noted; no display is needed.
- **Reboot reminder** after every install.

### Debian specifics

Debian's `nvidia-driver` is in the `non-free` component. If it isn't found, the tool says so and tells you to
enable `non-free` and `non-free-firmware`; it won't edit your apt sources. It installs `linux-headers-amd64`
alongside so DKMS can build the module. For the open modules on Debian it looks for `nvidia-open` from NVIDIA's
own CUDA repository; if that repository isn't configured you get the proprietary `nvidia-driver`.

### Arch note

Arch doesn't support partial upgrades, so `update` runs `pacman -Syu` for the whole system and says so before
asking. The version shown by `check` comes from your last sync.

### Fallback: NVIDIA `.run` installer

Used automatically only on unrecognised distros, or when you pass `--method run`. It downloads from
`download.nvidia.com` only (HTTPS, redirects disabled), checks the file's SHA-256 against NVIDIA's published
`.run.sha256sum`, deletes the file if it doesn't match, and installs with `--dkms` so kernel updates still rebuild
the module. Needs `gcc`, `make` and `dkms` already installed. The checksum comes from the same site as the file, so
it protects against corruption and partial downloads, not against NVIDIA's site being compromised. Refused for
GPUs older than Turing.

## Safety

- `check` changes nothing. `update` needs root (it says so and exits with 3 instead of failing oddly), shows the
  exact commands first, and asks before doing anything unless `--yes`.
- With no terminal and no `--yes`, it refuses rather than guessing.
- It never adds repositories, never edits apt sources, and never downloads from anywhere except the distro's
  repositories (through the package manager) or NVIDIA's own domain.
- Commands run as argument lists, not through a shell.

## Build

```bash
cd linux
cargo build --release
cargo test
```

Static musl binary (untested):

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

TLS uses rustls, so there is no OpenSSL dependency; the `ring` crate needs a C compiler for the target
(`musl-tools` on Debian/Ubuntu).

## Release packages

`.github/workflows/linux-release.yml` builds a static x86_64 binary and packages it as `.tar.gz`, `.deb`, `.rpm`
and `.AppImage`, plus a `SHA256SUMS` file. It runs on tags named `linux-v*` (or by hand from the Actions tab) and
attaches the files to a pre-release. **It has never been run**, so the first run will probably need small fixes.
The packages are static binaries with no dependencies; `pciutils` is only recommended (for GPU model names).

Not provided, on purpose: **Flatpak** (a sandboxed app can't install kernel drivers), **.apk** (Android doesn't
apply; Alpine uses musl, which NVIDIA's proprietary driver doesn't support), and **.exe/.msi** (those are for the
Windows app, which has its own installer).

## Reporting problems

Open an issue on GitHub and include the output of `driverkeep-linux check --json` plus the contents of
`/etc/os-release`. Remove anything you don't want public.

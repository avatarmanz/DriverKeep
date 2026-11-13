# DriverKeep

A fast, simple Windows app that keeps your computer up to date in one place.

- **One scan, three kinds of update:** drivers, Windows updates and your installed software. Update everything
  with one button, or one group at a time.
- **All your hardware:** everything connected now, and everything that used to be plugged in, with full system
  details on one page.
- **Safe driver updates:** your current driver is backed up first, a Windows restore point is made once a day,
  and you can put any driver back.
- **Runs in the background if you want:** start with Windows, scheduled scans, silent installs during the hours
  you choose, and automatic restarts.
- **Light and dark mode.**
- **Keeps itself current:** each scan checks this page for a later release, and Settings can install it automatically.

## Screenshots

**Updates**

![Updates](screenshots/updates.png)

| Light | OLED |
| --- | --- |
| ![Updates, light](screenshots/updates-light.png) | ![Updates, OLED](screenshots/updates-oled.png) |

**History, System Restore and System Info**

| History | System Restore |
| --- | --- |
| ![History](screenshots/backups.png) | ![System Restore](screenshots/restore.png) |

The System Restore picture uses made-up restore points. The real list needs administrator rights.

| System Info |
| --- |
| ![System Info](screenshots/system-info.png) |

**Settings**

| General | Customization |
| --- | --- |
| ![General](screenshots/settings-general.png) | ![Customization](screenshots/settings-customization.png) |
| **Scanning** | **Installing** |
| ![Scanning](screenshots/settings-scanning.png) | ![Installing](screenshots/settings-installing.png) |
| **Backup** | **About** |
| ![Backup](screenshots/settings-backup.png) | ![About](screenshots/settings-about.png) |
| **Hidden updates** | |
| ![Hidden updates](screenshots/settings-hidden.png) | |

**Customization in light and OLED**

| Light | OLED |
| --- | --- |
| ![Customization, light](screenshots/settings-customization-light.png) | ![Customization, OLED](screenshots/settings-customization-oled.png) |

**Dialogs**

| Closing while working | Install tools | Install without a backup |
| --- | --- | --- |
| ![Closing](screenshots/dialog-close.png) | ![Install tools](screenshots/dialog-tools.png) | ![No backup](screenshots/dialog-no-backup.png) |

## Where updates come from

Only public, official sources are used:

- Windows Update, for drivers and Windows updates
- The Microsoft Update Catalog, matched to your exact hardware and checked against Microsoft's published checksum
- The Windows Package Manager and Chocolatey, for other installed software

## Install

Download `DriverKeep-Setup.exe` from the Releases page and run it. It needs Windows 10 or 11. Because the
installer isn't code-signed yet, Windows SmartScreen may warn you: choose **More info**, then **Run anyway**.

Setup adds Microsoft's Visual C++ files (x64 and x86) if they're missing, so it works on a brand new PC. On first
launch DriverKeep offers to install the Windows Package Manager and Chocolatey in the background.

An `.msi` (`DriverKeep.msi`) is also on the Releases page, for managed computers and deployment tools. It installs
to `Program Files\DriverKeep` and doesn't add Microsoft's Visual C++ files. Use it on a clean computer or to upgrade
from an earlier `.msi`; don't install it over a copy from `DriverKeep-Setup.exe`.

To remove it, use **Installed apps** in Windows Settings. Your driver backups are kept.

## DriverKeep for Linux (command line, early test version)

A separate command-line tool in [`linux/`](linux/) checks for and installs NVIDIA GPU driver updates on Linux
(Debian, Ubuntu, Arch, Fedora/RHEL, openSUSE, or NVIDIA's `.run` installer as a fallback). It is **not** part of
the Windows app or installer. It has only been compiled and unit-tested so far, not run on real Linux machines;
see [`linux/README.md`](linux/README.md) for details and how to report problems.

## Credits

- Created by avatarmanz
- Built with Claude by Anthropic
- Made with Craft Anything

## Build it yourself

You need the Rust toolchain. Run `make-installer.cmd` to create `dist\DriverKeep-Setup.exe`. To also make an `.msi`, install WiX Toolset v3 and run `make-msi.cmd`
(creates `dist\DriverKeep.msi`).

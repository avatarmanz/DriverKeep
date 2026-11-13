//! Secure Boot and headless detection.

use crate::exec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecureBoot {
    Enabled,
    Disabled,
    Unknown,
}

/// Parse `mokutil --sb-state`: "SecureBoot enabled" / "SecureBoot disabled" /
/// "EFI variables are not supported on this system" (legacy BIOS boot => no Secure Boot).
pub fn parse_mokutil(text: &str) -> SecureBoot {
    let t = text.to_lowercase();
    if t.contains("secureboot enabled") {
        SecureBoot::Enabled
    } else if t.contains("secureboot disabled") || t.contains("not supported on this system") {
        SecureBoot::Disabled
    } else {
        SecureBoot::Unknown
    }
}

/// The SecureBoot EFI variable file is 4 attribute bytes followed by 1 value byte.
pub fn parse_efivar(bytes: &[u8]) -> SecureBoot {
    match bytes.get(4) {
        Some(1) => SecureBoot::Enabled,
        Some(0) => SecureBoot::Disabled,
        _ => SecureBoot::Unknown,
    }
}

pub fn secure_boot() -> SecureBoot {
    if let Some(o) = exec::capture("mokutil", &["--sb-state"]) {
        let r = parse_mokutil(&format!("{}{}", o.stdout, o.stderr));
        if r != SecureBoot::Unknown {
            return r;
        }
    }
    let dir = match std::fs::read_dir("/sys/firmware/efi/efivars") {
        Ok(d) => d,
        Err(_) => {
            // No efivars at all: legacy BIOS boot, or not a Linux host.
            return if std::path::Path::new("/sys/firmware/efi").exists() {
                SecureBoot::Unknown
            } else {
                SecureBoot::Disabled
            };
        }
    };
    for e in dir.flatten() {
        if e.file_name().to_string_lossy().starts_with("SecureBoot-") {
            if let Ok(b) = std::fs::read(e.path()) {
                return parse_efivar(&b);
            }
        }
    }
    SecureBoot::Unknown
}

/// No graphical session variables: typical server / SSH login.
pub fn is_headless() -> bool {
    std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() && {
        // A desktop session started from a login manager may not export these to root's sudo
        // environment, so also look at the systemd default target.
        exec::stdout_of("systemctl", &["get-default"])
            .map(|s| s.trim() != "graphical.target")
            .unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mokutil_states() {
        assert_eq!(parse_mokutil("SecureBoot enabled\n"), SecureBoot::Enabled);
        assert_eq!(parse_mokutil("SecureBoot disabled\n"), SecureBoot::Disabled);
        assert_eq!(parse_mokutil("EFI variables are not supported on this system\n"), SecureBoot::Disabled);
        assert_eq!(parse_mokutil("garbage"), SecureBoot::Unknown);
    }

    #[test]
    fn efivar_states() {
        assert_eq!(parse_efivar(&[6, 0, 0, 0, 1]), SecureBoot::Enabled);
        assert_eq!(parse_efivar(&[6, 0, 0, 0, 0]), SecureBoot::Disabled);
        assert_eq!(parse_efivar(&[6, 0]), SecureBoot::Unknown);
    }
}

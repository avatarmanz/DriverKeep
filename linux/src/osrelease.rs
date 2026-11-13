//! /etc/os-release parsing and mapping to the package-manager family we use.

use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct OsRelease {
    pub id: String,
    pub id_like: Vec<String>,
    pub name: String,
    pub version_id: String,
    pub pretty: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Debian,
    Ubuntu,
    Arch,
    Manjaro,
    Fedora,
    Rhel,
    Suse,
    Unknown,
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    let inner = if v.len() >= 2
        && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')))
    {
        &v[1..v.len() - 1]
    } else {
        v
    };
    // os-release allows shell-style escapes; handle the common ones.
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn parse(text: &str) -> OsRelease {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), unquote(v));
        }
    }
    let get = |k: &str| map.get(k).cloned().unwrap_or_default();
    OsRelease {
        id: get("ID").to_lowercase(),
        id_like: get("ID_LIKE").split_whitespace().map(|s| s.to_lowercase()).collect(),
        name: get("NAME"),
        version_id: get("VERSION_ID"),
        pretty: get("PRETTY_NAME"),
    }
}

impl OsRelease {
    pub fn display(&self) -> String {
        if !self.pretty.is_empty() {
            self.pretty.clone()
        } else if !self.name.is_empty() {
            format!("{} {}", self.name, self.version_id).trim().to_string()
        } else {
            "unknown Linux".to_string()
        }
    }

    fn like(&self, what: &str) -> bool {
        self.id_like.iter().any(|x| x == what)
    }

    pub fn family(&self) -> Family {
        let id = self.id.as_str();
        if id == "manjaro" || id == "manjaro-arm" {
            Family::Manjaro
        } else if id == "arch" || self.like("arch") {
            Family::Arch
        } else if id == "ubuntu" || self.like("ubuntu") {
            Family::Ubuntu
        } else if id == "debian" || self.like("debian") {
            Family::Debian
        } else if id == "fedora" || (self.like("fedora") && !self.like("rhel")) {
            Family::Fedora
        } else if matches!(id, "rhel" | "centos" | "rocky" | "almalinux" | "ol") || self.like("rhel") {
            Family::Rhel
        } else if id.contains("suse") || self.like("suse") || self.like("opensuse") {
            Family::Suse
        } else {
            Family::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEBIAN12: &str = r#"PRETTY_NAME="Debian GNU/Linux 12 (bookworm)"
NAME="Debian GNU/Linux"
VERSION_ID="12"
VERSION="12 (bookworm)"
VERSION_CODENAME=bookworm
ID=debian
HOME_URL="https://www.debian.org/"
"#;

    const UBUNTU: &str = "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\nID_LIKE=debian\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\n";
    const MINT: &str = "NAME=\"Linux Mint\"\nID=linuxmint\nID_LIKE=\"ubuntu debian\"\nVERSION_ID=\"22\"\n";
    const ARCH: &str = "NAME=\"Arch Linux\"\nPRETTY_NAME=\"Arch Linux\"\nID=arch\nBUILD_ID=rolling\n";
    const ENDEAVOUR: &str = "NAME=\"EndeavourOS\"\nID=endeavouros\nID_LIKE=arch\n";
    const MANJARO: &str = "NAME=\"Manjaro Linux\"\nID=manjaro\nID_LIKE=arch\n";
    const FEDORA: &str = "NAME=\"Fedora Linux\"\nVERSION_ID=41\nID=fedora\n";
    const ROCKY: &str = "NAME=\"Rocky Linux\"\nID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\nVERSION_ID=\"9.4\"\n";
    const LEAP: &str = "NAME=\"openSUSE Leap\"\nVERSION_ID=\"15.6\"\nID=\"opensuse-leap\"\nID_LIKE=\"suse opensuse\"\n";
    const TW: &str = "NAME=\"openSUSE Tumbleweed\"\nID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n";
    const ALPINE: &str = "NAME=\"Alpine Linux\"\nID=alpine\n";

    #[test]
    fn parses_fields_and_quotes() {
        let o = parse(DEBIAN12);
        assert_eq!(o.id, "debian");
        assert_eq!(o.version_id, "12");
        assert_eq!(o.pretty, "Debian GNU/Linux 12 (bookworm)");
        assert_eq!(o.display(), "Debian GNU/Linux 12 (bookworm)");
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let o = parse("# hi\n\nID='fedora'\n");
        assert_eq!(o.id, "fedora");
    }

    #[test]
    fn maps_families() {
        assert_eq!(parse(DEBIAN12).family(), Family::Debian);
        assert_eq!(parse(UBUNTU).family(), Family::Ubuntu);
        assert_eq!(parse(MINT).family(), Family::Ubuntu);
        assert_eq!(parse(ARCH).family(), Family::Arch);
        assert_eq!(parse(ENDEAVOUR).family(), Family::Arch);
        assert_eq!(parse(MANJARO).family(), Family::Manjaro);
        assert_eq!(parse(FEDORA).family(), Family::Fedora);
        assert_eq!(parse(ROCKY).family(), Family::Rhel);
        assert_eq!(parse(LEAP).family(), Family::Suse);
        assert_eq!(parse(TW).family(), Family::Suse);
        assert_eq!(parse(ALPINE).family(), Family::Unknown);
    }
}

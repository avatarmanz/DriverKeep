//! Small helpers for running external commands.

use std::process::{Command, Stdio};

pub struct Out {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run a command, capture its output. `None` if it could not be started (not installed).
/// Forces the C locale so we parse English output.
pub fn capture(prog: &str, args: &[&str]) -> Option<Out> {
    let o = Command::new(prog)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    Some(Out {
        ok: o.status.success(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    })
}

/// Stdout of a successful command, or None.
pub fn stdout_of(prog: &str, args: &[&str]) -> Option<String> {
    capture(prog, args).filter(|o| o.ok).map(|o| o.stdout)
}

/// Is `name` an executable somewhere on PATH?
pub fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
        .unwrap_or(false)
}

/// Run a command with its output going straight to the user's terminal.
pub fn run_visible(argv: &[String]) -> bool {
    let Some((prog, args)) = argv.split_first() else { return false };
    Command::new(prog)
        .args(args)
        .env("LC_ALL", "C")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Shell-style rendering for display only (we never pass this through a shell).
pub fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:+@%".contains(c)) {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn is_root() -> bool {
    stdout_of("id", &["-u"]).map(|s| s.trim() == "0").unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_for_display() {
        let v = vec!["apt-get".to_string(), "install".into(), "a b".into()];
        assert_eq!(shell_join(&v), "apt-get install 'a b'");
    }
}

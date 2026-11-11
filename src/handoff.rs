//! Lets the window ask the background helper to install updates. The helper runs as administrator
//! when "Install updates silently" is on, so nothing needs to restart and no permission box is
//! shown. The two talk through small files in DriverKeep's data folder.
//!
//! The request only names which updates to install. The helper looks for updates itself and
//! installs the ones that match, so a request can never make it install anything it didn't find.

use crate::system::{self, Settings};
use crate::wu::ItemState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const REQUEST_FILE: &str = "install-request.json";
pub const PROGRESS_FILE: &str = "install-progress.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub ids: Vec<String>,
    pub skip_backup: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Item {
    pub id: String,
    pub state: String,
    pub message: String,
    pub percent: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Progress {
    pub request: u64,
    pub percent: u32,
    pub text: String,
    pub items: Vec<Item>,
    pub notes: Vec<String>,
    pub finished: bool,
    /// The helper couldn't take the job (for example it isn't running as administrator).
    pub refused: bool,
    pub restart: bool,
    pub updated: i64,
}

impl Progress {
    pub fn new(request: u64) -> Self {
        Self { request, ..Default::default() }
    }

    fn item(&mut self, id: &str) -> &mut Item {
        if let Some(at) = self.items.iter().position(|i| i.id == id) {
            return &mut self.items[at];
        }
        self.items.push(Item { id: id.to_string(), ..Default::default() });
        self.items.last_mut().expect("just pushed")
    }

    pub fn set_state(&mut self, id: &str, state: &ItemState) {
        let (name, message) = match state {
            ItemState::Waiting => ("waiting", String::new()),
            ItemState::SavingBackup => ("saving", String::new()),
            ItemState::Downloading => ("downloading", String::new()),
            ItemState::Installing => ("installing", String::new()),
            ItemState::Installed => ("installed", String::new()),
            ItemState::NeedsRestart => ("restart", String::new()),
            ItemState::Skipped(why) => ("skipped", why.clone()),
            ItemState::Failed(why) => ("failed", why.clone()),
        };
        let item = self.item(id);
        item.state = name.to_string();
        item.message = message;
        item.percent = None;
    }

    pub fn set_percent(&mut self, id: &str, percent: u32) {
        self.item(id).percent = Some(percent);
    }
}

impl Item {
    pub fn to_state(&self) -> ItemState {
        match self.state.as_str() {
            "saving" => ItemState::SavingBackup,
            "downloading" => ItemState::Downloading,
            "installing" => ItemState::Installing,
            "installed" => ItemState::Installed,
            "restart" => ItemState::NeedsRestart,
            "skipped" => ItemState::Skipped(self.message.clone()),
            "failed" => ItemState::Failed(self.message.clone()),
            _ => ItemState::Waiting,
        }
    }
}

fn file(name: &str) -> Option<PathBuf> {
    let folder = system::data_dir()?;
    std::fs::create_dir_all(&folder).ok()?;
    Some(folder.join(name))
}

/// Written in one piece, so the other side never reads half a file.
fn write_whole(name: &str, text: &str) -> bool {
    let Some(path) = file(name) else { return false };
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, text).is_ok() && std::fs::rename(&temp, &path).is_ok()
}

/// True when the helper that runs as administrator is switched on in Settings.
pub fn helper_available(settings: &Settings) -> bool {
    settings.silent_install && system::task_exists()
}

/// Window side: leaves a request for the helper, and clears any older answer.
pub fn send(request: &Request) -> bool {
    if let Some(old) = file(PROGRESS_FILE) {
        let _ = std::fs::remove_file(old);
    }
    match serde_json::to_string(request) {
        Ok(text) => write_whole(REQUEST_FILE, &text),
        Err(_) => false,
    }
}

/// Helper side: picks up a waiting request, if any.
pub fn take_request() -> Option<Request> {
    let path = file(REQUEST_FILE)?;
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    serde_json::from_str(&text).ok()
}

/// Helper side: tells the window how far along things are.
pub fn write_progress(progress: &mut Progress) {
    progress.updated = chrono::Local::now().timestamp();
    if let Ok(text) = serde_json::to_string(progress) {
        write_whole(PROGRESS_FILE, &text);
    }
}

/// Window side: the helper's latest answer to this request.
pub fn read_progress(request: u64) -> Option<Progress> {
    let path = file(PROGRESS_FILE)?;
    let text = std::fs::read_to_string(path).ok()?;
    let progress: Progress = serde_json::from_str(&text).ok()?;
    (progress.request == request).then_some(progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_states_survive_the_trip_through_a_file() {
        let mut progress = Progress::new(7);
        progress.set_state("a", &ItemState::Downloading);
        progress.set_percent("a", 40);
        progress.set_state("b", &ItemState::Failed("no".into()));
        progress.set_state("c", &ItemState::NeedsRestart);
        let text = serde_json::to_string(&progress).unwrap();
        let back: Progress = serde_json::from_str(&text).unwrap();
        assert_eq!(back.request, 7);
        assert_eq!(back.items[0].to_state(), ItemState::Downloading);
        assert_eq!(back.items[0].percent, Some(40));
        assert_eq!(back.items[1].to_state(), ItemState::Failed("no".into()));
        assert_eq!(back.items[2].to_state(), ItemState::NeedsRestart);
    }

    #[test]
    fn a_new_state_clears_the_old_percentage() {
        let mut progress = Progress::new(1);
        progress.set_state("a", &ItemState::Downloading);
        progress.set_percent("a", 90);
        progress.set_state("a", &ItemState::Installing);
        assert_eq!(progress.items[0].percent, None);
    }
}

/// A number that's different for every request.
pub fn new_request_id() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |t| t.as_nanos() as u64)
}

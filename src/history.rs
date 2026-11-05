//! A simple record of every driver change this app makes.

use crate::system;
use serde::{Deserialize, Serialize};

const FILE: &str = "history.json";
const KEEP: usize = 500;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub when: i64,
    pub device: String,
    pub instance_id: String,
    pub from_version: String,
    pub to_version: String,
    pub source: String,
    pub backup: Option<String>,
    pub automatic: bool,
}

pub fn load() -> Vec<Record> {
    system::read_cache(FILE).unwrap_or_default()
}

pub fn add(records: Vec<Record>) {
    if records.is_empty() {
        return;
    }
    let mut all = load();
    all.splice(0..0, records);
    all.truncate(KEEP);
    system::write_cache(FILE, &all);
}

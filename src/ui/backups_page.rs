//! Backup, restore and recent changes together on one page.

use super::look::*;
use super::{App, Ask, Busy};
use crate::backup::{self, BackupEntry, BackupFile, RestoreState};
use crate::devices::Device;
use crate::history::Record;
use crate::system;
use chrono::{Datelike, NaiveDate};
use egui::{Align, Align2, FontId, Layout, RichText, Sense, Stroke, Vec2};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

/// Shown on a change that has no saved backup to go back to.
const NOT_RESTORABLE: &str = "Cannot be restored";

/// The first day of the month a number of months away.
fn shift_month(first: NaiveDate, by: i32) -> NaiveDate {
    let total = first.year() * 12 + first.month0() as i32 + by;
    NaiveDate::from_ymd_opt(total.div_euclid(12), (total.rem_euclid(12) + 1) as u32, 1).unwrap_or(first)
}

impl App {
    pub(super) fn backups_page(&mut self, ui: &mut egui::Ui) {
        let count = self
            .devices
            .iter()
            .filter(|d| self.backup_visible(d) && self.backup_picks.contains(&d.instance_id))
            .count();
        ui.horizontal(|ui| {
            // Clicking the title or the count scrolls down to the saved backups.
            let saved = self.backups.len();
            let title = egui::Label::new(RichText::new("Backups:").font(bold(20.0))).sense(Sense::click());
            let amount = egui::Label::new(RichText::new(format!("{saved} available")).font(bold(20.0)).color(teal()))
                .sense(Sense::click());
            let jump = ui.add(title).on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
                | ui.add(amount).on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
            if jump {
                self.jump_to_backups = true;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = format!("Back Up {count} Driver{}", plural(count));
                if primary(ui, &label, count > 0 && self.busy == Busy::Idle) {
                    self.run_backup(ui.ctx());
                }
                if secondary(ui, if self.choosing { "Done Choosing" } else { "Choose Drivers" }, true) {
                    self.choosing = !self.choosing;
                }
                if secondary(ui, "Open a Backup", self.busy == Busy::Idle) {
                    self.open_file();
                }
                if secondary(ui, "System Restore", true) {
                    system::open_windows_restore();
                }
            });
        });
        ui.add_space(6.0);
        if matches!(self.busy, Busy::BackingUp | Busy::Restoring) {
            self.working_card(ui);
        }
        self.restore_results(ui);

        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if self.choosing {
                self.choose_drivers(ui);
            }
            self.recent_changes(ui);
            self.backup_list(ui);
        });
    }

    fn restore_results(&mut self, ui: &mut egui::Ui) {
        if self.results.is_empty() {
            return;
        }
        let results = self.results.clone();
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            heading(ui, "Restore results");
            for r in &results {
                let (text, color) = match &r.state {
                    RestoreState::Restored { restart: true, .. } => {
                        (format!("Restored {}, restart to finish", r.target_version), CAUTION)
                    }
                    RestoreState::Restored { now_version, .. } => {
                        (format!("Restored, now on {now_version}"), SUCCESS)
                    }
                    RestoreState::Unconfirmed { now_version } => {
                        (format!("Windows kept {now_version}. It may finish after a restart"), CAUTION)
                    }
                    RestoreState::WaitingForDevice => ("Will apply when it's plugged in".to_string(), SKY),
                    RestoreState::Failed(why) => (why.clone(), CRITICAL),
                };
                ui.horizontal_wrapped(|ui| {
                    ui.label(r.device_name.clone());
                    ui.label(RichText::new(text).color(color));
                });
            }
        });
        ui.add_space(8.0);
    }

    fn choose_drivers(&mut self, ui: &mut egui::Ui) {
        let backable: Vec<Device> =
            self.devices.iter().filter(|d| self.backup_visible(d)).cloned().collect();
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                heading(ui, "Drivers to back up");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("None").clicked() {
                        self.backup_picks.clear();
                    }
                    if ui.small_button("All").clicked() {
                        self.backup_picks = backable.iter().map(|d| d.instance_id.clone()).collect();
                    }
                    ui.checkbox(&mut self.show_hidden_backups, "Show hidden devices");
                });
            });
            let mut groups: BTreeMap<String, Vec<&Device>> = BTreeMap::new();
            for d in &backable {
                groups.entry(d.class_title.clone()).or_default().push(d);
            }
            for (group, list) in &groups {
                let picked = list.iter().filter(|d| self.backup_picks.contains(&d.instance_id)).count();
                egui::CollapsingHeader::new(format!("{group}  ({picked} of {})", list.len()))
                    .id_salt(("pick", group))
                    .show(ui, |ui| {
                        for d in list {
                            let mut on = self.backup_picks.contains(&d.instance_id);
                            let version = d.driver.as_ref().map(|x| x.version.clone()).unwrap_or_default();
                            let mut text = format!("{}   {}", d.name, version);
                            if !d.present {
                                text.push_str("   (not plugged in)");
                            }
                            if ui.checkbox(&mut on, text).changed() {
                                if on {
                                    self.backup_picks.insert(d.instance_id.clone());
                                } else {
                                    self.backup_picks.remove(&d.instance_id);
                                }
                            }
                        }
                    });
            }
        });
        ui.add_space(8.0);
    }

    /// A month calendar of everything DriverKeep changed. Picking a day lists what happened
    /// that day, with Put Back wherever a backup was saved first.
    fn recent_changes(&mut self, ui: &mut egui::Ui) {
        if self.history.is_empty() {
            return;
        }
        heading(ui, "Changes");
        ui.add_space(2.0);
        let today = chrono::Local::now().date_naive();
        let mut counts: HashMap<NaiveDate, usize> = HashMap::new();
        for record in &self.history {
            *counts.entry(super::day_of(record.when)).or_default() += 1;
        }

        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if secondary(ui, "‹", true) {
                    self.calendar_month = shift_month(self.calendar_month, -1);
                }
                ui.label(RichText::new(self.calendar_month.format("%B %Y").to_string()).font(bold(15.0)));
                if secondary(ui, "›", true) {
                    self.calendar_month = shift_month(self.calendar_month, 1);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if secondary(ui, "Today", true) {
                        self.calendar_month = super::month_of(today);
                        self.selected_day = Some(today);
                    }
                });
            });
            ui.add_space(6.0);

            let cell_w = (ui.available_width() / 7.0).floor();
            let p = pal();
            ui.spacing_mut().item_spacing = Vec2::new(0.0, 4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for name in ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"] {
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(cell_w, 20.0), Sense::hover());
                    ui.painter().text(rect.center(), Align2::CENTER_CENTER, name, FontId::proportional(12.0), p.soft);
                }
            });

            let first = self.calendar_month;
            let lead = first.weekday().num_days_from_sunday() as usize;
            let days_in = (shift_month(first, 1) - first).num_days() as usize;
            let weeks = (lead + days_in).div_ceil(7);
            for week in 0..weeks {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for column in 0..7 {
                        let slot = week * 7 + column;
                        let (rect, response) = ui.allocate_exact_size(Vec2::new(cell_w, 48.0), Sense::click());
                        if slot < lead || slot >= lead + days_in {
                            continue;
                        }
                        let day = first + chrono::Days::new((slot - lead) as u64);
                        let changes = counts.get(&day).copied().unwrap_or(0);
                        let picked = self.selected_day == Some(day);
                        let inner = rect.shrink(2.0);
                        let fill = if picked {
                            teal().gamma_multiply(0.42)
                        } else if response.hovered() {
                            p.hover
                        } else if changes > 0 {
                            p.chosen
                        } else {
                            p.field
                        };
                        let painter = ui.painter();
                        painter.rect_filled(inner, 8.0, fill);
                        if picked {
                            painter.rect_stroke(inner, 8.0, Stroke::new(2.0, teal()), egui::StrokeKind::Inside);
                        } else if day == today {
                            painter.rect_stroke(inner, 8.0, Stroke::new(1.0, p.soft), egui::StrokeKind::Inside);
                        }
                        let number_font = if changes > 0 { bold(14.0) } else { FontId::proportional(14.0) };
                        let number_color = if changes > 0 || picked { p.text } else { p.soft };
                        painter.text(
                            inner.left_top() + Vec2::new(9.0, 5.0),
                            Align2::LEFT_TOP,
                            day.day().to_string(),
                            number_font,
                            number_color,
                        );
                        if changes > 0 {
                            let text = format!("{changes} change{}", plural(changes));
                            painter.circle_filled(inner.left_bottom() + Vec2::new(13.0, -10.0), 3.5, teal());
                            painter.text(
                                inner.left_bottom() + Vec2::new(21.0, -10.0),
                                Align2::LEFT_CENTER,
                                text,
                                FontId::proportional(11.5),
                                p.soft,
                            );
                        }
                        if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            self.selected_day = if picked { None } else { Some(day) };
                        }
                    }
                });
            }
        });
        ui.add_space(8.0);

        let Some(day) = self.selected_day else {
            soft(ui, "Pick a day to see what changed and put things back.");
            ui.add_space(8.0);
            return;
        };
        let mut records: Vec<Record> =
            self.history.iter().filter(|r| super::day_of(r.when) == day).cloned().collect();
        // Changes that can be put back come first; the ones that can't go below them.
        records.sort_by_key(|r| r.backup.is_none());
        let mut undo = None;
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(day.format("%A, %B %-d, %Y").to_string()).font(bold(15.0)));
                if !records.is_empty() {
                    pill(ui, &format!("{} change{}", records.len(), plural(records.len())), BLUE);
                }
            });
            if records.is_empty() {
                ui.add_space(4.0);
                ui.label(small("Nothing was changed on this day."));
            }
            for record in &records {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    // Room is kept on the right for the button or note, so a long name is cut short
                    // instead of running underneath it.
                    let right_side = if record.backup.is_some() {
                        110.0
                    } else {
                        let note = ui.painter().layout_no_wrap(
                            NOT_RESTORABLE.to_string(),
                            FontId::proportional(12.0),
                            pal().soft,
                        );
                        note.size().x + 28.0
                    };
                    beside(ui, right_side, |ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new(record.device.clone()).size(13.5));
                        let tag = if record.automatic { "automatic, " } else { "" };
                        let from = if record.from_version.is_empty() { "none" } else { &record.from_version };
                        let time = chrono::DateTime::from_timestamp(record.when, 0)
                            .map(|t| t.with_timezone(&chrono::Local).format("%-I:%M %p").to_string())
                            .unwrap_or_default();
                        ui.label(small(format!(
                            "{from}  →  {}   ({tag}{})   {time}",
                            record.to_version, record.source
                        )));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(path) = &record.backup {
                            if secondary(ui, "Put Back", self.busy == Busy::Idle) {
                                undo = Some((PathBuf::from(path), record.instance_id.clone()));
                            }
                        } else {
                            ui.label(small(NOT_RESTORABLE));
                        }
                    });
                });
            }
        });
        ui.add_space(10.0);
        if let Some((path, id)) = undo {
            match backup::read_info(&path) {
                Ok(info) => {
                    let entries = info.entries.into_iter().filter(|e| e.instance_id == id).collect();
                    self.ask = Some(Ask::Restore(path, entries));
                }
                Err(_) => {
                    self.message = Some(("That backup file is gone, so this can't be put back.".into(), true));
                }
            }
        }
    }

    fn backup_list(&mut self, ui: &mut egui::Ui) {
        let title = ui.label(RichText::new("Saved backups").font(bold(16.0)));
        if std::mem::take(&mut self.jump_to_backups) {
            title.scroll_to_me(Some(Align::TOP));
        }
        ui.add_space(2.0);
        if self.backups.is_empty() {
            soft(ui, "None yet. Back up now, or update a driver and one is saved for you.");
            return;
        }
        let backups = self.backups.clone();
        let mut open_it = None;
        let mut start = None;
        let mut remove = None;
        let open_path = self.open_backup.clone();
        for file in &backups {
            let is_open = open_path.as_ref() == Some(&file.path);
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    beside(ui, 190.0, |ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(file.info.created.clone()).size(14.0));
                            if file.info.automatic {
                                pill(ui, "Automatic", BLUE);
                            }
                        });
                        let n = file.info.entries.len();
                        ui.label(small(format!(
                            "{}  ·  {n} driver{}  ·  {}",
                            file.info.reason,
                            plural(n),
                            size_text(file.size)
                        )));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if secondary(ui, if is_open { "Close" } else { "Open" }, true) {
                            open_it = Some(if is_open { None } else { Some(file.path.clone()) });
                        }
                        if secondary(ui, "Delete", self.busy == Busy::Idle) {
                            remove = Some(file.path.clone());
                        }
                    });
                });
                if is_open {
                    ui.add_space(4.0);
                    for entry in &file.info.entries {
                        let key = &entry.instance_id;
                        let mut on = self.restore_picks.contains(key);
                        let current = self.devices.iter().find(|d| d.instance_id.eq_ignore_ascii_case(key));
                        let state = match current {
                            Some(d) if d.present => d
                                .driver
                                .as_ref()
                                .map(|x| format!("now {}", x.version))
                                .unwrap_or_else(|| "no driver now".into()),
                            Some(_) => "not plugged in".into(),
                            None => "not on this computer".into(),
                        };
                        let text = format!("{}   {}   ({state})", entry.device_name, entry.version);
                        if ui.checkbox(&mut on, text).changed() {
                            if on {
                                self.restore_picks.insert(key.clone());
                            } else {
                                self.restore_picks.remove(key);
                            }
                        }
                    }
                    let chosen: Vec<BackupEntry> = file
                        .info
                        .entries
                        .iter()
                        .filter(|e| self.restore_picks.contains(&e.instance_id))
                        .cloned()
                        .collect();
                    ui.add_space(4.0);
                    let label = format!("Restore {} Driver{}", chosen.len(), plural(chosen.len()));
                    if primary(ui, &label, !chosen.is_empty() && self.busy == Busy::Idle) {
                        start = Some((file.path.clone(), chosen));
                    }
                }
            });
            ui.add_space(6.0);
        }
        if let Some(choice) = open_it {
            if let Some(path) = &choice {
                self.restore_picks = backups
                    .iter()
                    .find(|b| &b.path == path)
                    .map(|b| b.info.entries.iter().map(|e| e.instance_id.clone()).collect())
                    .unwrap_or_default();
            }
            self.open_backup = choice;
            self.results.clear();
        }
        if let Some((path, entries)) = start {
            self.ask = Some(Ask::Restore(path, entries));
        }
        if let Some(path) = remove {
            self.ask = Some(Ask::Delete(path));
        }
    }

    fn open_file(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("Driver backup", &[backup::FILE_EXTENSION]).pick_file()
        else {
            return;
        };
        match backup::read_info(&path) {
            Ok(info) => {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                self.restore_picks = info.entries.iter().map(|e| e.instance_id.clone()).collect();
                self.open_backup = Some(path.clone());
                self.backups.retain(|b| b.path != path);
                self.backups.insert(0, BackupFile { path, size, info });
            }
            Err(error) => self.message = Some((error, true)),
        }
    }
}

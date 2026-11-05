//! Everything the original spreads across five Hardware pages, on one page, with the full
//! device list underneath.

use super::look::*;
use super::{App, Ask, Busy, Show};
use crate::devices::{self, Device};
use crate::sysinfo;
use egui::{Align, Align2, FontId, Layout, RichText, Sense, Vec2};
use std::collections::BTreeMap;

impl App {
    pub(super) fn system_page(&mut self, ui: &mut egui::Ui) {
        let (now, past, needs) = self.counts();
        ui.horizontal(|ui| {
            let name = self.system.as_ref().map(|s| s.computer.clone()).unwrap_or_default();
            let title = if name.is_empty() { "System".to_string() } else { name };
            ui.label(RichText::new(title).font(bold(20.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if secondary(ui, "Save as…", !self.devices.is_empty()) {
                    self.save_list();
                }
            });
        });
        ui.add_space(6.0);

        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let sections = self.system.as_ref().map(|s| s.sections.clone()).unwrap_or_default();
            let sections: Vec<_> = sections.into_iter().filter(|s| !s.facts.is_empty()).collect();
            if sections.is_empty() {
                soft(ui, "Reading your system…");
            }
            // Two balanced columns of cards.
            let mut sides: [Vec<&sysinfo::Section>; 2] = [Vec::new(), Vec::new()];
            let mut heights = [0usize; 2];
            for section in &sections {
                let side = (heights[1] < heights[0]) as usize;
                heights[side] += section.facts.len() + 2;
                sides[side].push(section);
            }
            ui.columns(2, |columns| {
                for (ui, side) in columns.iter_mut().zip(sides) {
                    for section in side {
                        card().show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(icon(&section.icon, 15.0, TEAL));
                                ui.label(RichText::new(section.title.clone()).font(bold(14.0)));
                            });
                            ui.add_space(2.0);
                            let room = (ui.available_width() - 112.0).max(100.0);
                            for (label, value) in &section.facts {
                                ui.horizontal_top(|ui| {
                                    ui.add_sized([100.0, 16.0], egui::Label::new(small(label.clone())).truncate());
                                    ui.scope(|ui| {
                                        ui.set_max_width(room);
                                        ui.add(egui::Label::new(RichText::new(value.clone()).size(12.5)).wrap());
                                    });
                                });
                            }
                        });
                        ui.add_space(6.0);
                    }
                }
            });

            ui.add_space(8.0);
            heading(ui, "All hardware");
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Search").desired_width(180.0));
                for (show, label) in [
                    (Show::All, format!("All {}", now + past)),
                    (Show::Connected, format!("Connected {now}")),
                    (Show::Past, format!("Used to be connected {past}")),
                    (Show::NeedsDriver, format!("Missing driver {needs}")),
                ] {
                    if ui.selectable_label(self.show == show, label).clicked() {
                        self.show = show;
                    }
                }
                ui.checkbox(&mut self.show_system_items, "Windows system items");
            });
            ui.add_space(6.0);
            self.device_groups(ui);
        });
    }

    fn device_groups(&mut self, ui: &mut egui::Ui) {
        let needle = self.filter.to_lowercase();
        let picked = self.picked_device.clone();
        let filtering = !needle.is_empty() || self.show != Show::All;
        let mut groups: BTreeMap<String, Vec<Device>> = BTreeMap::new();
        for d in &self.devices {
            if d.system_item && !self.show_system_items {
                continue;
            }
            let keep = match self.show {
                Show::All => true,
                Show::Connected => d.present,
                Show::Past => !d.present,
                Show::NeedsDriver => d.needs_driver(),
            };
            let matches = needle.is_empty()
                || d.name.to_lowercase().contains(&needle)
                || d.manufacturer.to_lowercase().contains(&needle)
                || d.class_title.to_lowercase().contains(&needle);
            if keep && matches {
                groups.entry(d.class_title.clone()).or_default().push(d.clone());
            }
        }
        if groups.is_empty() {
            soft(ui, if self.devices.is_empty() { "Reading your hardware…" } else { "Nothing matches." });
        }
        let p = pal();
        let mut clicked = None;
        for (group, list) in &groups {
            egui::CollapsingHeader::new(RichText::new(format!("{group}  ({})", list.len())).size(13.5))
                .id_salt(group)
                .default_open(filtering)
                .show(ui, |ui| {
                    for d in list {
                        let chosen = picked.as_deref() == Some(d.instance_id.as_str());
                        let (rect, response) =
                            ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::click());
                        if chosen {
                            ui.painter().rect_filled(rect, 5.0, p.chosen);
                        } else if response.hovered() {
                            ui.painter().rect_filled(rect, 5.0, p.hover);
                        }
                        let dot = if d.needs_driver() || d.has_problem() {
                            CRITICAL
                        } else if d.present {
                            SUCCESS
                        } else {
                            p.faint
                        };
                        ui.painter().circle_filled(rect.left_center() + Vec2::new(10.0, 0.0), 3.5, dot);
                        let right = if d.present {
                            d.driver.as_ref().map(|x| x.version.clone()).unwrap_or_default()
                        } else {
                            d.last_connected
                                .map(|t| format!("Last seen {}", devices::short_date(t)))
                                .unwrap_or_else(|| "Not connected".into())
                        };
                        let right_rect = ui.painter().text(
                            rect.right_center() - Vec2::new(8.0, 0.0),
                            Align2::RIGHT_CENTER,
                            right,
                            FontId::proportional(12.0),
                            p.soft,
                        );
                        let color = if d.present { p.text } else { p.soft };
                        let room = (rect.width() - right_rect.width() - 44.0).max(40.0);
                        let name = ui.painter().layout(d.name.clone(), FontId::proportional(13.5), color, room);
                        let line = name.rows.first().map(|r| r.text().to_string()).unwrap_or_default();
                        ui.painter().text(
                            rect.left_center() + Vec2::new(22.0, 0.0),
                            Align2::LEFT_CENTER,
                            line,
                            FontId::proportional(13.5),
                            color,
                        );
                        if response.clicked() {
                            clicked = Some(d.instance_id.clone());
                        }
                        if chosen {
                            self.device_details(ui, d);
                        }
                    }
                });
        }
        if let Some(id) = clicked {
            self.picked_device = if picked.as_deref() == Some(id.as_str()) { None } else { Some(id) };
        }
    }

    fn device_details(&mut self, ui: &mut egui::Ui, device: &Device) {
        card().fill(pal().field).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                if device.present {
                    pill(ui, "Connected", SUCCESS);
                } else {
                    pill(ui, "Not connected", pal().soft);
                }
                if device.needs_driver() {
                    pill(ui, "No driver", CRITICAL);
                } else if device.has_problem() {
                    pill(ui, &format!("Problem code {}", device.problem_code), CRITICAL);
                }
            });
            let mut fact = |label: &str, value: String| {
                if !value.is_empty() {
                    ui.add(egui::Label::new(small(format!("{label}:  {value}"))).wrap());
                }
            };
            fact("Maker", device.manufacturer.clone());
            fact("Where", device.location.clone());
            fact("First installed", device.first_installed.map(devices::short_date).unwrap_or_default());
            fact("Last connected", device.last_connected.map(devices::short_date).unwrap_or_default());
            fact("Last removed", device.last_removed.map(devices::short_date).unwrap_or_default());
            if let Some(driver) = &device.driver {
                fact("Driver", format!("{} ({}) from {}", driver.version, driver.date, driver.provider));
                fact("Driver file", driver.inf_name.clone());
            }
            fact("Device ID", device.instance_id.clone());
            fact("Hardware ID", device.hardware_ids.first().cloned().unwrap_or_default());
            if device.driver.is_some()
                && device.present
                && devices::can_roll_back(&device.instance_id)
                && secondary(ui, "Roll Back Driver", self.busy == Busy::Idle)
            {
                self.ask = Some(Ask::Rollback(device.instance_id.clone(), device.name.clone()));
            }
        });
        ui.add_space(4.0);
    }

    fn save_list(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("my-computer.txt")
            .add_filter("Text file", &["txt"])
            .save_file()
        else {
            return;
        };
        let mut lines = Vec::new();
        if let Some(info) = &self.system {
            for section in &info.sections {
                lines.push(section.title.to_uppercase());
                lines.extend(section.facts.iter().map(|(k, v)| format!("  {k}: {v}")));
                lines.push(String::new());
            }
        }
        lines.push("ALL HARDWARE".into());
        for d in self.devices.iter().filter(|d| !d.system_item || self.show_system_items) {
            let driver = d.driver.clone().unwrap_or_default();
            lines.push(format!(
                "  [{}] {} / {} / driver {} {}",
                if d.present { "connected" } else { "not connected" },
                d.class_title,
                d.name,
                driver.version,
                driver.date
            ));
        }
        self.message = Some(match std::fs::write(&path, lines.join("\r\n")) {
            Ok(()) => (format!("Saved to {}.", path.display()), false),
            Err(error) => (format!("Couldn't save it: {error}"), true),
        });
    }
}

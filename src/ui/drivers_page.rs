//! Scanning and all three kinds of update on one page: drivers, Windows and software.

use super::look::*;
use super::{App, Ask, Busy, Page, Row, Show};
use crate::devices::{Device, OTHER_DEVICES_CLASS};
use crate::wu::{ItemState, Kind, Source};
use egui::{Align, Align2, FontId, Frame, Layout, Margin, RichText, Sense, Stroke, Vec2};
use std::collections::HashSet;

const GROUPS: [(Kind, &str, &str); 3] = [
    (Kind::Driver, "Drivers", "Update Drivers"),
    (Kind::Windows, "Windows updates", "Update Windows"),
    (Kind::Software, "Software", "Update Software"),
];

/// What a click on a page asks for, handled once drawing has finished.
#[derive(Default)]
struct Clicks {
    install: Option<Vec<String>>,
    no_backup: Option<String>,
    ignore: Option<String>,
    fold: Option<Kind>,
    all: Option<(Kind, bool)>,
}

/// A newer DriverKeep itself, shown in its own section rather than with the software.
fn is_app(row: &Row) -> bool {
    row.update.source == Source::GitHub
}

fn can_install(row: &Row) -> bool {
    matches!(row.state, ItemState::Waiting | ItemState::Failed(_) | ItemState::Skipped(_))
}

fn row_name(row: &Row, list: &[&Device]) -> String {
    if let Some(device) = list.first() {
        return device.name.clone();
    }
    if row.update.model.is_empty() { row.update.title.clone() } else { row.update.model.clone() }
}

fn row_detail(row: &Row, list: &[&Device]) -> String {
    let update = &row.update;
    let size = if update.size > 0 { format!("   {}", size_text(update.size)) } else { String::new() };
    let target = if update.version.is_empty() { update.date.clone() } else { update.version.clone() };
    match update.kind {
        Kind::Driver => {
            let current = list
                .iter()
                .find_map(|d| d.driver.as_ref())
                .map(|d| d.version.clone())
                .unwrap_or_else(|| "none".into());
            format!("{current}  →  {target}   {}{size}", update.source.label())
        }
        Kind::Windows => format!("{}{size}", update.source.label()),
        Kind::Software => format!("{}  →  {target}   {}", update.current, update.source.label()),
    }
}

fn state_pill(row: &Row) -> Option<(String, egui::Color32)> {
    let with_percent = |word: &str| match row.percent {
        Some(p) => format!("{word} {p}%"),
        None => word.to_string(),
    };
    Some(match &row.state {
        ItemState::Waiting => return None,
        ItemState::SavingBackup => ("Saving backup".into(), SKY),
        ItemState::Downloading => (with_percent("Downloading"), SKY),
        ItemState::Installing => (with_percent("Installing"), SKY),
        ItemState::Installed => ("Installed".into(), SUCCESS),
        ItemState::NeedsRestart => ("Restart to finish".into(), CAUTION),
        ItemState::Skipped(_) => ("Skipped".into(), CAUTION),
        ItemState::Failed(_) => ("Failed".into(), CRITICAL),
    })
}

fn draw_row(ui: &mut egui::Ui, row: &mut Row, devices: &[Device], installing: bool, clicks: &mut Clicks) {
    let list: Vec<&Device> =
        devices.iter().filter(|d| row.update.devices.contains(&d.instance_id)).collect();
    Frame::new().inner_margin(Margin::symmetric(8, 5)).show(ui, |ui| {
        let mut text_left = ui.cursor().left();
        ui.horizontal(|ui| {
            ui.add_enabled(!installing, egui::Checkbox::without_text(&mut row.selected));
            text_left = ui.cursor().left();
            // The status pill ("Downloading 100%") plus the ··· menu is the widest thing on the right.
            beside(ui, if installing { 360.0 } else { 220.0 }, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(row_name(row, &list)).size(14.0));
                    if list.len() > 1 {
                        pill(ui, &format!("{} devices", list.len()), BLUE);
                    }
                    if list.first().is_some_and(|d| d.needs_driver()) {
                        pill(ui, "No driver yet", CRITICAL);
                    }
                });
                ui.label(small(row_detail(row, &list)));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if installing {
                    // While a job runs the menu only had this one choice, so it's a button instead.
                    // An update that's already installing can't be taken back, so it isn't offered.
                    let early = matches!(row.state, ItemState::Waiting | ItemState::SavingBackup | ItemState::Downloading);
                    if early && pill_button(ui, "Ignore This Update", CAUTION) {
                        clicks.ignore = Some(row.update.id.clone());
                    }
                } else {
                    ui.menu_button("···", |ui| {
                        let retry = matches!(row.state, ItemState::Failed(_) | ItemState::Skipped(_));
                        if retry && ui.button("Try again").clicked() {
                            clicks.install = Some(vec![row.update.id.clone()]);
                            ui.close();
                        }
                        let driver = row.update.kind == Kind::Driver && can_install(row);
                        if driver && ui.button("Install without a backup").clicked() {
                            clicks.no_backup = Some(row.update.id.clone());
                            ui.close();
                        }
                        if ui.button("Ignore this update").clicked() {
                            clicks.ignore = Some(row.update.id.clone());
                            ui.close();
                        }
                    });
                }
                match state_pill(row) {
                    Some((text, color)) => pill(ui, &text, color),
                    // While a job runs the row only offers Ignore; "Update" would just sit greyed out.
                    None if installing => {}
                    None => {
                        if secondary(ui, "Update", true) {
                            clicks.install = Some(vec![row.update.id.clone()]);
                        }
                    }
                }
            });
        });
        if let (ItemState::Downloading | ItemState::Installing, Some(percent)) = (&row.state, row.percent) {
            ui.add_space(2.0);
            progress_bar(ui, percent);
        }
        if let ItemState::Failed(why) | ItemState::Skipped(why) = &row.state {
            // Lined up under the update's name, not under the checkbox.
            ui.horizontal(|ui| {
                ui.add_space((text_left - ui.cursor().left()).max(0.0));
                ui.add(egui::Label::new(RichText::new(why.clone()).size(12.0).color(CAUTION)).wrap());
            });
        }
    });
}

impl App {
    /// The big dial shown while scanning and on the empty page.
    fn dial(&mut self, ui: &mut egui::Ui, scanning: bool) -> bool {
        let p = pal();
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(216.0), Sense::click());
        let painter = ui.painter();
        let center = rect.center();
        let radius = 100.0;
        let angle = |t: f32| -std::f32::consts::FRAC_PI_2 + t * std::f32::consts::TAU;
        let at = |t: f32, r: f32| center + Vec2::angled(angle(t)) * r;
        for step in 0..72 {
            let t = step as f32 / 72.0;
            let lit = scanning && (t - self.spin).rem_euclid(1.0) < 0.07;
            painter.line_segment(
                [at(t, radius - 22.0), at(t, radius - 12.0)],
                Stroke::new(2.0, if lit { SKY } else { p.line }),
            );
        }
        let filled = if scanning { self.percent as f32 / 100.0 } else { 1.0 };
        let track = 120;
        for step in 0..track {
            let a = step as f32 / track as f32;
            let b = (step + 1) as f32 / track as f32;
            let color = if a < filled { lerp_color(BLUE, teal(), a) } else { p.line };
            painter.line_segment([at(a, radius), at(b, radius)], Stroke::new(7.0, color));
        }
        if scanning {
            painter.text(
                center - Vec2::new(0.0, 12.0),
                Align2::CENTER_CENTER,
                format!("{}%", self.percent),
                bold(38.0),
                p.text,
            );
            painter.text(
                center + Vec2::new(0.0, 26.0),
                Align2::CENTER_CENTER,
                "STOP",
                FontId::proportional(14.0),
                if response.hovered() { p.text } else { p.soft },
            );
        } else {
            painter.text(center - Vec2::new(0.0, 10.0), Align2::CENTER_CENTER, "Scan", bold(30.0), p.text);
            painter.text(
                center + Vec2::new(0.0, 22.0),
                Align2::CENTER_CENTER,
                "Now",
                FontId::proportional(16.0),
                p.soft,
            );
        }
        response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
    }

    /// The section at the very top when a newer DriverKeep exists. Its colour pulses gently.
    fn app_update_banner(&mut self, ui: &mut egui::Ui, clicks: &mut Clicks) {
        let Some(row) = self.rows.iter().find(|r| is_app(r)) else { return };
        let busy = self.busy != Busy::Idle;
        let pulse = 0.5 + 0.5 * (ui.input(|i| i.time) as f32 * 3.0).sin();
        let waiting = can_install(row);
        let text = format!("DriverKeep {} is available (you have {}).", row.update.version, row.update.current);
        let id = row.update.id.clone();
        let pill_state = state_pill(row);
        card().fill(teal().gamma_multiply(0.14 + 0.16 * pulse)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("DriverKeep update").font(bold(14.0)));
            ui.horizontal(|ui| {
                beside(ui, 200.0, |ui| {
                    ui.label(text);
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| match pill_state {
                    Some((label, color)) if !waiting => pill(ui, &label, color),
                    _ => {
                        if primary(ui, "Update DriverKeep", !busy) {
                            clicks.install = Some(vec![id.clone()]);
                        }
                    }
                });
            });
            if let ItemState::Failed(why) | ItemState::Skipped(why) = &row.state {
                ui.label(RichText::new(why.clone()).size(12.0).color(CAUTION));
            }
        });
        ui.add_space(8.0);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
    }

    fn group_shown(&self, kind: Kind) -> bool {
        match kind {
            Kind::Driver => self.settings.scan_drivers,
            Kind::Windows => self.settings.scan_windows,
            Kind::Software => self.settings.scan_software,
        }
    }

    fn group_card(
        &mut self,
        ui: &mut egui::Ui,
        (kind, title, button): (Kind, &str, &str),
        devices: &[Device],
        clicks: &mut Clicks,
    ) {
        let installing = self.busy == Busy::Installing;
        let idle = self.busy == Busy::Idle;
        let count = self.rows.iter().filter(|r| r.update.kind == kind && !is_app(r)).count();
        let open: Vec<String> = self
            .rows
            .iter()
            .filter(|r| r.update.kind == kind && !is_app(r) && r.selected && can_install(r))
            .map(|r| r.update.id.clone())
            .collect();
        let selected = self.rows.iter().filter(|r| r.update.kind == kind && !is_app(r) && r.selected).count();
        let folded = self.collapsed.contains(&kind);
        card().inner_margin(Margin::symmetric(6, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            Frame::new().inner_margin(Margin::symmetric(8, 3)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if count > 0 {
                        let mut all = selected == count;
                        let check = egui::Checkbox::without_text(&mut all).indeterminate(selected > 0 && selected < count);
                        if ui.add_enabled(!installing, check).changed() {
                            clicks.all = Some((kind, all));
                        }
                    }
                    ui.label(RichText::new(format!("{title} ({count})")).font(bold(14.0)));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if count == 0 {
                            ui.label(icon(ICON_CHECK, 11.0, SUCCESS));
                            ui.label(small("Up to date"));
                            return;
                        }
                        if ui.small_button(if folded { "Show" } else { "Hide" }).clicked() {
                            clicks.fold = Some(kind);
                        }
                        // Hidden during a job, since that group is already being updated.
                        if idle && secondary(ui, button, !open.is_empty()) {
                            clicks.install = Some(open.clone());
                        }
                    });
                });
            });
            if !folded {
                for row in self.rows.iter_mut().filter(|r| r.update.kind == kind && !is_app(r)) {
                    draw_row(ui, row, devices, installing, clicks);
                }
            }
        });
        ui.add_space(8.0);
    }

    pub(super) fn drivers_page(&mut self, ui: &mut egui::Ui) {
        let waiting = self.open_rows();
        if self.busy == Busy::Scanning {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() - 300.0).max(0.0) / 2.0);
                self.spin = (self.spin + 0.008).fract();
                if self.dial(ui, true) {
                    self.stop_scan();
                }
                ui.add_space(14.0);
                ui.label(RichText::new("Scanning…").font(bold(17.0)));
                soft(ui, self.status.clone());
            });
            ui.ctx().request_repaint();
            return;
        }
        if self.rows.is_empty() {
            self.empty_drivers(ui);
            return;
        }

        let installing = self.busy == Busy::Installing;
        let open: Vec<String> = self
            .rows
            .iter()
            .filter(|r| r.selected && can_install(r))
            .map(|r| r.update.id.clone())
            .collect();

        egui::Panel::bottom("update_all")
            .show_separator_line(false)
            .frame(Frame::new().inner_margin(Margin { top: 10, ..Default::default() }))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if self.windows_update_pending {
                        ui.add(egui::Spinner::new().size(12.0).color(teal()));
                        ui.label(small("Still checking Windows Update"));
                    } else {
                        let last = self.settings.last_scan.map(ago).unwrap_or_default();
                        ui.label(small(format!("Last scan: {last}")));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if installing {
                            // Replaces both buttons until the job has stopped or finished.
                            let text = if self.cancelling { "Cancelling…" } else { "Cancel Updates" };
                            if cancel_button(ui, text, !self.cancelling) {
                                self.cancel_updates();
                            }
                            return;
                        }
                        // After a job only Scan shows; the update button returns once a scan has finished.
                        if self.updates_ready {
                            let label = if open.len() == waiting { "Update Everything" } else { "Update Selected" };
                            if primary(ui, label, !open.is_empty() && self.busy == Busy::Idle) {
                                self.ask = Some(Ask::Update(open.clone()));
                            }
                            ui.add_space(4.0);
                        }
                        if scan_again_button(ui, self.busy == Busy::Idle) {
                            self.start_scan();
                        }
                    });
                });
            });

        ui.horizontal(|ui| {
            ui.label(RichText::new("Updates:").font(bold(20.0)));
            ui.label(RichText::new(format!("{waiting} available")).font(bold(20.0)).color(teal()));
        });
        ui.add_space(6.0);
        if installing {
            self.working_card(ui);
        }
        for note in self.notes.clone() {
            ui.label(small(note));
        }

        // Taken out while drawing so a long device list isn't copied every frame.
        let devices = std::mem::take(&mut self.devices);
        let mut clicks = Clicks::default();
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            self.app_update_banner(ui, &mut clicks);
            for group in GROUPS {
                if self.group_shown(group.0) {
                    self.group_card(ui, group, &devices, &mut clicks);
                }
            }
            if self.settings.scan_drivers {
                self.up_to_date(ui, &devices);
            }
        });
        self.devices = devices;

        if let Some(ids) = clicks.install {
            self.ask = Some(Ask::Update(ids));
        }
        if let Some(id) = clicks.no_backup {
            self.ask = Some(Ask::UpdateNoBackup(id));
        }
        if let Some(kind) = clicks.fold {
            if !self.collapsed.remove(&kind) {
                self.collapsed.insert(kind);
            }
        }
        if let Some((kind, on)) = clicks.all {
            for row in self.rows.iter_mut().filter(|r| r.update.kind == kind && !is_app(r)) {
                row.selected = on;
            }
        }
        if let Some(id) = clicks.ignore {
            self.ignore_update(id);
        }
    }

    fn up_to_date(&mut self, ui: &mut egui::Ui, devices: &[Device]) {
        let updating: HashSet<&String> = self.rows.iter().flat_map(|r| r.update.devices.iter()).collect();
        // The original's rule: everything except "Other devices" with no hardware id.
        let mut fine: Vec<&Device> = devices
            .iter()
            .filter(|d| d.present && !d.system_item && !updating.contains(&d.instance_id))
            .filter(|d| !d.class_guid.eq_ignore_ascii_case(OTHER_DEVICES_CLASS) || !d.hardware_ids.is_empty())
            .collect();
        fine.sort_by_key(|d| d.name.to_lowercase());
        card().inner_margin(Margin::symmetric(14, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let header = ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Drivers up to date ({})", fine.len())).size(14.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(small(if self.show_uptodate { "Hide" } else { "Show" }));
                });
            });
            if header.response.interact(Sense::click()).clicked() {
                self.show_uptodate = !self.show_uptodate;
            }
            if self.show_uptodate {
                ui.add_space(4.0);
                for d in fine {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(d.name.clone()).size(13.0));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(icon(ICON_CHECK, 11.0, SUCCESS));
                            ui.label(small(d.driver.as_ref().map(|x| x.version.clone()).unwrap_or_default()));
                        });
                    });
                }
            }
        });
    }

    fn empty_drivers(&mut self, ui: &mut egui::Ui) {
        let (now, past, needs) = self.counts();
        ui.vertical_centered(|ui| {
            ui.add_space(((ui.available_height() - 440.0) / 2.0).max(4.0));
            let (line, detail) = if !self.notes.is_empty() {
                ("The check didn't fully finish".to_string(), self.notes.join(" "))
            } else if self.searched {
                ("Your computer is up to date".to_string(), "Nothing newer was found.".to_string())
            } else {
                (
                    "Check your computer".to_string(),
                    "One click checks your drivers, Windows and installed software.".to_string(),
                )
            };
            ui.label(RichText::new(line).font(bold(22.0)));
            soft(ui, detail);
            if let Some(when) = self.settings.last_scan {
                ui.label(small(format!("Last scan: {}", ago(when))));
            }
            ui.add_space(12.0);
            if self.dial(ui, false) && self.busy == Busy::Idle {
                self.start_scan();
            }
            ui.add_space(16.0);
            let width = ui.available_width().min(560.0);
            ui.allocate_ui_with_layout(Vec2::new(width, 90.0), Layout::left_to_right(Align::Min), |ui| {
                let needs_color = if needs > 0 { CRITICAL } else { pal().text };
                let tiles = [
                    ("Connected now", now.to_string(), SUCCESS, Show::Connected),
                    ("Used to be connected", past.to_string(), pal().text, Show::Past),
                    ("Missing a driver", needs.to_string(), needs_color, Show::NeedsDriver),
                ];
                ui.columns(3, |columns| {
                    for (ui, (label, value, color, show)) in columns.iter_mut().zip(tiles) {
                        let response = card()
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.vertical_centered(|ui| {
                                    ui.label(RichText::new(value).font(bold(22.0)).color(color));
                                    ui.label(small(label));
                                });
                            })
                            .response
                            .interact(Sense::click());
                        if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            self.show = show;
                            self.page = Page::System;
                        }
                    }
                });
            });
        });
    }
}

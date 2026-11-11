//! System Restore on its own page: how many restore points there are, how much of the drive they may
//! use, and making a new one. It's kept apart from the driver backups, which are a different thing.

use super::look::*;
use super::{App, RestoreJob};
use crate::restore::{self, Action, Info};
use crate::system;
use egui::{Align, Layout, RichText, Vec2};

const GB: u64 = 1024 * 1024 * 1024;

impl App {
    /// Starts something that needs administrator rights. Windows asks for permission first, unless
    /// DriverKeep is already running as an administrator.
    fn ask_restore(&mut self, action: Action, what: &'static str) {
        if self.restore_job.is_some() {
            return;
        }
        match restore::start(action) {
            Some(id) => {
                self.restore_job = Some(RestoreJob { id, started: std::time::Instant::now(), what });
                self.message = None;
            }
            None => {
                self.message = Some(("Windows didn't give permission, so nothing was changed.".into(), true));
            }
        }
    }

    /// Picks up the answer from the administrator copy.
    pub(super) fn poll_restore(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.restore_job else { return };
        ctx.request_repaint_after(std::time::Duration::from_millis(400));
        if let Some(outcome) = restore::outcome(job.id) {
            self.restore_job = None;
            if let Some(info) = outcome.info {
                self.restore_gb = if info.limit > 0 { info.limit_gb().to_string() } else { String::new() };
                self.restore = Some(info);
            }
            if !outcome.message.is_empty() {
                self.message = Some((outcome.message, !outcome.ok));
            }
        } else if job.started.elapsed().as_secs() > 120 {
            self.restore_job = None;
            self.message = Some(("No answer came back from the administrator copy. Try again.".into(), true));
        }
    }

    pub(super) fn restore_page(&mut self, ui: &mut egui::Ui) {
        // Running as an administrator, there's nothing to ask, so look straight away.
        if self.restore.is_none() && self.restore_job.is_none() && !self.restore_tried && self.admin {
            self.restore_tried = true;
            self.ask_restore(Action::Read, "Checking restore points");
        }
        let busy = self.restore_job.is_some();
        let points = self.restore.as_ref().map(|i| i.points.len());
        ui.horizontal(|ui| {
            ui.label(RichText::new("System Restore:").font(bold(20.0)));
            let text = match points {
                Some(n) => format!("{n} restore point{}", plural(n)),
                None => "not checked yet".to_string(),
            };
            ui.label(RichText::new(text).font(bold(20.0)).color(teal()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let ready = self.restore.as_ref().is_some_and(|i| i.on) && !busy;
                if primary_compact(ui, "Create Restore Point", ready) {
                    self.ask_restore(Action::Create, "Making a restore point");
                }
                if secondary(ui, "Open Windows System Restore", true) {
                    system::open_windows_restore();
                }
                if secondary(ui, if busy { "Working…" } else { "Refresh" }, !busy) {
                    self.ask_restore(Action::Read, "Checking restore points");
                }
            });
        });
        ui.add_space(6.0);

        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if let Some(job) = &self.restore_job {
                card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(15.0).color(teal()));
                        ui.label(job.what);
                    });
                    ui.add_space(2.0);
                    ui.label(small("Windows may ask for permission first. DriverKeep stays open."));
                });
                ui.add_space(8.0);
            }
            match self.restore.clone() {
                None => {
                    if !busy {
                        card().show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new("Check your restore points").font(bold(15.0)));
                            ui.add_space(2.0);
                            ui.label(small(
                                "Windows only lets an administrator see restore points and change how much space they \
                                 use. Press the button and Windows will ask for permission once. DriverKeep stays open.",
                            ));
                            ui.add_space(8.0);
                            if primary(ui, "Check Restore Points", true) {
                                self.ask_restore(Action::Read, "Checking restore points");
                            }
                        });
                    }
                }
                Some(info) => self.restore_details(ui, &info, busy),
            }
        });
    }

    fn restore_details(&mut self, ui: &mut egui::Ui, info: &Info, busy: bool) {
        if !info.on {
            card().fill(CAUTION.gamma_multiply(0.16)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    beside_centered(ui, 130.0, |ui| {
                        ui.label(format!("System Restore is turned off for {}, so no restore points are being made.", info.drive));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if secondary(ui, "Turn On", !busy) {
                            self.ask_restore(Action::TurnOn, "Turning System Restore on");
                        }
                    });
                });
            });
            ui.add_space(8.0);
        }

        group_title(ui, "Storage");
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            if info.limit > 0 {
                let percent = (info.used.saturating_mul(100) / info.limit).min(100) as u32;
                ui.label(format!(
                    "Restore points use {} of the {} System Restore may use on {}.",
                    size_text(info.used),
                    size_text(info.limit),
                    info.drive
                ));
                ui.add_space(4.0);
                progress_bar(ui, percent);
            } else {
                ui.label(format!(
                    "Restore points use {}. No limit is set, so System Restore can use as much of {} as it needs.",
                    size_text(info.used),
                    info.drive
                ));
            }
            ui.add_space(2.0);
            ui.label(small(format!("{} is {} in all.", info.drive, size_text(info.drive_size))));
        });
        ui.add_space(4.0);

        let typed = self.restore_gb.parse::<u64>().unwrap_or(0);
        let most = info.drive_size / GB;
        let mut apply = false;
        let draft = &mut self.restore_gb;
        setting_with(
            ui,
            300.0,
            ICON_SHIELD,
            "Space System Restore can use",
            "Set in gigabytes. When it's full, Windows removes the oldest restore points.",
            |ui| {
                // Laid out right to left: Apply at the far end, then "GB", then the box.
                apply = secondary(ui, "Apply", !busy && typed >= 1 && typed <= most && typed != info.limit_gb());
                ui.label(small("GB"));
                ui.add_sized(
                    [80.0, 32.0],
                    egui::TextEdit::singleline(draft)
                        .align(egui::Align2::CENTER_CENTER)
                        .margin(egui::Margin::symmetric(8, 7))
                        .hint_text("GB"),
                );
                // Whole numbers only.
                draft.retain(|c| c.is_ascii_digit());
                draft.truncate(6);
            },
        );
        if apply {
            self.ask_restore(Action::SetLimit(typed as u32), "Changing the space limit");
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Restore points").font(bold(16.0)));
            if !info.points.is_empty() {
                pill(ui, &info.points.len().to_string(), BLUE);
            }
        });
        ui.add_space(2.0);
        if info.points.is_empty() {
            soft(ui, "There are no restore points yet.");
            return;
        }
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            for point in &info.points {
                ui.add_space(6.0);
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(RichText::new(&point.description).size(13.5));
                let when = chrono::DateTime::from_timestamp(point.created, 0)
                    .map(|t| t.with_timezone(&chrono::Local).format("%B %-d, %Y  %-I:%M %p").to_string())
                    .unwrap_or_default();
                ui.label(small(format!("{}  ·  {when}", Info::kind_label(point.kind))));
            }
            ui.add_space(6.0);
        });
        ui.add_space(6.0);
        ui.label(small("To go back to one of these, press Open Windows System Restore."));
        ui.add_space(8.0);
        let _ = Vec2::ZERO;
    }

    /// Made-up restore points, for README screenshots only.
    pub(super) fn sample_restore() -> Info {
        let now = chrono::Local::now().timestamp();
        let day = 86_400;
        let point = |number, description: &str, kind, days_ago: i64| restore::Point {
            number,
            description: description.into(),
            kind,
            created: now - days_ago * day,
        };
        Info {
            drive: "C:".into(),
            drive_size: 476 * GB,
            used: 6 * GB,
            limit: 20 * GB,
            on: true,
            points: vec![
                point(5, "DriverKeep: Before installing driver updates", 10, 1),
                point(4, "Windows Update", 0, 3),
                point(3, "Installed Visual Studio Community", 0, 8),
                point(2, "DriverKeep: restore point made by hand", 10, 15),
                point(1, "Windows Modules Installer", 12, 30),
            ],
        }
    }
}

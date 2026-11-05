//! Every option on one page: theme, startup, schedules, silent installs and safety.

use super::look::*;
use super::{App, Ask};
use crate::system::{self, CloseChoice, Frequency, Settings, Theme};
use chrono::Datelike;
use egui::RichText;

const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

impl App {
    pub(super) fn settings_page(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Settings").font(bold(20.0)));
        let mut startup_changed = false;
        let mut changed = false;
        let mut area = egui::ScrollArea::vertical().auto_shrink(false);
        if self.options.page.as_deref() == Some("about") {
            area = area.stick_to_bottom(true);
        }
        area.show(ui, |ui| {
            let s = &mut self.settings;
            group_title(ui, "Appearance");
            setting(ui, ICON_THEME, "Theme", "Light, dark, or match Windows.", |ui| {
                for (theme, label) in [(Theme::System, "System"), (Theme::Light, "Light"), (Theme::Dark, "Dark")] {
                    changed |= ui.selectable_value(&mut s.theme, theme, label).changed();
                }
            });

            group_title(ui, "Startup");
            setting(ui, ICON_POWER, "Run when Windows starts", "Starts with your computer.", |ui| {
                startup_changed |= toggle(ui, &mut s.run_on_startup);
            });
            setting(ui, ICON_HIDE, "Launch in background", "Starts in the system tray with no window.", |ui| {
                startup_changed |= toggle(ui, &mut s.start_hidden);
            });
            setting(ui, ICON_TRAY, "When closing the window", "Tray keeps the background helper working.", |ui| {
                for (choice, label) in [
                    (CloseChoice::Tray, "Stay in tray"),
                    (CloseChoice::Exit, "Exit"),
                    (CloseChoice::Ask, "Ask every time"),
                ] {
                    startup_changed |= ui.selectable_value(&mut s.close_choice, choice, label).changed();
                }
            });

            group_title(ui, "Default scanning options");
            setting(ui, ICON_DRIVERS, "Check drivers", "Looks for newer drivers for your hardware.", |ui| {
                changed |= toggle(ui, &mut s.scan_drivers);
            });
            setting(ui, ICON_SYSTEM, "Check Windows updates", "Security and system updates from Microsoft.", |ui| {
                changed |= toggle(ui, &mut s.scan_windows);
            });
            setting(ui, ICON_BACKUP, "Check installed software", "Newer versions of your other programs.", |ui| {
                changed |= toggle(ui, &mut s.scan_software);
            });
            let (winget_ready, choco_ready) = self.tools.unwrap_or((true, true));
            let mut get_winget = false;
            let mut get_chocolatey = false;
            setting(ui, ICON_SETTINGS, "Windows Package Manager", "Finds updates for the programs already on your PC.", |ui| {
                if winget_ready {
                    changed |= toggle(ui, &mut s.use_winget);
                } else {
                    get_winget = secondary(ui, "Get It", true);
                }
            });
            setting(ui, ICON_SETTINGS, "Chocolatey", "Updates programs that Chocolatey installed.", |ui| {
                if choco_ready {
                    changed |= toggle(ui, &mut s.use_chocolatey);
                } else {
                    get_chocolatey = secondary(ui, "Install", winget_ready);
                }
            });
            if get_winget {
                system::open_link(crate::software::WINGET_STORE_LINK);
            }
            if get_chocolatey {
                self.ask = Some(Ask::InstallChocolatey);
            }
            let s = &mut self.settings;
            setting(ui, ICON_SCAN, "Include optional Windows updates", "Previews Windows doesn't install by itself.", |ui| {
                changed |= toggle(ui, &mut s.scan_optional);
            });
            let detail = "Runs in the tray helper, or the next time you open the app.";
            setting(ui, ICON_CLOCK, "Scan in the background", detail, |ui| {
                let text = match s.scan_frequency {
                    Frequency::Never => "Off",
                    Frequency::Daily => "Every day",
                    Frequency::Weekly => "Every week",
                    Frequency::Monthly => "Every month",
                };
                egui::ComboBox::from_id_salt("scan_frequency")
                    .selected_text(text)
                    .width(110.0)
                    .show_ui(ui, |ui| {
                        for (value, label) in [
                            (Frequency::Never, "Off"),
                            (Frequency::Daily, "Every day"),
                            (Frequency::Weekly, "Every week"),
                            (Frequency::Monthly, "Every month"),
                        ] {
                            if ui.selectable_value(&mut s.scan_frequency, value, label).changed() {
                                startup_changed = true;
                                if value == Frequency::Monthly {
                                    s.scan_month_day = system::monthly_day(chrono::Local::now().day());
                                }
                            }
                        }
                    });
                if s.scan_frequency == Frequency::Monthly {
                    ui.label(small(format!("Day {}", s.scan_month_day)));
                }
                if s.scan_frequency == Frequency::Weekly {
                    egui::ComboBox::from_id_salt("scan_day")
                        .selected_text(DAYS[s.scan_weekday.min(6) as usize])
                        .show_ui(ui, |ui| {
                            for (index, day) in DAYS.iter().enumerate() {
                                changed |= ui.selectable_value(&mut s.scan_weekday, index as u32, *day).changed();
                            }
                        });
                }
                if s.scan_frequency != Frequency::Never {
                    changed |= hour_picker(ui, "scan_hour", &mut s.scan_hour);
                }
            });
            setting(ui, ICON_SCAN, "Scan when the helper starts", "One background scan each time your PC starts.", |ui| {
                changed |= toggle(ui, &mut s.scan_at_startup);
            });
            setting(ui, ICON_BELL, "Notify me", "Shows a small tray message when updates are found.", |ui| {
                changed |= toggle(ui, &mut s.notify);
            });
            setting(ui, ICON_GLOBE, "Search the Microsoft Update Catalog", "Finds drivers Windows Update doesn't offer on its own.", |ui| {
                changed |= toggle(ui, &mut s.use_catalog);
            });

            group_title(ui, "Default installation options");
            ui.label(small("Which groups start ticked, ready for Update Everything."));
            ui.add_space(2.0);
            setting(ui, ICON_BELL, "Ask before updating drivers", "Shows a reminder before driver updates start.", |ui| {
                changed |= toggle(ui, &mut s.update_all_remind);
            });
            setting(ui, ICON_DRIVERS, "Include drivers", "Ticked after each scan.", |ui| {
                changed |= toggle(ui, &mut s.pick_drivers);
            });
            setting(ui, ICON_SYSTEM, "Include Windows updates", "Ticked after each scan.", |ui| {
                changed |= toggle(ui, &mut s.pick_windows);
            });
            setting(ui, ICON_BACKUP, "Include software", "Ticked after each scan.", |ui| {
                changed |= toggle(ui, &mut s.pick_software);
            });

            group_title(ui, "Silent installs");
            let mut silent = s.silent_install;
            let mut ask = None;
            setting(ui, ICON_SILENT, "Install updates silently", "Installs in the background after each scheduled scan.", |ui| {
                if toggle(ui, &mut silent) {
                    ask = Some(silent);
                }
            });
            if let Some(on) = ask {
                self.ask = Some(Ask::HelperTask(on));
            }
            let s = &mut self.settings;
            if s.silent_install {
                setting(ui, ICON_CLOCK, "Only during these hours", "Set both the same to allow any time.", |ui| {
                    changed |= hour_picker(ui, "to", &mut s.install_to);
                    ui.label(small("to"));
                    changed |= hour_picker(ui, "from", &mut s.install_from);
                });
                setting(ui, ICON_DRIVERS, "Install drivers silently", "From Windows Update.", |ui| {
                    changed |= toggle(ui, &mut s.silent_drivers);
                });
                setting(ui, ICON_GLOBE, "Include Microsoft Catalog drivers", "Off keeps silent drivers to Windows Update only.", |ui| {
                    changed |= toggle(ui, &mut s.silent_catalog);
                });
                setting(ui, ICON_SYSTEM, "Install Windows updates silently", "May need a restart.", |ui| {
                    changed |= toggle(ui, &mut s.silent_windows);
                });
                setting(ui, ICON_BACKUP, "Install software silently", "Programs being updated may close.", |ui| {
                    changed |= toggle(ui, &mut s.silent_software);
                });
                setting(ui, ICON_RESTART, "Restart automatically", "If a driver needs it. You get a 5 minute warning.", |ui| {
                    changed |= toggle(ui, &mut s.auto_restart);
                });
                if s.auto_restart {
                    setting(ui, ICON_CLOCK, "Restart only during those hours", "Otherwise it restarts right after installing.", |ui| {
                        changed |= toggle(ui, &mut s.restart_only_in_hours);
                    });
                }
            }

            group_title(ui, "Safety");
            setting(ui, ICON_BACKUP, "Back up before updating", "Saves the current driver so you can put it back.", |ui| {
                changed |= toggle(ui, &mut s.backup_before_update);
            });
            setting(ui, ICON_SHIELD, "Create a Windows restore point", "Made once a day before driver updates.", |ui| {
                changed |= toggle(ui, &mut s.make_restore_point);
            });
            let folder = s.backup_folder.display().to_string();
            let mut pick_folder = false;
            let mut open_folder = false;
            setting(ui, ICON_FOLDER, "Backup folder", &folder, |ui| {
                pick_folder = secondary(ui, "Change", true);
                open_folder = secondary(ui, "Open", true);
            });
            if pick_folder {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    self.settings.backup_folder = folder;
                    changed = true;
                    self.reload_backups();
                }
            }
            if open_folder {
                let _ = std::fs::create_dir_all(&self.settings.backup_folder);
                system::open_folder(&self.settings.backup_folder);
            }
            let ignored = self.settings.ignored.len();
            if ignored > 0 {
                let mut clear = false;
                setting(ui, ICON_RESET, "Ignored updates", &format!("{ignored} hidden from your list."), |ui| {
                    clear = secondary(ui, "Show Them Again", true);
                });
                if clear {
                    self.settings.ignored.clear();
                    changed = true;
                    self.rebuild_rows();
                }
            }
            let mut reset = false;
            setting(ui, ICON_RESET, "Reset all settings", "Back to how the app started.", |ui| {
                reset = secondary(ui, "Reset", true);
            });

            group_title(ui, "About");
            let version = format!("Version {}", env!("CARGO_PKG_VERSION"));
            setting(ui, ICON_ABOUT, system::APP_NAME, &version, |_| {});
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Credits").size(14.0));
                ui.label(small("Created by avatarmanz"));
                ui.label(small("Built with Claude by Anthropic"));
                ui.label(small("Made with Craft Anything"));
            });
            ui.add_space(4.0);
            if reset {
                let keep = self.settings.last_scan;
                self.settings = Settings { last_scan: keep, ..Default::default() };
                startup_changed = true;
            }
        });
        if startup_changed {
            self.apply_settings();
        } else if changed {
            self.settings.save();
        }
    }
}

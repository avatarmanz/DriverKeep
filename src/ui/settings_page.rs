//! Every option, sorted into five tabs so each one is short: General, Scanning, Installing,
//! Backup and About.

use super::look::*;
use super::{App, AppCheck, Ask};
use crate::system::{self, CloseChoice, Frequency, Settings, Theme};
use chrono::Datelike;
use egui::{Color32, RichText, Sense, Stroke, Vec2};

const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

#[derive(Clone, Copy, PartialEq)]
pub(super) enum SettingsTab {
    General,
    Customization,
    Scanning,
    Installing,
    Backup,
    About,
}

impl SettingsTab {
    const ALL: [(SettingsTab, &'static str); 6] = [
        (SettingsTab::General, "General"),
        (SettingsTab::Customization, "Customization"),
        (SettingsTab::Scanning, "Scanning"),
        (SettingsTab::Installing, "Installing"),
        (SettingsTab::Backup, "Backup"),
        (SettingsTab::About, "About"),
    ];

    /// The `--page` value used by screenshot tests: settings, settings-scanning, about...
    pub(super) fn from_page(page: Option<&str>) -> Self {
        match page {
            Some("settings-customization") => Self::Customization,
            Some("settings-scanning") => Self::Scanning,
            Some("settings-installing") => Self::Installing,
            Some("settings-backup") => Self::Backup,
            Some("about") => Self::About,
            _ => Self::General,
        }
    }
}

fn tab_button(ui: &mut egui::Ui, text: &str, selected: bool) -> bool {
    let (fill, color) = if selected { (teal(), Color32::WHITE) } else { (pal().hover, pal().soft) };
    let button = egui::Button::new(RichText::new(text).font(bold(13.5)).color(color))
        .fill(fill)
        .corner_radius(egui::CornerRadius::same(8))
        .min_size(Vec2::new(0.0, 32.0));
    ui.add(button).clicked()
}

impl App {
    pub(super) fn settings_page(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Settings").font(bold(20.0)));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            for (tab, label) in SettingsTab::ALL {
                if tab_button(ui, label, self.settings_tab == tab) {
                    self.settings_tab = tab;
                }
            }
        });
        ui.add_space(2.0);
        let tab = self.settings_tab;
        let mut startup_changed = false;
        let mut changed = false;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            match tab {
                SettingsTab::General => self.general_tab(ui, &mut changed, &mut startup_changed),
                SettingsTab::Customization => self.customization_tab(ui, &mut changed),
                SettingsTab::Scanning => self.scanning_tab(ui, &mut changed, &mut startup_changed),
                SettingsTab::Installing => self.installing_tab(ui, &mut changed),
                SettingsTab::Backup => self.backup_tab(ui, &mut changed),
                SettingsTab::About => self.about_tab(ui, &mut changed),
            }
            ui.add_space(4.0);
        });
        if startup_changed {
            self.apply_settings();
        } else if changed {
            self.settings.save();
        }
    }

    fn general_tab(&mut self, ui: &mut egui::Ui, changed: &mut bool, startup_changed: &mut bool) {
        let s = &mut self.settings;
        group_title(ui, "Startup");
        setting(ui, ICON_POWER, "Run when Windows starts", "Starts with your computer.", |ui| {
            *startup_changed |= toggle(ui, &mut s.run_on_startup);
        });
        setting(ui, ICON_HIDE, "Launch in background", "Starts in the system tray with no window.", |ui| {
            *startup_changed |= toggle(ui, &mut s.start_hidden);
        });
        setting(ui, ICON_TRAY, "When closing the window", "Tray keeps the background helper working.", |ui| {
            for (choice, label) in [
                (CloseChoice::Tray, "Stay in tray"),
                (CloseChoice::Exit, "Exit"),
                (CloseChoice::Ask, "Ask every time"),
            ] {
                *startup_changed |= ui.selectable_value(&mut s.close_choice, choice, label).changed();
            }
        });

        group_title(ui, "Other");
        setting(ui, ICON_BELL, "Notify me", "Shows a small tray message when updates are found.", |ui| {
            *changed |= toggle(ui, &mut s.notify);
        });
        let ignored = self.settings.ignored.len();
        if ignored > 0 {
            let mut clear = false;
            setting_with(ui, 150.0, ICON_RESET, "Ignored updates", &format!("{ignored} hidden from your list."), |ui| {
                clear = secondary(ui, "Show Them Again", true);
            });
            if clear {
                self.settings.ignored.clear();
                *changed = true;
                self.rebuild_rows();
            }
        }
        let mut reset = false;
        setting(ui, ICON_RESET, "Reset all settings", "Back to how the app started.", |ui| {
            reset = secondary(ui, "Reset", true);
        });
        if reset {
            let keep = self.settings.last_scan;
            self.settings = Settings { last_scan: keep, ..Default::default() };
            *startup_changed = true;
        }
    }

    fn customization_tab(&mut self, ui: &mut egui::Ui, changed: &mut bool) {
        let s = &mut self.settings;
        group_title(ui, "Theme");
        setting_with(ui, 370.0, ICON_THEME, "Light or dark", "OLED uses true black, which saves power on OLED screens.", |ui| {
            // This row is laid out right to left, so the list is backwards to read System, Light, Dark, OLED.
            for (theme, label) in
                [(Theme::Oled, "OLED"), (Theme::Dark, "Dark"), (Theme::Light, "Light"), (Theme::System, "System")]
            {
                *changed |= ui.selectable_value(&mut s.theme, theme, label).changed();
            }
        });

        group_title(ui, "Accent color");
        setting(ui, ICON_THEME, "Follow Windows color", "Uses the accent color from your Windows settings, and keeps up if it changes.", |ui| {
            *changed |= toggle(ui, &mut s.follow_windows_accent);
        });
        let follow = s.follow_windows_accent;
        ui.add_space(6.0);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let note = if follow {
                "Turn off Follow Windows color to choose your own."
            } else {
                "Used for buttons, progress, switches and highlights."
            };
            ui.label(small(note));
            ui.add_space(8.0);
            ui.add_enabled_ui(!follow, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    for (name, rgb) in ACCENTS {
                        let (rect, response) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
                        ui.painter().circle_filled(rect.center(), 13.0, Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
                        if s.accent == rgb {
                            ui.painter().circle_stroke(rect.center(), 15.5, Stroke::new(2.0, pal().text));
                        }
                        if response.on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            s.accent = rgb;
                            *changed = true;
                        }
                    }
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label("Your own color");
                    *changed |= ui.color_edit_button_srgb(&mut s.accent).changed();
                    if secondary(ui, "Back to teal", s.accent != DEFAULT_ACCENT) {
                        s.accent = DEFAULT_ACCENT;
                        *changed = true;
                    }
                });
            });
        });

        group_title(ui, "Preview");
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            // The sample sits in its own outlined box so it reads as an example, not as settings.
            // Everything in it works, so the colors can be tried out.
            egui::Frame::new()
                .fill(pal().bg)
                .stroke(Stroke::new(1.0, pal().line))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::same(14))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if self.preview_running {
                        let step = ui.input(|i| i.stable_dt).min(0.1) * 30.0;
                        self.preview_percent = (self.preview_percent + step).min(100.0);
                        if self.preview_percent >= 100.0 {
                            self.preview_running = false;
                        }
                        ui.ctx().request_repaint();
                    }
                    ui.label(RichText::new("Updates").font(bold(15.0)));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if primary(ui, "Update Everything", true) {
                            if self.preview_percent >= 100.0 {
                                self.preview_percent = 0.0;
                            }
                            self.preview_running = true;
                        }
                        if secondary(ui, "Scan Again", true) {
                            self.preview_running = false;
                            self.preview_percent = 0.0;
                        }
                        ui.add_space(6.0);
                        toggle(ui, &mut self.preview_on);
                        ui.label(small(if self.preview_on { "Switch is on" } else { "Switch is off" }));
                    });
                    ui.add_space(12.0);
                    let percent = self.preview_percent.round() as u32;
                    ui.horizontal(|ui| {
                        ui.label("Voice Clarity");
                        let (label, color) = if percent >= 100 {
                            ("Installed".to_string(), SUCCESS)
                        } else if self.preview_running || percent > 0 {
                            (format!("Installing {percent}%"), SKY)
                        } else {
                            ("Waiting".to_string(), pal().soft)
                        };
                        pill(ui, &label, color);
                    });
                    ui.add_space(2.0);
                    if drag_bar(ui, &mut self.preview_percent) {
                        self.preview_running = false;
                    }
                    ui.add_space(2.0);
                    ui.label(small("The bar fills as an update downloads or installs. Click or drag it, or press Update Everything."));
                });
        });
    }

    fn scanning_tab(&mut self, ui: &mut egui::Ui, changed: &mut bool, startup_changed: &mut bool) {
        let s = &mut self.settings;
        group_title(ui, "Drivers");
        setting(ui, ICON_DRIVERS, "Check for new drivers", "Looks for newer drivers for your hardware.", |ui| {
            *changed |= toggle(ui, &mut s.scan_drivers);
        });
        setting(ui, ICON_GLOBE, "Microsoft Update Catalog", "Finds drivers Windows Update doesn't offer on its own.", |ui| {
            *changed |= toggle(ui, &mut s.use_catalog);
        });

        group_title(ui, "Windows updates");
        setting(ui, ICON_SYSTEM, "Check for Windows updates", "Security and system updates from Microsoft.", |ui| {
            *changed |= toggle(ui, &mut s.scan_windows);
        });
        setting(ui, ICON_SCAN, "Include optional updates", "Previews Windows doesn't install by itself.", |ui| {
            *changed |= toggle(ui, &mut s.scan_optional);
        });

        group_title(ui, "Third-party software");
        setting(ui, ICON_BACKUP, "Check installed programs", "Newer versions of your other programs.", |ui| {
            *changed |= toggle(ui, &mut s.scan_software);
        });
        let (winget_ready, choco_ready) = self.tools.unwrap_or((true, true));
        let mut get_winget = false;
        let mut get_chocolatey = false;
        let mut open_store = false;
        let free = !self.tools_installing;
        let install_text = if free { "Install" } else { "Installing…" };
        setting_with(ui, 280.0, ICON_SETTINGS, "Windows Package Manager", "Finds updates for the programs already on your PC.", |ui| {
            if winget_ready {
                *changed |= toggle(ui, &mut s.use_winget);
            } else {
                get_winget = secondary(ui, install_text, free);
                open_store = secondary(ui, "Microsoft Store", true);
            }
        });
        setting_with(ui, 150.0, ICON_SETTINGS, "Chocolatey", "Updates programs that Chocolatey installed.", |ui| {
            if choco_ready {
                *changed |= toggle(ui, &mut s.use_chocolatey);
            } else {
                get_chocolatey = secondary(ui, install_text, free);
            }
        });
        if open_store {
            use crate::software::{WINGET_STORE_LINK, WINGET_WEB_LINK};
            std::thread::spawn(|| {
                system::open_store_page(WINGET_STORE_LINK, WINGET_WEB_LINK);
            });
        }
        if get_winget || get_chocolatey {
            self.ask = Some(Ask::InstallTools(get_winget, get_chocolatey));
        }

        let s = &mut self.settings;
        group_title(ui, "Scheduling");
        let detail = "Runs in the tray helper, or the next time you open the app.";
        setting_with(ui, 440.0, ICON_CLOCK, "Scan on a schedule", detail, |ui| {
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
                            *startup_changed = true;
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
                            *changed |= ui.selectable_value(&mut s.scan_weekday, index as u32, *day).changed();
                        }
                    });
            }
            if s.scan_frequency != Frequency::Never {
                *changed |= hour_picker(ui, "scan_hour", &mut s.scan_hour);
            }
        });
        setting(ui, ICON_SCAN, "Scan when the helper starts", "One background scan each time your PC starts.", |ui| {
            *changed |= toggle(ui, &mut s.scan_at_startup);
        });
    }

    fn installing_tab(&mut self, ui: &mut egui::Ui, changed: &mut bool) {
        let s = &mut self.settings;
        // The switch for silent installs comes first, since the silent options below depend on it.
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
        let silent_on = s.silent_install;
        if silent_on {
            setting_with(ui, 260.0, ICON_CLOCK, "Only during these hours", "Set both the same to allow any time.", |ui| {
                *changed |= hour_picker(ui, "to", &mut s.install_to);
                ui.label(small("to"));
                *changed |= hour_picker(ui, "from", &mut s.install_from);
            });
        }

        group_title(ui, "Drivers");
        setting(ui, ICON_BELL, "Ask before updating drivers", "Shows a reminder before driver updates start.", |ui| {
            *changed |= toggle(ui, &mut s.update_all_remind);
        });
        setting(ui, ICON_DRIVERS, "Include drivers in Update Everything", "Ticked after each scan.", |ui| {
            *changed |= toggle(ui, &mut s.pick_drivers);
        });
        if silent_on {
            setting(ui, ICON_SILENT, "Install drivers silently", "From Windows Update.", |ui| {
                *changed |= toggle(ui, &mut s.silent_drivers);
            });
            setting(ui, ICON_GLOBE, "Include Microsoft Catalog drivers", "Off keeps silent drivers to Windows Update only.", |ui| {
                *changed |= toggle(ui, &mut s.silent_catalog);
            });
        }

        group_title(ui, "Windows updates");
        setting(ui, ICON_SYSTEM, "Include Windows updates in Update Everything", "Ticked after each scan.", |ui| {
            *changed |= toggle(ui, &mut s.pick_windows);
        });
        if silent_on {
            setting(ui, ICON_SILENT, "Install Windows updates silently", "May need a restart.", |ui| {
                *changed |= toggle(ui, &mut s.silent_windows);
            });
        }

        group_title(ui, "Third-party software");
        setting(ui, ICON_BACKUP, "Include software in Update Everything", "Ticked after each scan.", |ui| {
            *changed |= toggle(ui, &mut s.pick_software);
        });
        if silent_on {
            setting(ui, ICON_SILENT, "Install software silently", "Programs being updated may close.", |ui| {
                *changed |= toggle(ui, &mut s.silent_software);
            });
        }

        if silent_on {
            group_title(ui, "Restart options");
            setting(ui, ICON_RESTART, "Restart automatically", "If a driver needs it. You get a 5 minute warning.", |ui| {
                *changed |= toggle(ui, &mut s.auto_restart);
            });
            if s.auto_restart {
                setting(ui, ICON_CLOCK, "Restart only during those hours", "Otherwise it restarts right after installing.", |ui| {
                    *changed |= toggle(ui, &mut s.restart_only_in_hours);
                });
            }
        }
    }

    fn backup_tab(&mut self, ui: &mut egui::Ui, changed: &mut bool) {
        let s = &mut self.settings;
        group_title(ui, "Before updating");
        setting(ui, ICON_BACKUP, "Back up the current driver", "Saves it so you can put it back.", |ui| {
            *changed |= toggle(ui, &mut s.backup_before_update);
        });
        setting(ui, ICON_SHIELD, "Create a Windows restore point", "Made once a day before driver updates.", |ui| {
            *changed |= toggle(ui, &mut s.make_restore_point);
        });
        let detail = "If a backup fails, the driver installs anyway and can't be put back.";
        setting(ui, ICON_BACKUP, "Install even if a backup can't be made", detail, |ui| {
            *changed |= toggle(ui, &mut s.install_without_backup);
        });

        group_title(ui, "Where backups go");
        let folder = s.backup_folder.display().to_string();
        let mut pick_folder = false;
        let mut open_folder = false;
        setting_with(ui, 190.0, ICON_FOLDER, "Backup folder", &folder, |ui| {
            pick_folder = secondary(ui, "Change", true);
            open_folder = secondary(ui, "Open", true);
        });
        if pick_folder {
            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                self.settings.backup_folder = folder;
                *changed = true;
                self.reload_backups();
            }
        }
        if open_folder {
            let _ = std::fs::create_dir_all(&self.settings.backup_folder);
            system::open_folder(&self.settings.backup_folder);
        }
    }

    fn about_tab(&mut self, ui: &mut egui::Ui, changed: &mut bool) {
        group_title(ui, "DriverKeep");
        // Like the other rows, but the version number is in the accent color.
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(icon(ICON_ABOUT, 17.0, pal().soft));
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(RichText::new(system::APP_NAME).size(14.0));
                    let mut line = egui::text::LayoutJob::default();
                    let format = |color| egui::TextFormat::simple(egui::FontId::proportional(12.0), color);
                    line.append("Version ", 0.0, format(pal().soft));
                    line.append(env!("CARGO_PKG_VERSION"), 0.0, format(teal()));
                    ui.label(line);
                });
            });
        });
        ui.add_space(4.0);
        let mut check = false;
        let mut view = false;
        let checking = self.checking_app;
        let result = self.app_check.clone();
        setting_with(ui, 470.0, ICON_RESET, "Check for updates", "Looks for a newer DriverKeep on GitHub.", |ui| {
            // Laid out right to left: the result sits at the far right, with the button to its left.
            if let Some((text, kind)) = &result {
                let color = match kind {
                    AppCheck::Latest => SUCCESS,
                    AppCheck::Available => SKY,
                    AppCheck::Failed => CRITICAL,
                };
                status_bar(ui, text, color);
                if *kind == AppCheck::Available {
                    view = secondary(ui, "View Update", true);
                }
            }
            check = secondary(ui, if checking { "Checking…" } else { "Check for Updates" }, !checking);
        });
        if check {
            self.check_app_update(ui.ctx());
        }
        if view {
            self.page = super::Page::Drivers;
        }
        let s = &mut self.settings;
        setting(ui, ICON_DRIVERS, "Update DriverKeep automatically", "Each scan checks GitHub and installs any later version by itself.", |ui| {
            *changed |= toggle(ui, &mut s.auto_update_app);
        });
        group_title(ui, "Credits");
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(small("Created by avatarmanz"));
            ui.label(small("Built with Claude by Anthropic"));
            ui.label(small("Made with Craft Anything"));
        });
    }
}

/// A progress bar the person can click or drag to set, for the preview. Gives true when moved.
fn drag_bar(ui: &mut egui::Ui, percent: &mut f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 18.0), Sense::click_and_drag());
    let track = egui::Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 6.0));
    ui.painter().rect_filled(track, 3.0, pal().line);
    let mut filled = track;
    filled.set_width(track.width() * (*percent / 100.0).clamp(0.0, 1.0));
    ui.painter().rect_filled(filled, 3.0, teal());
    let mut moved = false;
    if response.clicked() || response.dragged() {
        if let Some(at) = response.interact_pointer_pos() {
            *percent = ((at.x - rect.left()) / rect.width() * 100.0).clamp(0.0, 100.0);
            moved = true;
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand);
    moved
}

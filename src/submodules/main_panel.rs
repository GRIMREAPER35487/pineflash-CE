use std::path::{Path, PathBuf};
use std::time::Duration;

use eframe::egui::{
    self, Button, CentralPanel, Color32, Frame, Image, Layout, Margin, RichText, Rounding, ScrollArea,
    SidePanel, Vec2,
};
use egui::{emath, Context};
use egui_file::FileDialog;
use simple_home_dir::home_dir;
#[cfg(not(target_os = "windows"))]
use std::process::Command;
#[cfg(not(target_os = "windows"))]
use version_compare::Version;

use super::flash::backup_dir;
use crate::Flasher;

pub const GREEN: Color32 = Color32::from_rgb(29, 158, 117);
pub const AMBER: Color32 = Color32::from_rgb(214, 140, 40);
pub const RED: Color32 = Color32::from_rgb(226, 75, 74);

/// Icon glyphs from the bundled Nerd Font (Font Awesome range).
pub mod icon {
    pub const CHECK: &str = "\u{f058}";
    pub const CROSS: &str = "\u{f057}";
    pub const WARN: &str = "\u{f071}";
    pub const FOLDER: &str = "\u{f07c}";
    pub const BOLT: &str = "\u{f0e7}";
    pub const SUN: &str = "\u{f185}";
    pub const MOON: &str = "\u{f186}";
}

enum Status {
    Flashing,
    Ready,
    WrongIron(String),
    Disconnected,
}

fn friendly(int_name: &str) -> &str {
    match int_name {
        "Pinecil" => "Pinecil V1",
        "Pinecilv2" => "Pinecil V2",
        "Both" => "Pinecil V1 and V2",
        other => other,
    }
}

/// A coloured status pill.
fn chip(ui: &mut egui::Ui, color: Color32, glyph: &str, text: &str) {
    Frame::none()
        .fill(color.gamma_multiply(0.16))
        .rounding(Rounding::same(8.))
        .inner_margin(Margin::symmetric(10., 4.))
        .show(ui, |ui| {
            ui.label(RichText::new(format!("{}  {}", glyph, text)).color(color));
        });
}

/// Small muted label above a field, like the mockup's "Iron", "Firmware".
fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(12.5).weak());
}

/// Number of PineFlash backups in the backup folder.
pub fn count_backups() -> usize {
    std::fs::read_dir(backup_dir())
        .map(|d| {
            d.filter_map(Result::ok)
                .filter(|e| e.path().extension().map(|x| x == "bin").unwrap_or(false))
                .count()
        })
        .unwrap_or(0)
}

fn open_folder(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(dir).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
}

impl Flasher {
    fn busy(&self) -> bool {
        self.config.flash_events.is_some() || self.config.download || self.config.flash
    }

    fn status(&self) -> Status {
        if self.busy() {
            return Status::Flashing;
        }
        match self.config.iron_connected.as_deref() {
            None => Status::Disconnected,
            Some(c) if c == self.config.int_name || c == "Both" => Status::Ready,
            Some(other) => Status::WrongIron(other.to_string()),
        }
    }

    /// Why the Flash button is unavailable, or None if it can be pressed.
    fn flash_blocker(&self) -> Option<String> {
        match self.status() {
            Status::Flashing => return Some("Working...".into()),
            Status::Disconnected => {
                return Some("Plug your iron in while it's in flashing mode. See the connection guide below.".into())
            }
            Status::WrongIron(other) => {
                return Some(format!(
                    "A {} is plugged in, but {} is selected above.",
                    friendly(&other),
                    friendly(&self.config.int_name)
                ))
            }
            Status::Ready => {}
        }
        let chosen = if self.config.version == *"Custom" {
            self.config.picked_path.is_some()
        } else {
            self.config.version != *"Select"
        };
        if !chosen {
            return Some("Choose a firmware version or a file.".into());
        }
        None
    }

    fn guide_visible(&self) -> bool {
        self.config
            .show_guide
            .unwrap_or(self.config.iron_connected.is_none() && !self.busy())
    }

    pub fn render_main_windows(&mut self, ctx: &Context) {
        self.config.ready_to_flash = self.flash_blocker().is_none();
        self.render_sidebar(ctx);
        let p = crate::palette(self.saved_config.dark_mode);
        CentralPanel::default()
            .frame(Frame::none().fill(p.page).inner_margin(Margin::symmetric(22., 18.)))
            .show(ctx, |ui| {
            ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                self.render_flash_form(ui);
                if self.guide_visible() {
                    ui.add_space(14.);
                    self.render_guide(ui);
                }
                ui.add_space(14.);
                self.render_log(ui);
            });
        });
        self.show_file_dialog(ctx);
        #[cfg(target_os = "windows")]
        self.show_restore_dialog(ctx);
        self.toasts.show(ctx);
    }

    // ------------------------------------------------------------------
    // Sidebar: connected iron, backups, help
    // ------------------------------------------------------------------
    fn render_sidebar(&mut self, ctx: &Context) {
        let p = crate::palette(self.saved_config.dark_mode);
        SidePanel::left("sidebar")
            .resizable(false)
            .exact_width(200.)
            .frame(
                Frame::none()
                    .fill(p.sidebar)
                    .stroke(egui::Stroke::new(1.0_f32, p.border))
                    .inner_margin(Margin::symmetric(16., 18.)),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 4.;
                field_label(ui, "Connected iron");
                match self.status() {
                    Status::Flashing => chip(ui, AMBER, icon::BOLT, "Working..."),
                    Status::Ready => chip(
                        ui,
                        GREEN,
                        icon::CHECK,
                        friendly(self.config.iron_connected.as_deref().unwrap_or("")),
                    ),
                    Status::WrongIron(other) => chip(ui, AMBER, icon::WARN, friendly(&other)),
                    Status::Disconnected => chip(ui, RED, icon::CROSS, "Not connected"),
                }
                match (&self.config.v2_serial_path, self.status()) {
                    (_, Status::WrongIron(other)) => {
                        ui.label(RichText::new(format!("Select {} to flash it", friendly(&other))).size(12.5).weak());
                    }
                    (Some(port), _) => {
                        ui.label(RichText::new(format!("{} · flashing mode", port)).size(12.5).weak());
                    }
                    (None, Status::Disconnected) => {
                        ui.label(RichText::new("Waiting for an iron").size(12.5).weak());
                    }
                    _ => {}
                }

                ui.add_space(18.);
                field_label(ui, "Backups");
                let n = self.config.backup_count;
                ui.label(match n {
                    0 => "None yet".to_string(),
                    1 => "1 saved".to_string(),
                    n => format!("{} saved", n),
                });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.;
                    #[cfg(target_os = "windows")]
                    {
                        self.render_restore_link(ui);
                        ui.label(RichText::new("·").weak());
                    }
                    if ui.link("Open folder").clicked() {
                        open_folder(&backup_dir());
                    }
                });
                if cfg!(target_os = "windows") {
                    ui.label(RichText::new("Made automatically before each Pinecil V2 flash.").size(11.5).weak());
                }

                ui.add_space(18.);
                field_label(ui, "Help");
                let shown = self.guide_visible();
                if ui.link(if shown { "Hide connection guide" } else { "Connection guide" }).clicked() {
                    self.config.show_guide = Some(!shown);
                }
            });
    }

    #[cfg(target_os = "windows")]
    fn render_restore_link(&mut self, ui: &mut egui::Ui) {
        let v2_connected = matches!(self.config.iron_connected.as_deref(), Some("Pinecilv2") | Some("Both"));
        let enabled = v2_connected && !self.busy() && self.config.backup_count > 0;
        let resp = ui
            .add_enabled(enabled, egui::Link::new("Restore"))
            .on_hover_text("Put a firmware backup made by PineFlash back on your Pinecil V2.");
        let resp = if self.config.backup_count == 0 {
            resp.on_disabled_hover_text("No backups yet.")
        } else {
            resp.on_disabled_hover_text("Plug in a Pinecil V2 in flashing mode first.")
        };
        if resp.clicked() {
            let dir = backup_dir();
            let _ = std::fs::create_dir_all(&dir);
            let mut dialog = FileDialog::open_file(Some(dir))
                .default_size(emath::Vec2 { x: 420., y: 320. })
                .show_files_filter(Box::new(|p: &Path| p.extension().map(|e| e == "bin").unwrap_or(false)));
            dialog.open();
            self.config.restore_dialog = Some(dialog);
        }
    }


    #[cfg(target_os = "windows")]
    fn show_restore_dialog(&mut self, ctx: &Context) {
        let mut picked = None;
        if let Some(dialog) = &mut self.config.restore_dialog {
            if dialog.show(ctx).selected() {
                picked = dialog.path().map(|p| p.to_path_buf());
            }
        }
        if let Some(path) = picked {
            self.config.restore_dialog = None;
            self.start_restore(path);
        }
    }

    // ------------------------------------------------------------------
    // Main form: iron, firmware, language, flash button
    // ------------------------------------------------------------------
    fn render_flash_form(&mut self, ui: &mut egui::Ui) {
        let p = crate::palette(self.saved_config.dark_mode);
        let busy = self.busy();

        field_label(ui, "Iron");
        ui.add_enabled_ui(!busy, |ui| {
            egui::ComboBox::from_id_source("iron")
                .width(ui.available_width())
                .selected_text(self.config.iron.to_string())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.config.iron, "Pinecil V1".to_string(), "Pinecil V1");
                    ui.selectable_value(&mut self.config.iron, "Pinecil V2".to_string(), "Pinecil V2");
                });
        });
        if self.config.iron == "Pinecil V1" {
            self.config.int_name = "Pinecil".to_string();
        } else if self.config.iron == "Pinecil V2" {
            self.config.int_name = "Pinecilv2".to_string();
        }
        // A file picked for one iron must never be flashed to the other.
        if self.config.iron != self.config.last_iron {
            self.config.last_iron = self.config.iron.clone();
            if self.config.picked_path.take().is_some() {
                self.config.version = "Select".to_string();
                self.config.logs.push_str("PineFlash: Iron changed, custom file cleared.\n");
            }
        }

        ui.add_space(12.);
        let mut open_picker = false;
        ui.columns(2, |cols| {
            field_label(&mut cols[0], "Firmware");
            cols[0].add_enabled_ui(!busy, |ui| {
                let latest_stable = self.config.vers.iter().find(|v| !self.config.prereleases.contains(v)).cloned();
                let describe = |v: &str| {
                    if self.config.prereleases.iter().any(|p| p == v) {
                        format!("{}  (pre-release)", v)
                    } else if latest_stable.as_deref() == Some(v) {
                        format!("{}  (latest)", v)
                    } else {
                        v.to_string()
                    }
                };
                let shown = match self.config.version.as_str() {
                    "Select" if !self.config.versions_checked => "Loading versions...".to_string(),
                    "Select" => "Choose a version".to_string(),
                    "Custom" => "File on this PC".to_string(),
                    v => describe(v),
                };
                egui::ComboBox::from_id_source("version")
                    .width(ui.available_width())
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        for v in self.config.vers.clone().iter() {
                            let label = describe(v);
                            ui.selectable_value(&mut self.config.version, v.clone(), label);
                        }
                        ui.separator();
                        if ui
                            .selectable_label(self.config.version == "Custom", "Use a file on this PC...")
                            .clicked()
                        {
                            open_picker = true;
                        }
                    });
            });
            cols[0].add_space(4.);
            let mut show_pre = self.saved_config.show_prereleases;
            let toggle = cols[0]
                .add_enabled(
                    !busy,
                    egui::Checkbox::new(&mut show_pre, RichText::new("Show pre-releases").size(12.5).color(p.muted)),
                )
                .on_hover_text("Include IronOS release candidates newer than the latest stable release. They are test builds and may have bugs.");
            if toggle.changed() {
                self.saved_config.show_prereleases = show_pre;
                self.save_settings();
                self.refresh_versions();
                self.config.logs.push_str(if show_pre {
                    "PineFlash: Showing pre-releases.\n"
                } else {
                    "PineFlash: Hiding pre-releases.\n"
                });
            }

            field_label(&mut cols[1], "Language");
            let lang_enabled = self.config.version != *"Select"
                && self.config.version != *"Custom"
                && !self.config.download_metadata
                && !busy;
            cols[1].add_enabled_ui(lang_enabled, |ui| {
                let current = self
                    .config
                    .code_names
                    .iter()
                    .position(|c| *c == self.config.lang)
                    .map(|i| self.config.fancy_names[i].clone())
                    .unwrap_or_else(|| self.config.lang.clone());
                egui::ComboBox::from_id_source("language")
                    .width(ui.available_width())
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for i in 0..self.config.code_names.len() {
                            let code = self.config.code_names[i].clone();
                            let name = self.config.fancy_names[i].clone();
                            ui.selectable_value(&mut self.config.lang, code, name);
                        }
                    });
            });
        });
        if open_picker {
            let mut dialog = FileDialog::open_file(home_dir()).default_size(emath::Vec2 { x: 420., y: 320. });
            dialog.open();
            self.config.open_file_dialog = Some(dialog);
        }

        if self.config.version != self.config.last_version {
            self.config.last_version = self.config.version.clone();
            if self.config.version != *"Custom" {
                self.config.picked_path = None;
            }
            if self.config.version != *"Custom" && self.config.version != *"Select" {
                // Each release has its own language list.
                self.config.fancy_names.clear();
                self.config.code_names.clear();
                self.config.download_metadata = true;
                self.config.run_once_vers = true;
            }
        }

        if let Some(path) = self.config.picked_path.clone() {
            ui.add_space(4.);
            ui.horizontal(|ui| {
                let name = Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or(path.clone());
                ui.label(RichText::new(format!("{}  {}", icon::FOLDER, name)).size(12.5).weak())
                    .on_hover_text(&path);
                if !busy && ui.link("Clear").clicked() {
                    self.config.picked_path = None;
                    self.config.version = "Select".to_string();
                }
            });
        }

        ui.add_space(16.);
        if self.config.flash_events.is_some() {
            ui.add(
                egui::ProgressBar::new(self.config.flash_progress)
                    .desired_height(34.)
                    .rounding(Rounding::same(8.))
                    .text(format!("{}  {:.0}%", self.config.flash_stage, self.config.flash_progress * 100.0)),
            );
            ui.add_space(4.);
            ui.label(RichText::new("Keep the iron plugged in.").size(12.5).weak());
        } else {
            let label = match self.config.version.as_str() {
                "Custom" => "Flash file".to_string(),
                "Select" => "Flash".to_string(),
                v => format!("Flash {}", v),
            };
            // One solid primary button, like the mockup.
            let button = Button::new(RichText::new(label).size(15.).color(p.page).strong())
                .fill(p.text)
                .stroke(egui::Stroke::NONE)
                .rounding(Rounding::same(8.))
                .min_size(Vec2::new(ui.available_width(), 36.));
            if ui.add_enabled(self.config.ready_to_flash, button).clicked() {
                if self.config.version != *"Custom" {
                    self.config.download = true;
                } else {
                    self.toasts.dismiss_all_toasts();
                    self.toasts.info("Flashing").set_duration(None).set_closable(false);
                    self.config.flash = true;
                }
            }
            ui.add_space(4.);
            match self.flash_blocker() {
                Some(reason) if !busy => {
                    ui.label(RichText::new(reason).size(12.5).weak());
                }
                _ if self.config.int_name == "Pinecilv2" && cfg!(target_os = "windows") => {
                    ui.label(
                        RichText::new("Your current firmware is backed up first, and everything written is verified.")
                            .size(12.5)
                            .weak(),
                    );
                }
                _ => {}
            }
        }
    }


    // ------------------------------------------------------------------
    // Connection guide
    // ------------------------------------------------------------------
    fn render_guide(&mut self, ui: &mut egui::Ui) {
        let p = crate::palette(self.saved_config.dark_mode);
        Frame::none()
            .stroke(egui::Stroke::new(1.0_f32, p.border))
            .inner_margin(Margin::same(14.))
            .rounding(Rounding::same(10.))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Connection guide").strong());
                    ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.link("Hide").clicked() {
                            self.config.show_guide = Some(false);
                        }
                    });
                });
                ui.add_space(6.);
                let iron = friendly(&self.config.int_name).to_string();
                ui.label(format!("Put your {} into flashing mode:", iron));
                let steps = [
                    "Unplug the iron from USB and from any power supply.",
                    "Press and hold the minus (-) button.",
                    "Keep holding it and plug the USB-C cable into this computer. Hold for 3 more seconds, then let go.",
                    "The screen stays blank. The status on the left turns green when PineFlash finds the iron.",
                ];
                for (i, s) in steps.iter().enumerate() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("{}.", i + 1)).strong());
                        ui.label(*s);
                    });
                }
                ui.add_space(4.);
                ui.label(
                    RichText::new(
                        "Not found? Try another cable (charge-only cables don't carry data) or plug straight into the computer instead of a hub.",
                    )
                    .small()
                    .weak(),
                );
                if self.config.int_name == "Pinecil" && cfg!(target_os = "windows") {
                    ui.label(
                        RichText::new(
                            "Pinecil V1 on Windows: install the WinUSB driver for the iron once, using Zadig.",
                        )
                        .small()
                        .color(AMBER),
                    );
                }

                ui.add_space(8.);
                let w = ui.available_width();
                let size = Vec2::new(w, w / 3.4);
                match self.config.current_step {
                    0 => ui.add(Image::new(egui::include_image!("../../assets/Step1.svg")).fit_to_exact_size(size)),
                    1 => ui.add(Image::new(egui::include_image!("../../assets/Step2.svg")).fit_to_exact_size(size)),
                    _ => ui.add(Image::new(egui::include_image!("../../assets/Step3.svg")).fit_to_exact_size(size)),
                };
                ui.horizontal(|ui| {
                    if ui.add_enabled(self.config.current_step > 0, Button::new("Previous")).clicked() {
                        self.config.current_step -= 1;
                    }
                    ui.label(RichText::new(format!("Picture {} of 3", self.config.current_step + 1)).weak());
                    if ui.add_enabled(self.config.current_step < 2, Button::new("Next")).clicked() {
                        self.config.current_step += 1;
                    }
                });
            });
    }

    // ------------------------------------------------------------------
    // Log
    // ------------------------------------------------------------------
    fn render_log(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            field_label(ui, "Log");
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.link("Copy").clicked() {
                    ui.output_mut(|o| o.copied_text = self.config.logs.clone());
                }
            });
        });
        Frame::none()
            .fill(ui.visuals().extreme_bg_color)
            .rounding(Rounding::same(8.))
            .inner_margin(Margin::same(10.))
            .show(ui, |ui| {
                ScrollArea::vertical()
                    .id_source("log")
                    .max_height(160.)
                    .auto_shrink([false, true])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.label(RichText::new(self.config.logs.as_str()).monospace().small());
                    });
            });
    }

    // ------------------------------------------------------------------
    // Custom firmware file picker
    // ------------------------------------------------------------------
    fn show_file_dialog(&mut self, ctx: &Context) {
        let mut picked: Option<PathBuf> = None;
        if let Some(dialog) = &mut self.config.open_file_dialog {
            if dialog.show(ctx).selected() {
                picked = dialog.path().map(|p| p.to_path_buf());
            }
        }
        let Some(file) = picked else { return };
        self.config.open_file_dialog = None;

        let ext = file.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        // On Windows the V2 is flashed by the built-in flasher, which takes raw .bin files.
        let native_v2 = cfg!(target_os = "windows") && self.config.int_name == "Pinecilv2";
        let ok_ext = if self.config.int_name == "Pinecil" {
            ext == "dfu"
        } else if native_v2 {
            ext == "bin"
        } else {
            ext == "bin" || ext == "dfu"
        };
        if !ok_ext {
            self.toasts.dismiss_all_toasts();
            let want = if self.config.int_name == "Pinecil" {
                ".dfu"
            } else if native_v2 {
                ".bin"
            } else {
                ".bin or .dfu"
            };
            self.toasts
                .error(format!("Wrong file type\nThis iron needs a {} file.", want))
                .set_duration(Some(Duration::from_secs(4)))
                .set_closable(false);
            self.config.logs.push_str("PineFlash: Incorrect filetype selected.\n");
            self.config.picked_path = None;
        } else if !native_v2 && self.config.int_name == "Pinecilv2" && ext == "dfu" && !self.blisp_supports_dfu() {
            self.toasts.dismiss_all_toasts();
            self.toasts
                .error("Your blisp can't flash boot logos\nUpdate blisp to 0.0.4 or newer.")
                .set_duration(Some(Duration::from_secs(4)))
                .set_closable(false);
            self.config.logs.push_str(
                format!("PineFlash: Old or missing blisp (version '{}').\n", self.config.blisp_version).as_str(),
            );
            self.config.picked_path = None;
        } else {
            self.config.picked_path = Some(file.display().to_string());
            self.config.version = "Custom".to_string();
            self.toasts.dismiss_all_toasts();
            self.toasts
                .success("File selected")
                .set_duration(Some(Duration::from_secs(4)))
                .set_closable(false);
            self.config.logs.push_str("PineFlash: Custom file selected.\n");
        }
    }

    /// blisp >= 0.0.4 is needed to flash .dfu files to a V2 (Linux/macOS, where blisp is still used).
    #[cfg(not(target_os = "windows"))]
    fn blisp_supports_dfu(&mut self) -> bool {
        #[cfg(feature = "appimage")]
        let blisppath = std::env::current_dir().unwrap_or_default().join("linux").join("blisp");
        #[cfg(not(feature = "appimage"))]
        let blisppath = std::path::PathBuf::from("blisp");
        let out = match Command::new(&blisppath)
            .env("PATH", "/usr/local/sbin:/usr/local/bin:/usr/bin:/bin")
            .arg("--version")
            .output()
        {
            Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
            Err(_) => {
                self.config.blisp_version = "not found".to_string();
                return false;
            }
        };
        for line in out.lines().filter(|l| l.contains('v')) {
            if let Some(v) = line.split('v').find(|p| p.contains('.')) {
                self.config.blisp_version = v.trim().to_string();
            }
        }
        match (Version::from(self.config.blisp_version.as_str()), Version::from("0.0.4")) {
            (Some(have), Some(need)) => have >= need,
            _ => false,
        }
    }

    #[cfg(target_os = "windows")]
    fn blisp_supports_dfu(&mut self) -> bool {
        false // not used: Windows flashes the V2 with the built-in flasher
    }
}

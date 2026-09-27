use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::config::{Config, FileConfig};
use crate::ipc::StatusInfo;
use crate::keys::{self, Key};

use super::bridge;
use super::settings::Settings;

pub struct PatpansApp {
    settings: Settings,
    saved: Settings,
    config_path: PathBuf,
    config_error: Option<String>,
    status: Option<StatusInfo>,
    status_error: Option<String>,
    message: Option<(String, bool)>,
    capture_rx: Option<mpsc::Receiver<anyhow::Result<Option<String>>>>,
    next_poll: Instant,
}

impl Default for PatpansApp {
    fn default() -> Self {
        Self::new()
    }
}

impl PatpansApp {
    pub fn new() -> Self {
        let config_path = bridge::config_path();
        let (settings, config_error) = match load_settings(&config_path) {
            Ok(settings) => (settings, None),
            Err(err) => (
                Settings::from_config(&Config::default()),
                Some(format!("{err:#}")),
            ),
        };
        Self {
            saved: settings.clone(),
            settings,
            config_path,
            config_error,
            status: None,
            status_error: None,
            message: None,
            capture_rx: None,
            next_poll: Instant::now(),
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if Instant::now() >= self.next_poll {
            match bridge::status() {
                Ok(status) => {
                    self.status_error = None;
                    self.adopt_daemon_config(&status);
                    self.status = Some(status);
                }
                Err(err) => {
                    self.status = None;
                    self.status_error = Some(err.message());
                }
            }
            self.next_poll = Instant::now() + Duration::from_millis(500);
        }
        if let Some(rx) = &self.capture_rx
            && let Ok(result) = rx.try_recv()
        {
            match result {
                Ok(Some(name)) => {
                    if let Some(key) = keys::by_name(&name) {
                        self.settings.toggle = Some(key);
                    }
                    self.set_message(format!("captured: {name}"), false);
                }
                Ok(None) => self.set_message("no key captured (timeout)".to_string(), true),
                Err(err) => self.set_message(format!("{err:#}"), true),
            }
            self.capture_rx = None;
        }
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn set_message(&mut self, text: String, error: bool) {
        self.message = Some((text, error));
    }

    fn adopt_daemon_config(&mut self, status: &StatusInfo) {
        let path = PathBuf::from(&status.config_path);
        let changed = path != self.config_path;
        if changed {
            self.config_path = path;
        }
        if self.settings != self.saved {
            if changed {
                self.set_message(
                    format!(
                        "the daemon uses {}; unsaved edits will be saved there",
                        self.config_path.display()
                    ),
                    false,
                );
            }
            return;
        }
        if !changed && self.config_error.is_none() {
            return;
        }
        match load_settings(&self.config_path) {
            Ok(settings) => {
                self.saved = settings.clone();
                self.settings = settings;
                self.config_error = None;
            }
            Err(err) => self.config_error = Some(format!("{err:#}")),
        }
    }

    fn apply(&mut self, result: anyhow::Result<StatusInfo>) {
        match result {
            Ok(status) => self.status = Some(status),
            Err(err) => self.set_message(format!("{err:#}"), true),
        }
    }

    fn apply_unit(&mut self, result: anyhow::Result<()>) {
        match result {
            Ok(()) => self.status = None,
            Err(err) => self.set_message(format!("{err:#}"), true),
        }
    }

    fn start_capture(&mut self) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(bridge::capture_key());
        });
        self.capture_rx = Some(rx);
        self.set_message("waiting for a key press...".to_string(), false);
    }

    fn status_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(status) = &self.status {
            ui.horizontal(|ui| {
                ui.label(format!("daemon v{}", status.version));
                ui.separator();
                let (text, color) = if status.enabled {
                    ("snap tap: ON", egui::Color32::from_rgb(46, 160, 67))
                } else {
                    ("snap tap: OFF", egui::Color32::from_rgb(190, 70, 70))
                };
                ui.colored_label(color, text);
                if status.elevated {
                    ui.separator();
                    ui.label("administrator");
                }
            });
            ui.horizontal(|ui| {
                if ui.button("Toggle").clicked() {
                    self.apply(bridge::toggle());
                }
                if ui.button("Stop daemon").clicked() {
                    self.apply_unit(bridge::stop());
                }
            });
        } else {
            if let Some(err) = &self.status_error {
                ui.colored_label(egui::Color32::from_rgb(190, 70, 70), err);
            } else {
                ui.label("daemon is not running");
            }
            ui.horizontal(|ui| {
                if ui.button("Start daemon").clicked() {
                    self.apply_unit(bridge::start_daemon());
                }
                #[cfg(windows)]
                if ui.button("Start as administrator").clicked() {
                    self.apply_unit(bridge::start_daemon_elevated());
                }
            });
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.horizontal(|ui| {
            ui.label("Toggle key:");
            egui::ComboBox::from_id_salt("toggle")
                .selected_text(self.settings.toggle.map_or("none", |key| key.name))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.settings.toggle, None, "none");
                    for key in keys::KEYS {
                        ui.selectable_value(&mut self.settings.toggle, Some(*key), key.name);
                    }
                });
            let capture_enabled = self.status.is_some() && self.capture_rx.is_none();
            if ui
                .add_enabled(capture_enabled, egui::Button::new("Capture"))
                .clicked()
            {
                self.start_capture();
            }
        });
        ui.checkbox(
            &mut self.settings.sticky,
            "Sticky keys (restore the held key)",
        );
        ui.checkbox(&mut self.settings.tray, "Show the tray icon");
        ui.separator();
        ui.label("Opposite key groups:");
        let mut remove = None;
        for (index, pair) in self.settings.groups.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("#{}", index + 1));
                key_combo(ui, ("group-a", index), &mut pair[0]);
                key_combo(ui, ("group-b", index), &mut pair[1]);
                if ui.button("Remove").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            self.settings.groups.remove(index);
        }
        if ui.button("Add group").clicked() {
            self.settings
                .groups
                .push([keys::default_key("A"), keys::default_key("D")]);
        }
    }

    fn footer_ui(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        let dirty = self.settings != self.saved;
        let can_save = dirty && self.config_error.is_none();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(can_save, egui::Button::new("Save"))
                .clicked()
            {
                match bridge::save_settings(&self.settings, &self.config_path) {
                    Ok(()) => {
                        self.saved = self.settings.clone();
                        self.config_error = None;
                        self.set_message("saved".to_string(), false);
                        if self.status.is_some() {
                            self.apply(bridge::reload());
                        }
                    }
                    Err(err) => self.set_message(format!("{err:#}"), true),
                }
            }
            ui.label(format!("config: {}", self.config_path.display()));
        });
        if let Some(err) = &self.config_error {
            ui.colored_label(
                egui::Color32::from_rgb(190, 70, 70),
                format!("config file error: {err} — saving is disabled until the file is fixed"),
            );
        }
        if let Some((text, error)) = &self.message {
            let color = if *error {
                egui::Color32::from_rgb(190, 70, 70)
            } else {
                egui::Color32::GRAY
            };
            ui.colored_label(color, text);
        }
    }
}

impl eframe::App for PatpansApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        egui::Panel::top("status").show(ui, |ui| {
            self.status_ui(ui);
        });
        egui::Panel::bottom("footer").show(ui, |ui| {
            self.footer_ui(ui);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.settings_ui(ui);
            });
        });
    }
}

fn key_combo(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, key: &mut Key) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(key.name)
        .show_ui(ui, |ui| {
            for candidate in keys::KEYS {
                ui.selectable_value(key, *candidate, candidate.name);
            }
        });
}

fn load_settings(path: &Path) -> anyhow::Result<Settings> {
    Ok(Settings::from_config(
        &FileConfig::load(path)?.into_config()?,
    ))
}

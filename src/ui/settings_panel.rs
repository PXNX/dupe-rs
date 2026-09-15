use crate::app::{DupeApp, ExtensionMode, HashProgress, ScanState};
use crate::config::SizeUnit;
use egui::{Panel, Ui};
use humansize::{DECIMAL, format_size};
use std::time::Duration;

fn format_eta(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn hash_progress_text(progress: &HashProgress) -> String {
    let done = format_size(progress.done_bytes, DECIMAL);
    let total = format_size(progress.total_bytes, DECIMAL);
    match progress.eta() {
        Some(eta) => format!("Hashed {done} / {total} — ETA {}", format_eta(eta)),
        None if progress.done_bytes >= progress.total_bytes && progress.total_bytes > 0 => {
            format!("Hashed {done} / {total} — finishing up...")
        }
        None => format!("Hashed {done} / {total} — estimating..."),
    }
}

pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    Panel::top("settings_panel").show(ui, |ui| {
        ui.add_space(4.0);
        ui.heading("dupe-rs");

        ui.horizontal(|ui| {
            if ui.button("Add Folder(s)").clicked()
                && let Some(folders) = rfd::FileDialog::new().pick_folders()
            {
                for folder in folders {
                    if !app.config.folders.contains(&folder) {
                        app.config.folders.push(folder);
                    }
                }
            }
            ui.checkbox(&mut app.config.exclude_subfolders, "Exclude subfolders");
        });

        let mut remove_index = None;
        for (i, folder) in app.config.folders.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui.small_button("x").clicked() {
                    remove_index = Some(i);
                }
                ui.label(folder.display().to_string());
            });
        }
        if let Some(i) = remove_index {
            app.config.folders.remove(i);
        }

        ui.horizontal(|ui| {
            ui.label("Size range:");
            ui.add(egui::TextEdit::singleline(&mut app.min_size_text).desired_width(60.0));
            ui.label("to");
            ui.add(egui::TextEdit::singleline(&mut app.max_size_text).desired_width(60.0));
            egui::ComboBox::from_id_salt("size_unit")
                .selected_text(app.size_unit.label())
                .show_ui(ui, |ui| {
                    for unit in SizeUnit::ALL {
                        ui.selectable_value(&mut app.size_unit, unit, unit.label());
                    }
                });
        });

        ui.horizontal(|ui| {
            ui.label("Extensions:");
            ui.radio_value(&mut app.extension_mode, ExtensionMode::All, "All");
            ui.radio_value(&mut app.extension_mode, ExtensionMode::Include, "Include");
            ui.radio_value(&mut app.extension_mode, ExtensionMode::Exclude, "Exclude");
            if app.extension_mode != ExtensionMode::All {
                ui.add(
                    egui::TextEdit::singleline(&mut app.extension_text)
                        .hint_text("jpg, png, mp4")
                        .desired_width(200.0),
                );
            }
        });

        ui.horizontal(|ui| {
            let scanning = app.is_scanning();
            let can_scan = !app.config.folders.is_empty() && !scanning;
            if scanning {
                if ui.button("Cancel").clicked() {
                    app.cancel_scan();
                }
            } else if ui
                .add_enabled(can_scan, egui::Button::new("Scan"))
                .clicked()
            {
                app.start_scan();
            }

            match &app.scan_state {
                ScanState::Running {
                    scanned,
                    hash_progress,
                    ..
                } => {
                    ui.spinner();
                    match hash_progress {
                        Some(progress) => ui.label(hash_progress_text(progress)),
                        None => ui.label(format!("Scanned {scanned} files...")),
                    };
                }
                ScanState::Done { elapsed_ms } => {
                    ui.label(format!(
                        "Done in {:.2}s — {} duplicate group(s) found.",
                        *elapsed_ms as f64 / 1000.0,
                        app.groups.len()
                    ));
                }
                ScanState::Idle => {}
            }
        });

        ui.add_space(4.0);
    });
}

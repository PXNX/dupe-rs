use crate::app::DupeApp;
use chrono::{DateTime, Local};
use egui::Ui;
use egui_extras::{Column, TableBuilder};
use humansize::{DECIMAL, format_size};
use std::path::PathBuf;
use std::time::SystemTime;

fn format_modified(t: SystemTime) -> String {
    let dt: DateTime<Local> = t.into();
    dt.format("%Y-%m-%d %H:%M").to_string()
}

fn hex_prefix(hash: &[u8; 32]) -> String {
    hash[..8].iter().map(|b| format!("{b:02x}")).collect()
}

pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    if app.groups.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label("No duplicate groups yet. Add folders above and click Scan.");
        });
        return;
    }

    let mut toggled: Vec<PathBuf> = Vec::new();

    TableBuilder::new(ui)
        .id_salt("results_table")
        .striped(true)
        .column(Column::auto().at_least(24.0))
        .column(Column::remainder().at_least(200.0))
        .column(Column::auto().at_least(80.0))
        .column(Column::auto().at_least(120.0))
        .column(Column::auto().at_least(60.0))
        .header(20.0, |mut header| {
            header.col(|ui| {
                ui.label("");
            });
            header.col(|ui| {
                ui.label("Filename");
            });
            header.col(|ui| {
                ui.label("Size");
            });
            header.col(|ui| {
                ui.label("Modified");
            });
            header.col(|ui| {
                ui.label("Group");
            });
        })
        .body(|mut body| {
            for (group_idx, group) in app.groups.iter().enumerate() {
                let skip = if app.only_show_duplicates { 1 } else { 0 };
                for (file_idx, file) in group.files.iter().enumerate().skip(skip) {
                    let is_original = file_idx == 0;
                    let mut checked = app.selection.contains(&file.path);
                    body.row(22.0, |mut row| {
                        row.col(|ui| {
                            if ui.checkbox(&mut checked, "").changed() {
                                toggled.push(file.path.clone());
                            }
                        });
                        row.col(|ui| {
                            let name = file
                                .path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default();
                            if is_original {
                                ui.colored_label(
                                    egui::Color32::LIGHT_GREEN,
                                    format!("{name}  [Original]"),
                                );
                            } else {
                                ui.label(name);
                            }
                        });
                        row.col(|ui| {
                            ui.label(format_size(file.size, DECIMAL));
                        });
                        row.col(|ui| {
                            ui.label(format_modified(file.modified));
                        });
                        row.col(|ui| {
                            ui.label((group_idx + 1).to_string())
                                .on_hover_text(format!("hash: {}", hex_prefix(&group.hash)));
                        });
                    });
                }
            }
        });

    for path in toggled {
        if !app.selection.remove(&path) {
            app.selection.insert(path);
        }
    }
}

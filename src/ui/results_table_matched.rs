use crate::app::{DupeApp, SortColumn};
use crate::ui::controls::sort_header;
use crate::ui::format::format_timestamp;
use egui::{Sense, Ui};
use egui_extras::{Column, TableBuilder};
use humansize::{DECIMAL, format_size};
use std::path::PathBuf;

const ROW_HEIGHT: f32 = 22.0;

/// Flat, sortable list for `ScanMode::MatchingFiles`: every file that passed
/// the filters, to select and delete like duplicates. Only rows in view are
/// built, so very long lists stay responsive.
pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    if app.matched_files.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(if app.is_scanning() {
                "Looking for matching files..."
            } else {
                "No matching files yet. Set the size, extension, or name filters above and click Scan."
            });
        });
        return;
    }

    let order = app.matched_order().to_vec();
    let mut toggled: Vec<PathBuf> = Vec::new();
    let mut open_path: Option<PathBuf> = None;
    let mut sort = app.sort;

    TableBuilder::new(ui)
        .id_salt("results_table_matched")
        .striped(true)
        .column(Column::auto().at_least(24.0))
        .column(Column::remainder().at_least(160.0).resizable(true))
        .column(Column::remainder().at_least(200.0).resizable(true))
        .column(Column::auto().at_least(80.0).resizable(true))
        .column(Column::auto().at_least(120.0).resizable(true))
        .column(Column::auto().at_least(120.0).resizable(true))
        .header(20.0, |mut header| {
            header.col(|ui| {
                ui.label("");
            });
            for (label, column) in [
                ("Filename", SortColumn::Filename),
                ("Folder", SortColumn::Path),
                ("Size", SortColumn::Size),
                ("Created", SortColumn::Created),
                ("Modified", SortColumn::Modified),
            ] {
                header.col(|ui| sort_header(ui, label, column, &mut sort));
            }
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, order.len(), |mut row| {
                let file = &app.matched_files[order[row.index()]];
                let mut checked = app.selection.contains(&file.path);
                row.col(|ui| {
                    if ui.checkbox(&mut checked, "").changed() {
                        toggled.push(file.path.clone());
                    }
                });
                row.col(|ui| {
                    let name = file
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if ui
                        .add(egui::Label::new(name).truncate().sense(Sense::click()))
                        .double_clicked()
                    {
                        open_path = Some(file.path.clone());
                    }
                });
                row.col(|ui| {
                    let parent = file
                        .path
                        .parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    ui.add(egui::Label::new(parent).truncate())
                        .on_hover_text(file.path.display().to_string());
                });
                row.col(|ui| {
                    ui.label(format_size(file.size, DECIMAL));
                });
                row.col(|ui| {
                    ui.label(format_timestamp(file.created));
                });
                row.col(|ui| {
                    ui.label(format_timestamp(file.modified));
                });
            });
        });

    app.sort = sort;
    for path in toggled {
        if !app.selection.remove(&path) {
            app.selection.insert(path);
        }
    }
    if let Some(path) = open_path
        && let Err(err) = open::that(&path)
    {
        app.status_message = Some(format!("Couldn't open {}: {err}", path.display()));
    }
}

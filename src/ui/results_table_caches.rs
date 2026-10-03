use crate::app::{CacheSortColumn, DupeApp};
use crate::ui::controls::sort_header;
use crate::ui::format::format_timestamp;
use egui::{Sense, Ui};
use egui_extras::{Column, TableBuilder};
use humansize::{DECIMAL, format_size};
use std::path::PathBuf;

const ROW_HEIGHT: f32 = 22.0;

/// Flat, sortable list for `ScanMode::BuildCaches`: one row per cache
/// folder, to review and then select and delete like duplicates. Only rows
/// in view are built, so very long lists stay responsive.
pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    if app.cache_dirs.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(if app.is_scanning() {
                "Looking for build caches..."
            } else {
                "No build caches yet. Add folders above and click Scan."
            });
        });
        return;
    }

    let order = app.cache_order().to_vec();
    let mut toggled: Vec<PathBuf> = Vec::new();
    let mut open_path: Option<PathBuf> = None;
    let mut sort = app.cache_sort;

    TableBuilder::new(ui)
        .id_salt("results_table_caches")
        .striped(true)
        .column(Column::auto().at_least(24.0))
        .column(Column::auto().at_least(140.0).resizable(true))
        .column(Column::remainder().at_least(200.0).resizable(true))
        .column(Column::auto().at_least(120.0).resizable(true))
        .column(Column::auto().at_least(80.0).resizable(true))
        .column(Column::auto().at_least(70.0).resizable(true))
        .column(Column::auto().at_least(120.0).resizable(true))
        .header(20.0, |mut header| {
            header.col(|ui| {
                ui.label("");
            });
            for (label, column) in [
                ("Folder", CacheSortColumn::Name),
                ("Project", CacheSortColumn::Project),
                ("Type", CacheSortColumn::Kind),
                ("Size", CacheSortColumn::Size),
                ("Files", CacheSortColumn::Files),
                ("Last modified", CacheSortColumn::Modified),
            ] {
                header.col(|ui| sort_header(ui, label, column, &mut sort));
            }
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, order.len(), |mut row| {
                let dir = &app.cache_dirs[order[row.index()]];
                let mut checked = app.selection.contains(&dir.path);
                row.col(|ui| {
                    if ui.checkbox(&mut checked, "").changed() {
                        toggled.push(dir.path.clone());
                    }
                });
                row.col(|ui| {
                    let name = dir
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if ui
                        .add(egui::Label::new(name).truncate().sense(Sense::click()))
                        .on_hover_text("Double-click to open")
                        .double_clicked()
                    {
                        open_path = Some(dir.path.clone());
                    }
                });
                row.col(|ui| {
                    let parent = dir
                        .path
                        .parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    ui.add(egui::Label::new(parent).truncate())
                        .on_hover_text(dir.path.display().to_string());
                });
                row.col(|ui| {
                    ui.label(dir.kind.label());
                });
                row.col(|ui| {
                    ui.label(format_size(dir.size, DECIMAL));
                });
                row.col(|ui| {
                    ui.label(dir.file_count.to_string());
                });
                row.col(|ui| {
                    ui.label(format_timestamp(dir.modified));
                });
            });
        });

    app.cache_sort = sort;
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

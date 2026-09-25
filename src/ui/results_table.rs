use crate::app::{DupeApp, SortColumn};
use crate::ui::controls::sort_header;
use crate::ui::format::{format_timestamp, hex_prefix};
use egui::{Sense, Ui};
use egui_extras::{Column, TableBuilder};
use humansize::{DECIMAL, format_size};
use std::path::PathBuf;

const ROW_HEIGHT: f32 = 22.0;

fn cell_text(text: String, is_winner: bool) -> egui::RichText {
    let rt = egui::RichText::new(text);
    if is_winner { rt.strong() } else { rt }
}

/// Renders the exact-duplicates results as a table. Reads the pre-filtered,
/// pre-sorted row list from `app.exact_rows_cache` (refreshed once per frame
/// in `DupeApp::ui`, not rebuilt here) and hands it to `TableBody::rows`,
/// which only constructs widgets for rows actually within the scrolled
/// viewport — together these two things are what let this stay responsive
/// on scans with hundreds of thousands of duplicate files.
pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    if app.groups.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label("No duplicate groups yet. Add folders above and click Scan.");
        });
        return;
    }

    let row_count = app.exact_rows_cache.rows.len();
    let mut toggled: Vec<PathBuf> = Vec::new();
    let mut open_path: Option<PathBuf> = None;

    TableBuilder::new(ui)
        .id_salt("results_table")
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
            header.col(|ui| {
                sort_header(ui, "Filename", SortColumn::Filename, &mut app.sort);
            });
            header.col(|ui| {
                sort_header(ui, "Path", SortColumn::Path, &mut app.sort);
            });
            header.col(|ui| {
                sort_header(ui, "Size", SortColumn::Size, &mut app.sort);
            });
            header.col(|ui| {
                sort_header(ui, "Created", SortColumn::Created, &mut app.sort);
            });
            header.col(|ui| {
                sort_header(ui, "Modified", SortColumn::Modified, &mut app.sort);
            });
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, row_count, |mut row| {
                let info = &app.exact_rows_cache.rows[row.index()];
                let mut checked = app.selection.contains(&info.path);
                if info.is_group_start && row.index() != 0 {
                    row.set_overline(true);
                }

                row.col(|ui| {
                    if ui.checkbox(&mut checked, "").changed() {
                        toggled.push(info.path.clone());
                    }
                });
                row.col(|ui| {
                    let name = info
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let text = if info.is_original {
                        egui::RichText::new(format!("{name}  [Original]"))
                            .color(egui::Color32::LIGHT_GREEN)
                    } else {
                        egui::RichText::new(name)
                    };
                    let response = ui.add(egui::Label::new(text).sense(Sense::click()));
                    let response = response.on_hover_text(format!("hash: {}", hex_prefix(&info.hash)));
                    if response.double_clicked() {
                        open_path = Some(info.path.clone());
                    }
                });
                row.col(|ui| {
                    let parent = info
                        .path
                        .parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    ui.add(egui::Label::new(parent).truncate())
                        .on_hover_text(info.path.display().to_string());
                });
                row.col(|ui| {
                    ui.label(cell_text(format_size(info.size, DECIMAL), info.is_largest_size));
                });
                row.col(|ui| {
                    ui.label(cell_text(format_timestamp(info.created), info.is_oldest_created));
                });
                row.col(|ui| {
                    ui.label(cell_text(format_timestamp(info.modified), info.is_oldest_modified));
                });
            });
        });

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

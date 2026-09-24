use crate::app::DupeApp;
use crate::model::SimilarGroup;
use crate::ui::format::format_timestamp;
use egui::{Color32, Sense, Stroke, Ui};
use egui_extras::{Column, TableBuilder};
use humansize::{DECIMAL, format_size};
use std::path::PathBuf;

const GROUP_GAP_HEIGHT: f32 = 8.0;

/// Indices, within a group's files, of the row that "wins" each highlighted
/// column — drawn in bold so a glance shows which copy is largest/highest-res
/// /oldest, independent of which one is the overall `[Original]`.
struct GroupHighlights {
    largest_size: usize,
    highest_resolution: usize,
    oldest_created: usize,
    oldest_modified: usize,
}

impl GroupHighlights {
    fn compute(group: &SimilarGroup) -> Self {
        let max_by = |key: fn(&crate::model::MediaEntry) -> u64| {
            group
                .files
                .iter()
                .enumerate()
                .max_by_key(|(_, f)| key(f))
                .map(|(i, _)| i)
                .unwrap_or(0)
        };
        Self {
            largest_size: max_by(|f| f.size),
            highest_resolution: max_by(|f| f.width as u64 * f.height as u64),
            oldest_created: group
                .files
                .iter()
                .enumerate()
                .min_by_key(|(_, f)| f.created)
                .map(|(i, _)| i)
                .unwrap_or(0),
            oldest_modified: group
                .files
                .iter()
                .enumerate()
                .min_by_key(|(_, f)| f.modified)
                .map(|(i, _)| i)
                .unwrap_or(0),
        }
    }
}

fn cell_text(text: String, is_winner: bool) -> egui::RichText {
    let rt = egui::RichText::new(text);
    if is_winner { rt.strong() } else { rt }
}

/// Table for `ScanMode::SimilarMedia` results: same checkbox/group-divider
/// treatment as the exact-match table, plus a Resolution column, but no
/// column sorting or grid view (a picture grid over a totally different
/// grouping shape wasn't worth the added complexity for this first pass).
pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    if app.similar_groups.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(
                "No similar-media groups yet. Add folders above and click Scan \
                 (mode: Similar media).",
            );
        });
        return;
    }

    let mut toggled: Vec<PathBuf> = Vec::new();
    let mut open_path: Option<PathBuf> = None;
    let table_x_range = ui.max_rect().x_range();

    TableBuilder::new(ui)
        .id_salt("results_table_similar")
        .striped(true)
        .column(Column::auto().at_least(24.0))
        .column(Column::remainder().at_least(160.0).resizable(true))
        .column(Column::remainder().at_least(200.0).resizable(true))
        .column(Column::auto().at_least(100.0).resizable(true))
        .column(Column::auto().at_least(80.0).resizable(true))
        .column(Column::auto().at_least(120.0).resizable(true))
        .column(Column::auto().at_least(120.0).resizable(true))
        .header(20.0, |mut header| {
            header.col(|ui| {
                ui.label("");
            });
            header.col(|ui| {
                ui.label("Filename");
            });
            header.col(|ui| {
                ui.label("Path");
            });
            header.col(|ui| {
                ui.label("Resolution");
            });
            header.col(|ui| {
                ui.label("Size");
            });
            header.col(|ui| {
                ui.label("Created");
            });
            header.col(|ui| {
                ui.label("Modified");
            });
        })
        .body(|mut body| {
            let groups: &[SimilarGroup] = &app.similar_groups;
            let skip = if app.only_show_duplicates { 1 } else { 0 };
            let last_group = groups.len().saturating_sub(1);
            for (group_pos, group) in groups.iter().enumerate() {
                let highlights = GroupHighlights::compute(group);
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
                            let text = if is_original {
                                egui::RichText::new(format!("{name}  [Original]"))
                                    .color(egui::Color32::LIGHT_GREEN)
                            } else {
                                egui::RichText::new(name)
                            };
                            let response = ui.add(egui::Label::new(text).sense(Sense::click()));
                            if response.double_clicked() {
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
                            ui.label(cell_text(
                                format!("{}×{}", file.width, file.height),
                                file_idx == highlights.highest_resolution,
                            ));
                        });
                        row.col(|ui| {
                            ui.label(cell_text(
                                format_size(file.size, DECIMAL),
                                file_idx == highlights.largest_size,
                            ));
                        });
                        row.col(|ui| {
                            ui.label(cell_text(
                                format_timestamp(file.created),
                                file_idx == highlights.oldest_created,
                            ));
                        });
                        row.col(|ui| {
                            ui.label(cell_text(
                                format_timestamp(file.modified),
                                file_idx == highlights.oldest_modified,
                            ));
                        });
                    });
                }

                if group_pos != last_group {
                    body.row(GROUP_GAP_HEIGHT, |mut row| {
                        row.col(|ui| {
                            let y = ui.max_rect().center().y;
                            ui.ctx().layer_painter(ui.layer_id()).hline(
                                table_x_range,
                                y,
                                Stroke::new(1.5, Color32::from_gray(90)),
                            );
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

    if let Some(path) = open_path
        && let Err(err) = open::that(&path)
    {
        app.status_message = Some(format!("Couldn't open {}: {err}", path.display()));
    }
}

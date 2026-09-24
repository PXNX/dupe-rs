use crate::app::{DupeApp, SortColumn, SortDirection};
use crate::model::DupeGroup;
use crate::selection::filter_by_name_pattern;
use crate::ui::format::{format_timestamp, hex_prefix};
use egui::{Color32, Sense, Stroke, Ui};
use egui_extras::{Column, TableBuilder};
use egui_material_icons::icons;
use humansize::{DECIMAL, format_size};
use std::path::PathBuf;

const GROUP_GAP_HEIGHT: f32 = 10.0;
const COLUMN_COUNT: usize = 6;

/// Renders a header label that cycles Asc -> Desc -> unsorted on each click,
/// showing an arrow when it's the active sort column.
fn sort_header(
    ui: &mut Ui,
    label: &str,
    column: SortColumn,
    sort: &mut Option<(SortColumn, SortDirection)>,
) {
    let arrow = match sort {
        Some((c, SortDirection::Asc)) if *c == column => icons::ICON_ARROW_UPWARD.codepoint,
        Some((c, SortDirection::Desc)) if *c == column => icons::ICON_ARROW_DOWNWARD.codepoint,
        _ => "",
    };
    let text = if arrow.is_empty() {
        label.to_string()
    } else {
        format!("{label} {arrow}")
    };
    if ui.add(egui::Button::new(text).frame(false)).clicked() {
        *sort = match sort {
            Some((c, SortDirection::Asc)) if *c == column => Some((column, SortDirection::Desc)),
            Some((c, SortDirection::Desc)) if *c == column => None,
            _ => Some((column, SortDirection::Asc)),
        };
    }
}

/// Orders groups by the given column/direction using each group's original
/// (index 0) file as the representative — individual files within a group
/// keep their original-first order so the "original" highlighting still
/// makes sense.
fn ordered_groups(
    groups: Vec<&DupeGroup>,
    sort: Option<(SortColumn, SortDirection)>,
) -> Vec<&DupeGroup> {
    let mut ordered = groups;
    if let Some((column, direction)) = sort {
        ordered.sort_by(|a, b| {
            let ord = match column {
                SortColumn::Filename => {
                    let a_name = a.files[0]
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    let b_name = b.files[0]
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    a_name.cmp(&b_name)
                }
                SortColumn::Path => a.files[0].path.cmp(&b.files[0].path),
                SortColumn::Size => a.files[0].size.cmp(&b.files[0].size),
                SortColumn::Created => a.files[0].created.cmp(&b.files[0].created),
                SortColumn::Modified => a.files[0].modified.cmp(&b.files[0].modified),
            };
            if direction == SortDirection::Desc {
                ord.reverse()
            } else {
                ord
            }
        });
    }
    ordered
}

pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    if app.groups.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label("No duplicate groups yet. Add folders above and click Scan.");
        });
        return;
    }

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
        .body(|mut body| {
            let filtered = filter_by_name_pattern(&app.groups, app.only_show_name_copies);
            let ordered = ordered_groups(filtered, app.sort);
            let skip = if app.only_show_duplicates { 1 } else { 0 };
            let last_group = ordered.len().saturating_sub(1);
            for (group_pos, group) in ordered.into_iter().enumerate() {
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
                            let response = response
                                .on_hover_text(format!("hash: {}", hex_prefix(&group.hash)));
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
                            ui.label(format_size(file.size, DECIMAL));
                        });
                        row.col(|ui| {
                            ui.label(format_timestamp(file.created));
                        });
                        row.col(|ui| {
                            ui.label(format_timestamp(file.modified));
                        });
                    });
                }

                if group_pos != last_group {
                    body.row(GROUP_GAP_HEIGHT, |mut row| {
                        for _ in 0..COLUMN_COUNT {
                            row.col(|ui| {
                                let rect = ui.max_rect();
                                ui.painter().hline(
                                    rect.x_range(),
                                    rect.center().y,
                                    Stroke::new(3.0, Color32::from_gray(90)),
                                );
                            });
                        }
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

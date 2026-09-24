use crate::app::DupeApp;
use crate::selection::filter_by_name_pattern;
use crate::ui::format::{format_timestamp, hex_prefix};
use crate::ui::thumbnails::ThumbState;
use egui::{Color32, Sense, Stroke, Ui, vec2};
use egui_material_icons::{MaterialIcon, icons};
use humansize::{DECIMAL, format_size};
use std::path::PathBuf;
use std::time::SystemTime;

const THUMB_SIZE: f32 = 128.0;
const CELL_WIDTH: f32 = THUMB_SIZE + 24.0;
const CELL_HEIGHT: f32 = THUMB_SIZE + 56.0;
const DIVIDER_ZONE: f32 = 10.0;
const ROW_HEIGHT: f32 = CELL_HEIGHT + DIVIDER_ZONE;

#[derive(Clone)]
struct GridEntry {
    path: PathBuf,
    file_name: String,
    size: u64,
    created: SystemTime,
    modified: SystemTime,
    hash: [u8; 32],
    is_original: bool,
}

/// A row of up to `columns` cells belonging to a single duplicate group. Groups
/// never share a row (a short trailing row is padded rather than filled with
/// the next group's files), so a thick divider drawn above a group-start row
/// always spans the full row cleanly.
struct GridRow {
    entries: Vec<GridEntry>,
    is_group_start: bool,
}

pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    app.thumbnail_cache.poll(ui.ctx());

    if app.groups.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label("No duplicate groups yet. Add folders above and click Scan.");
        });
        return;
    }

    let spacing = ui.spacing().item_spacing.x;
    let available_width = ui.available_width().max(CELL_WIDTH);
    let columns = ((available_width + spacing) / (CELL_WIDTH + spacing))
        .floor()
        .max(1.0) as usize;
    // Spread any leftover width evenly between cells instead of leaving it as
    // dead space on the right edge.
    let used_width = columns as f32 * CELL_WIDTH + (columns.saturating_sub(1)) as f32 * spacing;
    let extra_gap = if columns > 1 {
        (available_width - used_width).max(0.0) / (columns - 1) as f32
    } else {
        0.0
    };

    let skip = if app.only_show_duplicates { 1 } else { 0 };
    let filtered = filter_by_name_pattern(&app.groups, app.only_show_name_copies);
    let mut rows: Vec<GridRow> = Vec::new();
    for group in filtered {
        let group_entries: Vec<GridEntry> = group
            .files
            .iter()
            .enumerate()
            .skip(skip)
            .map(|(idx, f)| GridEntry {
                path: f.path.clone(),
                file_name: f
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                size: f.size,
                created: f.created,
                modified: f.modified,
                hash: group.hash,
                is_original: idx == 0,
            })
            .collect();
        if group_entries.is_empty() {
            continue;
        }
        for (chunk_idx, chunk) in group_entries.chunks(columns).enumerate() {
            rows.push(GridRow {
                entries: chunk.to_vec(),
                is_group_start: chunk_idx == 0,
            });
        }
    }

    let mut toggled: Vec<PathBuf> = Vec::new();
    let total_rows = rows.len();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, ROW_HEIGHT, total_rows, |ui, row_range| {
            for row_idx in row_range {
                let row = &rows[row_idx];

                let (divider_rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), DIVIDER_ZONE), Sense::hover());
                if row.is_group_start && row_idx != 0 {
                    ui.painter().hline(
                        divider_rect.x_range(),
                        divider_rect.center().y,
                        Stroke::new(3.0, Color32::from_gray(90)),
                    );
                }

                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.x = spacing + extra_gap;
                    ui.horizontal(|ui| {
                        for entry in &row.entries {
                            let mut checked = app.selection.contains(&entry.path);
                            let thumb_state = app.thumbnail_cache.get_or_request(&entry.path);
                            if draw_cell(ui, entry, &mut checked, thumb_state) {
                                toggled.push(entry.path.clone());
                            }
                        }
                    });
                });
            }
        });

    for path in toggled {
        if !app.selection.remove(&path) {
            app.selection.insert(path);
        }
    }
}

fn draw_cell(ui: &mut Ui, entry: &GridEntry, checked: &mut bool, thumb_state: ThumbState) -> bool {
    let mut toggled = false;
    let response = ui
        .allocate_ui(vec2(CELL_WIDTH, CELL_HEIGHT), |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    if ui.checkbox(checked, "").changed() {
                        toggled = true;
                    }
                    match thumb_state {
                        ThumbState::Ready(handle) => {
                            ui.add(
                                egui::Image::new(&handle)
                                    .fit_to_exact_size(vec2(THUMB_SIZE, THUMB_SIZE)),
                            );
                        }
                        ThumbState::Loading => {
                            ui.allocate_ui(vec2(THUMB_SIZE, THUMB_SIZE), |ui| {
                                ui.centered_and_justified(|ui| {
                                    ui.spinner();
                                });
                            });
                        }
                        ThumbState::Failed | ThumbState::Unsupported => {
                            draw_generic_icon(ui, THUMB_SIZE, icon_for_extension(&entry.file_name));
                        }
                    }
                });

                let display_name = if entry.is_original {
                    format!("{} [Original]", entry.file_name)
                } else {
                    entry.file_name.clone()
                };
                ui.add(egui::Label::new(truncate(&display_name, 22)).truncate());
                ui.label(format_size(entry.size, DECIMAL));
            });
        })
        .response;
    response.on_hover_ui(|ui| show_hover(ui, entry));
    toggled
}

fn show_hover(ui: &mut Ui, entry: &GridEntry) {
    ui.strong(&entry.file_name);
    if entry.is_original {
        ui.colored_label(Color32::LIGHT_GREEN, "Original");
    }
    ui.separator();
    ui.label(format!("Path: {}", entry.path.display()));
    ui.label(format!("Size: {}", format_size(entry.size, DECIMAL)));
    ui.label(format!("Created: {}", format_timestamp(entry.created)));
    ui.label(format!("Modified: {}", format_timestamp(entry.modified)));
    ui.label(format!("Hash: {}", hex_prefix(&entry.hash)));
}

fn draw_generic_icon(ui: &mut Ui, size: f32, icon: MaterialIcon) {
    let (rect, _response) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().rect_filled(rect, 6.0, Color32::from_gray(60));
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon.codepoint,
        egui::FontId::new(size * 0.4, icon.font_family()),
        Color32::WHITE,
    );
}

fn icon_for_extension(file_name: &str) -> MaterialIcon {
    let ext = file_name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "mp4" | "mkv" | "avi" | "mov" | "webm" => icons::ICON_MOVIE,
        "mp3" | "wav" | "flac" | "ogg" | "m4a" => icons::ICON_MUSIC_NOTE,
        "pdf" => icons::ICON_PICTURE_AS_PDF,
        "zip" | "rar" | "7z" | "tar" | "gz" => icons::ICON_ARCHIVE,
        "rs" | "py" | "js" | "ts" | "c" | "cpp" | "java" | "go" | "html" | "css" => {
            icons::ICON_CODE
        }
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" => icons::ICON_IMAGE,
        _ => icons::ICON_INSERT_DRIVE_FILE,
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

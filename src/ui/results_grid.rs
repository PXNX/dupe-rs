use crate::app::DupeApp;
use crate::ui::thumbnails::ThumbState;
use egui::{Color32, FontId, Sense, Ui, vec2};
use humansize::{DECIMAL, format_size};
use std::path::PathBuf;

const THUMB_SIZE: f32 = 128.0;
const CELL_WIDTH: f32 = THUMB_SIZE + 24.0;
const CELL_HEIGHT: f32 = THUMB_SIZE + 56.0;

struct GridEntry {
    path: PathBuf,
    file_name: String,
    size: u64,
    is_original: bool,
}

pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    app.thumbnail_cache.poll(ui.ctx());

    if app.groups.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label("No duplicate groups yet. Add folders above and click Scan.");
        });
        return;
    }

    let mut entries: Vec<GridEntry> = Vec::new();
    let skip = if app.only_show_duplicates { 1 } else { 0 };
    for group in &app.groups {
        for (file_idx, file) in group.files.iter().enumerate().skip(skip) {
            entries.push(GridEntry {
                path: file.path.clone(),
                file_name: file
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                size: file.size,
                is_original: file_idx == 0,
            });
        }
    }

    let spacing = ui.spacing().item_spacing.x;
    let available_width = ui.available_width().max(CELL_WIDTH);
    let columns = ((available_width + spacing) / (CELL_WIDTH + spacing))
        .floor()
        .max(1.0) as usize;
    let total_rows = entries.len().div_ceil(columns);

    let mut toggled: Vec<PathBuf> = Vec::new();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, CELL_HEIGHT, total_rows, |ui, row_range| {
            for row in row_range {
                ui.horizontal(|ui| {
                    for col in 0..columns {
                        let idx = row * columns + col;
                        let Some(entry) = entries.get(idx) else {
                            break;
                        };
                        let mut checked = app.selection.contains(&entry.path);
                        let thumb_state = app.thumbnail_cache.get_or_request(&entry.path);
                        if draw_cell(ui, entry, &mut checked, thumb_state) {
                            toggled.push(entry.path.clone());
                        }
                    }
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
    ui.allocate_ui(vec2(CELL_WIDTH, CELL_HEIGHT), |ui| {
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
                        draw_generic_icon(ui, THUMB_SIZE, &extension_label(&entry.file_name));
                    }
                }
            });

            let display_name = if entry.is_original {
                format!("{} [Original]", entry.file_name)
            } else {
                entry.file_name.clone()
            };
            ui.add(egui::Label::new(truncate(&display_name, 22)).truncate())
                .on_hover_text(&entry.file_name);
            ui.label(format_size(entry.size, DECIMAL));
        });
    });
    toggled
}

fn draw_generic_icon(ui: &mut Ui, size: f32, label: &str) {
    let (rect, _response) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().rect_filled(rect, 6.0, Color32::from_gray(60));
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label.to_uppercase(),
        FontId::proportional(16.0),
        Color32::WHITE,
    );
}

fn extension_label(file_name: &str) -> String {
    file_name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_string())
        .unwrap_or_else(|| "file".to_string())
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

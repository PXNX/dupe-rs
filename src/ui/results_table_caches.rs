use crate::app::{CacheSortColumn, DupeApp};
use crate::build_cache::CacheKind;
use crate::ui::controls::sort_header;
use crate::ui::format::format_timestamp;
use egui::{Color32, ImageSource, Response, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use egui_material_icons::icons;
use humansize::{DECIMAL, format_size};
use std::path::PathBuf;

const ROW_HEIGHT: f32 = 22.0;
const ICON_SIZE: f32 = 14.0;

/// Brand logo for each kind of cache, from Simple Icons (CC0, simpleicons.org)
/// with each fill set to a color that reads on light and dark themes.
fn kind_logo(kind: CacheKind) -> Option<ImageSource<'static>> {
    Some(match kind {
        CacheKind::Node => egui::include_image!("../../assets/cache_icons/node.svg"),
        CacheKind::Python => egui::include_image!("../../assets/cache_icons/python.svg"),
        CacheKind::Rust => egui::include_image!("../../assets/cache_icons/rust.svg"),
        CacheKind::Gradle => egui::include_image!("../../assets/cache_icons/android.svg"),
        CacheKind::Maven => egui::include_image!("../../assets/cache_icons/java.svg"),
        CacheKind::DotNet => egui::include_image!("../../assets/cache_icons/dotnet.svg"),
        CacheKind::Flutter => egui::include_image!("../../assets/cache_icons/flutter.svg"),
        // Not one ecosystem, so no brand to show.
        CacheKind::Tagged => return None,
    })
}

/// Shows `kind`'s logo, for the table's Type column and the settings
/// panel's kind checkboxes.
pub fn kind_icon(ui: &mut Ui, kind: CacheKind) -> Response {
    match kind_logo(kind) {
        Some(logo) => ui.add(egui::Image::new(logo).fit_to_exact_size(egui::vec2(ICON_SIZE, ICON_SIZE))),
        None => ui.label(icons::ICON_SELL.rich_text().color(Color32::GRAY)),
    }
}

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
                    kind_icon(ui, dir.kind);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_logo_rasterizes_to_visible_pixels() {
        for kind in CacheKind::ALL {
            let Some(ImageSource::Bytes { bytes, .. }) = kind_logo(kind) else {
                continue;
            };
            let image = egui_extras::image::load_svg_bytes(&bytes, &Default::default())
                .unwrap_or_else(|e| panic!("{kind:?} logo: {e}"));
            assert!(
                image.pixels.iter().any(|p| p.a() > 0),
                "{kind:?} logo is blank"
            );
        }
    }
}

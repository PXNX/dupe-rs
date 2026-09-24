use crate::app::DupeApp;
use crate::reverse_search::IndexState;
use crate::ui::format::format_timestamp;
use egui::{Color32, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use egui_material_icons::icons;
use humansize::{DECIMAL, format_size};

pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    ui.heading("Reverse Search");
    ui.label(
        "Build a searchable index of file hashes ahead of time, then pick a single file to \
         find every other indexed copy of it — even ones on drives that aren't attached right \
         now, since matches are looked up by volume rather than a live path.",
    );
    ui.add_space(8.0);

    show_index_section(app, ui);
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(8.0);
    show_search_section(app, ui);
}

fn show_index_section(app: &mut DupeApp, ui: &mut Ui) {
    ui.group(|ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(icons::ICON_STORAGE.rich_text());
            ui.strong("Index folders");
            if ui
                .button(RichText::from(format!(
                    "{} Add Folder(s)",
                    icons::ICON_FOLDER_OPEN.codepoint
                )))
                .clicked()
                && let Some(folders) = rfd::FileDialog::new().pick_folders()
            {
                for folder in folders {
                    if !app.reverse_search.index_folders.contains(&folder) {
                        app.reverse_search.index_folders.push(folder);
                    }
                }
            }
        });

        let mut remove_index = None;
        for (i, folder) in app.reverse_search.index_folders.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui
                    .small_button(icons::ICON_CLOSE.codepoint)
                    .on_hover_text("Remove")
                    .clicked()
                {
                    remove_index = Some(i);
                }
                ui.label(format!("{} {}", icons::ICON_FOLDER.codepoint, folder.display()));
            });
        }
        if let Some(i) = remove_index {
            app.reverse_search.index_folders.remove(i);
        }

        ui.horizontal(|ui| {
            let indexing = app.reverse_search.is_indexing();
            let can_index = !app.reverse_search.index_folders.is_empty() && !indexing;
            if indexing {
                if ui
                    .button(RichText::from(format!("{} Cancel", icons::ICON_STOP.codepoint)))
                    .clicked()
                {
                    app.reverse_search.cancel_indexing();
                }
                if let IndexState::Running { scanned, total, .. } = &app.reverse_search.index_state
                {
                    ui.spinner();
                    if *total > 0 {
                        ui.label(format!("Hashed {scanned} / {total} files..."));
                    } else {
                        ui.label(format!("Scanned {scanned} files..."));
                    }
                }
            } else if ui
                .add_enabled(
                    can_index,
                    egui::Button::new(RichText::from(format!(
                        "{} Index",
                        icons::ICON_SCANNER.codepoint
                    ))),
                )
                .on_hover_text(
                    "Walks the folders above and fully hashes every file, then merges the \
                     result into the persisted index (re-indexing a drive replaces its old \
                     entries).",
                )
                .clicked()
            {
                app.reverse_search.start_indexing();
            }
        });

        let drives = app.reverse_search.db.indexed_drives();
        if drives.is_empty() {
            ui.label("No drives indexed yet.");
        } else {
            let summary = drives
                .iter()
                .map(|(letter, label)| format!("{label} ({letter})"))
                .collect::<Vec<_>>()
                .join(", ");
            ui.label(format!(
                "Indexed: {summary} — {} file(s) total.",
                app.reverse_search.db.total_files()
            ));
        }

        if let Some(status) = &app.reverse_search.status {
            ui.colored_label(Color32::from_rgb(120, 200, 120), status);
        }
    });
}

fn show_search_section(app: &mut DupeApp, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.label(icons::ICON_SEARCH.rich_text());
        ui.strong("Find matches for a file");
        if ui
            .button(RichText::from(format!(
                "{} Choose File...",
                icons::ICON_FILE_OPEN.codepoint
            )))
            .clicked()
            && let Some(path) = rfd::FileDialog::new().pick_file()
        {
            app.reverse_search.pick_file(path);
        }
    });

    let Some(picked) = app.reverse_search.picked_file.clone() else {
        ui.label("No file selected.");
        return;
    };
    ui.label(format!("Selected: {}", picked.display()));
    ui.add_space(4.0);

    if app.reverse_search.results.is_empty() {
        ui.label("No matches found in the index.");
        return;
    }

    let mut open_path = None;
    TableBuilder::new(ui)
        .id_salt("reverse_search_results")
        .striped(true)
        .column(Column::auto().at_least(120.0).resizable(true))
        .column(Column::remainder().at_least(240.0).resizable(true))
        .column(Column::auto().at_least(80.0).resizable(true))
        .column(Column::auto().at_least(120.0).resizable(true))
        .header(20.0, |mut header| {
            header.col(|ui| {
                ui.label("Volume");
            });
            header.col(|ui| {
                ui.label("Path");
            });
            header.col(|ui| {
                ui.label("Size");
            });
            header.col(|ui| {
                ui.label("Modified");
            });
        })
        .body(|mut body| {
            let results = app.reverse_search.results.clone();
            for file in &results {
                body.row(22.0, |mut row| {
                    row.col(|ui| {
                        ui.label(format!("{} ({})", file.volume_label, file.drive_letter));
                    });
                    row.col(|ui| {
                        let attached = file.absolute_path().exists();
                        let text = if attached {
                            RichText::new(file.rel_path.display().to_string())
                        } else {
                            RichText::new(format!(
                                "{} (drive not attached)",
                                file.rel_path.display()
                            ))
                            .color(Color32::from_gray(140))
                        };
                        let response = ui.add(egui::Label::new(text).sense(Sense::click()));
                        let response = if attached {
                            response.on_hover_text("Double-click to open")
                        } else {
                            response
                        };
                        if attached && response.double_clicked() {
                            open_path = Some(file.absolute_path());
                        }
                    });
                    row.col(|ui| {
                        ui.label(format_size(file.size, DECIMAL));
                    });
                    row.col(|ui| {
                        ui.label(format_timestamp(file.modified));
                    });
                });
            }
        });

    if let Some(path) = open_path
        && let Err(err) = open::that(&path)
    {
        app.reverse_search.status = Some(format!("Couldn't open {}: {err}", path.display()));
    }
}

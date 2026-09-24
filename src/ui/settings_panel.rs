use crate::app::{DupeApp, ExtensionMode, HashProgress, ScanState, ViewMode};
use crate::config::{ScanMode, SizeUnit};
use egui::{Align, Layout, Panel, RichText, Ui};
use egui_material_icons::icons;
use humansize::{DECIMAL, format_size};
use std::time::Duration;

fn format_eta(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn hash_progress_text(progress: &HashProgress) -> String {
    let done = format_size(progress.done_bytes, DECIMAL);
    let total = format_size(progress.total_bytes, DECIMAL);
    match progress.eta() {
        Some(eta) => format!("Hashed {done} / {total} — ETA {}", format_eta(eta)),
        None if progress.done_bytes >= progress.total_bytes && progress.total_bytes > 0 => {
            format!("Hashed {done} / {total} — finishing up...")
        }
        None => format!("Hashed {done} / {total} — estimating..."),
    }
}

pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    Panel::top("settings_panel").show(ui, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(icons::ICON_FOLDER_SPECIAL.rich_text().size(20.0));
            ui.heading("dupe-rs");
        });
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label(icons::ICON_DIFFERENCE.rich_text());
            ui.label("Find:");
            ui.radio_value(
                &mut app.config.mode,
                ScanMode::ExactContent,
                "Exact duplicates",
            )
            .on_hover_text("Byte-identical files, found via content hashing.");
            ui.radio_value(
                &mut app.config.mode,
                ScanMode::SimilarMedia,
                "Similar media (any resolution)",
            )
            .on_hover_text(
                "Images/videos that look like the same shot saved at a different resolution, \
                 found via perceptual hashing. The highest-resolution (then oldest) copy is kept \
                 as the original. Video comparison needs ffmpeg on PATH.",
            );
            if app.config.mode == ScanMode::SimilarMedia {
                ui.separator();
                ui.label("Similarity:");
                ui.add(
                    egui::Slider::new(&mut app.config.similarity_threshold, 0..=30)
                        .text("max hash distance"),
                )
                .on_hover_text("Lower = stricter match, higher = allows more visual difference.");
            }
        });
        ui.add_space(4.0);

        // Folders and filters side by side so the panel uses the window's full
        // width instead of stacking two half-empty rows.
        ui.columns(2, |columns| {
            columns[0].group(|ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    if ui
                        .button(RichText::from(format!(
                            "{} Add Folder(s)",
                            icons::ICON_FOLDER_OPEN.codepoint
                        )))
                        .clicked()
                        && let Some(folders) = rfd::FileDialog::new().pick_folders()
                    {
                        for folder in folders {
                            if !app.config.folders.contains(&folder) {
                                app.config.folders.push(folder);
                            }
                        }
                    }
                    ui.separator();
                    ui.checkbox(&mut app.config.exclude_subfolders, "This folder only")
                        .on_hover_text("Don't descend into subfolders — scan only the top level of each added folder.");
                    ui.checkbox(&mut app.config.same_folder_only, "Same folder only")
                        .on_hover_text("Only mark files as duplicates if they live in the same folder as each other; cross-folder matches are ignored.");
                });

                let mut remove_index = None;
                for (i, folder) in app.config.folders.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui
                            .small_button(icons::ICON_CLOSE.codepoint)
                            .on_hover_text("Remove")
                            .clicked()
                        {
                            remove_index = Some(i);
                        }
                        ui.label(format!(
                            "{} {}",
                            icons::ICON_FOLDER.codepoint,
                            folder.display()
                        ));
                    });
                }
                if let Some(i) = remove_index {
                    app.config.folders.remove(i);
                }
            });

            columns[1].group(|ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(icons::ICON_STORAGE.rich_text());
                    ui.label("Size range:");
                    ui.add(egui::TextEdit::singleline(&mut app.min_size_text).desired_width(60.0));
                    ui.label("to");
                    ui.add(egui::TextEdit::singleline(&mut app.max_size_text).desired_width(60.0));
                    egui::ComboBox::from_id_salt("size_unit")
                        .selected_text(app.size_unit.label())
                        .show_ui(ui, |ui| {
                            for unit in SizeUnit::ALL {
                                ui.selectable_value(&mut app.size_unit, unit, unit.label());
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label(icons::ICON_FILTER.rich_text());
                    ui.label("Extensions:");
                    ui.radio_value(&mut app.extension_mode, ExtensionMode::All, "All");
                    ui.radio_value(&mut app.extension_mode, ExtensionMode::Include, "Include");
                    ui.radio_value(&mut app.extension_mode, ExtensionMode::Exclude, "Exclude");
                    if app.extension_mode != ExtensionMode::All {
                        ui.add(
                            egui::TextEdit::singleline(&mut app.extension_text)
                                .hint_text("jpg, png, mp4")
                                .desired_width(200.0),
                        );
                    }
                });
            });
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            // The right-aligned view-mode switch is added last (see below):
            // a `right_to_left` child placed first would claim the whole row
            // for itself and push every later widget off past the right edge.
            let scanning = app.is_scanning();
            let can_scan = !app.config.folders.is_empty() && !scanning;
            if scanning {
                if ui
                    .button(RichText::from(format!(
                        "{} Cancel",
                        icons::ICON_STOP.codepoint
                    )))
                    .clicked()
                {
                    app.cancel_scan();
                }
            } else if ui
                .add_enabled(
                    can_scan,
                    egui::Button::new(RichText::from(format!(
                        "{} Scan",
                        icons::ICON_SCANNER.codepoint
                    ))),
                )
                .clicked()
            {
                app.start_scan();
            }

            match &app.scan_state {
                ScanState::Running {
                    scanned,
                    hash_progress,
                    ..
                } => {
                    ui.spinner();
                    match hash_progress {
                        Some(progress) => ui.label(hash_progress_text(progress)),
                        None => ui.label(format!("Scanned {scanned} files...")),
                    };
                }
                ScanState::Done { elapsed_ms } => {
                    ui.label(format!(
                        "{} Done in {:.2}s",
                        icons::ICON_CHECK_CIRCLE.codepoint,
                        *elapsed_ms as f64 / 1000.0,
                    ));
                }
                ScanState::Idle => {}
            }

            // View-mode switch anchored to the right edge of the scan row;
            // only meaningful once exact-match results are showing (the
            // similar-media mode only has a table view so far).
            if app.active_mode == ScanMode::ExactContent {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.selectable_value(
                        &mut app.view_mode,
                        ViewMode::Grid,
                        RichText::from(format!("{} Grid", icons::ICON_GRID_VIEW.codepoint)),
                    );
                    ui.selectable_value(
                        &mut app.view_mode,
                        ViewMode::Table,
                        RichText::from(format!("{} Table", icons::ICON_TABLE.codepoint)),
                    );
                });
            }
        });

        match app.active_mode {
            ScanMode::ExactContent if !app.groups.is_empty() => {
                ui.add_space(4.0);
                ui.separator();
                crate::ui::stats_panel::show(ui, &app.groups);
            }
            ScanMode::SimilarMedia if !app.similar_groups.is_empty() => {
                ui.add_space(4.0);
                ui.separator();
                crate::ui::stats_panel::show_similar(ui, &app.similar_groups);
            }
            _ => {}
        }

        ui.add_space(4.0);
    });
}

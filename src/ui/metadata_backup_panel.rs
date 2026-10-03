use crate::app::{ConfirmAction, DupeApp};
use crate::metadata_backup::restore::{Comparison, FileChange};
use crate::metadata_backup::{JobKind, MetadataJob, file_name};
use crate::ui::dialogs::PickPurpose;
use crate::ui::format::format_eta;
use chrono::{DateTime, Local, Utc};
use egui::{Color32, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use egui_material_icons::icons;

const ROW_HEIGHT: f32 = 22.0;
const CHANGED: Color32 = Color32::from_rgb(230, 190, 90);

pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    ui.heading("Metadata Backup");
    ui.label(
        "Save every file's name, location, size, created and modified dates, and photo \
         EXIF data (date taken, GPS position, camera) in a backup. If renaming, moving, \
         copying or editing files later loses any of it, compare the backup with the \
         files and put it back.",
    );
    ui.add_space(8.0);

    let busy = app.metadata_backup.is_running();
    ui.horizontal(|ui| {
        ui.label(icons::ICON_FOLDER_OPEN.rich_text());
        ui.strong("Folder");
        if ui
            .add_enabled(!busy, egui::Button::new("Choose..."))
            .clicked()
        {
            app.start_pick(PickPurpose::MetadataBackupRoot);
        }
        match &app.metadata_backup.root {
            Some(p) => ui.label(p.display().to_string()),
            None => ui.weak("no folder chosen"),
        };
    });
    ui.horizontal(|ui| {
        let state = &mut app.metadata_backup;
        if ui
            .add_enabled(
                !busy && state.root.is_some(),
                egui::Button::new(RichText::from(format!(
                    "{} Back up metadata",
                    icons::ICON_BACKUP.codepoint
                ))),
            )
            .clicked()
        {
            state.back_up();
        }
        if ui
            .button(RichText::from(format!(
                "{} Open backups folder",
                icons::ICON_FOLDER_OPEN.codepoint
            )))
            .on_hover_text(state.backups_dir.display().to_string())
            .clicked()
        {
            let _ = std::fs::create_dir_all(&state.backups_dir);
            let _ = open::that_detached(&state.backups_dir);
        }
    });
    ui.add_space(8.0);
    ui.separator();

    ui.horizontal(|ui| {
        ui.label(icons::ICON_SETTINGS_BACKUP_RESTORE.rich_text());
        ui.strong("Backup");
        let state = &mut app.metadata_backup;
        let selected = state
            .selected_backup
            .as_deref()
            .map_or_else(|| "none yet".to_owned(), file_name);
        let mut picked = None;
        ui.add_enabled_ui(!busy, |ui| {
            egui::ComboBox::from_id_salt("metadata_backup_select")
                .selected_text(selected)
                .width(320.0)
                .show_ui(ui, |ui| {
                    for path in &state.backups {
                        let is_selected = state.selected_backup.as_ref() == Some(path);
                        if ui.selectable_label(is_selected, file_name(path)).clicked() {
                            picked = Some(path.clone());
                        }
                    }
                });
        });
        if let Some(path) = picked {
            state.select_backup(path);
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Browse..."))
            .on_hover_text("Use a backup file from somewhere else")
            .clicked()
        {
            app.start_pick(PickPurpose::MetadataBackupFile);
        }
        let state = &mut app.metadata_backup;
        if ui
            .add_enabled(
                !busy && state.selected_backup.is_some(),
                egui::Button::new(RichText::from(format!(
                    "{} Compare with files now",
                    icons::ICON_COMPARE_ARROWS.codepoint
                ))),
            )
            .clicked()
        {
            state.compare();
        }
    });

    let mut cancel = false;
    if let Some(job) = &mut app.metadata_backup.job {
        cancel = show_progress(job, ui);
    }
    if cancel {
        app.pending_confirm = Some(ConfirmAction::CancelMetadataBackup);
    }

    let state = &mut app.metadata_backup;
    if let Some(msg) = &state.status {
        ui.label(msg);
    }
    if !state.errors.is_empty() {
        ui.colored_label(
            Color32::from_rgb(230, 160, 60),
            format!(
                "{} error(s); first: {}",
                state.errors.len(),
                state.errors[0]
            ),
        )
        .on_hover_text(state.errors.join("\n"));
    }

    let Some(comparison) = &state.comparison else {
        return;
    };
    ui.add_space(4.0);
    ui.label(format!(
        "Backup of {} from {}: {} file(s), {} unchanged, {} changed, {} not found.",
        comparison.root.display(),
        local(comparison.taken_at),
        comparison.backed_up,
        comparison.unchanged,
        comparison.changes.len(),
        comparison.missing.len(),
    ));
    if !comparison.missing.is_empty() {
        ui.weak(
            "Files not found were deleted, or moved and changed at the same time; they \
             can't be restored from metadata.",
        )
        .on_hover_text(
            comparison
                .missing
                .iter()
                .take(50)
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    if comparison.changes.is_empty() {
        ui.label("Nothing to restore.");
        return;
    }

    ui.horizontal(|ui| {
        ui.add_enabled_ui(!busy, |ui| {
            ui.checkbox(
                &mut state.options.locations,
                format!("Names and locations ({})", comparison.moved_count()),
            );
            ui.checkbox(
                &mut state.options.times,
                format!("Created / modified dates ({})", comparison.times_count()),
            );
            ui.checkbox(
                &mut state.options.exif,
                format!("Date taken and GPS ({})", comparison.exif_count()),
            )
            .on_hover_text("Written back into JPEG, TIFF, WebP and HEIC files");
        });
    });
    let steps = state.restore_steps();
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !busy && steps > 0,
                egui::Button::new(RichText::from(format!(
                    "{} Restore",
                    icons::ICON_RESTORE.codepoint
                ))),
            )
            .on_hover_text("The files' current state is saved as a backup first")
            .clicked()
        {
            state.restore();
        }
        ui.weak("Nothing is ever overwritten; the current state is backed up first.");
    });
    ui.separator();
    if let Some(comparison) = &state.comparison {
        show_changes(comparison, ui);
    }
}

/// Returns whether "Cancel" was clicked.
fn show_progress(job: &mut MetadataJob, ui: &mut Ui) -> bool {
    let mut cancel = false;
    ui.group(|ui| {
        ui.set_min_width(ui.available_width());
        let (verb, unit) = match job.kind {
            JobKind::Backup => ("Backing up", "files"),
            JobKind::Compare => ("Comparing", "files"),
            JobKind::Restore => ("Restoring", "changes"),
        };
        if job.total() == 0 {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("{verb}: listing files..."));
            });
        } else {
            ui.add(egui::ProgressBar::new(job.fraction()).text(format!(
                "{verb}: {} / {} {unit}",
                job.done(),
                job.total()
            )));
            let eta = job
                .eta()
                .map_or_else(|| "estimating...".to_owned(), format_eta);
            ui.label(format!("ETA: {eta}"));
        }
        if !job.is_cancelled() {
            ui.horizontal(|ui| {
                if crate::ui::controls::pause_resume_button(ui, job.is_paused()).clicked() {
                    job.toggle_pause();
                }
                if ui
                    .button(RichText::from(format!(
                        "{} Cancel",
                        icons::ICON_STOP.codepoint
                    )))
                    .clicked()
                {
                    cancel = true;
                }
            });
        }
    });
    cancel
}

fn local(time: DateTime<Utc>) -> String {
    time.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

fn describe(change: &FileChange) -> Vec<String> {
    let mut parts = Vec::new();
    if change.moved() {
        parts.push(format!("now {}", change.current_path.display()));
    }
    let date = |label: &str, was: Option<DateTime<Utc>>, now: Option<DateTime<Utc>>| {
        format!(
            "{label} {} (now {})",
            was.map_or_else(|| "?".to_owned(), local),
            now.map_or_else(|| "?".to_owned(), local)
        )
    };
    if change.created_changed {
        parts.push(date(
            "created",
            change.backup.created,
            change.current_created,
        ));
    }
    if change.modified_changed {
        parts.push(date(
            "modified",
            change.backup.modified,
            change.current_modified,
        ));
    }
    if change.exif_changed
        && let Some(exif) = &change.backup.exif
    {
        if let Some(taken) = &exif.date_taken {
            parts.push(format!("taken {taken}"));
        }
        if let Some(gps) = &exif.gps {
            parts.push(format!("GPS {:.5}, {:.5}", gps.latitude, gps.longitude));
        }
    }
    parts
}

fn show_changes(comparison: &Comparison, ui: &mut Ui) {
    TableBuilder::new(ui)
        .id_salt("metadata_backup_changes")
        .striped(true)
        .column(Column::remainder().at_least(240.0).resizable(true))
        .column(Column::remainder().at_least(300.0))
        .header(20.0, |mut header| {
            header.col(|ui| {
                ui.strong("Backed-up file");
            });
            header.col(|ui| {
                ui.strong("What gets restored");
            });
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, comparison.changes.len(), |mut row| {
                let change = &comparison.changes[row.index()];
                row.col(|ui| {
                    ui.add(egui::Label::new(change.backup.path.display().to_string()).truncate());
                });
                row.col(|ui| {
                    let text = describe(change).join(" · ");
                    ui.add(egui::Label::new(RichText::new(text.clone()).color(CHANGED)).truncate())
                        .on_hover_text(text);
                });
            });
        });
}

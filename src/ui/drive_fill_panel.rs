use crate::app::DupeApp;
use crate::drive_fill::{CopyJob, DriveFillState, FolderSortColumn, FolderStatus, MeasureState};
use crate::ui::controls::sort_header;
use crate::ui::format::{format_eta, format_timestamp};
use egui::{Color32, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use egui_material_icons::icons;
use humansize::{BINARY, DECIMAL, format_size};

const ROW_HEIGHT: f32 = 22.0;

pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    ui.heading("Drive Fill");
    ui.label(
        "Copy whole top-level folders of a source folder onto a target drive, picking the \
         combination that fills the target's free space as completely as possible. Every \
         copied file is hashed on the way and added to the reverse-search index, so you can \
         later look up which drive a file ended up on.",
    );
    ui.add_space(8.0);

    let state = &mut app.drive_fill;
    show_locations(state, ui);
    ui.add_space(8.0);
    show_plan_summary(state, ui);
    ui.add_space(4.0);

    if let Some(job) = &mut state.copy {
        show_copy_progress(job, ui);
        ui.add_space(4.0);
    }
    if let Some(msg) = &state.status {
        ui.label(msg);
        ui.add_space(4.0);
    }
    ui.separator();
    show_folder_table(state, ui);
}

fn show_locations(state: &mut DriveFillState, ui: &mut Ui) {
    ui.group(|ui| {
        ui.set_min_width(ui.available_width());
        let busy = state.is_busy();
        egui::Grid::new("drive_fill_locations")
            .num_columns(3)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label(icons::ICON_FOLDER_OPEN.rich_text());
                ui.strong("Source");
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new("Choose..."))
                        .on_hover_text("The folder whose subfolders get distributed across drives")
                        .clicked()
                        && let Some(dir) = rfd::FileDialog::new().pick_folder()
                    {
                        state.set_source(dir);
                    }
                    match &state.source {
                        Some(p) => ui.label(p.display().to_string()),
                        None => ui.weak("no folder chosen"),
                    };
                    if let MeasureState::Running { done, total, .. } = &state.measure {
                        ui.spinner();
                        ui.label(format!("Measuring folders {done} / {total}..."));
                    }
                });
                ui.end_row();

                ui.label(icons::ICON_HARD_DRIVE.rich_text());
                ui.strong("Target");
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!state.is_copying(), egui::Button::new("Choose..."))
                        .on_hover_text("Where on the drive to be filled the folders are copied to")
                        .clicked()
                        && let Some(dir) = rfd::FileDialog::new().pick_folder()
                    {
                        state.set_target(dir);
                    }
                    match &state.target {
                        Some(p) => ui.label(p.display().to_string()),
                        None => ui.weak("no folder chosen"),
                    };
                    if let Some(space) = state.space {
                        ui.separator();
                        ui.label(format!(
                            "{} free of {} (clusters of {})",
                            format_size(space.free, DECIMAL),
                            format_size(space.total, DECIMAL),
                            format_size(space.cluster, BINARY),
                        ));
                        if ui
                            .small_button(icons::ICON_REFRESH.codepoint)
                            .on_hover_text("Re-read free space")
                            .clicked()
                        {
                            state.refresh_space();
                        }
                    }
                });
                ui.end_row();

                ui.label(icons::ICON_DATA_USAGE.rich_text());
                ui.strong("Keep free");
                ui.horizontal(|ui| {
                    let changed = ui
                        .add(
                            egui::DragValue::new(&mut state.reserve_mb)
                                .range(0..=1_000_000)
                                .suffix(" MiB"),
                        )
                        .on_hover_text("Headroom left unused on the target, on top of the estimate")
                        .changed();
                    if changed {
                        state.replan();
                    }
                });
                ui.end_row();
            });
    });
}

fn show_plan_summary(state: &mut DriveFillState, ui: &mut Ui) {
    ui.horizontal(|ui| {
        let plan = &state.plan;
        let count = plan.planned_count();
        let can_copy = count > 0 && !state.is_busy();
        if ui
            .add_enabled(
                can_copy,
                egui::Button::new(RichText::from(format!(
                    "{} Copy {count} folder(s)",
                    icons::ICON_FOLDER_COPY.codepoint
                ))),
            )
            .clicked()
        {
            state.start_copy();
            return;
        }
        if state.target.is_none() || state.folders.is_empty() {
            ui.weak("Choose a source and a target to see a plan.");
            return;
        }
        let fill = if plan.capacity == 0 {
            0.0
        } else {
            plan.planned_footprint as f64 / plan.capacity as f64 * 100.0
        };
        ui.label(format!(
            "Plan: {count} folder(s), {} — uses {fill:.2}% of the {} available",
            format_size(plan.planned_bytes, DECIMAL),
            format_size(plan.capacity, DECIMAL),
        ));
    });
}

fn show_copy_progress(job: &mut CopyJob, ui: &mut Ui) {
    ui.group(|ui| {
        ui.set_min_width(ui.available_width());
        ui.add(egui::ProgressBar::new(job.fraction()).text(format!(
            "{} / {}",
            format_size(job.bytes_done, DECIMAL),
            format_size(job.total_bytes, DECIMAL)
        )));
        let current = job
            .current
            .as_ref()
            .map_or_else(|| "starting...".to_owned(), |p| p.display().to_string());
        ui.horizontal(|ui| {
            ui.label(if job.is_paused() {
                "Paused at:"
            } else {
                "Copying:"
            });
            ui.add(egui::Label::new(RichText::new(current).monospace()).truncate());
        });
        ui.horizontal(|ui| {
            let speed = job.bytes_per_sec().map_or_else(
                || "—".to_owned(),
                |r| format!("{}/s", format_size(r as u64, DECIMAL)),
            );
            let eta = job
                .eta()
                .map_or_else(|| "estimating...".to_owned(), format_eta);
            ui.label(format!(
                "{} file(s) copied   Speed: {speed}   ETA: {eta}",
                job.files_copied
            ));
            if !job.errors.is_empty() {
                ui.colored_label(
                    Color32::from_rgb(230, 160, 60),
                    format!("{} error(s)", job.errors.len()),
                )
                .on_hover_text(job.errors.join("\n"));
            }
            if !job.is_cancelled() {
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
                    job.cancel();
                }
            }
        });
    });
}

/// Overview of every top-level source folder: name, size, file count,
/// timestamps, and where it stands in the plan, with a checkbox to leave it
/// out. Sortable by any column; double-click a name to open the folder.
fn show_folder_table(state: &mut DriveFillState, ui: &mut Ui) {
    if state.folders.is_empty() {
        return;
    }
    let total: u64 = state.folders.iter().map(|f| f.size).sum();
    let files: u64 = state.folders.iter().map(|f| f.file_count).sum();
    ui.label(format!(
        "{} folder(s), {} in {files} file(s)",
        state.folders.len(),
        format_size(total, DECIMAL)
    ));

    let editable = !state.is_copying();
    let order = state.sorted_indices();
    let mut toggled = None;
    let mut open_path = None;
    let mut sort = state.sort;

    TableBuilder::new(ui)
        .id_salt("drive_fill_folders")
        .striped(true)
        .column(Column::auto().at_least(24.0))
        .column(Column::remainder().at_least(200.0).resizable(true))
        .column(Column::auto().at_least(90.0).resizable(true))
        .column(Column::auto().at_least(70.0).resizable(true))
        .column(Column::auto().at_least(120.0).resizable(true))
        .column(Column::auto().at_least(120.0).resizable(true))
        .column(Column::auto().at_least(110.0))
        .header(20.0, |mut header| {
            header.col(|ui| {
                ui.label("");
            });
            let columns = [
                ("Folder", FolderSortColumn::Name),
                ("Size", FolderSortColumn::Size),
                ("Files", FolderSortColumn::Files),
                ("Modified", FolderSortColumn::Modified),
                ("Created", FolderSortColumn::Created),
                ("Status", FolderSortColumn::Status),
            ];
            for (label, column) in columns {
                header.col(|ui| sort_header(ui, label, column, &mut sort));
            }
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, order.len(), |mut row| {
                let i = order[row.index()];
                let folder = &state.folders[i];
                let status = state.plan.statuses.get(i).copied();
                row.col(|ui| {
                    let mut included = !state.excluded.contains(&folder.path);
                    if ui
                        .add_enabled(editable, egui::Checkbox::without_text(&mut included))
                        .on_hover_text("Consider this folder for the plan")
                        .changed()
                    {
                        toggled = Some((i, !included));
                    }
                });
                row.col(|ui| {
                    let text = format!("{} {}", icons::ICON_FOLDER.codepoint, folder.name);
                    let response = ui
                        .add(egui::Label::new(text).truncate().sense(Sense::click()))
                        .on_hover_text(folder.path.display().to_string());
                    if response.double_clicked() {
                        open_path = Some(folder.path.clone());
                    }
                });
                row.col(|ui| {
                    ui.label(format_size(folder.size, DECIMAL));
                });
                row.col(|ui| {
                    ui.label(folder.file_count.to_string());
                });
                row.col(|ui| {
                    ui.label(format_timestamp(folder.modified));
                });
                row.col(|ui| {
                    ui.label(format_timestamp(folder.created));
                });
                row.col(|ui| {
                    if let Some(status) = status {
                        ui.colored_label(status_color(status), status.label());
                    }
                });
            });
        });

    state.sort = sort;
    if let Some((i, excluded)) = toggled {
        state.set_excluded(i, excluded);
    }
    if let Some(path) = open_path
        && let Err(err) = open::that(&path)
    {
        state.status = Some(format!("Couldn't open {}: {err}", path.display()));
    }
}

pub fn status_color(status: FolderStatus) -> Color32 {
    match status {
        FolderStatus::Planned => Color32::from_rgb(120, 200, 120),
        FolderStatus::DoesNotFit => Color32::from_rgb(200, 200, 120),
        FolderStatus::Excluded | FolderStatus::ExistsInTarget => Color32::GRAY,
    }
}

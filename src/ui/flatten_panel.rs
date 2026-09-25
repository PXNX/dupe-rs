use crate::app::{ConfirmAction, DupeApp};
use crate::flatten::{FlattenState, MoveJob, relative};
use crate::ui::dialogs::PickPurpose;
use crate::ui::format::format_eta;
use egui::{Color32, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use egui_material_icons::icons;

const ROW_HEIGHT: f32 = 22.0;

pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    ui.heading("Flatten");
    ui.label(
        "Move every file out of a folder's subfolders into the folder itself. Name clashes \
         are renamed like Windows does (\"name (2).ext\"), split-archive volumes keep \
         matching names, and nothing is ever overwritten. Check the preview, then move; the \
         last run can be undone.",
    );
    ui.add_space(8.0);

    let busy = app.flatten.is_running();
    ui.horizontal(|ui| {
        ui.label(icons::ICON_FOLDER_OPEN.rich_text());
        ui.strong("Folder");
        if ui
            .add_enabled(!busy, egui::Button::new("Choose..."))
            .clicked()
        {
            app.start_pick(PickPurpose::FlattenRoot);
        }
        match &app.flatten.root {
            Some(p) => ui.label(p.display().to_string()),
            None => ui.weak("no folder chosen"),
        };
    });
    ui.add_enabled(
        !busy,
        egui::Checkbox::new(
            &mut app.flatten.remove_empty_dirs,
            "Remove subfolders left empty afterwards",
        ),
    );
    ui.add_space(4.0);

    let mut cancel = false;
    ui.horizontal(|ui| {
        let state = &mut app.flatten;
        let count = state.plan.len();
        if ui
            .add_enabled(
                count > 0 && !state.is_running() && !state.is_planning(),
                egui::Button::new(RichText::from(format!(
                    "{} Move {count} file(s)",
                    icons::ICON_DRIVE_FILE_MOVE.codepoint
                ))),
            )
            .clicked()
        {
            state.start();
        }
        if ui
            .add_enabled(
                !state.last_run.is_empty() && !state.is_running(),
                egui::Button::new(RichText::from(format!(
                    "{} Undo last run",
                    icons::ICON_UNDO.codepoint
                ))),
            )
            .on_hover_text("Move the files from the last run back to their subfolders")
            .clicked()
        {
            state.undo();
        }
        if state.is_planning() {
            ui.spinner();
            ui.label("Building preview...");
        } else if state.root.is_some() && !state.is_running() {
            ui.label(format!(
                "{count} file(s) to move, {} renamed to avoid clashes",
                state.renamed_count()
            ));
        }
        if let Some(job) = &mut state.job
            && !job.is_cancelled()
        {
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
        }
    });
    if cancel {
        app.pending_confirm = Some(ConfirmAction::CancelFlatten);
    }

    let state = &mut app.flatten;
    if let Some(job) = &state.job {
        show_progress(job, ui);
    }
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
    ui.separator();
    show_preview(state, ui);
}

fn show_progress(job: &MoveJob, ui: &mut Ui) {
    ui.group(|ui| {
        ui.set_min_width(ui.available_width());
        ui.add(egui::ProgressBar::new(job.fraction()).text(format!(
            "{} / {} files{}",
            job.done,
            job.total,
            if job.undo { " (undo)" } else { "" }
        )));
        let current = job
            .current
            .as_ref()
            .map_or_else(|| "starting...".to_owned(), |p| p.display().to_string());
        ui.horizontal(|ui| {
            ui.label(if job.is_paused() {
                "Paused at:"
            } else {
                "Moving:"
            });
            ui.add(egui::Label::new(RichText::new(current).monospace()).truncate());
        });
        let rate = job
            .items_per_sec()
            .map_or_else(|| "—".to_owned(), |r| format!("{r:.1} items/s"));
        let eta = job
            .eta()
            .map_or_else(|| "estimating...".to_owned(), format_eta);
        ui.label(format!("Speed: {rate}   ETA: {eta}"));
    });
}

fn show_preview(state: &FlattenState, ui: &mut Ui) {
    let Some(root) = &state.root else {
        return;
    };
    if state.plan.is_empty() {
        if !state.is_planning() && !state.is_running() {
            ui.label("Nothing to move: no files in subfolders.");
        }
        return;
    }
    TableBuilder::new(ui)
        .id_salt("flatten_preview")
        .striped(true)
        .column(Column::remainder().at_least(260.0).resizable(true))
        .column(Column::remainder().at_least(200.0))
        .header(20.0, |mut header| {
            header.col(|ui| {
                ui.strong("From");
            });
            header.col(|ui| {
                ui.strong("New name");
            });
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, state.plan.len(), |mut row| {
                let m = &state.plan[row.index()];
                row.col(|ui| {
                    ui.add(egui::Label::new(relative(&m.from, root)).truncate());
                });
                row.col(|ui| {
                    let name = relative(&m.to, root);
                    if m.renamed {
                        ui.colored_label(
                            Color32::from_rgb(230, 190, 90),
                            format!("{name} (renamed)"),
                        );
                    } else {
                        ui.label(name);
                    }
                });
            });
        });
}

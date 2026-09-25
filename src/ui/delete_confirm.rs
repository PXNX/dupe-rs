use crate::app::{ConfirmAction, DeleteJob, DupeApp};
use crate::ui::format::format_eta;
use egui::{Context, RichText, Window};
use egui_material_icons::icons;
use humansize::{DECIMAL, format_size};

pub fn show(app: &mut DupeApp, ctx: &Context) {
    if app.is_deleting()
        && let Some(id) = show_jobs(ctx, &mut app.delete_jobs)
    {
        app.pending_confirm = Some(ConfirmAction::CancelDelete(id));
    }

    let Some(confirm) = &mut app.delete_confirm else {
        return;
    };
    let count = confirm.paths.len();
    let size_str = format_size(confirm.total_size, DECIMAL);

    let mut do_confirm = false;
    let mut do_cancel = false;

    Window::new(RichText::from(format!(
        "{} Confirm Delete",
        icons::ICON_WARNING.codepoint
    )))
    .collapsible(false)
    .resizable(false)
    .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
    .show(ctx, |ui| {
        if confirm.permanent {
            ui.label(format!(
                "Permanently delete {count} file(s) ({size_str})? They will not go to the \
                 Recycle Bin and cannot be recovered."
            ));
        } else {
            ui.label(format!(
                "Move {count} file(s) ({size_str}) to the trash? This cannot be undone from within dupe-rs."
            ));
        }
        ui.checkbox(
            &mut confirm.permanent,
            "Delete permanently (skip the Recycle Bin)",
        );
        ui.horizontal(|ui| {
            if ui
                .button(
                    RichText::new("Delete").color(egui::Color32::from_rgb(255, 120, 120)),
                )
                .clicked()
            {
                do_confirm = true;
            }
            if ui.button("Cancel").clicked() {
                do_cancel = true;
            }
        });
    });

    if do_confirm {
        app.confirm_delete();
    } else if do_cancel {
        app.delete_confirm = None;
    }
}

/// Non-modal, bottom-right list of every running delete, so the results stay
/// usable (and further deletes can be started) while they work. Returns the
/// id of a job whose Cancel button was clicked.
fn show_jobs(ctx: &Context, jobs: &mut [DeleteJob]) -> Option<u64> {
    let mut cancel = None;
    let title = if jobs.len() == 1 {
        format!("{} Deleting...", icons::ICON_DELETE.codepoint)
    } else {
        format!("{} Deleting ({} jobs)...", icons::ICON_DELETE.codepoint, jobs.len())
    };
    Window::new(RichText::from(title))
        .id(egui::Id::new("delete_jobs"))
        .collapsible(true)
        .resizable(false)
        // Clear of the three-row status bar along the bottom edge.
        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -110.0))
        .show(ctx, |ui| {
            // Fixed width so the window doesn't jitter as the current path
            // and rate text change length every frame.
            ui.set_width(420.0);
            for (i, job) in jobs.iter_mut().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                if ui.push_id(job.id, |ui| show_job(ui, job)).inner {
                    cancel = Some(job.id);
                }
            }
        });
    cancel
}

/// Returns whether Cancel was clicked.
fn show_job(ui: &mut egui::Ui, job: &mut DeleteJob) -> bool {
    let mut cancel = false;
    let (done, total) = (job.done, job.total);
    let fraction = if total == 0 { 1.0 } else { done as f32 / total as f32 };
    ui.add(egui::ProgressBar::new(fraction).text(format!("{done} / {total} files")));

    let current = job
        .current
        .as_ref()
        .map_or_else(|| "starting...".to_owned(), |p| p.display().to_string());
    let verb = match (job.is_cancelled(), job.is_paused(), job.permanent) {
        (true, _, _) => "Stopping after:",
        (_, true, _) => "Paused at:",
        (_, _, true) => "Deleting:",
        _ => "Moving to trash:",
    };
    ui.horizontal(|ui| {
        ui.label(verb);
        ui.add(egui::Label::new(RichText::new(current).monospace()).truncate());
    });

    let rate = job
        .items_per_sec()
        .map_or_else(|| "—".to_owned(), |r| format!("{r:.1} items/s"));
    let eta = job.eta().map_or_else(|| "estimating...".to_owned(), format_eta);
    ui.horizontal(|ui| {
        ui.label(format!("Speed: {rate}   ETA: {eta}"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(
                    !job.is_cancelled(),
                    egui::Button::new(RichText::from(format!(
                        "{} Cancel",
                        icons::ICON_STOP.codepoint
                    ))),
                )
                .clicked()
            {
                cancel = true;
            }
            if !job.is_cancelled()
                && crate::ui::controls::pause_resume_button(ui, job.is_paused()).clicked()
            {
                let paused = job.is_paused();
                job.set_paused(!paused);
            }
        });
    });
    cancel
}

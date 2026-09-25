use crate::app::{ConfirmAction, DupeApp};
use egui::{Color32, Context, Id, Modal, RichText};
use egui_material_icons::icons;

/// The "are you sure?" modal for cancelling a job or closing the window.
/// Everything behind it is blocked until the user decides.
pub fn show(app: &mut DupeApp, ctx: &Context) {
    let Some(action) = app.pending_confirm else {
        return;
    };
    let (title, message, confirm_label) = describe(app, action);

    let mut confirmed = false;
    let mut dismissed = false;
    let response = Modal::new(Id::new("confirm_dialog")).show(ctx, |ui| {
        ui.set_max_width(420.0);
        ui.heading(format!("{} {title}", icons::ICON_WARNING.codepoint));
        ui.add_space(4.0);
        ui.label(message);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .button(RichText::new(confirm_label).color(Color32::from_rgb(255, 120, 120)))
                .clicked()
            {
                confirmed = true;
            }
            if ui.button("Keep going").clicked() {
                dismissed = true;
            }
        });
    });
    if response.should_close() {
        dismissed = true;
    }

    if confirmed {
        app.pending_confirm = None;
        app.apply_confirmed(action, ctx);
    } else if dismissed {
        app.pending_confirm = None;
    }
}

fn describe(app: &DupeApp, action: ConfirmAction) -> (&'static str, String, &'static str) {
    match action {
        ConfirmAction::CancelScan => (
            "Cancel scan?",
            "The scan stops and only the duplicates found so far stay listed.".into(),
            "Cancel scan",
        ),
        ConfirmAction::CancelIndexing => (
            "Cancel indexing?",
            "Nothing from this indexing pass will be added to the reverse-search index.".into(),
            "Cancel indexing",
        ),
        ConfirmAction::CancelDelete(_) => (
            "Stop deleting?",
            "Files already deleted stay deleted; the rest are left alone.".into(),
            "Stop deleting",
        ),
        ConfirmAction::CancelCopy => (
            "Cancel copying?",
            "Folders already copied stay on the target and are still added to the index; \
             the file in progress is removed."
                .into(),
            "Cancel copying",
        ),
        ConfirmAction::CancelReencode => (
            "Cancel re-encoding?",
            "Files already re-encoded keep their new versions; the one in progress is \
             discarded and its original left untouched."
                .into(),
            "Cancel re-encoding",
        ),
        ConfirmAction::CloseWindow => {
            let running = app.running_work();
            let message = if running.is_empty() {
                "The current scan results will be lost.".to_string()
            } else {
                format!(
                    "Still running: {}. Closing stops it; anything already finished is kept.",
                    running.join(", ")
                )
            };
            ("Close dupe-rs?", message, "Close anyway")
        }
    }
}

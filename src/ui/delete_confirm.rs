use crate::app::{DeleteState, DupeApp};
use egui::{Context, RichText, Window};
use egui_material_icons::icons;
use humansize::{DECIMAL, format_size};

pub fn show(app: &mut DupeApp, ctx: &Context) {
    if let DeleteState::Running { total, done, .. } = &app.delete_state {
        show_progress(ctx, *done, *total);
        return;
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

fn show_progress(ctx: &Context, done: usize, total: usize) {
    Window::new(RichText::from(format!(
        "{} Deleting...",
        icons::ICON_DELETE.codepoint
    )))
    .collapsible(false)
    .resizable(false)
    .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
    .show(ctx, |ui| {
        ui.set_min_width(280.0);
        let fraction = if total == 0 { 1.0 } else { done as f32 / total as f32 };
        ui.add(
            egui::ProgressBar::new(fraction)
                .text(format!("{done} / {total} files")),
        );
    });
}

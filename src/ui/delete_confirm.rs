use crate::app::DupeApp;
use egui::{Context, Window};
use humansize::{DECIMAL, format_size};

pub fn show(app: &mut DupeApp, ctx: &Context) {
    let Some(confirm) = &app.delete_confirm else {
        return;
    };
    let count = confirm.paths.len();
    let size_str = format_size(confirm.total_size, DECIMAL);

    let mut do_confirm = false;
    let mut do_cancel = false;

    Window::new("Confirm Delete")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.label(format!(
                "Move {count} file(s) ({size_str}) to the trash? This cannot be undone from within dupe-rs."
            ));
            ui.horizontal(|ui| {
                if ui.button("Delete").clicked() {
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

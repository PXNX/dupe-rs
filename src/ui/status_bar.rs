use crate::app::DupeApp;
use crate::selection::compute_visible_entries;
use egui::{Panel, Ui};
use humansize::{DECIMAL, format_size};

pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    Panel::bottom("status_bar").show(ui, |ui| {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let visible = compute_visible_entries(&app.groups, app.only_show_duplicates);
            let shown_size: u64 = visible.iter().map(|f| f.size).sum();
            let selected_count = visible
                .iter()
                .filter(|f| app.selection.contains(&f.path))
                .count();
            let selected_size: u64 = visible
                .iter()
                .filter(|f| app.selection.contains(&f.path))
                .map(|f| f.size)
                .sum();

            ui.checkbox(&mut app.only_show_duplicates, "Only show duplicates");
            ui.separator();

            if ui.button("Select All Shown").clicked() {
                for f in &visible {
                    app.selection.insert(f.path.clone());
                }
            }
            if ui.button("Select None").clicked() {
                app.selection.clear();
            }
            ui.separator();

            ui.colored_label(
                egui::Color32::from_rgb(120, 200, 120),
                format!(
                    "Selected: {selected_count} files, {}",
                    format_size(selected_size, DECIMAL)
                ),
            );
            ui.label(format!(
                "Shown: {} files, {}",
                visible.len(),
                format_size(shown_size, DECIMAL)
            ));

            ui.separator();
            let can_delete = !app.selection.is_empty();
            if ui
                .add_enabled(can_delete, egui::Button::new("Delete Selected"))
                .clicked()
            {
                app.delete_selected();
            }

            if let Some(msg) = app.status_message.clone() {
                ui.separator();
                ui.label(msg);
            }
        });
        ui.add_space(2.0);
    });
}

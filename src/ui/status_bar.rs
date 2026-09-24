use crate::app::{DupeApp, SelectCriterion};
use crate::selection::{compute_visible_entries, filter_by_name_pattern};
use egui::{Panel, RichText, Ui};
use egui_material_icons::icons;
use humansize::{DECIMAL, format_size};
use std::path::PathBuf;

pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    Panel::bottom("status_bar").show(ui, |ui| {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(icons::ICON_TASK_ALT.rich_text());
            ui.label("Select per group:");
            egui::ComboBox::from_id_salt("select_criterion")
                .selected_text(app.select_criterion.label())
                .show_ui(ui, |ui| {
                    for criterion in SelectCriterion::ALL {
                        ui.selectable_value(
                            &mut app.select_criterion,
                            criterion,
                            criterion.label(),
                        );
                    }
                });
            ui.checkbox(&mut app.select_invert, "Invert (select all others)");
            if ui.button("Apply").clicked() {
                app.select_by_criterion(app.select_criterion, app.select_invert);
            }
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let filtered_groups = filter_by_name_pattern(&app.groups, app.only_show_name_copies);
            let visible = compute_visible_entries(filtered_groups, app.only_show_duplicates);
            let shown_size: u64 = visible.iter().map(|f| f.size).sum();
            let visible_len = visible.len();
            let selected_count = visible
                .iter()
                .filter(|f| app.selection.contains(&f.path))
                .count();
            let selected_size: u64 = visible
                .iter()
                .filter(|f| app.selection.contains(&f.path))
                .map(|f| f.size)
                .sum();
            // Snapshot owned paths so `visible` (borrowed from `app.groups`) can
            // be dropped before the sections below need `&mut app`.
            let visible_paths: Vec<PathBuf> = visible.iter().map(|f| f.path.clone()).collect();
            drop(visible);

            // Trailing controls anchored to the right edge, so the row spreads
            // across the full panel width instead of bunching at the left.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let can_delete = !app.selection.is_empty();
                if ui
                    .add_enabled(
                        can_delete,
                        egui::Button::new(RichText::from(format!(
                            "{} Delete Selected",
                            icons::ICON_DELETE.codepoint
                        )))
                        .fill(if can_delete {
                            egui::Color32::from_rgb(120, 40, 40)
                        } else {
                            ui.visuals().widgets.inactive.bg_fill
                        }),
                    )
                    .clicked()
                {
                    app.delete_selected();
                }

                ui.separator();
                ui.label(format!(
                    "Shown: {visible_len} files, {}",
                    format_size(shown_size, DECIMAL)
                ));
                ui.colored_label(
                    egui::Color32::from_rgb(120, 200, 120),
                    format!(
                        "Selected: {selected_count} files, {}",
                        format_size(selected_size, DECIMAL)
                    ),
                );

                if let Some(msg) = app.status_message.clone() {
                    ui.separator();
                    ui.label(msg);
                }
            });

            ui.checkbox(&mut app.only_show_duplicates, "Only show duplicates");
            ui.checkbox(&mut app.only_show_name_copies, "Copy-named only")
                .on_hover_text(
                    "Only show groups where a duplicate's filename looks like an OS/user-generated \
                     copy of the original, e.g. \"photo (2).jpg\" or \"photo - Kopie.jpg\" next to \"photo.jpg\".",
                );
            ui.separator();

            if ui
                .button(RichText::from(format!(
                    "{} Select All Shown",
                    icons::ICON_SELECT_ALL.codepoint
                )))
                .clicked()
            {
                for path in &visible_paths {
                    app.selection.insert(path.clone());
                }
            }
            if ui
                .button(RichText::from(format!(
                    "{} Select None",
                    icons::ICON_DESELECT.codepoint
                )))
                .clicked()
            {
                app.selection.clear();
            }
        });
        ui.add_space(2.0);
    });
}

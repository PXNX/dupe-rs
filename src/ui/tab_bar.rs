use crate::app::{AppTab, DupeApp};
use egui::{Panel, Ui};
use egui_material_icons::icons;

/// Switches the window between the regular duplicate-scan UI and the
/// reverse-search tab. Always shown, above every other panel.
pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    Panel::top("tab_bar").show(ui, |ui| {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut app.tab,
                AppTab::Scan,
                format!("{} Duplicates", icons::ICON_FOLDER_SPECIAL.codepoint),
            );
            ui.selectable_value(
                &mut app.tab,
                AppTab::ReverseSearch,
                format!("{} Reverse Search", icons::ICON_MANAGE_SEARCH.codepoint),
            );
        });
        ui.add_space(2.0);
    });
}

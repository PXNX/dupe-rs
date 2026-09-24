use crate::app::{DupeApp, ViewMode};
use egui::{Align, Layout, Panel, RichText, Ui};
use egui_material_icons::icons;

/// Thin bar docked directly above the results table/grid, holding just the
/// view-mode switch in the top-right corner so it reads as a control over the
/// results area rather than getting lost among the selection actions below it.
pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    Panel::top("view_toolbar").show(ui, |ui| {
        ui.add_space(2.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.selectable_value(
                &mut app.view_mode,
                ViewMode::Grid,
                RichText::from(format!("{} Grid", icons::ICON_GRID_VIEW.codepoint)),
            );
            ui.selectable_value(
                &mut app.view_mode,
                ViewMode::Table,
                RichText::from(format!("{} Table", icons::ICON_TABLE.codepoint)),
            );
        });
        ui.add_space(2.0);
    });
}

use crate::model::DupeGroup;
use crate::stats;
use egui::Ui;
use egui_material_icons::icons;
use humansize::{DECIMAL, format_size};

pub fn show(ui: &mut Ui, groups: &[DupeGroup]) {
    let s = stats::compute(groups);
    ui.horizontal(|ui| {
        stat(ui, icons::ICON_LAYERS, "Groups", s.group_count.to_string());
        ui.separator();
        stat(
            ui,
            icons::ICON_FILE_COPY,
            "Duplicates",
            s.duplicate_file_count.to_string(),
        );
        ui.separator();
        stat(
            ui,
            icons::ICON_STORAGE,
            "Scanned",
            format!(
                "{} ({} files)",
                format_size(s.total_bytes, DECIMAL),
                s.total_file_count
            ),
        );
        ui.separator();
        ui.label(icons::ICON_SAVINGS.rich_text().color(egui::Color32::from_rgb(120, 200, 120)));
        ui.colored_label(
            egui::Color32::from_rgb(120, 200, 120),
            format!("Reclaimable: {}", format_size(s.wasted_bytes, DECIMAL)),
        );
    });
}

fn stat(ui: &mut Ui, icon: egui_material_icons::MaterialIcon, label: &str, value: String) {
    ui.label(icon.rich_text());
    ui.label(format!("{label}: {value}"));
}

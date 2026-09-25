use crate::app::SortDirection;
use egui::{Response, RichText, Ui};
use egui_material_icons::icons;

/// The Pause/Resume toggle shared by every pausable background job (scan,
/// index, delete), so they all look and read the same.
pub fn pause_resume_button(ui: &mut Ui, paused: bool) -> Response {
    let (icon, label) = if paused {
        (icons::ICON_PLAY_ARROW, "Resume")
    } else {
        (icons::ICON_PAUSE, "Pause")
    };
    ui.button(RichText::from(format!("{} {label}", icon.codepoint)))
}

/// Renders a header label that cycles Asc -> Desc -> unsorted on each click,
/// showing an arrow when it's the active sort column. Generic over the
/// table's own column enum.
pub fn sort_header<C: Copy + PartialEq>(
    ui: &mut Ui,
    label: &str,
    column: C,
    sort: &mut Option<(C, SortDirection)>,
) {
    let arrow = match sort {
        Some((c, SortDirection::Asc)) if *c == column => icons::ICON_ARROW_UPWARD.codepoint,
        Some((c, SortDirection::Desc)) if *c == column => icons::ICON_ARROW_DOWNWARD.codepoint,
        _ => "",
    };
    let text = if arrow.is_empty() {
        label.to_string()
    } else {
        format!("{label} {arrow}")
    };
    if ui.add(egui::Button::new(text).frame(false)).clicked() {
        *sort = match sort {
            Some((c, SortDirection::Asc)) if *c == column => Some((column, SortDirection::Desc)),
            Some((c, SortDirection::Desc)) if *c == column => None,
            _ => Some((column, SortDirection::Asc)),
        };
    }
}

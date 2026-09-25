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

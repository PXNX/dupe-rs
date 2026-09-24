use dupe_rs::app::DupeApp;

const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

fn main() -> eframe::Result<()> {
    let icon = eframe::icon_data::from_png_bytes(ICON_PNG).expect("bundled icon.png is valid");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(
        "dupe-rs",
        options,
        Box::new(|cc| {
            egui_material_icons::initialize(&cc.egui_ctx);
            Ok(Box::new(DupeApp::default()))
        }),
    )
}

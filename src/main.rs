// Release builds run as a GUI app with no console window; debug builds keep
// the console attached so `println!`/panics are still visible while developing.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use dupe_rs::app::DupeApp;

const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

fn main() -> eframe::Result<()> {
    // Started elevated by a drive health check: read SMART and exit.
    if let Some(code) = dupe_rs::smart::run_helper_from_args() {
        std::process::exit(code);
    }
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
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(DupeApp::default()))
        }),
    )
}

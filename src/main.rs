mod app;
mod config;
mod model;
mod scanner;
mod selection;
mod ui;

use app::DupeApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([960.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "dupe-rs",
        options,
        Box::new(|_cc| Ok(Box::new(DupeApp::default()))),
    )
}

#![windows_subsystem = "windows"]

mod app;
mod fonts;
mod gedcom;
mod graph;
mod minimap;
mod model;
mod tidy;
mod tree;
mod views;
mod widgets;

fn main() -> eframe::Result {
    let open = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Genie")
            .with_app_id("genie")
            .with_inner_size([1320.0, 860.0])
            .with_min_inner_size([860.0, 560.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Genie", options, Box::new(|cc| Ok(Box::new(app::GenieApp::new(cc, open)))))
}

mod app;
mod bridge;
mod settings;

pub use app::PatpansApp;
pub use settings::Settings;

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([460.0, 620.0])
            .with_min_inner_size([380.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "patpans",
        options,
        Box::new(|_cc| Ok(Box::new(PatpansApp::new()))),
    )
}

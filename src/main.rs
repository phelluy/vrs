mod app;
mod clangd;
mod editor;
mod files;
mod format;
mod language;
mod strings;
mod syntax;
mod terminal;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1024.0, 720.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };
    let app_creator = move |cc: &eframe::CreationContext| {
        Ok(Box::new(app::VrsApp::new(&cc.egui_ctx)) as Box<dyn eframe::App>)
    };
    eframe::run_native(strings::APP_NAME, options, Box::new(app_creator))
}

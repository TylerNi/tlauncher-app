#![windows_subsystem = "windows"]

mod api;
mod app;
mod install;
mod os;
mod update;

use std::sync::Arc;

use egui_software_backend::SoftwareBackendAppConfiguration;

fn main() {
    if let Some(link) = std::env::args().find(|a| a.starts_with("drop://")) {
        let _ = api::handshake(&link);
        return;
    }
    update::cleanup();
    std::thread::spawn(|| {
        let _ = update::run();
    });
    let icon = image::load_from_memory(include_bytes!("../assets/icon.png"))
        .unwrap()
        .to_rgba8();
    let icon = egui::IconData {
        width: icon.width(),
        height: icon.height(),
        rgba: icon.into_raw(),
    };
    let settings = SoftwareBackendAppConfiguration::new()
        .title(Some("Tlauncher".into()))
        .app_id(Some("tlauncher".into()))
        .inner_size(Some(egui::vec2(640.0, 420.0)))
        .min_inner_size(Some(egui::vec2(480.0, 120.0)))
        .icon(Some(Arc::new(icon)));
    let _ = egui_software_backend::run_app_with_software_backend(settings, app::App::new);
}

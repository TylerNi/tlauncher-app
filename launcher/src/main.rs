#![windows_subsystem = "windows"]

mod api;
mod app;
mod install;
mod os;
mod update;

use std::sync::Arc;

use egui_software_backend::SoftwareBackendAppConfiguration;

fn main() {
    #[cfg(windows)]
    if let Some(start) = std::env::args_os()
        .skip_while(|a| a != "--pick-folder")
        .nth(1)
    {
        os::folder_dialog(std::path::Path::new(&start));
        return;
    }
    if let Some(link) = std::env::args().find(|a| a.starts_with("drop://")) {
        let _ = api::handshake(&link);
        return;
    }
    let _ = std::fs::create_dir_all(os::data_dir());
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(os::data_dir().join("lock"));
    if let Ok(lock) = &lock
        && let Err(std::fs::TryLockError::WouldBlock) = lock.try_lock()
    {
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
    let mut settings = SoftwareBackendAppConfiguration::new()
        .title(Some("Tlauncher".into()))
        .app_id(Some("tlauncher".into()))
        .inner_size(Some(egui::vec2(640.0, 420.0)))
        .min_inner_size(Some(egui::vec2(480.0, 120.0)))
        .icon(Some(Arc::new(icon)));
    settings.viewport_builder.drag_and_drop = Some(false);
    let _ = egui_software_backend::run_app_with_software_backend(settings, app::App::new);
}

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use egui::{Align, Color32, ColorImage, Image, Layout, ProgressBar, RichText, TextureHandle, vec2};
use serde::{Deserialize, Serialize};

use crate::api::{self, Game, Version};
use crate::install::{self, Installed, Progress};
use crate::{os, update};

const BLUE: Color32 = Color32::from_rgb(37, 99, 235);
const RED: Color32 = Color32::from_rgb(185, 28, 28);
const GREEN: Color32 = Color32::from_rgb(74, 222, 128);
const AMBER: Color32 = Color32::from_rgb(251, 191, 36);

fn button(text: &str, fill: Color32) -> egui::Button<'_> {
    egui::Button::new(RichText::new(text).color(Color32::WHITE)).fill(fill)
}

fn status(ui: &mut egui::Ui, color: Color32, text: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        ui.colored_label(color, text);
    });
}

enum Launcher {
    Unknown,
    UpToDate,
    Ready(PathBuf),
}

enum Screen {
    Loading,
    Ready,
}

#[derive(Serialize, Deserialize, Default)]
struct State {
    root: Option<PathBuf>,
    installed: HashMap<String, Installed>,
}

impl State {
    fn root(&self) -> PathBuf {
        self.root
            .clone()
            .unwrap_or_else(|| os::data_dir().join("jeux"))
    }

    fn save(&self) {
        let _ = fs::create_dir_all(os::data_dir());
        let _ = fs::write(
            os::data_dir().join("state.json"),
            serde_json::to_vec(self).unwrap(),
        );
    }
}

struct Shared {
    screen: Screen,
    error: Option<String>,
    games: Vec<Game>,
    latest: HashMap<String, Version>,
    jobs: HashMap<String, Arc<Progress>>,
    covers: Vec<(String, ColorImage)>,
    state: State,
    launcher: Launcher,
}

enum Action {
    Folder,
    Install(Game),
    Play(String),
    Uninstall(String),
    Restart(PathBuf),
}

fn load(shared: &Mutex<Shared>, ctx: &egui::Context) {
    let result = api::games();
    let installed: Vec<String> = {
        let mut s = shared.lock().unwrap();
        match result {
            Ok(games) => s.games = games,
            Err(e) => {
                s.error = Some(e);
                s.games = s
                    .state
                    .installed
                    .iter()
                    .map(|(id, i)| Game {
                        id: id.clone(),
                        m_name: i.name.clone(),
                        m_cover_object_id: String::new(),
                        library_path: String::new(),
                    })
                    .collect();
            }
        }
        s.screen = Screen::Ready;
        s.state.installed.keys().cloned().collect()
    };
    ctx.request_repaint();

    for id in installed {
        if let Ok(Some(version)) = api::latest(&id) {
            shared.lock().unwrap().latest.insert(id, version);
            ctx.request_repaint();
        }
    }

    let covers: Vec<(String, String)> = shared
        .lock()
        .unwrap()
        .games
        .iter()
        .filter(|g| !g.m_cover_object_id.is_empty())
        .map(|g| (g.id.clone(), g.m_cover_object_id.clone()))
        .collect();
    let cache = os::data_dir().join("covers");
    let _ = fs::create_dir_all(&cache);
    for (id, object) in covers {
        let path = cache.join(&object);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => {
                let Ok(mut response) =
                    api::open(&format!("{}/api/v1/client/object/{object}", api::SERVER))
                else {
                    continue;
                };
                let Ok(bytes) = response.body_mut().read_to_vec() else {
                    continue;
                };
                let _ = fs::write(&path, &bytes);
                bytes
            }
        };
        let Ok(image) = image::load_from_memory(&bytes) else {
            continue;
        };
        let image = image.thumbnail(108, 144).to_rgba8();
        let image = ColorImage::from_rgba_unmultiplied(
            [image.width() as usize, image.height() as usize],
            &image,
        );
        shared.lock().unwrap().covers.push((id, image));
        ctx.request_repaint();
    }
}

pub struct App {
    shared: Arc<Mutex<Shared>>,
    ctx: egui::Context,
    textures: HashMap<String, TextureHandle>,
    confirm: Option<String>,
    height: f32,
}

impl App {
    pub fn new(ctx: egui::Context) -> Self {
        let mut fonts = egui::FontDefinitions::empty();
        fonts.font_data.insert(
            "Ubuntu-Light".into(),
            Arc::new(egui::FontData::from_static(epaint_default_fonts::UBUNTU_LIGHT)),
        );
        for family in fonts.families.values_mut() {
            family.push("Ubuntu-Light".into());
        }
        ctx.set_fonts(fonts);
        ctx.global_style_mut(|style| style.spacing.button_padding = vec2(12.0, 6.0));
        let state = fs::read(os::data_dir().join("state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let app = App {
            shared: Arc::new(Mutex::new(Shared {
                screen: Screen::Loading,
                error: None,
                games: Vec::new(),
                latest: HashMap::new(),
                jobs: HashMap::new(),
                covers: Vec::new(),
                state,
                launcher: Launcher::Unknown,
            })),
            ctx,
            textures: HashMap::new(),
            confirm: None,
            height: 0.0,
        };
        app.boot();
        app
    }

    fn spawn(&self, task: impl FnOnce(&Mutex<Shared>) + Send + 'static) {
        let shared = self.shared.clone();
        let ctx = self.ctx.clone();
        thread::spawn(move || {
            task(&shared);
            ctx.request_repaint();
        });
    }

    fn boot(&self) {
        let ctx = self.ctx.clone();
        self.spawn(move |shared| load(shared, &ctx));
        self.spawn(|shared| {
            let launcher = match update::run() {
                Ok(Some(exe)) => Launcher::Ready(exe),
                Ok(None) => Launcher::UpToDate,
                Err(_) => Launcher::Unknown,
            };
            shared.lock().unwrap().launcher = launcher;
        });
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::Folder => self.spawn(|shared| {
                let start = shared.lock().unwrap().state.root();
                let _ = fs::create_dir_all(&start);
                if let Some(folder) = os::pick_folder(&start) {
                    let mut s = shared.lock().unwrap();
                    s.state.root = Some(folder);
                    s.state.save();
                }
            }),
            Action::Install(game) => {
                let progress = Arc::new(Progress::new());
                {
                    let mut s = self.shared.lock().unwrap();
                    s.error = None;
                    s.jobs.insert(game.id.clone(), progress.clone());
                }
                let shared = self.shared.clone();
                let ctx = self.ctx.clone();
                let id = game.id.clone();
                thread::spawn(move || {
                    while shared.lock().unwrap().jobs.contains_key(&id) {
                        ctx.request_repaint();
                        thread::sleep(Duration::from_millis(500));
                    }
                });
                self.spawn(move |shared| {
                    let result = (|| -> Result<(Installed, Version), String> {
                        let version = api::latest(&game.id)?
                            .ok_or("Aucune version disponible pour ce jeu")?;
                        let (dir, previous) = {
                            let s = shared.lock().unwrap();
                            match s.state.installed.get(&game.id) {
                                Some(i) => (i.dir.clone(), Some(i.version.clone())),
                                None => (s.state.root().join(&game.library_path), None),
                            }
                        };
                        let launch = install::install(
                            &game.id,
                            &version,
                            &dir,
                            previous.as_deref(),
                            &progress,
                        )?;
                        let installed = Installed {
                            name: game.m_name.clone(),
                            version: version.version_id.clone(),
                            platform: version.platform.clone(),
                            dir,
                            command: launch.command,
                            umu: launch.umu_id_override,
                        };
                        Ok((installed, version))
                    })();
                    let mut s = shared.lock().unwrap();
                    s.jobs.remove(&game.id);
                    match result {
                        Ok((installed, version)) => {
                            s.state.installed.insert(game.id.clone(), installed);
                            s.latest.insert(game.id, version);
                            s.state.save();
                        }
                        Err(e) => s.error = Some(e),
                    }
                });
            }
            Action::Play(id) => {
                let installed = {
                    let mut s = self.shared.lock().unwrap();
                    s.error = None;
                    s.state.installed.get(&id).cloned()
                };
                let Some(mut installed) = installed else {
                    return;
                };
                self.spawn(move |shared| {
                    if install::refresh_launch(&id, &mut installed).is_ok() {
                        let mut s = shared.lock().unwrap();
                        if let Some(stored) = s.state.installed.get_mut(&id) {
                            stored.command = installed.command.clone();
                            stored.umu = installed.umu.clone();
                            s.state.save();
                        }
                    }
                    if let Err(e) = install::play(&id, &installed) {
                        shared.lock().unwrap().error = Some(e);
                    }
                });
            }
            Action::Uninstall(id) => {
                self.confirm = None;
                let installed = {
                    let mut s = self.shared.lock().unwrap();
                    s.latest.remove(&id);
                    let installed = s.state.installed.remove(&id);
                    s.state.save();
                    installed
                };
                if let Some(installed) = installed {
                    self.spawn(move |_| install::uninstall(&id, &installed));
                }
            }
            Action::Restart(exe) => update::restart(&exe),
        }
    }
}

impl egui_software_backend::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _backend: &mut egui_software_backend::SoftwareBackend) {
        let mut action = None;
        let shared = self.shared.clone();
        let mut s = shared.lock().unwrap();
        for (id, image) in s.covers.drain(..) {
            let texture = self.ctx.load_texture(&id, image, Default::default());
            self.textures.insert(id, texture);
        }

        ui.painter()
            .rect_filled(self.ctx.viewport_rect(), 0.0, ui.visuals().panel_fill);
        egui::CentralPanel::default().show_inside(ui, |ui| {
            if let Some(error) = &s.error {
                ui.colored_label(Color32::from_rgb(248, 113, 113), error);
                ui.add_space(8.0);
            }
            match s.screen {
                Screen::Loading => {
                    ui.centered_and_justified(|ui| ui.spinner());
                }
                Screen::Ready => {
                    ui.horizontal(|ui| {
                        ui.heading(RichText::new("Tlauncher").strong());
                        match &s.launcher {
                            Launcher::Ready(_) => status(ui, AMBER, "Mise à jour disponible"),
                            Launcher::UpToDate => status(ui, GREEN, "À jour"),
                            Launcher::Unknown => {}
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .button("Dossier des jeux")
                                .on_hover_text(s.state.root().display().to_string())
                                .clicked()
                            {
                                action = Some(Action::Folder);
                            }
                            let exe = match &s.launcher {
                                Launcher::Ready(exe) => Some(exe.clone()),
                                _ => None,
                            };
                            if ui
                                .add_enabled(
                                    exe.is_some() && s.jobs.is_empty(),
                                    button("Mettre à jour", BLUE),
                                )
                                .clicked()
                            {
                                action = exe.map(Action::Restart);
                            }
                        });
                    });
                    ui.separator();
                    if s.games.is_empty() {
                        ui.label("Aucun jeu pour le moment.");
                    }
                    let top = ui.cursor().top();
                    let list = egui::ScrollArea::vertical().show(ui, |ui| {
                        for game in &s.games {
                            ui.horizontal(|ui| {
                                let size = vec2(54.0, 72.0);
                                match self.textures.get(&game.id) {
                                    Some(texture) => {
                                        ui.add_sized(
                                            size,
                                            Image::new(texture).fit_to_exact_size(size),
                                        );
                                    }
                                    None => {
                                        ui.allocate_space(size);
                                    }
                                }
                                let installed = s.state.installed.get(&game.id);
                                let update = installed.is_some_and(|i| {
                                    s.latest
                                        .get(&game.id)
                                        .is_some_and(|v| v.version_id != i.version)
                                });
                                ui.vertical(|ui| {
                                    ui.add_space(14.0);
                                    ui.label(RichText::new(&game.m_name).size(16.0).strong());
                                    if update {
                                        status(ui, AMBER, "Mise à jour disponible");
                                    } else if installed.is_some() && s.latest.contains_key(&game.id)
                                    {
                                        status(ui, GREEN, "À jour");
                                    } else if installed.is_some() {
                                        ui.label("Installé");
                                    }
                                });
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if let Some(progress) = s.jobs.get(&game.id) {
                                        let done = progress.done.load(Ordering::Relaxed) as f32;
                                        let total = progress.total.load(Ordering::Relaxed) as f32;
                                        let text = if total == 0.0 {
                                            "Préparation…".to_string()
                                        } else {
                                            let speed = done
                                                / progress.start.elapsed().as_secs_f32()
                                                / 1_000_000.0;
                                            format!(
                                                "{:.0} % · {speed:.1} Mo/s",
                                                done / total * 100.0
                                            )
                                        };
                                        let fraction =
                                            if total == 0.0 { 0.0 } else { done / total };
                                        ui.add(
                                            ProgressBar::new(fraction)
                                                .desired_width(180.0)
                                                .text(text),
                                        );
                                    } else if installed.is_none() {
                                        if ui.add(button("Installer", BLUE)).clicked() {
                                            action = Some(Action::Install(game.clone()));
                                        }
                                    } else if self.confirm.as_ref() == Some(&game.id) {
                                        if ui.button("Annuler").clicked() {
                                            self.confirm = None;
                                        }
                                        if ui.add(button("Confirmer", RED)).clicked() {
                                            action = Some(Action::Uninstall(game.id.clone()));
                                        }
                                    } else {
                                        if ui.button("Désinstaller").clicked() {
                                            self.confirm = Some(game.id.clone());
                                        }
                                        if ui
                                            .add_enabled(update, button("Mettre à jour", BLUE))
                                            .clicked()
                                        {
                                            action = Some(Action::Install(game.clone()));
                                        }
                                        if ui.add(button("Jouer", BLUE)).clicked() {
                                            action = Some(Action::Play(game.id.clone()));
                                        }
                                    }
                                });
                            });
                            ui.add_space(6.0);
                        }
                    });
                    let height = (top + list.content_size.y + 8.0)
                        .clamp(120.0, 720.0)
                        .round();
                    if height != self.height {
                        self.height = height;
                        let width = self.ctx.viewport_rect().width();
                        self.ctx
                            .send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(
                                width, height,
                            )));
                    }
                }
            }
        });
        drop(s);
        if let Some(action) = action {
            self.act(action);
        }
    }
}

use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::os;

pub const SERVER: &str = "https://tlauncher.srvnt.ca";
pub const INVALID: &str = "Code d'accès invalide";
pub const BLOCKED: &str = "Accès bloqué : trop d'essais avec un mauvais code";

pub static CODE: Mutex<String> = Mutex::new(String::new());

static DEVICE: LazyLock<String> = LazyLock::new(|| {
    os::machine_id()
        .map(|id| hex::encode(Sha256::digest(format!("tlauncher:{id}"))))
        .unwrap_or_default()
});

pub static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_per_read(Some(Duration::from_secs(30)))
        .build()
        .into()
});

#[cfg(windows)]
pub const PLATFORM: &str = "Windows";
#[cfg(not(windows))]
pub const PLATFORM: &str = "Linux";

pub fn error(e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(401) => INVALID.into(),
        ureq::Error::StatusCode(403) => BLOCKED.into(),
        ureq::Error::StatusCode(c) => format!("Erreur du serveur ({c})"),
        e => format!("Connexion impossible ({e})"),
    }
}

pub fn request(url: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
    AGENT
        .get(url)
        .header("X-Tlauncher-Code", CODE.lock().unwrap().as_str())
        .header("X-Tlauncher-Device", DEVICE.as_str())
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Game {
    pub id: String,
    pub m_name: String,
    pub m_cover_object_id: String,
    pub library_path: String,
}

#[derive(Deserialize)]
struct StorePage {
    results: Vec<Game>,
    count: usize,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    pub version_id: String,
    pub platform: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Launch {
    pub command: String,
    pub platform: String,
    pub umu_id_override: Option<String>,
}

#[derive(Deserialize)]
pub struct VersionDetail {
    pub launches: Vec<Launch>,
}

pub fn get<T: DeserializeOwned>(path: &str) -> Result<T, String> {
    open(&format!("{SERVER}{path}"))?
        .body_mut()
        .with_config()
        .limit(u64::MAX)
        .read_json()
        .map_err(error)
}

pub fn open(url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
    request(url).call().map_err(error)
}

pub fn games() -> Result<Vec<Game>, String> {
    let platforms = if cfg!(windows) {
        "windows"
    } else {
        "linux,windows"
    };
    let mut games = Vec::new();
    loop {
        let page: StorePage = get(&format!(
            "/api/v1/store?take=50&skip={}&sort=name&order=asc&platform={platforms}",
            games.len()
        ))?;
        let empty = page.results.is_empty();
        games.extend(page.results);
        if empty || games.len() >= page.count {
            return Ok(games);
        }
    }
}

pub fn latest(game_id: &str) -> Result<Option<Version>, String> {
    let versions: Vec<Version> = get(&format!("/api/v1/client/game/{game_id}/versions"))?;
    let native = versions.iter().find(|v| v.platform == PLATFORM);
    let fallback = || {
        versions
            .iter()
            .find(|v| cfg!(not(windows)) && v.platform == "Windows")
    };
    Ok(native.or_else(fallback).cloned())
}

pub fn launch(game_id: &str, version_id: &str, platform: &str) -> Result<Launch, String> {
    let detail: VersionDetail = get(&format!(
        "/api/v1/client/game/{game_id}/version/{version_id}"
    ))?;
    detail
        .launches
        .into_iter()
        .find(|l| l.platform == platform)
        .ok_or_else(|| "Aucune commande de lancement pour ce jeu".into())
}

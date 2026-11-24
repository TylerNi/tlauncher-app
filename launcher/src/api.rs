use std::sync::LazyLock;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;

pub const SERVER: &str = "https://jeux.srvnt.ca";

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

fn error(e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(c) => format!("Erreur du serveur ({c})"),
        e => format!("Connexion impossible ({e})"),
    }
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
    AGENT.get(url).call().map_err(error)
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

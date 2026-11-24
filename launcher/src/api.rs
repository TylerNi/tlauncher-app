use std::fs;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use p384::ecdsa::signature::Signer;
use p384::ecdsa::{Signature, SigningKey};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::os;

pub const SERVER: &str = "https://tlauncher.srvnt.ca";

pub static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .build()
        .into()
});

#[cfg(windows)]
pub const PLATFORM: &str = "Windows";
#[cfg(not(windows))]
pub const PLATFORM: &str = "Linux";

pub enum Error {
    Unauthorized,
    Other(String),
}

impl From<ureq::Error> for Error {
    fn from(e: ureq::Error) -> Self {
        match e {
            ureq::Error::StatusCode(401 | 403) => Error::Unauthorized,
            ureq::Error::StatusCode(c) => Error::Other(format!("Erreur du serveur ({c})")),
            e => Error::Other(format!("Connexion impossible ({e})")),
        }
    }
}

impl From<Error> for String {
    fn from(e: Error) -> Self {
        match e {
            Error::Unauthorized => "Accès refusé par le serveur".into(),
            Error::Other(s) => s,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Auth {
    id: String,
    private: String,
    web: Option<String>,
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

pub struct Session {
    id: String,
    private: String,
    key: SigningKey,
    web: Mutex<Option<String>>,
}

impl Session {
    pub fn load() -> Option<Session> {
        let auth: Auth =
            serde_json::from_slice(&fs::read(os::data_dir().join("auth.json")).ok()?).ok()?;
        Some(Session {
            key: auth.private.parse().ok()?,
            id: auth.id,
            private: auth.private,
            web: Mutex::new(auth.web),
        })
    }

    pub fn forget() {
        let _ = fs::remove_file(os::data_dir().join("auth.json"));
    }

    fn jwt(&self) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = format!(r#"{{"nbf":{now},"exp":{}}}"#, now + 10);
        let message = format!(
            "{}.{}",
            B64.encode(r#"{"alg":"ES384","typ":"JWT"}"#),
            B64.encode(claims)
        );
        let signature: Signature = self.key.sign(message.as_bytes());
        format!(
            "JWT {} {}.{}",
            self.id,
            message,
            B64.encode(signature.to_bytes())
        )
    }

    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        Ok(self
            .open(&format!("{SERVER}{path}"))?
            .body_mut()
            .with_config()
            .limit(u64::MAX)
            .read_json()?)
    }

    pub fn open(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>, Error> {
        Ok(AGENT.get(url).header("Authorization", self.jwt()).call()?)
    }

    pub fn games(&self) -> Result<Vec<Game>, Error> {
        match self.store() {
            Err(Error::Unauthorized) => {
                *self.web.lock().unwrap() = None;
                self.store()
            }
            r => r,
        }
    }

    fn store(&self) -> Result<Vec<Game>, Error> {
        let token = self.web_token()?;
        let platforms = if cfg!(windows) {
            "windows"
        } else {
            "linux,windows"
        };
        let mut games = Vec::new();
        loop {
            let page: StorePage = AGENT
                .get(format!(
                    "{SERVER}/api/v1/store?take=50&skip={}&sort=name&order=asc&platform={platforms}",
                    games.len()
                ))
                .header("Authorization", format!("Bearer {token}"))
                .call()?
                .body_mut()
                .read_json()?;
            let empty = page.results.is_empty();
            games.extend(page.results);
            if empty || games.len() >= page.count {
                return Ok(games);
            }
        }
    }

    fn web_token(&self) -> Result<String, Error> {
        let mut web = self.web.lock().unwrap();
        if let Some(token) = web.as_ref() {
            return Ok(token.clone());
        }
        let token = AGENT
            .post(format!("{SERVER}/api/v1/client/user/webtoken"))
            .header("Authorization", self.jwt())
            .send_empty()?
            .body_mut()
            .read_to_string()?;
        let auth = Auth {
            id: self.id.clone(),
            private: self.private.clone(),
            web: Some(token.clone()),
        };
        let _ = fs::write(
            os::data_dir().join("auth.json"),
            serde_json::to_vec(&auth).unwrap(),
        );
        *web = Some(token.clone());
        Ok(token)
    }

    pub fn latest(&self, game_id: &str) -> Result<Option<Version>, Error> {
        let versions: Vec<Version> =
            self.get(&format!("/api/v1/client/game/{game_id}/versions"))?;
        let native = versions.iter().find(|v| v.platform == PLATFORM);
        let fallback = || {
            versions
                .iter()
                .find(|v| cfg!(not(windows)) && v.platform == "Windows")
        };
        Ok(native.or_else(fallback).cloned())
    }

    pub fn launch(
        &self,
        game_id: &str,
        version_id: &str,
        platform: &str,
    ) -> Result<Launch, String> {
        let detail: VersionDetail = self.get(&format!(
            "/api/v1/client/game/{game_id}/version/{version_id}"
        ))?;
        detail
            .launches
            .into_iter()
            .find(|l| l.platform == platform)
            .ok_or_else(|| "Aucune commande de lancement pour ce jeu".into())
    }
}

pub fn begin_login() -> Result<(), String> {
    os::register_link();
    let path = AGENT
        .post(format!("{SERVER}/api/v1/client/auth/initiate"))
        .send_json(json!({
            "name": format!("{} (Tlauncher)", os::hostname()),
            "platform": std::env::consts::OS,
            "capabilities": {},
            "mode": "callback",
        }))
        .map_err(Error::from)?
        .body_mut()
        .read_to_string()
        .map_err(Error::from)?;
    os::open_url(&format!("{SERVER}{path}"));
    Ok(())
}

pub fn handshake(link: &str) -> Result<(), String> {
    #[derive(Deserialize)]
    struct Response {
        private: String,
        id: String,
    }
    let rest = link
        .trim_start_matches("drop://handshake/")
        .trim_end_matches('/');
    let (id, token) = rest.split_once('/').ok_or("Lien invalide")?;
    let response: Response = AGENT
        .post(format!("{SERVER}/api/v1/client/auth/handshake"))
        .send_json(json!({ "clientId": id, "token": token }))
        .map_err(Error::from)?
        .body_mut()
        .read_json()
        .map_err(Error::from)?;
    let auth = Auth {
        id: response.id,
        private: response.private,
        web: None,
    };
    fs::create_dir_all(os::data_dir()).map_err(|e| e.to_string())?;
    fs::write(
        os::data_dir().join("auth.json"),
        serde_json::to_vec(&auth).unwrap(),
    )
    .map_err(|e| e.to_string())
}

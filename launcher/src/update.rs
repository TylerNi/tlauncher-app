use std::env;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::api::AGENT;

#[cfg(windows)]
const ASSET: &str = "tlauncher.exe";
#[cfg(not(windows))]
const ASSET: &str = "tlauncher-linux";

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

fn parse(version: &str) -> Vec<u64> {
    version
        .trim_start_matches('v')
        .split('.')
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

pub fn cleanup() {
    if let Ok(exe) = env::current_exe() {
        let _ = fs::remove_file(exe.with_extension("old"));
    }
}

pub fn run() -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    let release: Release = AGENT
        .get("https://api.github.com/repos/TylerNi/tlauncher-app/releases/latest")
        .call()?
        .body_mut()
        .read_json()?;
    if parse(&release.tag_name) <= parse(env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let Some(asset) = release.assets.iter().find(|a| a.name == ASSET) else {
        return Ok(None);
    };
    let exe = env::current_exe()?;
    let new = exe.with_extension("new");
    let mut response = AGENT.get(&asset.browser_download_url).call()?;
    io::copy(
        &mut response.body_mut().as_reader(),
        &mut File::create(&new)?,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&new, fs::Permissions::from_mode(0o755))?;
    }
    fs::rename(&exe, exe.with_extension("old"))?;
    fs::rename(&new, &exe)?;
    Ok(Some(exe))
}

pub fn restart(exe: &Path) {
    if Command::new(exe).arg("--restart").spawn().is_ok() {
        std::process::exit(0);
    }
}

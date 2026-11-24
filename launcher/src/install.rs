use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::Instant;

use aes::cipher::{KeyIvInit, StreamCipher};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::api::{Error, Launch, Session, Version};
use crate::os;

type Aes128Ctr64LE = ctr::Ctr64LE<aes::Aes128>;

const THREADS: usize = 4;
const BUFFER: usize = 1 << 20;

#[derive(Serialize, Deserialize, Clone)]
pub struct Installed {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub dir: PathBuf,
    pub command: String,
    pub umu: Option<String>,
}

pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub start: Instant,
}

impl Progress {
    pub fn new() -> Self {
        Progress {
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            start: Instant::now(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    file_list: HashMap<String, String>,
    manifests: HashMap<String, Droplet>,
}

#[derive(Deserialize)]
struct Droplet {
    key: [u8; 16],
    chunks: HashMap<String, Chunk>,
}

#[derive(Deserialize)]
struct Chunk {
    files: Vec<Entry>,
    checksum: String,
    iv: [u8; 16],
}

#[derive(Deserialize)]
struct Entry {
    filename: String,
    start: u64,
    length: u64,
    #[cfg_attr(windows, allow(dead_code))]
    permissions: u32,
}

#[derive(Deserialize)]
struct Depot {
    endpoint: String,
}

struct Job<'a> {
    version: &'a str,
    id: &'a str,
    chunk: &'a Chunk,
    key: &'a [u8; 16],
}

fn io(e: std::io::Error) -> String {
    format!("Erreur d'écriture sur le disque ({e})")
}

pub fn install(
    session: &Session,
    game_id: &str,
    version: &Version,
    dir: &Path,
    previous: Option<&str>,
    progress: &Progress,
) -> Result<Launch, String> {
    let launch = session.launch(game_id, &version.version_id, &version.platform)?;
    let previous_query = previous
        .map(|p| format!("&previous={p}"))
        .unwrap_or_default();
    let manifest: Manifest = session.get(&format!(
        "/api/v1/client/game/manifest?version={}{previous_query}",
        version.version_id
    ))?;

    if let Some(previous) = previous {
        let old: Manifest =
            session.get(&format!("/api/v1/client/game/manifest?version={previous}"))?;
        for file in old.file_list.keys() {
            if !manifest.file_list.contains_key(file) {
                let _ = fs::remove_file(dir.join(file));
            }
        }
    }

    fs::create_dir_all(dir).map_err(io)?;
    let marker = dir.join(".tlauncher");
    let mut done = HashSet::new();
    match fs::read_to_string(&marker) {
        Ok(text) if text.lines().next() == Some(version.version_id.as_str()) => {
            done.extend(text.lines().skip(1).map(String::from));
        }
        _ => fs::write(&marker, format!("{}\n", version.version_id)).map_err(io)?,
    }

    let mut sizes: HashMap<&str, u64> = HashMap::new();
    let mut jobs = Vec::new();
    let mut total = 0;
    for (version_id, droplet) in &manifest.manifests {
        for (id, chunk) in &droplet.chunks {
            for entry in &chunk.files {
                if manifest.file_list.get(&entry.filename) == Some(version_id) {
                    let size = sizes.entry(&entry.filename).or_default();
                    *size = (*size).max(entry.start + entry.length);
                }
            }
            if !done.contains(id) {
                total += chunk.files.iter().map(|e| e.length).sum::<u64>();
                jobs.push(Job {
                    version: version_id,
                    id,
                    chunk,
                    key: &droplet.key,
                });
            }
        }
    }
    progress.total.store(total, Ordering::Relaxed);

    for (file, size) in sizes {
        let path = dir.join(file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io)?;
        }
        let handle = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(io)?;
        handle.set_len(size).map_err(io)?;
    }

    let depots: Vec<Depot> = session.get("/api/v1/client/depots")?;
    let depot = depots.first().ok_or("Aucun dépôt sur le serveur")?;
    let depot = format!("{}/", depot.endpoint.trim_end_matches('/'));

    let next = AtomicUsize::new(0);
    let failure: Mutex<Option<String>> = Mutex::new(None);
    let log = Mutex::new(OpenOptions::new().append(true).open(&marker).map_err(io)?);
    thread::scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|| {
                while failure.lock().unwrap().is_none() {
                    let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    let mut attempts = 0;
                    loop {
                        let mut read = 0;
                        match download(
                            session,
                            &depot,
                            game_id,
                            job,
                            &manifest.file_list,
                            dir,
                            progress,
                            &mut read,
                        ) {
                            Ok(()) => {
                                let _ = writeln!(log.lock().unwrap(), "{}", job.id);
                                break;
                            }
                            Err(e) => {
                                progress.done.fetch_sub(read, Ordering::Relaxed);
                                attempts += 1;
                                if attempts == 3 {
                                    failure.lock().unwrap().get_or_insert(e);
                                    break;
                                }
                            }
                        }
                    }
                }
            });
        }
    });
    if let Some(e) = failure.into_inner().unwrap() {
        return Err(e);
    }
    let _ = fs::remove_file(&marker);
    Ok(launch)
}

#[allow(clippy::too_many_arguments)]
fn download(
    session: &Session,
    depot: &str,
    game_id: &str,
    job: &Job,
    file_list: &HashMap<String, String>,
    dir: &Path,
    progress: &Progress,
    read: &mut u64,
) -> Result<(), String> {
    let mut response = session
        .open(&format!(
            "{depot}content/{game_id}/{}/{}",
            job.version, job.id
        ))
        .map_err(String::from)?;
    let mut reader = response.body_mut().as_reader();
    let mut cipher = Aes128Ctr64LE::new(&(*job.key).into(), &job.chunk.iv.into());
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; BUFFER];
    for entry in &job.chunk.files {
        let path = dir.join(&entry.filename);
        let mut output = if file_list.get(&entry.filename).map(String::as_str) == Some(job.version)
        {
            let mut file = OpenOptions::new().write(true).open(&path).map_err(io)?;
            file.seek(SeekFrom::Start(entry.start)).map_err(io)?;
            Some(file)
        } else {
            None
        };
        let mut left = entry.length;
        while left > 0 {
            let n = reader
                .read(&mut buffer[..left.min(BUFFER as u64) as usize])
                .map_err(|e| format!("Téléchargement interrompu ({e})"))?;
            if n == 0 {
                return Err("Téléchargement interrompu".into());
            }
            cipher.apply_keystream(&mut buffer[..n]);
            hasher.update(&buffer[..n]);
            if let Some(file) = output.as_mut() {
                file.write_all(&buffer[..n]).map_err(io)?;
            }
            left -= n as u64;
            *read += n as u64;
            progress.done.fetch_add(n as u64, Ordering::Relaxed);
        }
        #[cfg(unix)]
        if output.is_some() {
            use std::os::unix::fs::PermissionsExt;
            let mode = if entry.permissions == 0 {
                0o744
            } else {
                entry.permissions
            };
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).map_err(io)?;
        }
    }
    if hex::encode(hasher.finalize()) != job.chunk.checksum {
        return Err("Fichier corrompu pendant le téléchargement".into());
    }
    Ok(())
}

pub fn uninstall(game_id: &str, installed: &Installed) {
    let _ = fs::remove_dir_all(&installed.dir);
    let _ = fs::remove_dir_all(os::data_dir().join("pfx").join(game_id));
}

pub fn play(game_id: &str, installed: &Installed) -> Result<(), String> {
    let parts =
        shell_words::split(&installed.command).map_err(|_| "Commande de lancement invalide")?;
    let index = parts
        .iter()
        .position(|p| !p.contains('='))
        .ok_or("Commande de lancement invalide")?;
    let env = &parts[..index];
    let mut program = parts[index].clone();
    let mut args = parts[index + 1..].to_vec();
    let local = installed.dir.join(&program);

    if installed.platform == "Windows" && cfg!(not(windows)) {
        args.insert(0, local.to_string_lossy().into_owned());
        program = "umu-run".into();
    } else if cfg!(windows) {
        let extension = local
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase());
        match extension.as_deref() {
            Some("exe" | "com") => program = local.to_string_lossy().into_owned(),
            Some("ps1") => {
                args.splice(
                    0..0,
                    ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]
                        .map(String::from)
                        .into_iter()
                        .chain([local.to_string_lossy().into_owned()]),
                );
                program = "powershell".into();
            }
            _ => {
                args.splice(0..0, ["/C".to_string(), program]);
                program = "cmd".into();
            }
        }
    } else if local.is_file() {
        program = local.to_string_lossy().into_owned();
    }

    let mut command = Command::new(&program);
    command.args(&args).current_dir(&installed.dir);
    for pair in env {
        if let Some((key, value)) = pair.split_once('=') {
            command.env(key, value);
        }
    }
    if installed.platform == "Windows" && cfg!(not(windows)) {
        let prefix = os::data_dir().join("pfx").join(game_id);
        fs::create_dir_all(&prefix).map_err(io)?;
        command
            .env(
                "GAMEID",
                installed
                    .umu
                    .as_deref()
                    .filter(|u| !u.is_empty())
                    .unwrap_or("umu-default"),
            )
            .env("WINEPREFIX", prefix);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().map_err(|e| {
        if program == "umu-run" {
            "umu-run est introuvable : installe umu-launcher pour jouer aux jeux Windows"
                .to_string()
        } else {
            format!("Impossible de lancer le jeu ({e})")
        }
    })?;
    thread::spawn(move || child.wait());
    Ok(())
}

pub fn refresh_launch(
    session: &Session,
    game_id: &str,
    installed: &mut Installed,
) -> Result<(), Error> {
    let detail: crate::api::VersionDetail = session.get(&format!(
        "/api/v1/client/game/{game_id}/version/{}",
        installed.version
    ))?;
    if let Some(launch) = detail
        .launches
        .into_iter()
        .find(|l| l.platform == installed.platform)
    {
        installed.command = launch.command;
        installed.umu = launch.umu_id_override;
    }
    Ok(())
}

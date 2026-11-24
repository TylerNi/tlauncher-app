use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasher, RandomState};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use aes::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use serde::{Deserialize, Serialize};
use sha2::digest::common::hazmat::{SerializableState, SerializedState};
use sha2::{Digest, Sha256};

use crate::api::{self, Launch, Version};
use crate::os;

type Aes128Ctr64LE = ctr::Ctr64LE<aes::Aes128>;

const THREADS: usize = 4;
const BUFFER: usize = 1 << 20;
const CHECKPOINT: u64 = 8 << 20;
const MIB: u64 = 1 << 20;
const TIER: u64 = 64 << 20;
const PAD: u64 = 64 << 20;

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
    pub paused: AtomicBool,
    pub offline: AtomicBool,
    mark: Mutex<(Instant, u64)>,
}

impl Progress {
    pub fn new() -> Self {
        Progress {
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            paused: AtomicBool::new(false),
            offline: AtomicBool::new(false),
            mark: Mutex::new((Instant::now(), 0)),
        }
    }

    pub fn speed(&self) -> f32 {
        let (start, base) = *self.mark.lock().unwrap();
        let done = self.done.load(Ordering::Relaxed).saturating_sub(base);
        done as f32 / start.elapsed().as_secs_f32().max(0.001) / 1_000_000.0
    }

    fn restart(&self) {
        *self.mark.lock().unwrap() = (Instant::now(), self.done.load(Ordering::Relaxed));
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

enum Kind<'a> {
    Chunk {
        version: &'a str,
        id: &'a str,
        chunk: &'a Chunk,
        key: &'a [u8; 16],
    },
    Pad,
}

struct Job<'a> {
    kind: Kind<'a>,
    length: u64,
    part: Mutex<Option<Part>>,
}

#[derive(Default)]
struct Part {
    offset: u64,
    hasher: Sha256,
}

impl Part {
    fn parse(offset: &str, state: &str) -> Option<Part> {
        let bytes = hex::decode(state).ok()?;
        let state: &SerializedState<Sha256> = bytes.as_slice().try_into().ok()?;
        Some(Part {
            offset: offset.parse().ok()?,
            hasher: Sha256::deserialize(state).ok()?,
        })
    }
}

struct Target<'a> {
    depot: &'a str,
    game_id: &'a str,
    file_list: &'a HashMap<String, String>,
    dir: &'a Path,
    log: Mutex<File>,
}

enum Stop {
    Paused,
    Network,
    Corrupt,
    Fatal(String),
}

fn io(e: std::io::Error) -> String {
    format!("Erreur d'écriture sur le disque ({e})")
}

fn disk(e: std::io::Error) -> Stop {
    Stop::Fatal(io(e))
}

fn stop(e: ureq::Error) -> Stop {
    match e {
        ureq::Error::StatusCode(416) => Stop::Corrupt,
        ureq::Error::StatusCode(c) if c != 408 && c != 429 && c < 500 => {
            Stop::Fatal(format!("Erreur du serveur ({c})"))
        }
        _ => Stop::Network,
    }
}

fn random() -> u64 {
    RandomState::new().hash_one(0)
}

fn save(target: &Target, id: &str, part: &Part, output: Option<&File>) -> Result<(), Stop> {
    if let Some(file) = output {
        file.sync_data().map_err(disk)?;
    }
    let line = format!(
        "{id} {} {}\n",
        part.offset,
        hex::encode(part.hasher.serialize())
    );
    let _ = target.log.lock().unwrap().write_all(line.as_bytes());
    Ok(())
}

pub fn install(
    game_id: &str,
    version: &Version,
    dir: &Path,
    previous: Option<&str>,
    progress: &Progress,
) -> Result<Launch, String> {
    let launch = api::launch(game_id, &version.version_id, &version.platform)?;
    let old: Option<Manifest> =
        previous.and_then(|p| api::get(&format!("/api/v1/client/game/manifest?version={p}")).ok());
    let previous_query = previous
        .filter(|_| old.is_some())
        .map(|p| format!("&previous={p}"))
        .unwrap_or_default();
    let manifest: Manifest = api::get(&format!(
        "/api/v1/client/game/manifest?version={}{previous_query}",
        version.version_id
    ))?;

    if let Some(old) = old {
        for file in old.file_list.keys() {
            if !manifest.file_list.contains_key(file) {
                let _ = fs::remove_file(dir.join(file));
            }
        }
    }

    fs::create_dir_all(dir).map_err(io)?;
    let marker = dir.join(".tlauncher");
    let mut done = HashSet::new();
    let mut parts = HashMap::new();
    match fs::read_to_string(&marker) {
        Ok(text) if text.lines().next() == Some(version.version_id.as_str()) => {
            for line in text.lines().skip(1) {
                match line.split(' ').collect::<Vec<_>>()[..] {
                    [id] => {
                        done.insert(id.to_string());
                    }
                    [id, offset, state] => {
                        if let Some(part) = Part::parse(offset, state) {
                            parts.insert(id.to_string(), part);
                        }
                    }
                    _ => {}
                }
            }
            if !text.ends_with('\n') {
                OpenOptions::new()
                    .append(true)
                    .open(&marker)
                    .and_then(|mut f| f.write_all(b"\n"))
                    .map_err(io)?;
            }
        }
        _ => fs::write(&marker, format!("{}\n", version.version_id)).map_err(io)?,
    }

    let mut sizes: HashMap<&str, u64> = HashMap::new();
    let mut jobs = Vec::new();
    let mut total = 0;
    let mut finished = 0;
    for (version_id, droplet) in &manifest.manifests {
        for (id, chunk) in &droplet.chunks {
            for entry in &chunk.files {
                if manifest.file_list.get(&entry.filename) == Some(version_id) {
                    let size = sizes.entry(&entry.filename).or_default();
                    *size = (*size).max(entry.start + entry.length);
                }
            }
            let length = chunk.files.iter().map(|e| e.length).sum::<u64>();
            total += length;
            if done.contains(id) {
                finished += length;
                continue;
            }
            let part = parts.remove(id).filter(|p: &Part| p.offset <= length);
            finished += part.as_ref().map_or(0, |p| p.offset);
            jobs.push(Job {
                kind: Kind::Chunk {
                    version: version_id,
                    id,
                    chunk,
                    key: &droplet.key,
                },
                length,
                part: Mutex::new(part),
            });
        }
    }
    let remaining = total - finished;
    if remaining > 0 {
        let tier = remaining.next_power_of_two().max(TIER);
        let mut pad = tier - remaining + random() % (tier / 16);
        total += pad;
        while pad > 0 {
            let length = (MIB + random() % (PAD - MIB)).min(pad);
            pad -= length;
            jobs.push(Job {
                kind: Kind::Pad,
                length,
                part: Mutex::new(None),
            });
        }
        for i in (1..jobs.len()).rev() {
            jobs.swap(i, random() as usize % (i + 1));
        }
    }
    progress.total.store(total, Ordering::Relaxed);
    progress.done.store(finished, Ordering::Relaxed);
    progress.restart();

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

    let depots: Vec<Depot> = api::get("/api/v1/client/depots")?;
    let depot = depots.first().ok_or("Aucun dépôt sur le serveur")?;
    let depot = format!("{}/", depot.endpoint.trim_end_matches('/'));

    let target = Target {
        depot: &depot,
        game_id,
        file_list: &manifest.file_list,
        dir,
        log: Mutex::new(OpenOptions::new().append(true).open(&marker).map_err(io)?),
    };
    let next = AtomicUsize::new(0);
    let failure: Mutex<Option<String>> = Mutex::new(None);
    let stopped = || failure.lock().unwrap().is_some();
    thread::scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|| {
                while !stopped() {
                    let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    let mut part = job.part.lock().unwrap().take().unwrap_or_default();
                    let mut resets = 0;
                    let mut delay = 1;
                    loop {
                        if progress.paused.load(Ordering::Relaxed) {
                            while progress.paused.load(Ordering::Relaxed) && !stopped() {
                                thread::sleep(Duration::from_millis(200));
                            }
                            progress.restart();
                        }
                        if stopped() {
                            break;
                        }
                        let before = part.offset;
                        let result = match job.kind {
                            Kind::Chunk { id, .. } => {
                                fetch(&target, job, &mut part, progress).map(|()| Some(id))
                            }
                            Kind::Pad => pad(job, &mut part, progress).map(|()| None),
                        };
                        match result {
                            Ok(id) => {
                                if let Some(id) = id {
                                    let line = format!("{id}\n");
                                    let _ = target.log.lock().unwrap().write_all(line.as_bytes());
                                }
                                break;
                            }
                            Err(Stop::Paused) => {}
                            Err(Stop::Network) => {
                                if part.offset > before {
                                    delay = 1;
                                }
                                progress.offline.store(true, Ordering::Relaxed);
                                let until = Instant::now() + Duration::from_secs(delay);
                                while Instant::now() < until
                                    && !progress.paused.load(Ordering::Relaxed)
                                    && !stopped()
                                {
                                    thread::sleep(Duration::from_millis(200));
                                }
                                delay = (delay * 2).min(15);
                            }
                            Err(Stop::Corrupt) => {
                                progress.done.fetch_sub(part.offset, Ordering::Relaxed);
                                part = Part::default();
                                if let Kind::Chunk { id, .. } = job.kind {
                                    let _ = save(&target, id, &part, None);
                                }
                                resets += 1;
                                if resets == 3 {
                                    failure.lock().unwrap().get_or_insert(
                                        "Fichier corrompu pendant le téléchargement".into(),
                                    );
                                }
                            }
                            Err(Stop::Fatal(e)) => {
                                failure.lock().unwrap().get_or_insert(e);
                            }
                        }
                    }
                }
            });
        }
    });
    drop(target);
    if let Some(e) = failure.into_inner().unwrap() {
        return Err(e);
    }
    let _ = fs::remove_file(&marker);
    Ok(launch)
}

fn fetch(target: &Target, job: &Job, part: &mut Part, progress: &Progress) -> Result<(), Stop> {
    let Kind::Chunk {
        version,
        id,
        chunk,
        key,
    } = job.kind
    else {
        return Ok(());
    };
    if part.offset < job.length {
        let mut request = api::AGENT.get(&format!(
            "{}content/{}/{version}/{id}",
            target.depot, target.game_id
        ));
        if part.offset > 0 {
            request = request.header("Range", format!("bytes={}-", part.offset));
        }
        let mut response = request.call().map_err(stop)?;
        let mut skip = part.offset;
        if response.status() == 206 {
            let range = response
                .headers()
                .get("content-range")
                .and_then(|r| r.to_str().ok())
                .unwrap_or("");
            if !range.starts_with(&format!("bytes {}-", part.offset)) {
                return Err(Stop::Corrupt);
            }
            skip = 0;
        }
        let mut reader = response.body_mut().as_reader();
        let mut buffer = vec![0u8; BUFFER];
        while skip > 0 {
            match reader.read(&mut buffer[..skip.min(BUFFER as u64) as usize]) {
                Ok(n) if n > 0 => skip -= n as u64,
                _ => return Err(Stop::Network),
            }
        }
        let mut cipher = Aes128Ctr64LE::new(&(*key).into(), &chunk.iv.into());
        cipher.seek(part.offset);
        let mut saved = part.offset;
        let mut position = 0;
        for entry in &chunk.files {
            let end = position + entry.length;
            if end < part.offset || (end == part.offset && entry.length > 0) {
                position = end;
                continue;
            }
            let within = part.offset - position;
            let path = target.dir.join(&entry.filename);
            let mut output =
                if target.file_list.get(&entry.filename).map(String::as_str) == Some(version) {
                    let mut file = OpenOptions::new().write(true).open(&path).map_err(disk)?;
                    file.seek(SeekFrom::Start(entry.start + within))
                        .map_err(disk)?;
                    Some(file)
                } else {
                    None
                };
            let mut left = entry.length - within;
            while left > 0 {
                if progress.paused.load(Ordering::Relaxed) {
                    save(target, id, part, output.as_ref())?;
                    return Err(Stop::Paused);
                }
                let n = match reader.read(&mut buffer[..left.min(BUFFER as u64) as usize]) {
                    Ok(n) if n > 0 => n,
                    _ => {
                        save(target, id, part, output.as_ref())?;
                        return Err(Stop::Network);
                    }
                };
                if progress.offline.swap(false, Ordering::Relaxed) {
                    progress.restart();
                }
                cipher.apply_keystream(&mut buffer[..n]);
                if let Some(file) = output.as_mut() {
                    file.write_all(&buffer[..n]).map_err(disk)?;
                }
                part.hasher.update(&buffer[..n]);
                part.offset += n as u64;
                left -= n as u64;
                progress.done.fetch_add(n as u64, Ordering::Relaxed);
                if part.offset - saved >= CHECKPOINT {
                    save(target, id, part, output.as_ref())?;
                    saved = part.offset;
                }
            }
            if let Some(file) = &output {
                file.sync_data().map_err(disk)?;
            }
            #[cfg(unix)]
            if output.is_some() {
                use std::os::unix::fs::PermissionsExt;
                let mode = if entry.permissions == 0 {
                    0o744
                } else {
                    entry.permissions
                };
                fs::set_permissions(&path, fs::Permissions::from_mode(mode)).map_err(disk)?;
            }
            position = end;
        }
    }
    if hex::encode(part.hasher.clone().finalize()) != chunk.checksum {
        return Err(Stop::Corrupt);
    }
    Ok(())
}

fn pad(job: &Job, part: &mut Part, progress: &Progress) -> Result<(), Stop> {
    let response = api::AGENT
        .get(&format!("{}/pad.bin", api::SERVER))
        .header("Range", format!("bytes={}-{}", part.offset, job.length - 1))
        .call();
    let mut response = match response {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(_)) => {
            progress
                .total
                .fetch_sub(job.length - part.offset, Ordering::Relaxed);
            return Ok(());
        }
        Err(_) => return Err(Stop::Network),
    };
    let mut reader = response.body_mut().as_reader();
    let mut buffer = vec![0u8; BUFFER];
    while part.offset < job.length {
        if progress.paused.load(Ordering::Relaxed) {
            return Err(Stop::Paused);
        }
        let n = match reader
            .read(&mut buffer[..(job.length - part.offset).min(BUFFER as u64) as usize])
        {
            Ok(n) if n > 0 => n,
            _ => return Err(Stop::Network),
        };
        if progress.offline.swap(false, Ordering::Relaxed) {
            progress.restart();
        }
        part.offset += n as u64;
        progress.done.fetch_add(n as u64, Ordering::Relaxed);
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

pub fn refresh_launch(game_id: &str, installed: &mut Installed) -> Result<(), String> {
    let detail: api::VersionDetail = api::get(&format!(
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

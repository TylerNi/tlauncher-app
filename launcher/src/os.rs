use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
pub fn data_dir() -> PathBuf {
    PathBuf::from(env::var_os("LOCALAPPDATA").unwrap()).join("Tlauncher")
}

#[cfg(not(windows))]
pub fn data_dir() -> PathBuf {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap()).join(".local/share"))
        .join("tlauncher")
}

#[cfg(windows)]
pub fn hostname() -> String {
    env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into())
}

#[cfg(not(windows))]
pub fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "PC".into())
}

#[cfg(windows)]
pub fn open_url(url: &str) {
    let _ = Command::new("explorer").arg(url).spawn();
}

#[cfg(not(windows))]
pub fn open_url(url: &str) {
    let _ = Command::new("xdg-open").arg(url).spawn();
}

#[cfg(windows)]
pub fn register_link() {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
    let exe = env::current_exe().unwrap();
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let set = |key: &str, name: Option<&str>, value: &str| {
        let key = wide(key);
        let name = name.map(wide);
        let value = wide(value);
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            );
        }
    };
    set(r"Software\Classes\drop", None, "URL:Tlauncher");
    set(r"Software\Classes\drop", Some("URL Protocol"), "");
    set(
        r"Software\Classes\drop\shell\open\command",
        None,
        &format!("\"{}\" \"%1\"", exe.display()),
    );
}

#[cfg(not(windows))]
pub fn register_link() {
    let exe = env::current_exe().unwrap();
    let apps = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap()).join(".local/share"))
        .join("applications");
    let _ = std::fs::create_dir_all(&apps);
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Tlauncher\nExec=\"{}\" %u\nTerminal=false\nMimeType=x-scheme-handler/drop;\n",
        exe.display()
    );
    let _ = std::fs::write(apps.join("tlauncher.desktop"), entry);
    let _ = Command::new("xdg-mime")
        .args(["default", "tlauncher.desktop", "x-scheme-handler/drop"])
        .status();
    let _ = Command::new("update-desktop-database").arg(&apps).status();
}

#[cfg(windows)]
pub fn pick_folder(start: &Path) -> Option<PathBuf> {
    let out = Command::new(env::current_exe().ok()?)
        .arg("--pick-folder")
        .arg(start)
        .output()
        .ok()?;
    let path = String::from_utf8(out.stdout).ok()?;
    (!path.is_empty()).then(|| PathBuf::from(path))
}

#[cfg(windows)]
pub fn folder_dialog(start: &Path) {
    if let Some(folder) = rfd::FileDialog::new().set_directory(start).pick_folder() {
        print!("{}", folder.display());
    }
}

#[cfg(not(windows))]
pub fn pick_folder(start: &Path) -> Option<PathBuf> {
    let zenity = format!("--filename={}/", start.display());
    let kdialog = start.display().to_string();
    let attempts: [&[&str]; 2] = [
        &["zenity", "--file-selection", "--directory", &zenity],
        &["kdialog", "--getexistingdirectory", &kdialog],
    ];
    for args in attempts {
        if let Ok(out) = Command::new(args[0]).args(&args[1..]).output() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            return (out.status.success() && !path.is_empty()).then(|| PathBuf::from(path));
        }
    }
    None
}

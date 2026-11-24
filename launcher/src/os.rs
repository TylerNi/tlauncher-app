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

#[cfg(windows)]
pub fn focus_window() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow,
    };
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let class = wide("Window Class");
    let title = wide("Tlauncher");
    unsafe {
        let window = FindWindowW(class.as_ptr(), title.as_ptr());
        if IsIconic(window) != 0 {
            ShowWindow(window, SW_RESTORE);
        }
        SetForegroundWindow(window);
    }
}

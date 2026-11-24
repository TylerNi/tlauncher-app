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
pub fn machine_id() -> Option<String> {
    use windows_sys::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY, RegGetValueW,
    };
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let key = wide("SOFTWARE\\Microsoft\\Cryptography");
    let name = wide("MachineGuid");
    let mut buffer = [0u16; 128];
    let mut size = (buffer.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    let length = (size as usize / 2).saturating_sub(1);
    (status == 0 && length > 0).then(|| String::from_utf16_lossy(&buffer[..length]))
}

#[cfg(not(windows))]
pub fn machine_id() -> Option<String> {
    let id = std::fs::read_to_string("/etc/machine-id").ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_string())
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
        FindWindowW, IsIconic, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
    };
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let class = wide("Window Class");
    let title = wide("Tlauncher");
    unsafe {
        let window = FindWindowW(class.as_ptr(), title.as_ptr());
        ShowWindow(
            window,
            if IsIconic(window) != 0 {
                SW_RESTORE
            } else {
                SW_SHOW
            },
        );
        SetForegroundWindow(window);
    }
}

#[cfg(windows)]
pub fn quit() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE};
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let class = wide("Window Class");
    let title = wide("Tlauncher");
    unsafe {
        let window = FindWindowW(class.as_ptr(), title.as_ptr());
        if !window.is_null() {
            PostMessageW(window, WM_CLOSE, 0, 0);
        }
    }
    if let Ok(lock) = std::fs::File::open(data_dir().join("lock")) {
        let _ = lock.lock();
    }
}

#[cfg(not(windows))]
pub fn install() {
    let target = data_dir().join("tlauncher");
    let Ok(exe) = env::current_exe() else {
        return;
    };
    if std::fs::canonicalize(&target).is_ok_and(|t| t == exe)
        || env::args().any(|a| a == "--restart")
    {
        return;
    }
    let new = target.with_extension("new");
    if std::fs::copy(&exe, &new).is_ok() {
        let _ = std::fs::rename(&new, &target);
    }
    if Command::new(&target)
        .args(env::args_os().skip(1))
        .arg("--restart")
        .spawn()
        .is_ok()
    {
        std::process::exit(0);
    }
}

#[cfg(windows)]
pub fn shortcut() {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows_sys::core::{GUID, HRESULT};

    #[repr(C)]
    struct ShellLink {
        query_interface:
            unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
        add_ref: usize,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        unused: [usize; 17],
        set_path: unsafe extern "system" fn(*mut c_void, *const u16) -> HRESULT,
    }
    #[repr(C)]
    struct PersistFile {
        unused: [usize; 2],
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        unused2: [usize; 3],
        save: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> HRESULT,
    }

    let (Ok(exe), Some(appdata)) = (env::current_exe(), env::var_os("APPDATA")) else {
        return;
    };
    let link =
        PathBuf::from(appdata).join("Microsoft\\Windows\\Start Menu\\Programs\\Tlauncher.lnk");
    let wide = |p: &Path| p.as_os_str().encode_wide().chain([0]).collect::<Vec<u16>>();
    let (exe, link) = (wide(&exe), wide(&link));
    unsafe {
        CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let mut shell: *mut *const ShellLink = std::ptr::null_mut();
        CoCreateInstance(
            &GUID::from_u128(0x00021401_0000_0000_c000_000000000046),
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &GUID::from_u128(0x000214f9_0000_0000_c000_000000000046),
            (&raw mut shell).cast(),
        );
        if !shell.is_null() {
            ((**shell).set_path)(shell.cast(), exe.as_ptr());
            let mut file: *mut *const PersistFile = std::ptr::null_mut();
            ((**shell).query_interface)(
                shell.cast(),
                &GUID::from_u128(0x0000010b_0000_0000_c000_000000000046),
                (&raw mut file).cast(),
            );
            if !file.is_null() {
                ((**file).save)(file.cast(), link.as_ptr(), 1);
                ((**file).release)(file.cast());
            }
            ((**shell).release)(shell.cast());
        }
        CoUninitialize();
    }
}

#[cfg(windows)]
pub fn autostart(on: bool) {
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW,
    };
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    let name = wide("Tlauncher");
    if !on {
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
        return;
    }
    let Ok(exe) = env::current_exe() else {
        return;
    };
    let command = wide(&format!("\"{}\" --background", exe.display()));
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            REG_SZ,
            command.as_ptr().cast(),
            (command.len() * 2) as u32,
        )
    };
}

#[cfg(not(windows))]
fn entry(args: &str) -> Option<String> {
    let exe = env::current_exe().ok()?;
    Some(format!(
        "[Desktop Entry]\nType=Application\nName=Tlauncher\nExec=\"{}\"{args}\nIcon={}\nCategories=Game;\n",
        exe.display(),
        data_dir().join("icon.png").display()
    ))
}

#[cfg(not(windows))]
pub fn shortcut() {
    let dir = data_dir().with_file_name("applications");
    let _ = std::fs::write(
        data_dir().join("icon.png"),
        include_bytes!("../assets/icon.png"),
    );
    if let Some(entry) = entry("") {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("tlauncher.desktop"), entry);
    }
}

#[cfg(not(windows))]
pub fn autostart(on: bool) {
    let dir = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap()).join(".config"))
        .join("autostart");
    let file = dir.join("tlauncher.desktop");
    match entry(" --background") {
        Some(entry) if on => {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(file, entry);
        }
        _ => {
            let _ = std::fs::remove_file(file);
        }
    }
}

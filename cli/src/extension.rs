use std::path::{Path, PathBuf};

/// Stable Chromium extension ID, derived from the public key baked into the
/// extension's manifest.json ("key" field). Keep in sync with the
/// zing-interceptor repo: if the key changes, this ID changes too.
const CHROMIUM_ID: &str = "bcpghfjbokiclpfonepejdcndaoomcpf";

/// The gecko extension ID declared in the extension's manifest.json
/// (browser_specific_settings.gecko.id). Firefox native-host manifests list
/// it verbatim, without a scheme prefix.
const GECKO_ID: &str = "oss.zing.intercept@tharukrj";

/// Install native-host manifests for every Chromium-based browser found on the
/// system plus Firefox. On Linux we scan:
///
/// - `~/.config/*/NativeMessagingHosts/` — native packages
/// - `~/.var/app/*/config/*/NativeMessagingHosts/` — Flatpak
/// - `~/snap/*/config/*/NativeMessagingHosts/` — Snap
///
/// New browsers are picked up automatically without code changes.
pub fn install() -> Result<(), String> {
    let host = host_executable()?;
    let manifest_name = format!("{}.json", host_name());
    let mut count = 0usize;

    // ── Chromium-based browsers: glob all NativeMessagingHosts dirs ──
    let chromium = chromium_manifest(&host);
    for dir in find_chromium_host_dirs() {
        let path = dir.join(&manifest_name);
        write_manifest(&path, &chromium)?;
        eprintln!("Installed native host -> {}", path.display());
        count += 1;
    }

    // ── Firefox ──
    if let Some(firefox_dir) = firefox_native_host_dir() {
        let path = firefox_dir.join(&manifest_name);
        std::fs::create_dir_all(&firefox_dir)
            .map_err(|e| format!("create {}: {e}", firefox_dir.display()))?;
        write_manifest(&path, &firefox_manifest(&host))?;
        eprintln!("Installed native host -> {}", path.display());
        count += 1;
    }

    // ── Windows registry ──
    #[cfg(target_os = "windows")]
    {
        let dir = windows_manifest_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let path = dir.join(&manifest_name);
        write_manifest(&path, &chromium)?;
        register_windows(&path)?;
        eprintln!("Installed native host -> {}", path.display());
        count += 1;
    }

    if count == 0 {
        eprintln!("No browser native-messaging directories found");
    }
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    let manifest_name = format!("{}.json", host_name());

    // ── Chromium-based browsers ──
    for dir in find_chromium_host_dirs() {
        let path = dir.join(&manifest_name);
        if path.exists() {
            match std::fs::remove_file(&path) {
                Ok(()) => eprintln!("Removed native host: {}", path.display()),
                Err(e) => return Err(format!("remove {}: {e}", path.display())),
            }
        }
    }

    // ── Firefox ──
    if let Some(firefox_dir) = firefox_native_host_dir() {
        let path = firefox_dir.join(&manifest_name);
        if path.exists() {
            match std::fs::remove_file(&path) {
                Ok(()) => eprintln!("Removed native host: {}", path.display()),
                Err(e) => return Err(format!("remove {}: {e}", path.display())),
            }
        }
    }

    // ── Windows registry ──
    #[cfg(target_os = "windows")]
    {
        unregister_windows()?;
        let path = windows_manifest_dir().join(&manifest_name);
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
    }

    Ok(())
}

/// Locate the `zing` binary that will act as the native messaging host.
/// Prefers the running executable; falls back to `zing` on PATH.
fn host_executable() -> Result<PathBuf, String> {
    if let Ok(exe) = std::env::current_exe() {
        if exe.exists() {
            return Ok(exe);
        }
    }
    Err("cannot locate the zing binary (expected a compiled executable)".to_string())
}

fn write_manifest(path: &Path, manifest: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, manifest).map_err(|e| format!("write {}: {e}", path.display()))
}

fn chromium_manifest(host: &Path) -> String {
    let name = host_name();
    format!(
        r#"{{
  "name": "{name}",
  "description": "zing download manager",
  "path": "{}",
  "type": "stdio",
  "allowed_origins": [
    "chrome-extension://{CHROMIUM_ID}/"
  ]
}}
"#,
        host.display()
    )
}

fn firefox_manifest(host: &Path) -> String {
    let name = host_name();
    format!(
        r#"{{
  "name": "{name}",
  "description": "zing download manager",
  "path": "{}",
  "type": "stdio",
  "allowed_extensions": [
    "{GECKO_ID}"
  ]
}}
"#,
        host.display()
    )
}

/// The native host name shared by the extension and the manifests.
pub const fn host_name() -> &'static str {
    "oss.zing.intercept"
}

/// Recursively find all NativeMessagingHosts directories for Chromium-based
/// browsers across native packages, Flatpak, and Snap.
#[cfg(not(target_os = "windows"))]
fn find_chromium_host_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return dirs,
    };

    // 1. Native packages: ~/.config/*/NativeMessagingHosts/
    let config = dirs::config_dir().unwrap_or_else(|| home.clone());
    if let Ok(entries) = std::fs::read_dir(&config) {
        for entry in entries.flatten() {
            let nm = entry.path().join("NativeMessagingHosts");
            if nm.is_dir() {
                dirs.push(nm);
            }
        }
    }

    // 2. Flatpak: ~/.var/app/*/config/*/NativeMessagingHosts/
    let var_app = home.join(".var").join("app");
    if let Ok(apps) = std::fs::read_dir(&var_app) {
        for app in apps.flatten() {
            let app_config = app.path().join("config");
            if let Ok(configs) = std::fs::read_dir(&app_config) {
                for cfg in configs.flatten() {
                    let nm = cfg.path().join("NativeMessagingHosts");
                    if nm.is_dir() {
                        dirs.push(nm);
                    }
                }
            }
        }
    }

    // 3. Snap: ~/snap/*/config/*/NativeMessagingHosts/
    let snap = home.join("snap");
    if let Ok(snaps) = std::fs::read_dir(&snap) {
        for s in snaps.flatten() {
            let snap_config = s.path().join("config");
            if let Ok(configs) = std::fs::read_dir(&snap_config) {
                for cfg in configs.flatten() {
                    let nm = cfg.path().join("NativeMessagingHosts");
                    if nm.is_dir() {
                        dirs.push(nm);
                    }
                }
            }
        }
    }

    dirs
}

#[cfg(target_os = "windows")]
fn find_chromium_host_dirs() -> Vec<PathBuf> {
    // On Windows, all Chromium browsers share one manifest dir.
    let dir = windows_manifest_dir();
    if dir.is_dir() {
        vec![dir]
    } else {
        vec![]
    }
}

/// Firefox native-messaging hosts live under ~/.mozilla/native-messaging-hosts/.
#[cfg(not(target_os = "windows"))]
fn firefox_native_host_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|d| d.join(".mozilla").join("native-messaging-hosts"))
}

/// On Windows, Firefox uses %APPDATA%\Mozilla\NativeMessagingHosts\.
#[cfg(target_os = "windows")]
fn firefox_native_host_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("Mozilla").join("NativeMessagingHosts"))
}

/// Local directory holding our native-host manifests on Windows. The registry
/// entries point browsers at these files.
#[cfg(target_os = "windows")]
fn windows_manifest_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::env::temp_dir())
        .join("zing")
        .join("native-messaging-hosts")
}

#[cfg(target_os = "windows")]
fn register_windows(manifest_path: &Path) -> Result<(), String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let bases = [
        r"Software\Google\Chrome\NativeMessagingHosts",
        r"Software\Microsoft\Edge\NativeMessagingHosts",
        r"Software\BraveSoftware\Brave-Browser\NativeMessagingHosts",
        r"Software\Mozilla\NativeMessagingHosts",
    ];
    for base in &bases {
        let (key, _) = hkcu
            .create_subkey(format!("{base}\\{}", host_name()))
            .map_err(|e| format!("create registry key: {e}"))?;
        key.set_value("", &manifest_path.to_string_lossy().to_string())
            .map_err(|e| format!("set registry default: {e}"))?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn unregister_windows() -> Result<(), String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let bases = [
        r"Software\Google\Chrome\NativeMessagingHosts",
        r"Software\Microsoft\Edge\NativeMessagingHosts",
        r"Software\BraveSoftware\Brave-Browser\NativeMessagingHosts",
        r"Software\Mozilla\NativeMessagingHosts",
    ];
    for base in &bases {
        let key = format!("{base}\\{}", host_name());
        match hkcu.delete_subkey_all(&key) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("delete registry key: {e}")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chromium_id_has_valid_shape() {
        assert_eq!(CHROMIUM_ID.len(), 32);
        assert!(
            CHROMIUM_ID.chars().all(|c| ('a'..='p').contains(&c)),
            "Chromium IDs use the a-p alphabet, got {CHROMIUM_ID}"
        );
    }

    #[test]
    fn chromium_manifest_lists_real_origin() {
        let manifest = chromium_manifest(Path::new("/usr/bin/zing"));
        assert!(
            manifest.contains(&format!("chrome-extension://{CHROMIUM_ID}/")),
            "chromium manifest must list the real origin"
        );
        assert!(!manifest.contains("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"));
    }

    #[test]
    fn firefox_manifest_uses_plain_gecko_id() {
        let manifest = firefox_manifest(Path::new("/usr/bin/zing"));
        assert!(
            manifest.contains(&format!("\n    \"{GECKO_ID}\"\n  ]")),
            "firefox manifest must list the plain gecko id"
        );
        assert!(
            !manifest.contains("chrome-extension://"),
            "firefox must not use a chrome-extension origin"
        );
    }

    #[test]
    fn chromium_manifest_is_valid_json() {
        let manifest = chromium_manifest(Path::new("/usr/bin/zing"));
        let v: serde_json::Value = serde_json::from_str(&manifest).expect("valid json");
        assert_eq!(v["name"], "oss.zing.intercept");
        assert_eq!(v["type"], "stdio");
        assert_eq!(
            v["allowed_origins"][0],
            format!("chrome-extension://{CHROMIUM_ID}/")
        );
    }

    #[test]
    fn firefox_manifest_is_valid_json() {
        let manifest = firefox_manifest(Path::new("/usr/bin/zing"));
        let v: serde_json::Value = serde_json::from_str(&manifest).expect("valid json");
        assert_eq!(v["allowed_extensions"][0], GECKO_ID);
        assert!(v.get("allowed_origins").is_none());
    }
}

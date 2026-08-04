use std::path::{Path, PathBuf};

/// Stable Chromium extension ID, derived from the public key baked into the
/// extension's manifest.json ("key" field). Keep in sync with the
/// zing-extension repo: if the key changes, this ID changes too.
const CHROMIUM_ID: &str = "bcpghfjbokiclpfonepejdcndaoomcpf";

/// The gecko extension ID declared in the extension's manifest.json
/// (browser_specific_settings.gecko.id). Firefox native-host manifests list
/// it verbatim, without a scheme prefix.
const GECKO_ID: &str = "zing@tharukrenuja.github.io";

/// Browser native-host manifests for Chrome, Edge, and Firefox.
///
/// Each manifest tells the browser where the `zing` binary lives and which
/// extension origins may spawn it. We install the manifest into the
/// per-user native-messaging directory; the browser validates it.
pub fn install() -> Result<(), String> {
    let host = host_executable()?;
    for browser in Browser::all() {
        let path = browser.manifest_path()?;
        let manifest = browser.manifest(&host);
        write_manifest(&path, &manifest)?;
        browser.register_native_host(&path)?;
        eprintln!(
            "Installed {} native host -> {}",
            browser.name(),
            path.display()
        );
    }
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    for browser in Browser::all() {
        let path = browser.manifest_path()?;
        browser.unregister_native_host(&path)?;
        match std::fs::remove_file(&path) {
            Ok(()) => eprintln!("Removed {} native host: {}", browser.name(), path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!(
                    "No {} native host to remove: {}",
                    browser.name(),
                    path.display()
                );
            }
            Err(e) => return Err(format!("remove {} manifest: {e}", browser.name())),
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
    // `zing nm` requires the `zing` binary itself; the browser invokes the
    // path stored in the manifest, so a bare name would not resolve.
    Err("cannot locate the zing binary (expected a compiled executable)".to_string())
}

fn write_manifest(path: &Path, manifest: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, manifest).map_err(|e| format!("write {}: {e}", path.display()))
}

enum Browser {
    Chrome,
    Edge,
    Firefox,
}

impl Browser {
    const fn all() -> [Browser; 3] {
        [Browser::Chrome, Browser::Edge, Browser::Firefox]
    }

    const fn name(&self) -> &'static str {
        match self {
            Browser::Chrome => "Chrome",
            Browser::Edge => "Edge",
            Browser::Firefox => "Firefox",
        }
    }

    /// Path where the native-host manifest for this browser lives.
    ///
    /// - Unix: the browser's per-user NativeMessagingHosts directory.
    /// - Windows: our own manifests dir; the browser is pointed at it via the
    ///   registry instead of scanning a directory.
    fn manifest_path(&self) -> Result<PathBuf, String> {
        #[cfg(not(target_os = "windows"))]
        {
            let dir = match self {
                Browser::Chrome => native_host_dir("google-chrome"),
                Browser::Edge => native_host_dir("microsoft-edge"),
                Browser::Firefox => firefox_native_host_dir(),
            };
            dir?.map(|d| d.join(format!("{}.json", host_name())))
                .ok_or_else(|| "cannot determine home/config directory".to_string())
        }
        #[cfg(target_os = "windows")]
        {
            let dir = windows_manifest_dir();
            std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
            Ok(dir.join(format!("{}.json", host_name())))
        }
    }

    /// On Windows the manifest is registered in HKCU so the browser can find
    /// it; on Unix dropping the file in the right directory is enough.
    fn register_native_host(&self, path: &Path) -> Result<(), String> {
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (self, path);
            Ok(())
        }
        #[cfg(target_os = "windows")]
        {
            use winreg::enums::HKEY_CURRENT_USER;
            use winreg::RegKey;

            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let (key, _) = hkcu
                .create_subkey(&self.registry_path())
                .map_err(|e| format!("create registry key: {e}"))?;
            key.set_value("", &path.to_string_lossy().to_string())
                .map_err(|e| format!("set registry default: {e}"))
        }
    }

    fn unregister_native_host(&self, _path: &Path) -> Result<(), String> {
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (self, _path);
            Ok(())
        }
        #[cfg(target_os = "windows")]
        {
            use winreg::enums::HKEY_CURRENT_USER;
            use winreg::RegKey;

            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            match hkcu.delete_subkey_all(&self.registry_path()) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("delete registry key: {e}")),
            }
        }
    }

    #[cfg(target_os = "windows")]
    fn registry_path(&self) -> String {
        let base = match self {
            Browser::Chrome => r"Software\Google\Chrome\NativeMessagingHosts",
            Browser::Edge => r"Software\Microsoft\Edge\NativeMessagingHosts",
            Browser::Firefox => r"Software\Mozilla\NativeMessagingHosts",
        };
        format!(r"{base}\{}", host_name())
    }

    fn manifest(&self, host: &Path) -> String {
        let name = host_name();
        match self {
            Browser::Chrome | Browser::Edge => format!(
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
            ),
            Browser::Firefox => format!(
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
            ),
        }
    }
}

/// The native host name shared by the extension and the manifests.
pub const fn host_name() -> &'static str {
    "com.zing.native_host"
}

/// Directory containing native-messaging host manifests for Chromium-based
/// browsers (Chrome / Edge). Falls back from the config dir to the home dir.
#[cfg(not(target_os = "windows"))]
fn native_host_dir(browser: &str) -> Result<Option<PathBuf>, String> {
    let base = dirs::config_dir().or_else(dirs::home_dir);
    Ok(base.map(|d| d.join(browser).join("NativeMessagingHosts")))
}

/// Firefox native-messaging hosts live under the home dir.
#[cfg(not(target_os = "windows"))]
fn firefox_native_host_dir() -> Result<Option<PathBuf>, String> {
    Ok(dirs::home_dir().map(|d| d.join(".mozilla").join("native-messaging-hosts")))
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
    fn chrome_manifest_lists_real_origin() {
        let manifest = Browser::Chrome.manifest(Path::new("/usr/bin/zing"));
        assert!(
            manifest.contains(&format!("chrome-extension://{CHROMIUM_ID}/")),
            "chrome manifest must list the real origin"
        );
        assert!(!manifest.contains("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"));
    }

    #[test]
    fn firefox_manifest_uses_plain_gecko_id() {
        let manifest = Browser::Firefox.manifest(Path::new("/usr/bin/zing"));
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
    fn chrome_manifest_is_valid_json() {
        let manifest = Browser::Chrome.manifest(Path::new("/usr/bin/zing"));
        let v: serde_json::Value = serde_json::from_str(&manifest).expect("valid json");
        assert_eq!(v["name"], "com.zing.native_host");
        assert_eq!(v["type"], "stdio");
        assert_eq!(
            v["allowed_origins"][0],
            format!("chrome-extension://{CHROMIUM_ID}/")
        );
    }

    #[test]
    fn firefox_manifest_is_valid_json() {
        let manifest = Browser::Firefox.manifest(Path::new("/usr/bin/zing"));
        let v: serde_json::Value = serde_json::from_str(&manifest).expect("valid json");
        assert_eq!(v["allowed_extensions"][0], GECKO_ID);
        assert!(v.get("allowed_origins").is_none());
    }
}

use std::path::{Path, PathBuf};

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

    /// Extension ID that the zing browser extension will be published under.
    /// Update when the extension repo is created and packed.
    const fn allowed_origin(&self) -> &'static str {
        // TODO: replace with the real extension ID once the extension repo
        // exists. Chromium IDs are derived from the extension's public key.
        "chrome-extension://zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz/"
    }

    fn manifest_path(&self) -> Result<PathBuf, String> {
        let dir = match self {
            Browser::Chrome => {
                #[cfg(target_os = "linux")]
                {
                    dirs::config_dir().map(|d| d.join("google-chrome").join("NativeMessagingHosts"))
                }
                #[cfg(target_os = "windows")]
                {
                    // HKEY_CURRENT_USER\Software\Google\Chrome\NativeMessagingHosts
                    return Err("Windows manifests use the registry (unsupported yet)".to_string());
                }
                #[cfg(target_os = "macos")]
                {
                    dirs::home_dir().map(|d| {
                        d.join("Library")
                            .join("Application Support")
                            .join("Google")
                            .join("Chrome")
                            .join("NativeMessagingHosts")
                    })
                }
            }
            Browser::Edge => {
                #[cfg(target_os = "linux")]
                {
                    dirs::config_dir()
                        .map(|d| d.join("microsoft-edge").join("NativeMessagingHosts"))
                }
                #[cfg(target_os = "windows")]
                {
                    return Err("Windows manifests use the registry (unsupported yet)".to_string());
                }
                #[cfg(target_os = "macos")]
                {
                    dirs::home_dir().map(|d| {
                        d.join("Library")
                            .join("Application Support")
                            .join("Microsoft Edge")
                            .join("NativeMessagingHosts")
                    })
                }
            }
            Browser::Firefox => {
                #[cfg(target_os = "linux")]
                {
                    dirs::home_dir().map(|d| d.join(".mozilla").join("native-messaging-hosts"))
                }
                #[cfg(target_os = "windows")]
                {
                    // HKEY_CURRENT_USER\Software\Mozilla\NativeMessagingHosts
                    return Err("Windows manifests use the registry (unsupported yet)".to_string());
                }
                #[cfg(target_os = "macos")]
                {
                    dirs::home_dir().map(|d| {
                        d.join("Library")
                            .join("Application Support")
                            .join("Mozilla")
                            .join("NativeMessagingHosts")
                    })
                }
            }
        };
        dir.map(|d| d.join(format!("{}.json", host_name())))
            .ok_or_else(|| "cannot determine home/config directory".to_string())
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
    "{}"
  ]
}}
"#,
                host.display(),
                self.allowed_origin()
            ),
            Browser::Firefox => format!(
                r#"{{
  "name": "{name}",
  "description": "zing download manager",
  "path": "{}",
  "type": "stdio",
  "allowed_extensions": [
    "{}"
  ]
}}
"#,
                host.display(),
                self.allowed_origin()
            ),
        }
    }
}

/// The native host name shared by the extension and the manifests.
pub const fn host_name() -> &'static str {
    "com.zing.native_host"
}

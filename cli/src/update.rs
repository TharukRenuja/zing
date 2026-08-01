use crate::config::Config;
use color_eyre::eyre::bail;
use color_eyre::Result;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CACHE_FILE: &str = "update-check.json";
const REPO: &str = "TharukRenuja/zing";

#[derive(Serialize, Deserialize)]
struct UpdateCache {
    latest_version: String,
    checked_at: u64,
}

#[allow(dead_code)]
pub async fn check_for_update(cfg: &Config) -> Option<String> {
    let interval_days = match cfg.update_check_interval_days {
        None => return None, // disabled
        Some(0) => 0,        // always check (skip cache)
        Some(n) => n,        // check every n days
    };

    let cache_path = cache_path();
    let cache = load_cache(&cache_path);

    // Skip cache when interval is 0 (always)
    if interval_days > 0 {
        if let Some(ref c) = cache {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let elapsed_days = (now - c.checked_at) / 86400;
            if elapsed_days < interval_days {
                let current = current_version();
                if version_cmp(&c.latest_version, &current) == Ordering::Greater {
                    return Some(c.latest_version.clone());
                }
                return None;
            }
        }
    }

    let latest = match fetch_latest_tag().await {
        Some(tag) => tag,
        None => {
            return cache.and_then(|c| {
                if version_cmp(&c.latest_version, &current_version()) == Ordering::Greater {
                    Some(c.latest_version)
                } else {
                    None
                }
            })
        }
    };

    save_cache(&cache_path, &latest);
    let current = current_version();
    if version_cmp(&latest, &current) == Ordering::Greater {
        Some(latest)
    } else {
        None
    }
}

pub async fn run_update() -> Result<()> {
    let tag = fetch_latest_tag()
        .await
        .ok_or_else(|| color_eyre::eyre::eyre!("Failed to fetch latest release"))?;
    let current = current_version();

    if version_cmp(&tag, &current) != Ordering::Greater {
        println!("Already up to date (v{current}).");
        return Ok(());
    }

    println!("Updating zing from v{current} to {tag}...");

    let exe_path = std::env::current_exe()?;
    let exe_dir = exe_path
        .parent()
        .ok_or_else(|| color_eyre::eyre::eyre!("Cannot determine executable directory"))?;

    // Check write permission before downloading
    let probe = exe_dir.join(".zing-update-probe");
    if std::fs::write(&probe, b"").is_err() {
        let hint = if cfg!(windows) {
            "Reopen terminal as Administrator"
        } else {
            "Run with sudo: sudo zing update"
        };
        bail!("No write permission to {}. {}", exe_dir.display(), hint);
    }
    let _ = std::fs::remove_file(&probe);

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let (suffix, ext, needs_update) = match (os, arch) {
        ("linux", "x86_64") => ("x86_64-linux", "tar.gz", false),
        ("linux", "aarch64") => ("aarch64-linux", "tar.gz", false),
        ("macos", "x86_64") => ("x86_64-mac", "tar.gz", true),
        ("macos", "aarch64") => ("aarch64-mac", "tar.gz", true),
        ("windows", "x86_64") | ("windows", "aarch64") => ("windows", "zip", true),
        _ => {
            println!("Update not supported on {os}-{arch}.");
            println!("Re-run install.sh: curl -fsSL https://raw.githubusercontent.com/{REPO}/main/install.sh | sh");
            return Ok(());
        }
    };

    let archive_suffix = if needs_update {
        format!("{suffix}-update")
    } else {
        suffix.to_string()
    };
    let url = format!(
        "https://github.com/{REPO}/releases/download/{tag}/zing-{tag}-{archive_suffix}.{ext}"
    );
    let tmp = std::env::temp_dir().join(format!("zing-update-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;

    println!("  Downloading {tag} for {os}-{arch}...");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let response = client.get(&url).send().await?;
    if !response.status().is_success() {
        bail!("Download failed: {}", response.status());
    }
    let body = response.bytes().await?;
    let archive_path = tmp.join(format!("zing.{ext}"));
    std::fs::write(&archive_path, &body)?;

    let new_binary = match ext {
        "tar.gz" => {
            println!("  Extracting...");
            let status = std::process::Command::new("tar")
                .arg("xzf")
                .arg(&archive_path)
                .current_dir(&tmp)
                .status()
                .map_err(|e| color_eyre::eyre::eyre!("tar not found: {e}"))?;
            if !status.success() {
                bail!("tar extraction failed");
            }
            // Find the extracted binary
            let expected = format!("zing-{tag}-{suffix}");
            let bin_path = tmp.join(&expected);
            if bin_path.exists() {
                bin_path
            } else {
                // Fallback: look for any file starting with zing- (not daemon)
                let mut found = None;
                for entry in std::fs::read_dir(&tmp)? {
                    let entry = entry?;
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("zing-") && !name.starts_with("zing-daemon-") {
                        found = Some(entry.path());
                        break;
                    }
                }
                found.ok_or_else(|| color_eyre::eyre::eyre!("Binary not found in archive"))?
            }
        }
        "zip" => {
            println!("  Extracting...");
            let file = std::fs::File::open(&archive_path)?;
            let mut archive = zip::ZipArchive::new(file)
                .map_err(|e| color_eyre::eyre::eyre!("Failed to read ZIP: {e}"))?;
            for i in 0..archive.len() {
                let mut entry = archive
                    .by_index(i)
                    .map_err(|e| color_eyre::eyre::eyre!("ZIP entry {i}: {e}"))?;
                let name = entry.name().replace('\\', "/");
                // Strip top-level directory if present (e.g. zing-0.1.2-x86_64-windows/zing.exe)
                let basename = name.rsplit('/').next().unwrap_or(&name).to_string();
                let out_path = tmp.join(&basename);
                if !entry.is_dir() {
                    if let Some(parent) = out_path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let mut out = std::fs::File::create(&out_path)?;
                    std::io::copy(&mut entry, &mut out)?;
                }
            }
            // Find the extracted zing binary
            let zing_bin = if cfg!(windows) { "zing.exe" } else { "zing" };
            let mut found = None;
            for entry in std::fs::read_dir(&tmp)? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name == zing_bin {
                    found = Some(entry.path());
                    break;
                }
            }
            found.ok_or_else(|| color_eyre::eyre::eyre!("{zing_bin} not found in ZIP"))?
        }
        _ => bail!("Unsupported archive format: {ext}"),
    };

    println!("  Installing...");
    swap_binary(&new_binary, &exe_path)?;

    // Also update zing-daemon if present
    let daemon_bin = if cfg!(windows) {
        "zing-daemon.exe"
    } else {
        "zing-daemon"
    };
    let daemon_path = exe_dir.join(daemon_bin);
    if daemon_path.exists() {
        let daemon_extracted = match ext {
            "tar.gz" => {
                let expected = format!("zing-daemon-{tag}-{suffix}");
                let p = tmp.join(&expected);
                if p.exists() {
                    Some(p)
                } else {
                    let mut found = None;
                    for entry in std::fs::read_dir(&tmp)? {
                        let entry = entry?;
                        let name = entry.file_name();
                        if name.to_string_lossy().starts_with("zing-daemon-") {
                            found = Some(entry.path());
                            break;
                        }
                    }
                    found
                }
            }
            "zip" => {
                // Already extracted from ZIP alongside zing.exe
                let p = tmp.join(daemon_bin);
                if p.exists() {
                    Some(p)
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(d) = daemon_extracted {
            swap_binary(&d, &daemon_path)?;
            println!("  zing-daemon updated");
        } else if ext == "tar.gz" || ext == "zip" {
            println!("  zing-daemon not found in archive, skipping");
        }
    }

    // Reconcile the Windows service registration (create/config/restart) so an
    // update behaves like a fresh MSI install — binary ACLs, service account,
    // and startup state stay correct without the user reinstalling.
    #[cfg(windows)]
    {
        if daemon_path.exists() {
            ensure_daemon_service(&daemon_path).await?;
        } else {
            println!("  zing-daemon binary missing, skipping service setup");
        }
    }

    // On Linux/macOS a running daemon keeps executing the old binary in memory,
    // so the fresh CLI would proxy to a stale daemon and hang. Restart it so
    // the new binary is picked up (mirrors the Windows service restart above).
    #[cfg(not(windows))]
    {
        if is_running_as_root() {
            println!("  Running as root, skipping daemon restart");
            println!("  Run `zing daemon restart` as your user to pick up the new daemon");
        } else if crate::daemon_client::daemon_is_running().await {
            println!("  Restarting zing-daemon...");
            let _ = crate::run_daemon_stop().await;
            // Wait for the old daemon to release the socket before starting anew.
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while crate::daemon_client::daemon_is_running().await
                && std::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            crate::run_daemon_start().await?;
            println!("  zing-daemon restarted");
        }
    }

    // Housekeeping: remove leftover backup files from previous swaps.
    let _ = std::fs::remove_file(exe_dir.join("zing.old"));
    let _ = std::fs::remove_file(exe_dir.join("zing-daemon.old"));

    let _ = std::fs::remove_dir_all(&tmp);
    println!("Done. v{current} -> {tag}");

    let cache_path = cache_path();
    save_cache(&cache_path, &tag);

    Ok(())
}

/// Reconcile the Windows `zing-daemon` service after a binary swap so the
/// update behaves like a fresh MSI install:
/// - create the service if it does not exist,
/// - reset the binary path, service account (LocalSystem) and auto-start,
/// - restart it so the new binary is live.
#[cfg(windows)]
async fn ensure_daemon_service(daemon_path: &std::path::Path) -> Result<()> {
    let bin_path = format!("\"{}\"", daemon_path.display());

    let exists = run_sc(&["query", "zing-daemon"]).await.is_ok();

    if exists {
        println!("  Reconfiguring zing-daemon service...");
        let res = run_sc(&[
            "config",
            "zing-daemon",
            "binPath=",
            &bin_path,
            "obj=",
            "NT AUTHORITY\\LocalSystem",
            "start=",
            "auto",
            "depend=",
            "Tcpip",
        ])
        .await;
        if let Err(e) = res {
            bail!("Failed to reconfigure zing-daemon service: {e}");
        }
    } else {
        println!("  Creating zing-daemon service...");
        let res = run_sc(&[
            "create",
            "zing-daemon",
            "type=",
            "own",
            "start=",
            "auto",
            "error=",
            "normal",
            "binPath=",
            &bin_path,
            "obj=",
            "NT AUTHORITY\\LocalSystem",
            "depend=",
            "Tcpip",
        ])
        .await;
        if let Err(e) = res {
            bail!("Failed to create zing-daemon service: {e}");
        }
    }

    // Restart the service so the new binary is picked up. Stopping a service
    // that is not running is fine.
    let _ = run_sc(&["stop", "zing-daemon"]).await;
    let start = run_sc(&["start", "zing-daemon"]).await;
    match start {
        Ok(_) => println!("  zing-daemon service restarted"),
        Err(e) => bail!("Failed to start zing-daemon service: {e}"),
    }
    Ok(())
}

/// Run `sc <args>` and return whether it succeeded. Prints stdout/stderr on
/// failure so `sc`'s real error (e.g. "Access is denied") is visible.
#[cfg(windows)]
async fn run_sc(args: &[&str]) -> std::result::Result<(), String> {
    let output = tokio::process::Command::new("sc")
        .args(args)
        .output()
        .await
        .map_err(|e| format!("sc {args:?}: {e}"))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.trim().is_empty() {
            println!("{}", stdout.trim());
        }
        Ok(())
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "sc {}: {} {}",
            args.join(" "),
            stdout.trim(),
            stderr.trim()
        ))
    }
}

fn swap_binary(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    let tmp_dst = dst.with_extension("tmp");
    if let Err(e) = std::fs::rename(src, &tmp_dst) {
        if e.raw_os_error() == Some(18) {
            std::fs::copy(src, &tmp_dst)?;
            let _ = std::fs::remove_file(src);
        } else {
            return Err(e.into());
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_dst, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(windows)]
    {
        let old = dst.with_extension("old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(dst, &old)?;
        // Use a copy (not rename) so the new binary inherits the destination
        // directory ACL. Rename preserves the source (%TEMP%) ACL, which omits
        // the service account and makes `sc start zing-daemon` fail with
        // "Access is denied (5)" after an update.
        std::fs::copy(&tmp_dst, dst)?;
        let _ = std::fs::remove_file(&tmp_dst);
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(&tmp_dst, dst)?;
    }
    Ok(())
}

fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[cfg(not(windows))]
fn is_running_as_root() -> bool {
    // SAFETY: geteuid takes no arguments and returns the effective UID.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(windows)]
fn is_running_as_root() -> bool {
    false
}

fn version_cmp(a: &str, b: &str) -> Ordering {
    let a_parts: Vec<u64> = a
        .trim_start_matches('v')
        .split('.')
        .filter_map(|s| s.parse().ok())
        .collect();
    let b_parts: Vec<u64> = b
        .trim_start_matches('v')
        .split('.')
        .filter_map(|s| s.parse().ok())
        .collect();
    let max_len = a_parts.len().max(b_parts.len());
    for i in 0..max_len {
        let av = a_parts.get(i).copied().unwrap_or(0);
        let bv = b_parts.get(i).copied().unwrap_or(0);
        if av != bv {
            return if av > bv {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
    }
    Ordering::Equal
}

async fn fetch_latest_tag() -> Option<String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let client = reqwest::Client::builder()
        .user_agent("zing-update")
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    json.get("tag_name")?.as_str().map(|s| s.to_string())
}

fn cache_path() -> PathBuf {
    let config_dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    config_dir.join("zing").join(CACHE_FILE)
}

#[allow(dead_code)]
fn load_cache(path: &PathBuf) -> Option<UpdateCache> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

#[allow(dead_code)]
fn save_cache(path: &PathBuf, version: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let cache = UpdateCache {
        latest_version: version.to_string(),
        checked_at: now,
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(content) = serde_json::to_string(&cache) {
        let _ = std::fs::write(path, content);
    }
}

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

pub async fn check_for_update(cfg: &Config) -> Option<String> {
    let interval_days = cfg.update_check_interval_days.unwrap_or(7);
    if interval_days == 0 {
        return None;
    }

    let cache_path = cache_path();
    let cache = load_cache(&cache_path);
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

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let (suffix, ext) = match (os, arch) {
        ("linux", "x86_64") => ("x86_64-linux", "tar.gz"),
        ("linux", "aarch64") => ("aarch64-linux", "tar.gz"),
        ("macos", "aarch64") => ("aarch64-mac", "dmg"),
        ("windows", "x86_64") => ("x86_64-windows", "exe"),
        ("windows", "aarch64") => ("aarch64-windows", "exe"),
        _ => {
            println!("Update not supported on {os}-{arch}.");
            println!("Re-run install.sh: curl -fsSL https://raw.githubusercontent.com/{REPO}/main/install.sh | sh");
            return Ok(());
        }
    };

    let url = format!("https://github.com/{REPO}/releases/download/{tag}/zing-{tag}-{suffix}.{ext}");
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
        "exe" => archive_path,
        _ => bail!("Unsupported archive format: {ext}"),
    };

    println!("  Installing...");
    swap_binary(&new_binary, &exe_path)?;

    // Also update zing-daemon if present
    let daemon_path = exe_dir.join("zing-daemon");
    if daemon_path.exists() {
        let daemon_extracted = match ext {
            "tar.gz" => {
                let expected = format!("zing-daemon-{tag}-{suffix}");
                let p = tmp.join(&expected);
                if p.exists() {
                    Some(p)
                } else {
                    // Look for any zing-daemon-* file
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
            _ => None,
        };
        if let Some(d) = daemon_extracted {
            swap_binary(&d, &daemon_path)?;
            println!("  zing-daemon updated");
        } else if ext == "tar.gz" {
            println!("  zing-daemon not found in archive, skipping");
        }
    }

    let _ = std::fs::remove_dir_all(&tmp);
    println!("Done. v{current} -> {tag}");

    let cache_path = cache_path();
    save_cache(&cache_path, &tag);

    Ok(())
}

fn swap_binary(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    let tmp_dst = dst.with_extension("tmp");
    std::fs::rename(src, &tmp_dst)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_dst, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp_dst, dst)?;
    Ok(())
}

fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
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

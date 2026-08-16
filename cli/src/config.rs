use std::path::PathBuf;

#[derive(Debug, serde::Deserialize, serde::Serialize, Default)]
pub struct Config {
    #[serde(default)]
    pub download_dir: Option<PathBuf>,
    #[serde(default)]
    pub prompt_location: bool,
    #[serde(default = "default_update_interval")]
    pub update_check_interval_days: Option<u64>,
    #[serde(default)]
    pub end_game: Option<bool>,
    #[serde(default)]
    pub throttle_reprobe: Option<bool>,
    #[serde(default)]
    pub max_concurrent_downloads: Option<usize>,
    #[serde(default)]
    pub default_connections: Option<u16>,
    #[serde(default)]
    pub connect_timeout: Option<u64>,
    #[serde(default)]
    pub max_transfer_time: Option<u64>,
    #[serde(default)]
    pub retry_count: Option<u32>,
    #[serde(default)]
    pub retry_wait_ms: Option<u64>,
    #[serde(default)]
    pub default_proxy: Option<String>,
    #[serde(default)]
    pub default_rate_limit: Option<String>,
    #[serde(default)]
    pub bwlimit_schedule: Option<String>,
    #[serde(default)]
    pub auto_rename: Option<bool>,
    #[serde(default)]
    pub allow_overwrite: Option<bool>,
    #[serde(default)]
    pub content_disposition: Option<bool>,
    #[serde(default)]
    pub active_hours_from: Option<String>,
    #[serde(default)]
    pub active_hours_to: Option<String>,
    #[serde(default)]
    pub clipboard_monitor: bool,
    #[serde(default = "default_true")]
    pub download_categories: bool,
    #[serde(default)]
    pub post_download_action: Option<String>,
}

fn default_update_interval() -> Option<u64> {
    Some(7)
}

fn default_true() -> bool {
    true
}

impl Config {
    pub fn load(path: Option<&std::path::Path>) -> Self {
        let config_path = path
            .map(|p| p.to_path_buf())
            .or_else(default_config_path)
            .unwrap_or_else(|| PathBuf::from("."));

        if !config_path.exists() {
            return Config::default();
        }

        match std::fs::read_to_string(&config_path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                tracing::warn!("Failed to parse config at {}: {e}", config_path.display());
                Config::default()
            }),
            Err(e) => {
                tracing::warn!("Failed to read config at {}: {e}", config_path.display());
                Config::default()
            }
        }
    }

    pub fn download_dir(&self) -> PathBuf {
        let raw = self
            .download_dir
            .clone()
            .or_else(dirs::download_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let raw_str = raw.to_string_lossy().to_string();
        let expanded = match shellexpand::full(&raw_str) {
            Ok(s) => s.to_string(),
            Err(_) => raw_str,
        };
        PathBuf::from(expanded)
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let path = default_config_path().ok_or("cannot determine config directory")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }
}

fn default_config_path() -> Option<PathBuf> {
    let config_dir = dirs::config_dir()?;
    Some(config_dir.join("zing").join("config.json"))
}

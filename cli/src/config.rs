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
}

fn default_update_interval() -> Option<u64> {
    Some(7)
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

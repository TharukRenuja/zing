use std::path::PathBuf;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct Config {
    #[serde(default)]
    pub download_dir: Option<PathBuf>,
    #[serde(default)]
    pub prompt_location: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            download_dir: None,
            prompt_location: false,
        }
    }
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
            Ok(content) => {
                serde_json::from_str(&content).unwrap_or_else(|e| {
                    tracing::warn!("Failed to parse config at {}: {e}", config_path.display());
                    Config::default()
                })
            }
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
}

fn default_config_path() -> Option<PathBuf> {
    let config_dir = dirs::config_dir()?;
    Some(config_dir.join("rxdl").join("config.json"))
}

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentEntry {
    pub id: usize,
    pub offset: u64,
    pub length: u64,
    pub downloaded: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlFile {
    pub version: u32,
    pub url: String,
    pub total_size: Option<u64>,
    pub filename: String,
    pub segments: Vec<SegmentEntry>,
    pub metadata: HashMap<String, String>,
    #[serde(default)]
    pub base_downloaded: u64,
}

impl ControlFile {
    pub fn new(url: &str, filename: &str, total_size: Option<u64>) -> Self {
        Self {
            version: 1,
            url: url.to_string(),
            total_size,
            filename: filename.to_string(),
            segments: Vec::new(),
            metadata: HashMap::new(),
            base_downloaded: 0,
        }
    }

    pub fn control_path(output_path: &Path) -> PathBuf {
        let mut p = output_path.to_path_buf();
        let mut name = p
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "download".to_string());
        name.push_str(".zing");
        p.set_file_name(name);
        p
    }

    #[must_use]
    pub async fn save(&self, path: &Path) -> std::io::Result<()> {
        let json =
            serde_json::to_string(self).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // Atomic write: write to temp file then rename
        let tmp_path = path.with_extension("zing.tmp");
        tokio::fs::write(&tmp_path, &json).await?;
        tokio::fs::rename(&tmp_path, path).await
    }

    pub async fn load(path: &Path) -> std::io::Result<Self> {
        let json = tokio::fs::read_to_string(path).await?;
        let cf: ControlFile =
            serde_json::from_str(&json).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(cf)
    }

    pub fn total_downloaded(&self) -> u64 {
        self.base_downloaded + self.segments.iter().map(|s| s.downloaded).sum::<u64>()
    }

    pub fn is_complete(&self) -> bool {
        if let Some(total) = self.total_size {
            self.total_downloaded() >= total
        } else if self.segments.is_empty() {
            false
        } else {
            self.segments.iter().all(|s| s.downloaded >= s.length)
        }
    }

    pub fn progress_pct(&self) -> f64 {
        match self.total_size {
            Some(total) if total > 0 => {
                self.total_downloaded() as f64 / total as f64 * 100.0
            }
            _ => {
                let total_len: u64 = self.segments.iter().map(|s| s.length).sum();
                if total_len > 0 {
                    self.total_downloaded() as f64 / total_len as f64 * 100.0
                } else {
                    0.0
                }
            }
        }
    }
}

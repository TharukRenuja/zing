use crate::task_manager::TaskManager;
use chrono::{Datelike, Timelike};
use rxext::filename;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Deserialize)]
pub struct ScheduleEntry {
    pub url: String,
    pub at: String,
    pub end: Option<String>,
    #[serde(default = "default_days")]
    pub days: Vec<String>,
    pub output: Option<String>,
    pub output_dir: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_connections")]
    pub connections: usize,
    #[serde(default)]
    pub insecure: bool,
    #[serde(default)]
    pub max_download_rate: u64,
    pub proxy: Option<String>,
}

fn default_days() -> Vec<String> {
    vec![
        "Mon".to_string(), "Tue".to_string(), "Wed".to_string(),
        "Thu".to_string(), "Fri".to_string(), "Sat".to_string(), "Sun".to_string(),
    ]
}

fn default_enabled() -> bool { true }
fn default_connections() -> usize { 4 }

pub struct Scheduler {
    config_path: PathBuf,
    manager: TaskManager,
}

impl Scheduler {
    pub fn new(manager: TaskManager) -> Self {
        let config_path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("rxdl")
            .join("schedule.json");

        Self { config_path, manager }
    }

    #[allow(dead_code)]
    pub fn with_config_path(mut self, path: PathBuf) -> Self {
        self.config_path = path;
        self
    }

    #[allow(dead_code)]
    pub async fn load_entries(&self) -> HashMap<String, ScheduleEntry> {
        let content = match tokio::fs::read_to_string(&self.config_path).await {
            Ok(c) => c,
            Err(_) => return HashMap::new(),
        };

        match serde_json::from_str::<HashMap<String, ScheduleEntry>>(&content) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!("Failed to parse schedule config: {e}");
                HashMap::new()
            }
        }
    }

    pub fn spawn(self) {
        let entries = Arc::new(Mutex::new(HashMap::new()));
        let manager = self.manager;

        {
            let entries = Arc::clone(&entries);
            let path = self.config_path;
            tokio::spawn(async move {
                loop {
                    let content = match tokio::fs::read_to_string(&path).await {
                        Ok(c) => c,
                        Err(_) => {
                            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                            continue;
                        }
                    };

                    let parsed = serde_json::from_str::<HashMap<String, ScheduleEntry>>(&content)
                        .unwrap_or_default();
                    *entries.lock().await = parsed;
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                }
            });
        }

        tokio::spawn(async move {
            let mut triggered_today: HashMap<String, String> = HashMap::new();

            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;

                let now = chrono::Local::now();
                let today_date = format!("{:04}-{:02}-{:02}", now.year(), now.month(), now.day());
                let current_minutes = (now.hour() as u16) * 60 + now.minute() as u16;

                triggered_today.retain(|_, date| date == &today_date);
                let today = match now.weekday() {
                    chrono::Weekday::Mon => "Mon",
                    chrono::Weekday::Tue => "Tue",
                    chrono::Weekday::Wed => "Wed",
                    chrono::Weekday::Thu => "Thu",
                    chrono::Weekday::Fri => "Fri",
                    chrono::Weekday::Sat => "Sat",
                    chrono::Weekday::Sun => "Sun",
                };

                let locked = entries.lock().await;
                for (id, entry) in locked.iter() {
                    if !entry.enabled {
                        continue;
                    }
                    if !entry.days.iter().any(|d| d.eq_ignore_ascii_case(today)) {
                        continue;
                    }
                    if triggered_today.contains_key(id) {
                        continue;
                    }

                    // Parse at time into minutes since midnight
                    let at_parts: Vec<&str> = entry.at.split(':').collect();
                    if at_parts.len() != 2 { continue; }
                    let at_h: u16 = at_parts[0].parse().unwrap_or(99);
                    let at_m: u16 = at_parts[1].parse().unwrap_or(99);
                    if at_h > 23 || at_m > 59 { continue; }
                    let at_minutes = at_h * 60 + at_m;

                    let in_window = match entry.end {
                        Some(ref end_str) => {
                            let end_parts: Vec<&str> = end_str.split(':').collect();
                            if end_parts.len() != 2 { continue; }
                            let end_h: u16 = end_parts[0].parse().unwrap_or(99);
                            let end_m: u16 = end_parts[1].parse().unwrap_or(99);
                            if end_h > 23 || end_m > 59 { continue; }
                            let end_minutes = end_h * 60 + end_m;

                            if at_minutes < end_minutes {
                                // Normal window: at <= now < end
                                current_minutes >= at_minutes && current_minutes < end_minutes
                            } else {
                                // Overnight window: at <= now < 24:00 OR 00:00 <= now < end
                                current_minutes >= at_minutes || current_minutes < end_minutes
                            }
                        }
                        None => {
                            // Point-in-time: exact minute match
                            current_minutes == at_minutes
                        }
                    };

                    if !in_window {
                        continue;
                    }

                    triggered_today.insert(id.clone(), today_date.clone());
                    tracing::info!("Scheduled task triggered: {id}");

                    let output_path = entry.output.clone().unwrap_or_else(|| {
                        filename::from_url(&entry.url)
                    });

                    let download_dir = entry.output_dir.clone()
                        .map(PathBuf::from)
                        .or_else(dirs::download_dir)
                        .unwrap_or_else(|| PathBuf::from("."));

                    let full_path = if entry.output.is_some() {
                        PathBuf::from(&output_path)
                    } else {
                        download_dir.join(&output_path)
                    };

                    if let Some(parent) = full_path.parent() {
                        let _ = tokio::fs::create_dir_all(parent).await;
                    }

                    manager.add_task(
                        &entry.url,
                        &full_path.to_string_lossy(),
                        entry.output.is_none(),
                        entry.connections,
                        entry.insecure,
                        entry.max_download_rate,
                        entry.proxy.clone(),
                        Vec::new(),
                        None,
                        Vec::new(),
                        0,
                    ).await;
                }
            }
        });
    }
}

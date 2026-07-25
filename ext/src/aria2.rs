use std::path::Path;

#[derive(Debug)]
pub struct Aria2Task {
    pub gid: String,
    pub url: String,
    pub filename: Option<String>,
    pub dir: Option<String>,
}

/// Parse an aria2 session file and extract download URIs.
/// Format: one line per task, tab-separated fields.
/// gid\tstat\tpath\turi
pub fn parse_session(path: &Path) -> Result<Vec<Aria2Task>, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read session: {e}"))?;

    let mut tasks = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 4 {
            continue;
        }
        tasks.push(Aria2Task {
            gid: parts[0].to_string(),
            url: parts[3].to_string(),
            filename: None,
            dir: None,
        });
    }
    Ok(tasks)
}

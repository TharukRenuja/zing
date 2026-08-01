/// Format a byte count in a human-readable form (e.g. "16.0 MB").
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let mut b = bytes as f64;
    let mut unit = 0;
    while b >= 1024.0 && unit < UNITS.len() - 1 {
        b /= 1024.0;
        unit += 1;
    }
    format!("{:.1} {}", b, UNITS[unit])
}

/// Format a speed in bytes/second (e.g. "5.0 MB/s").
pub fn human_speed(bytes_per_sec: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    if bytes_per_sec == 0 {
        return "0 B/s".to_string();
    }
    let mut b = bytes_per_sec as f64;
    let mut unit = 0;
    while b >= 1024.0 && unit < UNITS.len() - 1 {
        b /= 1024.0;
        unit += 1;
    }
    format!("{:.1} {}/s", b, UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_bytes() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512.0 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(16 * 1024 * 1024), "16.0 MB");
        assert_eq!(human_bytes(1024 * 1024 * 1024), "1.0 GB");
    }

    #[test]
    fn test_human_speed() {
        assert_eq!(human_speed(0), "0 B/s");
        assert_eq!(human_speed(512), "512.0 B/s");
        assert_eq!(human_speed(5 * 1024 * 1024), "5.0 MB/s");
        assert_eq!(human_speed(2 * 1024 * 1024 * 1024), "2.0 GB/s");
    }
}

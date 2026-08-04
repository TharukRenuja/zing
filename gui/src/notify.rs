//! Desktop notification helpers using `notify-rust`.

use notify_rust::Notification;

const APP_NAME: &str = "zing";

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}

pub fn started(filename: &str, url: &str) {
    let _ = Notification::new()
        .appname(APP_NAME)
        .summary("Download started")
        .body(&format!(
            "{}\n{}",
            truncate(filename, 80),
            truncate(url, 100),
        ))
        .show();
}

pub fn completed(filename: &str, bytes: u64) {
    let size = crate::app::format_bytes(bytes);
    let _ = Notification::new()
        .appname(APP_NAME)
        .summary("Download complete")
        .body(&format!("{} ({})", truncate(filename, 70), size))
        .show();
}

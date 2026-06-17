use crate::ratelimit::TokenBucket;
use std::sync::Arc;
use std::time::Duration;

/// A time-of-day bandwidth schedule entry.
#[derive(Debug, Clone)]
struct BwEntry {
    hour: u8,
    minute: u8,
    rate_bytes: u64,
}

/// Parse a bandwidth schedule string like "08:00,500K 12:00,5M 18:00,2M".
fn parse_schedule(input: &str) -> Vec<BwEntry> {
    let mut entries = Vec::new();
    for part in input.split_whitespace() {
        let (time_str, rate_str) = match part.split_once(',') {
            Some((t, r)) => (t, r),
            None => continue,
        };
        let (hour_str, minute_str) = match time_str.split_once(':') {
            Some((h, m)) => (h, m),
            None => continue,
        };
        let hour: u8 = match hour_str.parse() {
            Ok(h) if h < 24 => h,
            _ => continue,
        };
        let minute: u8 = match minute_str.parse() {
            Ok(m) if m < 60 => m,
            _ => continue,
        };
        let rate_bytes = parse_rate(rate_str);
        if rate_bytes > 0 {
            entries.push(BwEntry { hour, minute, rate_bytes });
        }
    }
    entries.sort_by_key(|e| (e.hour, e.minute));
    entries
}

fn parse_rate(s: &str) -> u64 {
    let s = s.trim();
    let upper = s.to_uppercase();
    let len = upper.len();
    if len == 0 {
        return 0;
    }

    let (num_str, multiplier) = if upper.ends_with("TB") {
        (&s[..len-2], 1024u64.pow(4))
    } else if upper.ends_with("GB") {
        (&s[..len-2], 1024u64.pow(3))
    } else if upper.ends_with("MB") {
        (&s[..len-2], 1024u64.pow(2))
    } else if upper.ends_with("KB") {
        (&s[..len-2], 1024u64)
    } else if upper.ends_with('B') && len > 1 && !matches!(upper.as_bytes()[len-2], b'K' | b'M' | b'G' | b'T') {
        (&s[..len-1], 1u64)
    } else if upper.ends_with('T') {
        (&s[..len-1], 1024u64.pow(4))
    } else if upper.ends_with('G') {
        (&s[..len-1], 1024u64.pow(3))
    } else if upper.ends_with('M') {
        (&s[..len-1], 1024u64.pow(2))
    } else if upper.ends_with('K') {
        (&s[..len-1], 1024u64)
    } else {
        (s, 1u64)
    };

    let num_str = num_str.trim();
    if num_str.is_empty() {
        return 0;
    }
    match num_str.parse::<f64>() {
        Ok(v) if v > 0.0 => (v * multiplier as f64) as u64,
        _ => 0,
    }
}

/// Spawn a background task that updates the rate limiter according to a timetable.
/// The schedule format: "08:00,500KB 12:00,5MB 18:00,2GB" (24h, KB/MB/GB/TB).
/// Pass 0 as rate to mean "unlimited" (sets a very high rate).
pub fn spawn_scheduler(limiter: Arc<TokenBucket>, schedule: &str) {
    let entries = parse_schedule(schedule);
    if entries.is_empty() {
        tracing::warn!("Invalid bandwidth schedule: {schedule}");
        return;
    }

    let initial_rate = entries.first().unwrap().rate_bytes;
    limiter.set_rate(initial_rate);
    tracing::info!(
        "Bandwidth schedule loaded ({} entries), initial rate: {}",
        entries.len(),
        initial_rate,
    );

    tokio::spawn(async move {
        loop {
            let now = chrono_now();
            let mut next = Duration::from_secs(86400); // default: tomorrow

            for entry in &entries {
                let entry_secs = entry.hour as u64 * 3600 + entry.minute as u64 * 60;
                if entry_secs > now {
                    let until = entry_secs - now;
                    if until < next.as_secs() {
                        next = Duration::from_secs(until);
                    }
                }
            }

            if next.as_secs() == 86400 {
                // All entries passed today, first entry is tomorrow
                let first = &entries[0];
                let tomorrow = 86400 - now + first.hour as u64 * 3600 + first.minute as u64 * 60;
                next = Duration::from_secs(tomorrow);
            }

            tokio::time::sleep(next).await;

            let now2 = chrono_now();
            for entry in &entries {
                let entry_secs = entry.hour as u64 * 3600 + entry.minute as u64 * 60;
                if entry_secs == now2 {
                    limiter.set_rate(entry.rate_bytes);
                    tracing::info!(
                        "Bandwidth schedule: rate set to {}",
                        entry.rate_bytes,
                    );
                }
            }
        }
    });
}

fn chrono_now() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (now.as_secs() % 86400) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rate() {
        assert_eq!(parse_rate("500KB"), 500 * 1024);
        assert_eq!(parse_rate("5MB"), 5 * 1024 * 1024);
        assert_eq!(parse_rate("2GB"), 2 * 1024u64.pow(3));
        assert_eq!(parse_rate("1TB"), 1024u64.pow(4));
        assert_eq!(parse_rate("512B"), 512);
        assert_eq!(parse_rate("0"), 0);
        assert_eq!(parse_rate("unlimited"), 0);
        assert_eq!(parse_rate("1.5MB"), (1.5 * 1024.0 * 1024.0) as u64);
    }

    #[test]
    fn test_parse_schedule() {
        let entries = parse_schedule("08:00,500KB 12:00,5MB 18:00,2GB");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].hour, 8);
        assert_eq!(entries[0].rate_bytes, 500 * 1024);
        assert_eq!(entries[1].rate_bytes, 5 * 1024 * 1024);
        assert_eq!(entries[2].rate_bytes, 2 * 1024u64.pow(3));
    }

    #[test]
    fn test_parse_schedule_sorted() {
        let entries = parse_schedule("18:00,2MB 08:00,500KB 12:00,5MB");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].hour, 8);
        assert_eq!(entries[1].hour, 12);
        assert_eq!(entries[2].hour, 18);
    }

    #[test]
    fn test_parse_schedule_invalid() {
        let entries = parse_schedule("invalid");
        assert_eq!(entries.len(), 0);
    }
}

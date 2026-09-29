/// Parse a bandwidth string (e.g. "500KB", "2MB", "1.5GB", "512") into bytes.
/// Supports KB (1024), MB (1024^2), GB (1024^3), TB (1024^4), B (bytes).
/// Decimals allowed: "1.5MB" = 1572864 bytes.
/// Returns `None` if the string is empty, malformed, or zero.
pub fn parse_rate(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    let upper = s.to_uppercase();
    let len = upper.len();

    let (num_str, multiplier) = if upper.ends_with("TB") {
        (&s[..len - 2], 1024u64.pow(4))
    } else if upper.ends_with("GB") {
        (&s[..len - 2], 1024u64.pow(3))
    } else if upper.ends_with("MB") {
        (&s[..len - 2], 1024u64.pow(2))
    } else if upper.ends_with("KB") {
        (&s[..len - 2], 1024u64)
    } else if upper.ends_with('T') {
        (&s[..len - 1], 1024u64.pow(4))
    } else if upper.ends_with('G') {
        (&s[..len - 1], 1024u64.pow(3))
    } else if upper.ends_with('M') {
        (&s[..len - 1], 1024u64.pow(2))
    } else if upper.ends_with('K') {
        (&s[..len - 1], 1024u64)
    } else if upper.ends_with('B') && len > 1 {
        (&s[..len - 1], 1u64)
    } else {
        (s, 1u64)
    };

    let num_str = num_str.trim();
    if num_str.is_empty() {
        return None;
    }

    match num_str.parse::<f64>() {
        Ok(v) if v > 0.0 => Some((v * multiplier as f64) as u64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rate_plain() {
        assert_eq!(parse_rate("512000"), Some(512000));
    }

    #[test]
    fn test_parse_rate_b() {
        assert_eq!(parse_rate("512B"), Some(512));
    }

    #[test]
    fn test_parse_rate_kb() {
        assert_eq!(parse_rate("500KB"), Some(500 * 1024));
        assert_eq!(parse_rate("500K"), Some(500 * 1024));
    }

    #[test]
    fn test_parse_rate_mb() {
        assert_eq!(parse_rate("2MB"), Some(2 * 1024 * 1024));
        assert_eq!(parse_rate("2M"), Some(2 * 1024 * 1024));
    }

    #[test]
    fn test_parse_rate_gb() {
        assert_eq!(parse_rate("1GB"), Some(1024u64.pow(3)));
        assert_eq!(parse_rate("1G"), Some(1024u64.pow(3)));
    }

    #[test]
    fn test_parse_rate_tb() {
        assert_eq!(parse_rate("1TB"), Some(1024u64.pow(4)));
    }

    #[test]
    fn test_parse_rate_decimal() {
        assert_eq!(parse_rate("1.5MB"), Some((1.5 * 1024.0 * 1024.0) as u64));
    }

    #[test]
    fn test_parse_rate_empty() {
        assert_eq!(parse_rate(""), None);
    }

    #[test]
    fn test_parse_rate_invalid() {
        assert_eq!(parse_rate("abc"), None);
        assert_eq!(parse_rate("-1"), None);
        assert_eq!(parse_rate("0"), None);
    }
}

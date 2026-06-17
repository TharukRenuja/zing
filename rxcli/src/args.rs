use std::path::PathBuf;
use clap::Parser;

/// Parse a bandwidth value like "500KB", "2MB", "1.5GB", or plain bytes.
/// Supports KB (1024), MB (1024^2), GB (1024^3), TB (1024^4), B (bytes).
/// Decimals allowed: "1.5MB" = 1572864 bytes.
fn parse_bandwidth(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty bandwidth value".to_string());
    }

    // Normalize to uppercase for suffix matching
    let upper = s.to_uppercase();

    let (num_str, multiplier) = if upper.ends_with("TB") {
        (&s[..s.len()-2], 1024u64.pow(4))
    } else if upper.ends_with('T') {
        (&s[..s.len()-1], 1024u64.pow(4))
    } else if upper.ends_with("GB") {
        (&s[..s.len()-2], 1024u64.pow(3))
    } else if upper.ends_with('G') {
        (&s[..s.len()-1], 1024u64.pow(3))
    } else if upper.ends_with("MB") {
        (&s[..s.len()-2], 1024u64.pow(2))
    } else if upper.ends_with('M') {
        (&s[..s.len()-1], 1024u64.pow(2))
    } else if upper.ends_with("KB") {
        (&s[..s.len()-2], 1024u64)
    } else if upper.ends_with('K') {
        (&s[..s.len()-1], 1024u64)
    } else if upper.ends_with('B') {
        (&s[..s.len()-1], 1u64)
    } else {
        (s, 1u64)
    };

    let num_str = num_str.trim();
    if num_str.is_empty() {
        return Err(format!("missing number before suffix in '{s}'"));
    }

    let value = match num_str.parse::<f64>() {
        Ok(v) if v >= 0.0 => v,
        _ => return Err(format!("invalid number '{num_str}' in bandwidth value")),
    };

    let bytes = (value * multiplier as f64) as u64;
    if bytes == 0 {
        return Err("bandwidth must be greater than 0".to_string());
    }
    Ok(bytes)
}

#[derive(Parser, Debug)]
#[command(name = "rxdl", version, about = "Download files with HTTP/1.1, HTTP/2, and HTTP/3.", long_about = None)]
pub struct Args {
    #[arg(required_unless_present = "daemon")]
    pub urls: Vec<String>,

    #[arg(long, short = 'o', help = "Output filename")]
    pub output: Option<PathBuf>,

    #[arg(long, short = 'd', help = "Output directory")]
    pub dir: Option<PathBuf>,

    #[arg(long, short = 'n', default_value = "4", help = "Max parallel connections")]
    pub connections: usize,

    #[arg(long, help = "Start daemon (Unix socket)")]
    pub daemon: bool,

    #[arg(long, short = 'q', help = "Quiet mode")]
    pub quiet: bool,

    #[arg(long, short = 'r', hide = true)]
    pub resume: bool,

    #[arg(long, help = "Skip TLS verification")]
    pub insecure: bool,

    #[arg(
        long,
        value_parser = parse_bandwidth,
        default_value = "0",
        help = "Max download rate (500KB, 2MB, 1.5GB, 0 = unlimited)"
    )]
    pub max_download_rate: u64,

    #[arg(long, help = "Verify checksum (auto-detect type by length)")]
    pub checksum: Option<String>,

    #[arg(long, help = "HTTP/HTTPS proxy")]
    pub proxy: Option<String>,

    #[arg(long, help = "Mirror URLs for failover")]
    pub mirror: Vec<String>,

    #[arg(
        long,
        help = "Bandwidth schedule (e.g. '08:00,500KB 18:00,2MB')"
    )]
    pub bwlimit: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bandwidth_plain() {
        assert_eq!(parse_bandwidth("512000"), Ok(512000));
    }

    #[test]
    fn test_parse_bandwidth_b() {
        assert_eq!(parse_bandwidth("512B"), Ok(512));
    }

    #[test]
    fn test_parse_bandwidth_kb() {
        assert_eq!(parse_bandwidth("500KB"), Ok(500 * 1024));
        assert_eq!(parse_bandwidth("500K"), Ok(500 * 1024));
    }

    #[test]
    fn test_parse_bandwidth_mb() {
        assert_eq!(parse_bandwidth("2MB"), Ok(2 * 1024 * 1024));
        assert_eq!(parse_bandwidth("2M"), Ok(2 * 1024 * 1024));
    }

    #[test]
    fn test_parse_bandwidth_gb() {
        assert_eq!(parse_bandwidth("1GB"), Ok(1024u64.pow(3)));
        assert_eq!(parse_bandwidth("1G"), Ok(1024u64.pow(3)));
    }

    #[test]
    fn test_parse_bandwidth_tb() {
        assert_eq!(parse_bandwidth("1TB"), Ok(1024u64.pow(4)));
    }

    #[test]
    fn test_parse_bandwidth_decimal() {
        assert_eq!(parse_bandwidth("1.5MB"), Ok((1.5 * 1024.0 * 1024.0) as u64));
    }

    #[test]
    fn test_parse_bandwidth_errors() {
        assert!(parse_bandwidth("").is_err());
        assert!(parse_bandwidth("abc").is_err());
        assert!(parse_bandwidth("-1").is_err());
        assert!(parse_bandwidth("0").is_err());
    }
}

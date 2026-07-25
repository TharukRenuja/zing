use clap::{Parser, Subcommand};
use std::path::PathBuf;

fn parse_bandwidth(s: &str) -> Result<u64, String> {
    if s.trim() == "0" {
        return Ok(0);
    }
    zing_ext::bandwidth::parse_rate(s).ok_or_else(|| format!("invalid bandwidth value: '{s}'"))
}

#[derive(Parser, Debug)]
#[command(name = "zing", version, about = "A modern HTTP downloader with segmented concurrent downloads", long_about = None, disable_help_subcommand = true)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Commands>,

    #[arg(hide = true)]
    pub urls: Vec<String>,

    #[arg(long = "output", short = 'o', help = "Output filename")]
    pub output: Option<PathBuf>,

    #[arg(long = "dir", short = 'd', help = "Output directory")]
    pub dir: Option<PathBuf>,

    #[arg(
        long = "connections",
        short = 'n',
        default_value = "4",
        help = "Max parallel connections"
    )]
    pub connections: usize,

    #[arg(long = "quiet", short = 'q', help = "Quiet mode")]
    pub quiet: bool,

    #[arg(long = "insecure", short = 'k', help = "Skip TLS verification")]
    pub insecure: bool,

    #[arg(
        long = "max-download-rate",
        short = 'r',
        value_parser = parse_bandwidth,
        default_value = "0",
        help = "Max download rate (500KB, 2MB, 1.5GB, 0 = unlimited)"
    )]
    pub max_download_rate: u64,

    #[arg(
        long = "max-concurrent",
        default_value = "1",
        help = "Max concurrent downloads (0 = unlimited)"
    )]
    pub max_concurrent: usize,

    #[arg(
        long = "max-filesize",
        short = 'S',
        value_parser = parse_bandwidth,
        default_value = "0",
        help = "Max file size (500KB, 2MB, 1GB, 0 = unlimited). Skips download if Content-Length exceeds this."
    )]
    pub max_filesize: u64,

    #[arg(
        long = "checksum",
        short = 'c',
        help = "Verify checksum (auto-detect type by length)"
    )]
    pub checksum: Option<String>,

    #[arg(long = "proxy", short = 'x', help = "HTTP/HTTPS proxy")]
    pub proxy: Option<String>,

    #[arg(long = "mirror", short = 'm', help = "Mirror URLs for failover")]
    pub mirror: Vec<String>,

    #[arg(
        long = "bwlimit",
        short = 'b',
        help = "Bandwidth schedule (e.g. '08:00,500KB 18:00,2MB')"
    )]
    pub bwlimit: Option<String>,

    #[arg(
        long = "header",
        short = 'H',
        help = "Custom HTTP header (e.g. 'User-Agent: MyApp/1.0'). Can be repeated."
    )]
    pub header: Vec<String>,

    #[arg(
        long = "metalink",
        short = 'M',
        help = "Metalink (.meta4) file — extracts mirrors, checksums, and filename"
    )]
    pub metalink: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(name = "daemon", about = "Manage the download daemon", alias = "d")]
    Daemon(DaemonArgs),

    #[command(
        name = "schedule",
        about = "Manage scheduled downloads",
        alias = "sched",
        alias = "s"
    )]
    Schedule(ScheduleArgs),

    #[command(
        name = "config",
        about = "Manage configuration",
        alias = "cfg",
        alias = "c"
    )]
    Config(ConfigArgs),

    #[command(
        name = "list",
        about = "List all downloads (daemon)",
        alias = "ls",
        alias = "tasks"
    )]
    List,

    #[command(name = "completions", about = "Generate shell completions", hide = true)]
    Completions {
        #[arg(value_enum, help = "Shell to generate completions for", hide = true)]
        shell: clap_complete::Shell,
    },

    #[command(name = "update", about = "Update zing to the latest version")]
    Update,
}

#[derive(Parser, Debug)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub action: DaemonAction,
}

#[derive(Subcommand, Debug)]
pub enum DaemonAction {
    #[command(about = "Start the download daemon in the foreground")]
    Start,

    #[command(about = "Install daemon as a systemd user service (auto-start on login)")]
    Install,

    #[command(about = "Remove the systemd user service")]
    Uninstall,

    #[command(about = "Show daemon status")]
    Status,
}

#[derive(Parser, Debug)]
pub struct ScheduleArgs {
    #[command(subcommand)]
    pub action: ScheduleAction,
}

#[derive(Subcommand, Debug)]
pub enum ScheduleAction {
    #[command(about = "List all scheduled downloads", alias = "ls")]
    List,

    #[command(about = "Add a scheduled download")]
    Add {
        #[arg(help = "Download URL")]
        url: String,

        #[arg(short = 't', long, help = "Start time in HH:MM format (e.g. 02:00)")]
        at: String,

        #[arg(
            short = 'e',
            long,
            help = "End time in HH:MM (e.g. 07:00). When set, triggers anytime within [at, end) window"
        )]
        end: Option<String>,

        #[arg(long, help = "Days of week (comma-separated, e.g. Mon,Wed,Fri)")]
        days: Option<String>,

        #[arg(short = 'o', long, help = "Output file path")]
        output: Option<String>,

        #[arg(long, short = 'n', default_value = "4", help = "Max connections")]
        connections: Option<usize>,
    },

    #[command(about = "Remove a scheduled download", alias = "rm")]
    Remove {
        #[arg(help = "Schedule entry ID")]
        id: String,
    },
}

#[derive(Parser, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    #[command(about = "List all configuration values", alias = "ls")]
    List,

    #[command(about = "Set a configuration value (e.g. zing config set download_dir ~/Downloads)")]
    Set {
        #[arg(help = "Configuration key")]
        key: String,

        #[arg(help = "Configuration value")]
        value: String,
    },

    #[command(about = "Get a configuration value")]
    Get {
        #[arg(help = "Configuration key")]
        key: String,
    },

    #[command(about = "Delete a configuration key", alias = "del", alias = "rm")]
    Delete {
        #[arg(help = "Configuration key")]
        key: String,
    },

    #[command(about = "Interactive configuration editor", alias = "e")]
    Edit,
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
        assert_eq!(parse_bandwidth("0"), Ok(0));
    }
}

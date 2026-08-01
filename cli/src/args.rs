use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Copy, Clone, Debug, PartialEq, ValueEnum)]
pub enum ProgressType {
    Bar,
    Json,
    None,
}

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

    #[arg(
        long = "quiet",
        short = 'q',
        help = "Quiet mode (suppress all progress output)"
    )]
    pub quiet: bool,

    #[arg(
        long = "progress",
        value_enum,
        default_value = "bar",
        help = "Progress output type: bar, json, or none"
    )]
    pub progress: ProgressType,

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
        long = "referer",
        short = 'e',
        help = "Referer URL (sets the Referer header)"
    )]
    pub referer: Option<String>,

    #[arg(
        long = "user",
        short = 'u',
        help = "HTTP basic auth username:password (e.g. 'user:pass' or 'token')"
    )]
    pub user: Option<String>,

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

    #[arg(
        long = "retry",
        default_value = "5",
        help = "Max retry attempts per connection"
    )]
    pub retry: u32,

    #[arg(
        long = "retry-wait",
        default_value = "500",
        help = "Base retry wait in milliseconds (doubles each attempt)"
    )]
    pub retry_wait: u64,

    #[arg(
        long = "connect-timeout",
        default_value = "30",
        help = "Connection timeout in seconds"
    )]
    pub connect_timeout: u64,

    #[arg(
        long = "max-time",
        default_value = "300",
        help = "Maximum total transfer time in seconds"
    )]
    pub max_time: u64,

    #[arg(
        long = "low-speed-limit",
        default_value = "0",
        help = "Low speed limit in bytes/sec (abort if below this, 0 = disabled)"
    )]
    pub low_speed_limit: u64,

    #[arg(
        long = "low-speed-time",
        default_value = "30",
        help = "Time in seconds to wait before aborting a slow connection"
    )]
    pub low_speed_time: u64,

    #[arg(
        long = "save-interval",
        default_value = "5",
        help = "Control file save interval in seconds"
    )]
    pub save_interval: u64,

    #[arg(
        long = "input-file",
        short = 'i',
        help = "Read URLs from file (one per line, # for comments)"
    )]
    pub input_file: Option<String>,

    #[arg(long = "continue", help = "Resume partially downloaded files")]
    pub resume: bool,

    #[arg(
        long = "method",
        short = 'X',
        help = "HTTP method (GET, POST, PUT, etc.)"
    )]
    pub method: Option<String>,

    #[arg(
        long = "upload-file",
        short = 'T',
        help = "Upload file as request body (PUT/POST)"
    )]
    pub upload_file: Option<String>,

    #[arg(
        long = "pipe",
        short = 'p',
        default_missing_value = "raw",
        num_args = 0..=1,
        require_equals = true,
        help = "Pipe output mode: 'raw' (default, no value), 'sh', 'run', 'bash', 'python', 'node', 'tar', 'app', 'install'"
    )]
    pub pipe: Option<String>,

    #[arg(long = "user-agent", short = 'A', help = "Custom User-Agent header")]
    pub user_agent: Option<String>,

    #[arg(
        long = "dry-run",
        help = "Show what would be downloaded without fetching content"
    )]
    pub dry_run: bool,

    #[arg(
        long = "standalone",
        help = "Force standalone mode even if the daemon is running"
    )]
    pub standalone: bool,

    #[arg(
        long = "auto-file-renaming",
        help = "Auto-rename file if exists (e.g. file(1).ext)"
    )]
    pub auto_file_renaming: bool,

    #[arg(
        long = "allow-overwrite",
        help = "Overwrite existing files without prompting"
    )]
    pub allow_overwrite: bool,

    #[arg(
        long = "end-game",
        help = "Enable end-game mode (all connections race for last blocks)"
    )]
    pub end_game: bool,
    #[arg(
        long = "no-end-game",
        help = "Disable end-game mode",
        conflicts_with = "end_game"
    )]
    pub no_end_game: bool,

    #[arg(
        long = "throttle-reprobe",
        help = "Enable throttling re-probe (restart download if speed drops too low)"
    )]
    pub throttle_reprobe: bool,
    #[arg(
        long = "no-throttle-reprobe",
        help = "Disable throttling re-probe",
        conflicts_with = "throttle_reprobe"
    )]
    pub no_throttle_reprobe: bool,

    #[arg(
        long = "content-disposition",
        short = 'C',
        help = "Use server-provided filename from Content-Disposition"
    )]
    pub content_disposition: bool,

    #[arg(
        long = "load-cookies",
        short = 'L',
        help = "Load cookies from Netscape-format cookie file"
    )]
    pub load_cookies: Option<String>,

    #[arg(
        long = "save-cookies",
        short = 's',
        help = "Save cookies to file after download"
    )]
    pub save_cookies: Option<String>,

    #[arg(
        long = "netrc",
        short = 'N',
        help = "Use .netrc file for authentication"
    )]
    pub netrc: bool,

    #[arg(
        long = "digest",
        help = "Use HTTP Digest authentication (requires --user)"
    )]
    pub digest: bool,

    #[arg(
        long = "cert",
        help = "TLS client certificate (PEM file, may include private key)"
    )]
    pub cert: Option<String>,

    #[arg(
        long = "cert-key",
        help = "TLS client certificate private key (PEM file, required if cert does not include the key)"
    )]
    pub cert_key: Option<String>,

    #[arg(long = "log", short = 'l', help = "Log to file instead of stderr")]
    pub log: Option<String>,

    #[arg(
        long = "on-download-complete",
        help = "Command to run when download completes ({} = file path)"
    )]
    pub on_download_complete: Option<String>,

    #[arg(
        long = "on-download-error",
        help = "Command to run when download fails ({} = file path)"
    )]
    pub on_download_error: Option<String>,
}

#[allow(clippy::large_enum_variant)]
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

    #[command(name = "pause", about = "Pause a download (daemon)", alias = "p")]
    Pause {
        #[arg(help = "Task ID to pause")]
        id: u64,
    },

    #[command(
        name = "resume",
        about = "Resume a paused download (daemon)",
        alias = "unpause"
    )]
    Resume {
        #[arg(help = "Task ID to resume")]
        id: u64,
    },

    #[command(
        name = "remove",
        about = "Remove a download (daemon)",
        alias = "rm",
        alias = "delete"
    )]
    Remove {
        #[arg(help = "Task ID to remove")]
        id: u64,
    },

    #[command(
        name = "completions",
        about = "Generate shell completions",
        hide = true
    )]
    Completions {
        #[arg(value_enum, help = "Shell to generate completions for", hide = true)]
        shell: clap_complete::Shell,
    },

    #[command(name = "update", about = "Update zing to the latest version")]
    Update,

    #[command(name = "tui", about = "Launch the terminal UI for downloads")]
    Tui {
        #[arg(help = "URLs to download")]
        urls: Vec<String>,

        #[arg(
            long = "connections",
            short = 'n',
            default_value = "4",
            help = "Max parallel connections per download"
        )]
        connections: usize,

        #[arg(long = "dir", short = 'd', help = "Output directory")]
        dir: Option<PathBuf>,

        #[arg(long = "output", short = 'o', help = "Output filename")]
        output: Option<PathBuf>,

        #[arg(
            long = "max-download-rate",
            short = 'r',
            value_parser = parse_bandwidth,
            default_value = "0",
            help = "Max download rate (500KB, 2MB, 1.5GB, 0 = unlimited)"
        )]
        max_download_rate: u64,

        #[arg(
            long = "max-filesize",
            short = 'S',
            value_parser = parse_bandwidth,
            default_value = "0",
            help = "Max file size (500KB, 2MB, 1GB, 0 = unlimited). Skips download if Content-Length exceeds this."
        )]
        max_filesize: u64,

        #[arg(long = "insecure", short = 'k', help = "Skip TLS verification")]
        insecure: bool,

        #[arg(long = "proxy", short = 'x', help = "HTTP/HTTPS proxy")]
        proxy: Option<String>,

        #[arg(long = "mirror", short = 'm', help = "Mirror URLs for failover")]
        mirror: Vec<String>,

        #[arg(long = "user-agent", short = 'A', help = "Custom User-Agent header")]
        user_agent: Option<String>,

        #[arg(
            long = "header",
            short = 'H',
            help = "Custom HTTP header (e.g. 'User-Agent: MyApp/1.0'). Can be repeated."
        )]
        header: Vec<String>,

        #[arg(
            long = "user",
            short = 'u',
            help = "HTTP basic auth username:password (e.g. 'user:pass' or 'token')"
        )]
        user: Option<String>,

        #[arg(
            long = "digest",
            help = "Use HTTP Digest authentication (requires --user)"
        )]
        digest: bool,

        #[arg(
            long = "retry",
            default_value = "5",
            help = "Max retry attempts per connection"
        )]
        retry: u32,

        #[arg(
            long = "retry-wait",
            default_value = "500",
            help = "Base retry wait in milliseconds (doubles each attempt)"
        )]
        retry_wait: u64,

        #[arg(
            long = "connect-timeout",
            default_value = "30",
            help = "Connection timeout in seconds"
        )]
        connect_timeout: u64,

        #[arg(
            long = "max-time",
            default_value = "300",
            help = "Maximum total transfer time in seconds"
        )]
        max_time: u64,

        #[arg(
            long = "end-game",
            help = "Enable end-game mode (all connections race for last blocks)"
        )]
        end_game: bool,
        #[arg(
            long = "no-end-game",
            help = "Disable end-game mode",
            conflicts_with = "end_game"
        )]
        no_end_game: bool,

        #[arg(
            long = "throttle-reprobe",
            help = "Enable throttling re-probe (restart download if speed drops too low)"
        )]
        throttle_reprobe: bool,
        #[arg(
            long = "no-throttle-reprobe",
            help = "Disable throttling re-probe",
            conflicts_with = "throttle_reprobe"
        )]
        no_throttle_reprobe: bool,

        #[arg(
            long = "load-cookies",
            short = 'L',
            help = "Load cookies from Netscape-format cookie file"
        )]
        load_cookies: Option<String>,

        #[arg(
            long = "save-cookies",
            short = 's',
            help = "Save cookies to file after download"
        )]
        save_cookies: Option<String>,
    },
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

    #[command(about = "Stop the running download daemon")]
    Stop,

    #[command(about = "Install daemon as a systemd user service (auto-start on login)")]
    Install,

    #[command(about = "Remove the systemd user service")]
    Uninstall,

    #[command(about = "Show daemon status")]
    Status,

    #[command(about = "Restart the download daemon")]
    Restart,
}

#[derive(Parser, Debug)]
pub struct ScheduleArgs {
    #[command(subcommand)]
    pub action: ScheduleAction,
}

#[allow(clippy::large_enum_variant)]
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

        #[arg(long, short = 'd', help = "Output directory")]
        output_dir: Option<String>,

        #[arg(long, short = 'n', default_value = "4", help = "Max connections")]
        connections: Option<usize>,

        #[arg(long, short = 'k', help = "Skip TLS verification")]
        insecure: bool,

        #[arg(
            long = "max-download-rate",
            short = 'r',
            value_parser = parse_bandwidth,
            default_value = "0",
            help = "Max download rate (500KB, 2MB, 1.5GB, 0 = unlimited)"
        )]
        max_download_rate: u64,

        #[arg(long, short = 'x', help = "HTTP/HTTPS proxy")]
        proxy: Option<String>,

        #[arg(
            long = "header",
            short = 'H',
            help = "Custom HTTP header. Can be repeated."
        )]
        header: Vec<String>,

        #[arg(
            long = "user",
            short = 'u',
            help = "HTTP basic auth username:password (e.g. 'user:pass')"
        )]
        user: Option<String>,

        #[arg(long = "referer", help = "Referer URL (sets the Referer header)")]
        referer: Option<String>,

        #[arg(long = "checksum", short = 'c', help = "Verify checksum")]
        checksum: Option<String>,

        #[arg(
            long = "mirror",
            short = 'm',
            help = "Mirror URLs for failover. Can be repeated."
        )]
        mirror: Vec<String>,

        #[arg(
            long = "max-filesize",
            short = 'S',
            value_parser = parse_bandwidth,
            default_value = "0",
            help = "Max file size (500KB, 2MB, 1GB, 0 = unlimited)"
        )]
        max_filesize: u64,
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

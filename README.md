# rxdl

Simple, modern, intelligent, cross-platform HTTP downloader with adaptive connection management, server probing, and concurrent segmented downloads.

```
rxdl https://example.com/file.zip
```

## Install

```bash
# Download pre-built binary (recommended)
curl -fsSL https://raw.githubusercontent.com/TharukRenuja/rxdl/main/install.sh | sh

# Or install with cargo (requires Rust)
cargo install rxdl

# Or build from source
cargo build --release
./target/release/rxdl --help
```

## How it works

```
rxdl https://example.com/file.zip

→ checks /tmp/rxdl.sock
  exists?  proxies to daemon, shows progress, exits
  absent?  downloads directly (like curl)
```

## Quick start

```bash
# Download a file (auto-named from URL)
rxdl https://example.com/file.zip

# Save to a specific filename
rxdl -o myfile.zip https://example.com/file.zip

# Download to a directory
rxdl -d downloads/ https://example.com/file.zip

# Rate limit to 2 MB/s
rxdl -r 2MB https://example.com/file.zip

# Skip TLS verification
rxdl -k https://example.com/file.zip

# Verify checksum after download
rxdl -c d41d8cd98f00b204e9800998ecf8427e https://example.com/file.zip

# Use a proxy
rxdl -x http://user:pass@proxy:8080 https://example.com/file.zip

# Use mirror URLs for failover
rxdl -m https://mirror1.example.com/file.zip https://example.com/file.zip

# Schedule bandwidth (8AM: 500KB/s, 6PM: 2MB/s)
rxdl -b "08:00,500KB 18:00,2MB" https://example.com/file.zip

# Pause mid-download with Ctrl+Z, resume with fg
# rxdl saves state on pause and resumes seamlessly
```

## Pause / Resume

In standalone mode, suspend a download with **Ctrl+Z** (`SIGTSTP`). The download saves its state to a `.rxdl` control file and stops. Bring it back to the foreground with **`fg`** (`SIGCONT`) — it resumes from where it left off.

```
rxdl https://example.com/large-file.zip
  ^Z  → pauses, saves state
  fg  → resumes download
```

## Daemon mode

Start a background daemon so downloads continue even after you close the terminal.

```bash
# Start the daemon (also: rxdl d)
rxdl daemon

# Now any rxdl download will proxy through the daemon automatically
rxdl https://example.com/file.zip
```

### systemd autostart

```ini
# ~/.config/systemd/user/rxdl.service
[Unit]
Description=rxdl download daemon

[Service]
ExecStart=/path/to/rxdl daemon
Restart=on-failure

[Install]
WantedBy=default.target
```

```bash
systemctl --user enable --now rxdl
```

## Scheduled downloads

Schedule downloads with an optional time window. The daemon must be running.

```bash
# Download at a specific time
rxdl schedule add https://example.com/file.zip --at 02:00

# Download within a time window (e.g. free hours: 00:00–07:00)
rxdl schedule add https://example.com/file.zip --at 00:00 --end 07:00

# On specific days only
rxdl schedule add https://example.com/file.zip --at 06:00 --end 07:00 --days Mon,Wed,Fri

# List scheduled downloads
rxdl schedule list       # also: rxdl schedule ls

# Remove a schedule
rxdl schedule remove <id>   # also: rxdl schedule rm
```

## Configuration

```bash
# List current config
rxdl config list       # also: rxdl config ls

# Get a value
rxdl config get download_dir

# Set a value (paths support ~, $HOME, $USER, etc.)
rxdl config set download_dir "~/Downloads"

# Delete a config key
rxdl config delete prompt_location   # also: rxdl config del, rxdl config rm

# Open config in $EDITOR
rxdl config edit       # also: rxdl config e
```

Config file: `~/.config/rxdl/config.json`

```json
{
  "download_dir": "~/Downloads",
  "prompt_location": false
}
```

Supports shell expansion: `~`, `$HOME`, `$USER`, `$HOME/Downloads` all resolve to your actual home directory.

### Subcommand aliases

| Full | Aliases |
|------|---------|
| `daemon` | `d` |
| `schedule` | `sched`, `s` |
| `config` | `cfg`, `c` |
| `schedule list` | `ls` |
| `schedule remove` | `rm` |
| `config list` | `ls` |
| `config delete` | `del`, `rm` |
| `config edit` | `e` |

## Features

- **Smart downloads** with automatic speed adjustment
- **Fast disk writing** for better performance
- **Supports the fastest web protocols** (HTTP/1.1, HTTP/2, HTTP/3)
- **Automatic server testing** to find the best download method
- **Smart connection management** to avoid throttling
- **Multi-URL mirror fallback** for better reliability
- **Token bucket rate limiter** for smooth downloads
- **Bandwidth scheduling** for time-of-day rate limits
- **Auto-naming** from server or URL when no filename is given
- **Checksum verification** to ensure file integrity
- **Proxy support** for secure downloads
- **Daemon mode** with live progress updates
- **Scheduled downloads** with cron-like day/time triggers
- **Download resume** to continue interrupted downloads
- **Pause/resume** via Ctrl+Z and fg
- **Preallocation** of file space for faster downloads

## All options

```
Usage: rxdl [OPTIONS] [COMMAND]

Commands:
  daemon    Start the download daemon
  schedule  Manage scheduled downloads
  config    Manage configuration
  help      Print this message or the help of the given subcommand(s)

Options:
  -o, --output <OUTPUT>                    Output filename
  -d, --dir <DIR>                          Output directory
  -n, --connections <CONNECTIONS>          Max parallel connections [default: 4]
  -q, --quiet                              Quiet mode
  -k, --insecure                           Skip TLS verification
  -r, --max-download-rate <RATE>           Max download rate (500KB, 2MB, 1.5GB, 0 = unlimited) [default: 0]
  -c, --checksum <CHECKSUM>                Verify checksum (auto-detect type by length)
  -x, --proxy <PROXY>                      HTTP/HTTPS proxy
  -m, --mirror <MIRROR>                    Mirror URLs for failover
  -b, --bwlimit <SCHEDULE>                 Bandwidth schedule (e.g. '08:00,500KB 18:00,2MB')
  -h, --help                               Print help
  -V, --version                            Print version
```

## Comparison

| Capability | rxdl | aria2 | curl | wget2 | gopeed |
| --- | --- | --- | --- | --- | --- |
| HTTP/1.1 + H2 + H3 | Yes | Yes | Yes | Yes | Yes |
| Segmented concurrent | PID + slow-start + steal | Static split | No | No | Static split |
| Adaptive connections | PID + gain-flattening | No | No | No | No |
| Intelligence probe | RTT + protocol + bw + Range | No | No | No | No |
| Throttling -> re-probe | Speed <30% for 3s | No | No | No | No |
| Lock-free disk writes | pwrite (no seek/mutex) | No | No | No | No |
| Retry + backoff | Exponential + jitter | Yes | No | No | No |
| Rate limiting | TokenBucket | Yes | No | No | No |
| Bandwidth scheduling | Yes | No | No | No | No |
| Mirror failover | Rotate on fail/throttle | Multi-URL | No | No | No |
| Proxy | Yes | Yes | Yes | Yes | Yes |
| Daemon + RPC | Unix socket JSON-RPC | RPC | No | No | Web |
| Scheduled downloads | Day/time triggers | No | No | No | No |
| Resume | .rxdl control file | .aria2 | Yes | Yes | Yes |
| Checksum verify | Post-download auto-detect | Metalink | No | No | No |

## Architecture

4 crates in a workspace:

- **rxcore**: Download engine: probe, segment management, PID control, rate limiting, retry, bandwidth scheduling, connection pool
- **rxcli**: CLI frontend with progress bar, daemon auto-detection, checksum verification, config/schedule management
- **rxdaemon**: Unix socket JSON-RPC server for background and scheduled downloads
- **rxext**: Utilities: checksum verification, filename extraction, aria2 session import

## Design

- **Pwrite over mmap**: Sequential streaming writes benefit from `pwrite`'s direct kernel path. Mmap adds page fault overhead for write-once workloads.
- **reqwest over hyper**: reqwest provides HTTP/2 ALPN negotiation, HTTP/3 via quinn, connection pooling, and proxy support out of the box.
- **Unix socket over TCP**: No port conflicts, filesystem permissions control access, no network exposure.
- **No BitTorrent / browser impersonation**: rxdl focuses on clean HTTP download intelligence.


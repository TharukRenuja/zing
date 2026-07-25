# zing

Simple, modern, intelligent, cross-platform HTTP downloader with adaptive connection management, server probing, and concurrent segmented downloads.

```
zing https://example.com/file.zip
```

## Install

```bash
# Download pre-built binary (recommended)

Download the latest release from [Releases](https://raw.githubusercontent.com/TharukRenuja/zing/releases).


# Or install with cargo (requires Rust)
cargo install zing

# Or build from source
cargo build --release
./target/release/zing --help
```

## How it works

```
zing https://example.com/file.zip

→ checks /tmp/zing.sock
  exists?  proxies to daemon, shows progress, exits
  absent?  downloads directly (like curl)
```

## Quick start

```bash
# Download a file (auto-named from URL)
zing https://example.com/file.zip

# Save to a specific filename
zing -o myfile.zip https://example.com/file.zip

# Download to a directory
zing -d downloads/ https://example.com/file.zip

# Rate limit to 2 MB/s
zing -r 2MB https://example.com/file.zip

# Skip TLS verification
zing -k https://example.com/file.zip

# Verify checksum after download
zing -c d41d8cd98f00b204e9800998ecf8427e https://example.com/file.zip

# Use a proxy
zing -x http://user:pass@proxy:8080 https://example.com/file.zip

# Use mirror URLs for failover
zing -m https://mirror1.example.com/file.zip https://example.com/file.zip

# Schedule bandwidth (8AM: 500KB/s, 6PM: 2MB/s)
zing -b "08:00,500KB 18:00,2MB" https://example.com/file.zip

# Limit file size (skip if Content-Length exceeds)
zing -S 1GB https://example.com/large-file.zip

# Custom HTTP headers
zing -H "Authorization: Bearer token" https://example.com/private.zip

# Use a Metalink file for mirrors + checksums
zing -M file.meta4
```

## Pause / Resume

In standalone mode, zing saves its state to a `.zing` control file on exit. Re-running the same URL resumes from where it left off.

```
zing https://example.com/large-file.zip
  ^C  → saves state, exits
  zing https://example.com/large-file.zip  → resumes
```

## Daemon mode

Start a background daemon so downloads continue even after you close the terminal.

```bash
# Start the daemon in the foreground
zing daemon start

# Install as a systemd user service (auto-start on login)
zing daemon install

# Check daemon status
zing daemon status

# Now any zing download will proxy through the daemon automatically
zing https://example.com/file.zip
```

## Scheduled downloads

Schedule downloads with an optional time window. The daemon must be running.

```bash
# Download at a specific time
zing schedule add https://example.com/file.zip --at 02:00

# Download within a time window
zing schedule add https://example.com/file.zip --at 00:00 --end 07:00

# On specific days only
zing schedule add https://example.com/file.zip --at 06:00 --end 07:00 --days Mon,Wed,Fri

# List scheduled downloads
zing schedule list

# Remove a schedule
zing schedule remove <id>
```

## Configuration

```bash
# List current config
zing config list

# Get a value
zing config get download_dir

# Set a value
zing config set download_dir "~/Downloads"

# Delete a config key
zing config delete download_dir

# Open config in $EDITOR
zing config edit
```

Config file: `~/.config/zing/config.json`

```json
{
  "download_dir": "~/Downloads",
  "prompt_location": false
}
```

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
- **Concurrent multi-URL downloads** with `--max-concurrent`
- **Metalink (.meta4)** support for mirrors + checksums

## Comparison

| Capability | zing | aria2 | curl | wget2 | gopeed |
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
| Metalink | .meta4 parser | Yes | No | No | No |
| Proxy | Yes | Yes | Yes | Yes | Yes |
| Daemon + RPC | Unix socket JSON-RPC | RPC | No | No | Web |
| Scheduled downloads | Day/time triggers | No | No | No | No |
| Resume | .zing control file | .aria2 | Yes | Yes | Yes |
| Checksum verify | Post-download auto-detect | Metalink | No | No | No |
| Concurrent multi-URL | Yes (--max-concurrent) | Yes | No | No | No |

## Architecture

4 crates in a workspace:

- **core**: Download engine: probe, segment management, PID control, rate limiting, retry, bandwidth scheduling, connection pool
- **cli**: CLI frontend with progress bar, daemon auto-detection, checksum verification, config/schedule management
- **daemon**: Unix socket JSON-RPC server for background and scheduled downloads
- **ext**: Utilities: checksum verification, filename extraction, aria2 session import, metalink parsing

## Design

- **Pwrite over mmap**: Sequential streaming writes benefit from `pwrite`'s direct kernel path. Mmap adds page fault overhead for write-once workloads.
- **reqwest over hyper**: reqwest provides HTTP/2 ALPN negotiation, HTTP/3 via quinn, connection pooling, and proxy support out of the box.
- **Unix socket over TCP**: No port conflicts, filesystem permissions control access, no network exposure.
- **No BitTorrent / browser impersonation**: zing focuses on clean HTTP download intelligence.

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

# Download to a directory (auto-created if missing)
rxdl -d downloads/ https://example.com/file.zip

# Rate limit to 2 MB/s
rxdl --max-download-rate 2MB https://example.com/file.zip

# Verify checksum after download
rxdl --checksum d41d8cd98f00b204e9800998ecf8427e https://example.com/file.zip

# Use a proxy
rxdl --proxy http://user:pass@proxy:8080 https://example.com/file.zip
```

## Daemon mode

The daemon runs in the background so downloads continue even after you close the terminal.

```bash
# Start the daemon
rxdl --daemon

# Now any rxdl command will use the daemon automatically
rxdl https://example.com/file.zip
```

### Autostart with systemd

```ini
# ~/.config/systemd/user/rxdl.service
[Unit]
Description=rxdl download daemon

[Service]
ExecStart=/path/to/rxdl --daemon
Restart=on-failure

[Install]
WantedBy=default.target
```

```bash
# Enable and start the daemon
systemctl --user enable --now rxdl
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
- **Download resume** to continue interrupted downloads
- **Preallocation** of file space for faster downloads


## Comparison

| Capability | rxdl | aria2 | curl | wget2 | gopeed |
|---|---|---|---|---|---|
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
| Resume | .rxdl control file | .aria2 | -C- | Yes | Yes |
| Checksum verify | Post-download auto-detect | Metalink | No | No | No |

## All options

```
rxdl --help
```

## Architecture

4 crates in a workspace:

- **rxcore**: Download engine: probe, segment management, PID control, rate limiting, retry, bandwidth scheduling, connection pool
- **rxcli**: CLI frontend with progress bar, daemon auto-detection, checksum verification
- **rxdaemon**: Unix socket JSON-RPC server for background downloads
- **rxext**: Utilities: checksum verification, filename extraction, aria2 session import

## Design

- **Pwrite over mmap**: Sequential streaming writes benefit from `pwrite`'s direct kernel path. Mmap adds page fault overhead for write-once workloads.
- **reqwest over hyper**: reqwest provides HTTP/2 ALPN negotiation, HTTP/3 via quinn, connection pooling, and proxy support out of the box.
- **Unix socket over TCP**: No port conflicts, filesystem permissions control access, no network exposure.
- **No BitTorrent / browser impersonation**: rxdl focuses on clean HTTP download intelligence.


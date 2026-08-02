# zing

> **⚠️ Beta:** zing is **still in active development**. Downloads may occasionally be corrupted or incomplete. **Use at your own risk**.

A modern, cross-platform HTTP downloader with segmented concurrent downloads, adaptive connection management, and server probing.

```
zing https://example.com/file.zip
```

## Contents

- [zing](#zing)
  - [Contents](#contents)
  - [How it works](#how-it-works)
  - [Install](#install)
      - [Install script (Linux/macOS)](#install-script-linuxmacos)
      - [Download pre-built binary](#download-pre-built-binary)
      - [Build from source](#build-from-source)
  - [Quick start](#quick-start)
  - [Features](#features)
    - [Downloading](#downloading)
    - [Daemon](#daemon)
    - [Scheduled downloads](#scheduled-downloads)
    - [Resume](#resume)
    - [Pipe mode](#pipe-mode)
    - [Cookies \& Authentication](#cookies--authentication)
    - [Event hooks](#event-hooks)
    - [Logging](#logging)
    - [Configuration](#configuration)
    - [Completions](#completions)
    - [Update \& Uninstall](#update--uninstall)
  - [Comparison](#comparison)
  - [Architecture](#architecture)
  - [Design](#design)

## How it works

```
zing https://example.com/file.zip

→ checks if daemon is running
  running?  proxies to daemon, shows progress, exits
  absent?  downloads directly (like curl)
```

## Install

#### Install script (Linux/macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/TharukRenuja/zing/main/install.sh | sh
```

Optionally sets up zing-daemon as a systemd service. Installs to `/usr/local/bin`.

#### Download pre-built binary

Grab the latest release from [Releases](https://github.com/TharukRenuja/zing/releases/latest).

- Linux: `zing-latest-{arch}-linux.tar.gz`
- macOS: `zing-latest-{arch}-mac.dmg`
- Windows: `zing-latest-windows.msi`

#### Build from source

```bash
cargo build --release
./target/release/zing --help
```

## Quick start

```bash
# Download a file (auto-named from URL)
zing https://example.com/file.zip

# Save to a specific filename / directory
zing -o myfile.zip https://example.com/file.zip
zing -d downloads/ https://example.com/file.zip

# Rate limit to 2 MB/s
zing -r 2MB https://example.com/file.zip

# Skip TLS verification
zing -k https://example.com/file.zip

# Verify checksum after download
zing -c d41d8cd98f00b204e9800998ecf8427e https://example.com/file.zip

# Use a proxy
zing -x http://user:pass@proxy:8080 https://example.com/file.zip

# Mirror URLs for failover
zing -m https://mirror1.example.com/file.zip https://example.com/file.zip

# Schedule bandwidth (8AM: 500KB/s, 6PM: 2MB/s)
zing -b "08:00,500KB 18:00,2MB" https://example.com/file.zip

# Custom headers and User-Agent
zing -H "Authorization: Bearer token" https://example.com/private.zip
zing -A "MyApp/1.0" https://example.com/file.zip

# Auto-rename if file exists (file-1.ext, file-2.ext, …)
zing --auto-file-renaming https://example.com/file.zip

# Overwrite an existing file without prompting
zing --allow-overwrite https://example.com/file.zip

# Use the server's Content-Disposition filename (on by default; disable with --no-content-disposition)
zing -C https://example.com/download

# Download multiple files concurrently
zing --max-concurrent 3 url1 url2 url3

# Progress output: bar (default), json, or none
zing --progress json https://example.com/file.zip

# Use a Metalink file for mirrors + checksums
zing -M file.meta4

# Dry-run: show URLs without downloading
zing --dry-run https://example.com/file.zip

# Force standalone mode (skip daemon even if running)
zing --standalone https://example.com/file.zip
```

## Features

### Downloading

<details>
<summary>Segmented downloads, probing, throttling, rate limits</summary>

- **Segmented concurrent downloads** with adaptive connection count (PID control + slow-start) instead of a fixed static split
- **Server probing** → measures RTT, protocol, and bandwidth to pick the best download strategy
- **Throttling detection** → if speed drops too low, re-probes and fails over to mirrors
- **End-game mode** → remaining connections race for the last few blocks to minimize tail latency
- **HTTP/1.1, HTTP/2, and HTTP/3** via reqwest
- **Happy Eyeballs DNS** (IPv6-first) and mirror pre-probing by RTT
- **Efficient disk writes** → lock-free `pwrite`, write cache with full-block flushes, `fallocate` pre-allocation on Linux
- **Metalink (.meta4)** with per-block hash validation during download
- **Token bucket rate limiter** and **bandwidth scheduling** for time-of-day limits
- **Retry with exponential backoff + jitter** and multi-URL mirror fallback
- **Checksum verification** (auto-detect by length), **digest auth** (RFC 7616), **TLS client certificates**
- **Auto-naming** from URL or server (Content-Disposition on by default), **conflict handling** that prompts to overwrite/rename/cancel (or `--auto-file-renaming` / `--allow-overwrite`), **dry-run** preview

</details>

### Daemon

<details>
<summary>Background downloads, task management, JSON-RPC</summary>

The daemon runs downloads in the background so they continue even after you close the terminal. Any `zing download` command automatically detects the daemon and proxies through it. Use `--standalone` to force direct download even when the daemon is running.

```bash
# Start / stop / restart
zing daemon start    # foreground
zing daemon stop
zing daemon restart
zing daemon status   # Unix only (systemd)

# Manage tasks
zing list
zing pause <id>
zing resume <id>
zing remove <id>

# Install systemd service (Unix only)
zing daemon install
zing daemon uninstall
```

</details>

### Scheduled downloads

<details>
<summary>Cron-like day/time triggers</summary>

The daemon must be running.

```bash
# Download at a specific time
zing schedule add https://example.com/file.zip --at 02:00

# Within a time window, on specific days
zing schedule add https://example.com/file.zip --at 00:00 --end 07:00 --days Mon,Wed,Fri

# List / remove schedules
zing schedule list
zing schedule remove <id>
```

</details>

### Resume

<details>
<summary>Control file resume + daemon resume command</summary>

zing saves state to a `.zing` control file on exit. Re-running the same URL resumes.

```
zing https://example.com/large-file.zip
  ^C  → saves state, exits
  zing https://example.com/large-file.zip  → resumes
```

For daemon-managed downloads:

```
zing resume <id>
```

</details>

### Pipe mode

<details>
<summary>Direct piping, script execution, and app install</summary>

`-p` / `--pipe` outputs content to stdout (suppressing all logs), optionally auto-piping to a command.

```bash
# Raw pipe
zing -p https://example.com/script.sh | sh

# Auto-pipe to interpreters
zing -p=sh     https://example.com/script.sh      # sh -s
zing -p=bash   https://example.com/script.sh      # bash -s
zing -p=python https://example.com/script.py      # python3
zing -p=node   https://example.com/script.js      # node

# Extract archives / install
zing -p=tar     https://example.com/pkg.tar.gz     # tar -xzf -
zing -p=app     https://example.com/tool.AppImage  # → ~/.local/bin (chmod +x)
zing -p=install https://example.com/tool.tar.gz    # extracts → ~/.local/bin/<name>
```

</details>

### Cookies & Authentication

<details>
<summary>Cookie files, .netrc, basic/digest auth, TLS certs</summary>

```bash
# Cookie jars (Netscape format)
zing -L cookies.txt https://example.com/file.zip               # load cookies
zing -s cookies.txt https://example.com/file.zip               # save cookies
zing -L cookies.txt -s cookies.txt https://example.com/...     # both

# .netrc auth
zing -N https://example.com/private.zip

# Basic auth (or bearer token with 'token:')
zing -u user:pass https://example.com/private.zip

# Digest auth (RFC 7616 MD5-sess)
zing --digest -u user:pass https://example.com/private.zip

# TLS client certificates
zing --cert client.pem --cert-key client-key.pem https://example.com/private.zip
```

</details>

### Event hooks

<details>
<summary>Run commands when downloads finish or fail</summary>

`{}` is replaced with the file path.

```bash
zing --on-download-complete "notify-send 'Done: {}'" https://example.com/file.zip
zing --on-download-error  "echo 'Failed: {}' >> ~/failures.log" https://example.com/file.zip
```

</details>

### Logging

<details>
<summary>Log to file instead of stderr</summary>

```bash
zing -l download.log https://example.com/file.zip
```

</details>

### Configuration

<details>
<summary>Config file, keys, and commands</summary>

```bash
zing config edit     # interactive wizard
zing config list     # or: config get <key>
zing config set download_dir "~/Downloads"
zing config delete download_dir
```

Config file: `~/.config/zing/config.json` (Linux/macOS) or `%APPDATA%\zing\config.json` (Windows)

```json
{
  "download_dir": "~/Downloads",
  "prompt_location": false,
  "update_check_interval_days": 7
}
```

| Key | Default | Description |
| --- | --- | --- |
| `download_dir` | `~/Downloads` | Default download directory |
| `prompt_location` | `false` | Ask for download location before each download |
| `update_check_interval_days` | `7` | Days between update checks (`0` = disabled) |

</details>

### Completions

<details>
<summary>Generate shell completions</summary>

```bash
zing completions bash
zing completions zsh
zing completions fish
zing completions powershell
```

</details>

### Update & Uninstall

<details>
<summary>Updating and removing zing</summary>

```bash
# Check for updates and apply
zing update

# Uninstall via script
zing -p https://raw.githubusercontent.com/TharukRenuja/zing/main/uninstall.sh | sh

# Or manually: remove binaries, ~/.config/zing, and the daemon service
# Windows: use Add/Remove Programs for the NSIS installer
```

</details>

## Comparison

| Capability | zing | aria2 | curl | wget2 | gopeed |
| --- | --- | --- | --- | --- | --- |
| HTTP/1.1 + H2 + H3 | Yes | H1.1 | Yes | H1.1 + H2 | H1.1 + H2 |
| Segmented concurrent download | Yes (adaptive) | Static split | No | Partial (H2 chunked) | Static split |
| Adaptive connection count | Yes | No | No | No | No |
| Server intelligence probe | Yes | No | No | No | No |
| Throttle detection → re-probe | Yes | No | No | No | No |
| Rate limiting | Yes | Yes | Yes (`--limit-rate`) | Yes | Yes |
| Bandwidth scheduling (time-of-day) | Yes | No | No | No | No |
| Mirror failover | Yes | Multi-URL | No | No | No |
| Metalink | .meta4 parser | Yes | No | No | No |
| Checksum verification | Auto-detect | Yes | No | No | No |
| End-game mode | Yes | No | No | No | No |
| Per-block hash validation | Yes | Metalink only | No | No | No |
| Pipe / script output | Yes | No | Yes | No | No |
| Daemon + RPC | Yes | JSON-RPC | No | No | Web |
| Scheduled downloads | Yes | No | No | No | No |
| Resume | Control file + RPC | `.aria2` | `-C -` | Yes | Yes |
| Happy Eyeballs DNS | Yes | Yes | Yes | No | No |

## Architecture

4 crates in a workspace:

- **core** → Download engine: probe, segment management, PID control, rate limiting, retry, bandwidth scheduling, connection pool, cookie store, cross-platform IPC (transport layer)
- **cli** → CLI frontend with progress bar, daemon auto-detection, checksum verification, config/schedule management, pipe modes, cookie/netrc auth, event hooks
- **daemon** → JSON-RPC server for background and scheduled downloads (Unix socket on Linux, TCP on Windows)
- **ext** → Utilities: checksum verification, filename extraction, aria2 session import, metalink parsing

## Design

- **Pwrite over mmap** → sequential streaming writes benefit from `pwrite`'s direct kernel path; mmap adds page-fault overhead for write-once workloads
- **reqwest over hyper** → HTTP/2 ALPN negotiation, HTTP/3 via quinn, connection pooling, and proxy support out of the box
- **Unix socket over TCP (Linux)** → no port conflicts, filesystem permissions control access, no network exposure
- **TCP fallback (Windows)** → daemon uses TCP on 127.0.0.1 with a random port stored in `%APPDATA%\zing\daemon.port`

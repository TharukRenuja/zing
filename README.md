# zing

Simple, modern, intelligent, cross-platform HTTP downloader with adaptive connection management, server probing, and concurrent segmented downloads.

```
zing https://example.com/file.zip
```

## Contents

- [How it works](#how-it-works)
- [Install](#install)
- [Uninstall](#uninstall)
- [Quick start](#quick-start)
- [Pipe mode](#pipe-mode)
- [Resume](#resume)
- [Cookies & Authentication](#cookies--authentication)
- [Daemon with systemd](#daemon-with-systemd)
- [Scheduled downloads](#scheduled-downloads)
- [Configuration](#configuration)
- [Features & Comparison](#features--comparison)
- [Architecture](#architecture)
- [Design](#design)

## How it works

```
zing https://example.com/file.zip

→ checks /tmp/zing.sock
  exists?  proxies to daemon, shows progress, exits
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

- Linux: `zing-<tag>-{arch}-linux.tar.gz` (contains `zing` + `zing-daemon`)
- macOS: `zing-<tag>-aarch64-mac.dmg`
- Windows: `zing-<tag>-{arch}-windows.exe`

#### Build from source
```bash
cargo build --release
./target/release/zing --help
```

## Uninstall

<details>
<summary>Uninstall Instructions</summary>

#### Using uninstall script
```bash
curl -fsSL https://raw.githubusercontent.com/TharukRenuja/zing/main/uninstall.sh | sh
# Or with zing's pipe mode:
zing -p=https://raw.githubusercontent.com/TharukRenuja/zing/main/uninstall.sh
```

#### Or manually
```bash
# Remove binaries
sudo rm /usr/local/bin/zing /usr/local/bin/zing-daemon

# Remove config and schedule files
rm -rf ~/.config/zing

# Remove daemon service (if installed)
zing daemon uninstall

# Remove socket and auth token
rm -f /tmp/zing.sock /tmp/zing.sock.auth
```

</details>

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

# Custom User-Agent
zing -A "MyApp/1.0" https://example.com/file.zip

# Content-Disposition: use server-provided filename
zing -C https://example.com/download

# Auto-rename if file exists (file(1).ext, file(2).ext, …)
zing --auto-file-renaming https://example.com/file.zip

# Dry-run: show URLs without downloading
zing --dry-run https://example.com/file.zip

# Use a Metalink file for mirrors + checksums
zing -M file.meta4
```

## Pipe mode

<details>
<summary>Direct piping, script execution, and app install</summary>

`-p` / `--pipe` outputs content to stdout (suppressing all logs), optionally auto-piping to a command.

```
# Raw pipe (same as before — pipe manually)
zing -p https://example.com/script.sh | sh

# Auto-pipe to interpreters
zing -p=sh     https://example.com/script.sh      # sh -s
zing -p=bash   https://example.com/script.sh      # bash -s
zing -p=run    https://example.com/script.sh      # sh -s (alias)
zing -p=python https://example.com/script.py      # python3
zing -p=node   https://example.com/script.js      # node

# Extract archives on the fly
zing -p=tar https://example.com/pkg.tar.gz         # tar -xzf -

# Install single binary
zing -p=app https://example.com/tool.AppImage
# → ~/.local/bin/tool.AppImage (chmod +x)

# Full install (archive extraction / AppImage / .sh installer)
zing -p=install https://example.com/tool.tar.gz
# → extracts, finds binary → ~/.local/bin/<name> (chmod +x)
zing -p=install https://example.com/tool.zip
zing -p=install https://example.com/tool.AppImage
zing -p=install https://example.com/installer.sh   # runs the installer
```

Use `-p` (no value) for raw output, `-p=<mode>` to auto-pipe.

</details>

## Resume

<details>
<summary>Control file resume + daemon resume command</summary>

### Standalone resume
zing saves state to a `.zing` control file on exit. Re-running the same URL resumes.

```
zing https://example.com/large-file.zip
  ^C  → saves state, exits
  zing https://example.com/large-file.zip  → resumes
```

### Daemon resume
Restart a paused download in the daemon:

```
zing resume <id>
```

</details>

## Cookies & Authentication

<details>
<summary>Cookie files, .netrc, event hooks, logging</summary>

### Cookie jars (Netscape format)
```
# Load cookies from file
zing -L cookies.txt https://example.com/file.zip

# Load cookies AND save updated ones after download
zing -L cookies.txt -s cookies.txt https://example.com/file.zip

# Save cookies only
zing -s cookies.txt https://example.com/file.zip
```

### .netrc authentication
```
zing -N https://example.com/private.zip
# Uses credentials from ~/.netrc matching the hostname
```

### Basic auth
```
zing -u user:pass https://example.com/private.zip
# Or with a bearer token:
zing -u "token:" https://example.com/api/download
```

### Event hooks
Run custom commands when downloads finish or fail:

```
zing --on-download-complete "notify-send 'Done: {}'" https://example.com/file.zip
zing --on-download-error  "echo 'Failed: {}' >> ~/failures.log" https://example.com/file.zip
```

`{}` is replaced with the file path.

### Logging
```
# Log to file instead of stderr
zing -l download.log https://example.com/file.zip
```

</details>

## Daemon with systemd

<details>
<summary>Background daemon with systemd integration</summary>

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

</details>

## Scheduled downloads

<details>
<summary>Cron-like time/day triggers</summary>

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

</details>

## Configuration

<details>
<summary>Config file, keys, and commands</summary>

```bash
# Interactive editor (guided prompts)
zing config edit

# List current config
zing config list

# Get a value
zing config get download_dir

# Set a value
zing config set download_dir "~/Downloads"

# Delete a config key
zing config delete download_dir
```

`zing config edit` opens an interactive wizard that shows all settings and lets you change them one by one.

Config file: `~/.config/zing/config.json`

```json
{
  "download_dir": "~/Downloads",
  "prompt_location": false
}
```

</details>

## Features & Comparison

### Features

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
- **Daemon mode** with live progress updates, session persistence, pause/resume via RPC
- **Scheduled downloads** with cron-like day/time triggers
- **Download resume** to continue interrupted downloads (control file + daemon RPC)
- **Concurrent multi-URL downloads** with `--max-concurrent`
- **Metalink (.meta4)** support for mirrors + checksums
- **Pipe mode** (`-p`) with raw output, script execution (sh/bash/python/node), archive extraction (tar), and app install
- **Cookie support** (`-L`/`-s`) for Netscape-format cookie files
- **.netrc auth** (`-N`) for automatic credential lookup
- **User-Agent** override (`-A`/`--user-agent`)
- **Content-Disposition** filename handling (`-C`)
- **Auto-file-renaming** (`--auto-file-renaming`) and **overwrite** control (`--allow-overwrite`)
- **Dry-run** (`--dry-run`) to preview downloads
- **Log to file** (`-l`/`--log`) instead of stderr
- **Event hooks** (`--on-download-complete`, `--on-download-error`) for custom post-download actions

### Comparison

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
| Daemon session persist | Save/restore on restart | Yes | No | No | No |
| Daemon resume RPC | `zing resume <id>` | No | No | No | No |
| Scheduled downloads | Day/time triggers | No | No | No | No |
| Resume | Control file + daemon RPC | .aria2 | Yes | Yes | Yes |
| Checksum verify | Post-download auto-detect | Metalink | No | No | No |
| Concurrent multi-URL | Yes (--max-concurrent) | Yes | No | No | No |
| Pipe mode | raw/sh/bash/python/node/tar/app/install | No | Yes | No | No |
| Cookie jar (Netscape) | Load + save | Yes | Yes | Yes | No |
| .netrc auth | Yes | No | Yes | Yes | No |
| Content-Disposition | Yes | Yes | No | Yes | No |
| Auto-file-renaming | Yes | No | No | No | No |
| Dry-run | Yes | No | Yes | Yes | No |
| Event hooks | on-complete / on-error | Yes | No | No | No |
| User-Agent override | Yes | Yes | Yes | Yes | Yes |

</details>

## Architecture

4 crates in a workspace:

- **core**: Download engine: probe, segment management, PID control, rate limiting, retry, bandwidth scheduling, connection pool, cookie store
- **cli**: CLI frontend with progress bar, daemon auto-detection, checksum verification, config/schedule management, pipe modes, cookie/netrc auth, event hooks
- **daemon**: Unix socket JSON-RPC server for background and scheduled downloads
- **ext**: Utilities: checksum verification, filename extraction, aria2 session import, metalink parsing

## Design

- **Pwrite over mmap**: Sequential streaming writes benefit from `pwrite`'s direct kernel path. Mmap adds page fault overhead for write-once workloads.
- **reqwest over hyper**: reqwest provides HTTP/2 ALPN negotiation, HTTP/3 via quinn, connection pooling, and proxy support out of the box.
- **Unix socket over TCP**: No port conflicts, filesystem permissions control access, no network exposure.
- **No BitTorrent / browser impersonation**: zing focuses on clean HTTP download intelligence.


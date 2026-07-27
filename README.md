# zing

Simple, modern, intelligent, cross-platform HTTP downloader with adaptive connection management, server probing, and concurrent segmented downloads.

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
  - [Update](#update)
  - [Uninstall](#uninstall)
      - [Using uninstall script](#using-uninstall-script)
      - [Or manually](#or-manually)
  - [Quick start](#quick-start)
  - [Pipe mode](#pipe-mode)
  - [Resume](#resume)
    - [Standalone resume](#standalone-resume)
    - [Daemon resume](#daemon-resume)
  - [Cookies \& Authentication](#cookies--authentication)
    - [Cookie jars (Netscape format)](#cookie-jars-netscape-format)
    - [.netrc authentication](#netrc-authentication)
    - [Basic auth](#basic-auth)
  - [Event hooks](#event-hooks)
  - [Logging](#logging)
  - [Daemon](#daemon)
    - [Start / Stop / Restart](#start--stop--restart)
    - [Manage tasks](#manage-tasks)
    - [Install systemd service (Unix only)](#install-systemd-service-unix-only)
  - [Scheduled downloads](#scheduled-downloads)
  - [Configuration](#configuration)
  - [Completions](#completions)
  - [Features \& Comparison](#features--comparison)
    - [Features](#features)
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

- Linux: `zing-<tag>-{arch}-linux.tar.gz`
- macOS: `zing-<tag>-{arch}-mac.dmg`
- Windows: `zing-<tag>-{arch}-windows-installer.exe` 

#### Build from source
```bash
cargo build --release
./target/release/zing --help
```

## Update

<details>
<summary>Check for updates & update instructions</summary>

```bash
# Check for updates and apply
zing update
```

`zing update` downloads the latest release for your platform (Linux, macOS, Windows; x86_64 or ARM), extracts it, and swaps the binary. If a `zing-daemon` binary is present in the same directory, it's updated too.

Update archives are named with a `-update` suffix on macOS and Windows (e.g. `zing-*-x86_64-mac-update.tar.gz`) to distinguish them from source archives. Linux archives use the plain name. zing automatically checks for updates every 7 days, configure with `update_check_interval_days` in config, or set to `0` to disable.

</details>

## Uninstall

<details>
<summary>Uninstall Instructions</summary>

#### Using uninstall script
```bash
zing -p https://raw.githubusercontent.com/TharukRenuja/zing/main/uninstall.sh | sh
```

#### Or manually
```bash
# Remove binaries
sudo rm /usr/local/bin/zing /usr/local/bin/zing-daemon     # Linux
# or: rm ~/.local/bin/zing ~/.local/bin/zing-daemon

# Remove config and schedule files
rm -rf ~/.config/zing

# Remove daemon service (if installed)
zing daemon uninstall

# Remove socket and auth token (Linux)
rm -f /tmp/zing.sock /tmp/zing.sock.auth

# Windows: use Add/Remove Programs for the NSIS installer,
# or manually delete the install directory and remove from PATH
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

# Download multiple files concurrently
zing --max-concurrent 3 url1 url2 url3

# Progress output: bar (default), json, or none
zing --progress json https://example.com/file.zip

# Log to file instead of stderr
zing -l download.log https://example.com/file.zip
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
<summary>Cookie files, .netrc, basic auth</summary>

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

</details>

## Event hooks

<details>
<summary>Run custom commands when downloads finish or fail</summary>

Run custom commands when downloads finish or fail:

```
zing --on-download-complete "notify-send 'Done: {}'" https://example.com/file.zip
zing --on-download-error  "echo 'Failed: {}' >> ~/failures.log" https://example.com/file.zip
```

`{}` is replaced with the file path.

</details>

## Logging

<details>
<summary>Log to file instead of stderr</summary>

```
# Log to file instead of stderr
zing -l download.log https://example.com/file.zip
```

</details>

## Daemon

<details>
<summary>Background daemon with task management</summary>

The daemon runs downloads in the background so they continue even after you close the terminal. Any `zing download` command automatically detects the daemon and proxies through it.

### Start / Stop / Restart

```bash
# Start the daemon in the foreground
zing daemon start

# Stop the daemon
zing daemon stop

# Restart the daemon
zing daemon restart

# Check daemon status (Unix only — systemd)
zing daemon status
```

### Manage tasks

```bash
# List all downloads
zing list

# Pause a download
zing pause <id>

# Resume a paused download
zing resume <id>

# Remove a download
zing remove <id>
```

### Install systemd service (Unix only)
```bash
zing daemon install
zing daemon uninstall
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

## Completions

<details>
<summary>Generate shell completions</summary>

```bash
# Generate shell completions
zing completions bash
zing completions zsh
zing completions fish
zing completions powershell
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
- **Update command** (`zing update`) to automatically upgrade to the latest release
- **Shell completions** (`zing completions <shell>`) for bash, zsh, fish, and powershell

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
| Daemon + RPC | Socket / TCP JSON-RPC | RPC | No | No | Web |
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

- **core**: Download engine: probe, segment management, PID control, rate limiting, retry, bandwidth scheduling, connection pool, cookie store, cross-platform IPC (transport layer)
- **cli**: CLI frontend with progress bar, daemon auto-detection, checksum verification, config/schedule management, pipe modes, cookie/netrc auth, event hooks
- **daemon**: JSON-RPC server for background and scheduled downloads (Unix socket on Linux, TCP on Windows)
- **ext**: Utilities: checksum verification, filename extraction, aria2 session import, metalink parsing

## Design

- **Pwrite over mmap**: Sequential streaming writes benefit from `pwrite`'s direct kernel path. Mmap adds page fault overhead for write-once workloads.
- **reqwest over hyper**: reqwest provides HTTP/2 ALPN negotiation, HTTP/3 via quinn, connection pooling, and proxy support out of the box.
- **Unix socket over TCP (Linux)**: No port conflicts, filesystem permissions control access, no network exposure.
- **TCP fallback (Windows)**: Windows daemon uses TCP on 127.0.0.1 with a random port, stored in `%APPDATA%\zing\daemon.port`.
- **No BitTorrent / browser impersonation**: zing focuses on clean HTTP download intelligence.


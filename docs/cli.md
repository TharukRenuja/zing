<!--
title: CLI Reference
section: Reference
order: 2
desc: Complete reference for all zing commands, flags, and options — download, daemon, tui, gui, schedule, config, completions.
keywords: zing, cli, command line, flags, options, download, daemon, tui, gui, schedule, config, completions
-->

# CLI Reference

## Synopsis

```
zing [FLAGS] [URLS...]
zing <COMMAND>
```

When no subcommand is given, `zing` downloads the provided URLs directly (standalone mode). If a daemon is running, it automatically proxies through the daemon instead.

## Commands

| Command | Aliases | Description |
|---------|---------|-------------|
| `zing download <urls>` | *(default)* | Download files |
| `zing tui <urls>` | | Launch the terminal UI |
| `zing daemon start` | `zing d start` | Start the background daemon |
| `zing daemon stop` | | Stop the daemon |
| `zing daemon restart` | | Restart the daemon |
| `zing daemon status` | | Show systemd status (Linux) |
| `zing daemon install` | | Install systemd user service |
| `zing daemon uninstall` | | Remove systemd user service |
| `zing list` | `zing ls`, `zing tasks` | List all daemon tasks |
| `zing pause <id>` | `zing p <id>` | Pause a download |
| `zing resume <id>` | `zing unpause <id>` | Resume a paused download |
| `zing remove <id>` | `zing rm <id>`, `zing delete <id>` | Remove a download |
| `zing schedule add` | `zing sched add`, `zing s add` | Add a scheduled download |
| `zing schedule list` | `zing sched ls` | List scheduled downloads |
| `zing schedule remove <id>` | `zing sched rm` | Remove a schedule |
| `zing config list` | `zing cfg ls` | List config keys |
| `zing config get <key>` | | Get a config value |
| `zing config set <key> <value>` | | Set a config value |
| `zing config delete <key>` | `zing cfg del` | Delete a config key |
| `zing config edit` | `zing cfg e` | Interactive config wizard |
| `zing completions <shell>` | | Generate shell completions (bash/zsh/fish/powershell) |
| `zing update` | | Self-update |
| `zing extension install` | `zing ext install` | Install browser native host manifests |
| `zing extension uninstall` | `zing ext uninstall` | Remove browser native host manifests |

## Flags

### General

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--quiet` | `-q` | off | Suppress all output |
| `--progress` | | `bar` | Progress mode: `bar`, `json`, or `none` |
| `--dry-run` | | off | Show what would be downloaded without downloading |
| `--standalone` | | off | Force in-process download, skip daemon |
| `--log` | `-l` | stderr | Write logs to a file |

### Output

| Flag | Short | Description |
|------|-------|-------------|
| `--output` | `-o` | Output filename |
| `--dir` | `-d` | Output directory |
| `--auto-file-renaming` | | Auto-rename if file exists (`file-1.ext`, `file-2.ext`, ...) |
| `--allow-overwrite` | | Overwrite existing files without prompting |
| `-C, --content-disposition` | | Use server-provided filename from Content-Disposition (on by default) |
| `--no-content-disposition` | | Ignore server-provided filename |

### Connection

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--connections` | `-n` | unlimited | Max parallel connections per download |
| `--max-concurrent` | | 3 | Max concurrent downloads (0 = unlimited) |
| `--connect-timeout` | | 30 | Connection timeout in seconds |
| `--max-time` | | 300 | Maximum total transfer time in seconds |
| `--proxy` | `-x` | | HTTP/HTTPS proxy URL |

### Rate limiting

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--max-download-rate` | `-r` | 0 (unlimited) | Max download rate (supports `500KB`, `2MB`, `1.5GB`) |
| `--max-filesize` | `-S` | 0 (unlimited) | Skip download if Content-Length exceeds this |
| `--bwlimit` | `-b` | | Bandwidth schedule (e.g. `"08:00,500KB 18:00,2MB"`) |

### Retry and resilience

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--retry` | | 5 | Max retry attempts per connection |
| `--retry-wait` | | 500 | Base retry wait in ms (doubles each attempt) |
| `--mirror` | `-m` | | Mirror URLs for failover (repeatable) |
| `--end-game` | | | Enable end-game mode (all connections race for last blocks) |
| `--no-end-game` | | | Disable end-game mode |
| `--throttle-reprobe` | | | Re-probe server when speed drops too low |
| `--no-throttle-reprobe` | | | Disable throttling re-probe |

### Authentication

| Flag | Short | Description |
|------|-------|-------------|
| `--user` | `-u` | HTTP basic auth `username:password` or `token` |
| `--digest` | | Use HTTP Digest auth (requires `--user`) |
| `-N, --netrc` | | Use `.netrc` for auth |
| `--cert` | | TLS client certificate (PEM) |
| `--cert-key` | | TLS private key (PEM) |
| `-L, --load-cookies` | | Load cookies from Netscape-format file |
| `-s, --save-cookies` | | Save cookies to file after download |

### HTTP

| Flag | Short | Description |
|------|-------|-------------|
| `--user-agent` | `-A` | Custom User-Agent header |
| `--header` | `-H` | Custom HTTP header (repeatable) |
| `-k, --insecure` | | Skip TLS verification |
| `-e, --referer` | | Referer header |
| `-X, --method` | | HTTP method |
| `-T, --upload-file` | | Upload file as request body |

### Input

| Flag | Short | Description |
|------|-------|-------------|
| `-i, --input-file` | | Read URLs from file (one per line) |
| `-M, --metalink` | | Use Metalink (.meta4) file for mirrors + checksums |
| `-c, --checksum` | | Verify checksum after download (auto-detects algorithm by length) |

### Pipe mode

| Flag | Short | Description |
|------|-------|-------------|
| `-p, --pipe` | | Pipe mode: `raw`, `sh`, `bash`, `python`, `node`, `tar`, `app`, `install` |

See [Pipe Mode](pipe-mode.md) for details.

### Hooks

| Flag | Description |
|------|-------------|
| `--on-download-complete` | Run command on success (`{}` = file path) |
| `--on-download-error` | Run command on failure (`{}` = file path) |

## URL input

`zing` accepts URLs as positional arguments or via `--input-file`:

```bash
# Positional
zing https://example.com/file.zip

# From file (one URL per line)
zing -i urls.txt

# Multiple URLs
zing https://example.com/a.zip https://example.com/b.zip
```

## Progress modes

| Mode | Output |
|------|--------|
| `bar` (default) | Progress bar with speed, ETA, downloaded/total |
| `json` | One JSON object per line with `id`, `filename`, `status`, `speed`, `downloaded`, `total`, `progress` |
| `none` | No progress output |

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | General error (download failed, bad args, etc.) |
| 2 | Partial success (some URLs failed in batch mode) |

## Examples

```bash
# Basic download
zing https://example.com/file.zip

# Save to specific path
zing -o ~/Downloads/myfile.zip https://example.com/file.zip

# Rate-limited download
zing -r 2MB https://example.com/file.zip

# Multiple files through daemon
zing --max-concurrent 5 url1 url2 url3 url4 url5

# Pipe a script directly to bash
zing -p=sh https://example.com/install.sh

# Verify checksum
zing -c d41d8cd98f00b204e9800998ecf8427e https://example.com/file.zip

# Use proxy
zing -x http://proxy:8080 https://example.com/file.zip

# Download with custom headers
zing -H "Authorization: Bearer token" https://api.example.com/data

# Dry-run
zing --dry-run https://example.com/file.zip
```

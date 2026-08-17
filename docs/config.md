<!--
title: Configuration
section: Reference
order: 3
desc: Config file location, all keys, zing config commands, daemon socket paths, and bandwidth schedule format.
keywords: zing, config, configuration, settings, config.json, download_dir, max_concurrent, bandwidth schedule
-->

# Configuration

## Config file location

| Platform | Path |
|----------|------|
| Linux | `~/.config/zing/config.json` |
| macOS | `~/Library/Application Support/zing/config.json` |
| Windows | `%APPDATA%\zing\config.json` |

## Config keys

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `download_dir` | string | `~/Downloads` | Default download directory. Supports `~`. |
| `prompt_location` | bool | `false` | Ask for download location before each download |
| `update_check_interval_days` | u64 | `7` | Days between automatic update checks (`0` = disabled) |
| `max_concurrent_downloads` | usize | `3` | Max parallel downloads in the daemon (`0` = unlimited) |
| `end_game` | bool | `true` | Default end-game mode for new downloads |
| `throttle_reprobe` | bool | `true` | Default throttle re-probe for new downloads |

## Config commands

```bash
# Interactive wizard
zing config edit

# List all keys
zing config list

# Get a specific key
zing config get download_dir

# Set a key
zing config set download_dir ~/Videos
zing config set max_concurrent_downloads 8
zing config set end_game false

# Delete a key (reverts to default)
zing config delete download_dir
```

## Example config

```json
{
  "download_dir": "~/Videos",
  "prompt_location": false,
  "update_check_interval_days": 7,
  "max_concurrent_downloads": 5,
  "end_game": true,
  "throttle_reprobe": true
}
```

## How config is used

1. **CLI downloads**: `download_dir` is used as the output directory when `--dir` is not specified. `end_game` and `throttle_reprobe` are applied when their CLI flags are not explicitly set.

2. **Daemon**: `max_concurrent_downloads` controls how many downloads can run simultaneously. The daemon reads this at startup and when `zing config set` is called.

3. **TUI/GUI**: Read `download_dir` as the default save location.

## Daemon socket and auth

The daemon stores its runtime state in platform-specific locations:

| Item | Linux | Windows |
|------|-------|---------|
| Socket | `$XDG_RUNTIME_DIR/zing.sock` or `/tmp/zing.sock` | TCP on `127.0.0.1` with random port |
| Auth token | `<socket>.auth` | `%PROGRAMDATA%\zing\auth.token` |
| Session | `~/.config/zing/session.json` | `%APPDATA%\zing\session.json` |
| Schedule | `~/.config/zing/schedule.json` | `%APPDATA%\zing\schedule.json` |

The auth token is a 32-byte random hex string generated on each daemon start. It is never logged or exposed to the user.

## Bandwidth schedule format

Used with `--bwlimit` / `-b`:

```
"HH:MM,rate HH:MM,rate ..."
```

Examples:
- `"08:00,500KB 18:00,2MB"` — 500 KB/s during work hours, 2 MB/s in the evening
- `"00:00,1MB 07:00,500KB 22:00,5MB"` — 1 MB at night, 500 KB in morning, 5 MB at night

Rate values support: `B`, `KB`, `MB`, `GB`, `TB` (case-insensitive, decimal allowed).

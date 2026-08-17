<!--
title: Daemon
section: Guides
order: 5
desc: Background downloads with zing-daemon — JSON-RPC server, task lifecycle, session persistence, scheduled downloads, concurrency control, systemd, and event hooks.
keywords: zing, daemon, background, rpc, json-rpc, unix socket, task manager, scheduler, systemd, session, event hooks
-->

# Daemon

The `zing-daemon` background process handles downloads that continue after the terminal closes. Any `zing download` command automatically detects the daemon and proxies through it.

## Quick start

```bash
zing daemon start      # start in foreground
zing daemon stop       # stop
zing daemon restart    # restart
zing daemon status     # show systemd status (Linux)

# Or as a systemd user service
zing daemon install
zing daemon uninstall
```

## How it works

```
CLI (zing)                    Daemon (zing-daemon)
    │                               │
    ├── zing.addUri ──────────────► │ spawns download worker
    ├── zing.list ────────────────► │ returns all tasks
    ├── zing.tellStatus {id} ────► │ returns task snapshot
    ├── zing.pause {id} ─────────► │ pauses the download
    ├── zing.resume {id} ────────► │ resumes the download
    ├── zing.stop {id} ──────────► │ stops the download
    ├── zing.remove {id} ────────► │ removes + cleans up files
    ├── zing.setMaxConcurrent ───► │ adjusts concurrency limit
    ├── zing.subscribe ──────────► │ streams events (long-lived)
    └── zing.shutdown ───────────► │ graceful shutdown
```

## Authentication

Each daemon start generates a random 32-byte hex token, written to a file next to the socket (or `%PROGRAMDATA%\zing\auth.token` on Windows). The CLI reads this file automatically — no manual pairing required.

The token is:
- Generated once per daemon lifetime
- Stored with `0o600` permissions (owner-only read/write)
- Never logged or displayed to the user
- Rotated on each daemon restart

## Task lifecycle

```
   addUri
      │
      ▼
   Pending ──(worker starts)──► Downloading
                                  │    ▲
                   pause ─────────┘    │ resume
                                  │    │
                                  ▼    │
                               Paused ─┘
                                  │
                   stop ─────────►│
                   error ────────►│
                                  ▼
                          Completed / Failed / Stopped
                                  │
                       remove ────┘
                                  ▼
                              (deleted)
```

## Session persistence

The daemon saves active tasks to `~/.config/zing/session.json`. On restart, tasks are restored:

- **Downloading/Pending** tasks: respawned automatically
- **Paused** tasks: stay paused, resumed on `zing resume <id>`
- **Completed/Failed/Stopped** tasks: not restored

The session file is updated on every task state change.

## Scheduled downloads

```bash
# Add a schedule
zing schedule add https://example.com/file.zip --at 02:00
zing schedule add https://example.com/daily.zip --at 00:00 --end 07:00 --days Mon,Wed,Fri

# List schedules
zing schedule list

# Remove a schedule
zing schedule remove <id>
```

Schedules are stored in `~/.config/zing/schedule.json`. The scheduler checks every 30 seconds and triggers downloads when the current time matches. Each schedule entry can specify:

| Field | Description |
|-------|-------------|
| `url` | URL to download |
| `at` | Start time (`HH:MM`) |
| `end` | End time window (`HH:MM`, optional) |
| `days` | Day filter (e.g. `Mon,Wed,Fri`, default: every day) |
| `output` | Output filename |
| `output_dir` | Output directory |
| `connections` | Max connections (default: 4) |
| `insecure` | Skip TLS verification |
| `max_download_rate` | Rate limit |
| `proxy` | Proxy URL |
| `headers` | Custom headers |
| `checksum` | Verify checksum |
| `mirrors` | Mirror URLs |
| `max_filesize` | Max file size |

## Concurrency control

The daemon uses a `tokio::sync::Semaphore` to limit concurrent downloads. Default: 3 (from `max_concurrent_downloads` config key). Change at runtime:

```bash
zing config set max_concurrent_downloads 8
```

Or via RPC: `zing.setMaxConcurrent { "max": 8 }`.

## RPC protocol

Communication uses newline-delimited JSON over Unix sockets (Linux) or TCP (Windows).

### Request format

```json
{
  "id": 1,
  "method": "zing.addUri",
  "params": { "url": "https://..." },
  "token": "abc123..."
}
```

### Response format

```json
{
  "id": 1,
  "result": { "id": 42 },
  "error": null
}
```

### Error response

```json
{
  "id": 1,
  "result": null,
  "error": { "code": -32000, "message": "task not found" }
}
```

### Methods

| Method | Params | Returns |
|--------|--------|---------|
| `zing.addUri` | `{ url, filename?, dir?, connections?, ... }` | `{ id }` |
| `zing.list` | — | `{ tasks: [...] }` |
| `zing.tellStatus` | `{ id }` | task snapshot |
| `zing.pause` | `{ id }` | `{ id, status: "paused" }` |
| `zing.resume` | `{ id }` | `{ id, status: "resumed" }` |
| `zing.stop` | `{ id }` | `{ ok: true }` |
| `zing.remove` | `{ id }` | `{ ok: true }` |
| `zing.setMaxConcurrent` | `{ max }` | `{ ok: true }` |
| `zing.version` | — | version string |
| `zing.shutdown` | — | `{ status: "shutting_down" }` |
| `zing.subscribe` | — | event stream (long-lived) |

### Event stream

`zing.subscribe` opens a long-lived connection. Each event is a JSON line:

```json
{"TaskCreated": {"id": 1, "url": "https://..."}}
{"TaskProgress": {"id": 1, "bytes_downloaded": 1048576, "total_bytes": 10485760, "speed_bytes_per_sec": 524288}}
{"TaskCompleted": {"id": 1, "total_bytes": 10485760, "duration": "12s"}}
```

Events include: `TaskCreated`, `TaskProgress`, `TaskCompleted`, `TaskFailed`, `Paused`, `ConnectionCreated`.

## Platform differences

| Feature | Linux/macOS | Windows |
|---------|-------------|---------|
| Transport | Unix domain socket | TCP on `127.0.0.1` |
| Socket path | `$XDG_RUNTIME_DIR/zing.sock` | `%PROGRAMDATA%\zing\daemon.port` |
| Auth token | `<socket>.auth` | `%PROGRAMDATA%\zing\auth.token` |
| Service | systemd user service | Windows service |

## Event hooks

The daemon runs hooks on download completion/failure:

```bash
# In session.json or via addUri params:
{
  "on_download_complete": "notify-send 'Done: {}'",
  "on_download_error": "echo 'Failed: {}' >> ~/failures.log"
}
```

`{}` is replaced with the file path.

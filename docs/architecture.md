<!--
title: Architecture
section: Internals
order: 10
desc: Workspace structure, crate responsibilities, dependency graph, transport layer, data flow, and key design decisions.
keywords: zing, architecture, workspace, crates, design, transport, ipc, unix socket, tcp, event bus, adaptive
-->

# Architecture

## Workspace structure

```
zing/
├── core/           zing-core: download engine, transport, RPC
├── cli/            zing: CLI frontend (default member)
├── tui/           zing-tui: terminal UI
├── daemon/         zing-daemon: background process
└── docs/           documentation
```

## Dependency graph

```
zing (cli)
├── zing-core
└── zing-tui (optional, feature="tui")

zing-daemon
└── zing-core

zing-tui
└── zing-core

zing-core
└── (standalone, no internal deps)
```

## Crate responsibilities

### zing-core

The foundation. Everything related to actually downloading files:

| Module | Purpose |
|--------|---------|
| `downloader` | Main download orchestrator: probing, segments, connections, end-game, resume |
| `engine` | Event system (`EventBus`, `EngineEvent`) |
| `connection` | HTTP connection pool (`reqwest` wrapper), Happy Eyeballs DNS |
| `segment` | Segment allocation, work stealer, dynamic sizing |
| `storage` | Binary control file (`.zing`) and block bitfield |
| `transport` | IPC transport: Unix socket (Linux) / TCP (Windows) |
| `rpc` | JSON-RPC client for daemon communication |
| `ratelimit` | Lock-free token-bucket rate limiter |
| `bwschedule` | Time-of-day bandwidth scheduling |
| `retry` | Exponential backoff with jitter |
| `cookie_store` | Netscape cookie file reader/writer |
| `probe` | Server probing (protocol, size, bandwidth, RTT) |
| `util` | Cross-platform `pwrite`/`pread`/`fallocate` |
| `constants` | Tuning constants |

### zing (cli)

The user-facing CLI. Handles:

- Argument parsing (clap derive)
- Progress bar rendering (indicatif)
- Daemon auto-detection and proxy
- Config management
- Shell completions
- Pipe mode (stdout streaming)
- Event hooks (`--on-download-complete`, `--on-download-error`)
- Cookie/netrc/TLS auth
- Self-update

### zing-tui

Terminal UI built with ratatui. Features:

- Task table with status, progress, speed
- Per-connection detail view
- Block map visualization
- Log panel
- Add URL input
- Daemon mode (RPC polling) and standalone mode (in-process)

### zing-daemon

Background download server. Features:

- JSON-RPC over Unix socket / TCP
- Auth token (random per start, never logged)
- Task manager (add/pause/resume/stop/remove)
- Session persistence (survives restart)
- Scheduled downloads (cron-like)
- Concurrency control (semaphore)
- Event streaming (`zing.subscribe`)
- Systemd service support

#### Utility modules

These live in `zing-core` rather than a separate crate: the engine already used
all of them, and a frontend needed the second manifest just to format a byte
count.

| Module | Purpose |
|--------|---------|
| `checksum` | MD5/SHA-1/SHA-256/SHA-512 file hashing |
| `filename` | URL-to-filename extraction, Content-Disposition parsing |
| `metalink` | .meta4 XML parser (mirrors, checksums, chunk hashes) |
| `bandwidth` | Human-readable bandwidth string parser (`"2MB"` → bytes) |
| `human` | Human-readable byte/speed formatting (presentation only) |
| `digest_auth` | HTTP Digest auth (RFC 2617) |

## Transport layer

The IPC transport (`core/src/transport.rs`) abstracts platform differences:

| | Linux/macOS | Windows |
|--|-------------|---------|
| Socket | Unix domain socket | TCP on `127.0.0.1` |
| Address | `$XDG_RUNTIME_DIR/zing.sock` | Port stored in `%PROGRAMDATA%\zing\daemon.port` |
| Auth file | `<socket>.auth` | `%PROGRAMDATA%\zing\auth.token` |

The transport provides `bind()`, `connect()`, and `auth_file()` functions. Both the CLI (as client) and daemon (as server) use the same transport code.

## Data flow

### CLI → Daemon (proxy mode)

```
zing download URL
  │
  ├─ detect daemon (try connect to socket)
  │   ├─ running → send addUri, poll tellStatus, render progress
  │   └─ not running → download directly (standalone)
  │
  └─ (standalone mode always downloads directly)
```

### Daemon internal

```
addUri
  │
  ├─ insert TaskInfo into HashMap
  ├─ save session to disk
  ├─ spawn worker (tokio task)
  │   │
  │   ├─ wait for semaphore permit
  │   ├─ create DownloadTask (from core)
  │   ├─ run task.run_with_shutdown()
  │   │   ├─ probe server
  │   │   ├─ allocate segments
  │   │   ├─ spawn connections
  │   │   ├─ monitor loop (work stealing, end-game)
  │   │   └─ save control file periodically
  │   │
  │   └─ on completion: verify checksum, run hooks, cleanup
  │
  └─ (task is now manageable via pause/resume/stop/remove)
```

### TUI → Daemon

```
zing tui URL
  │
  ├─ detect daemon → RemoteTask (implements TaskControl)
  │   ├─ snapshot() → tellStatus RPC (every 250ms)
  │   ├─ pause() → pause RPC
  │   ├─ resume() → resume RPC
  │   ├─ stop() → stop RPC
  │   └─ remove() → remove RPC
  │
  └─ (TUI renders TaskControl snapshots identically whether local or remote)
```

## Key design decisions

### Pwrite over mmap

Sequential streaming writes benefit from `pwrite`'s direct kernel path. mmap adds page-fault overhead for write-once workloads.

### reqwest over hyper

HTTP/2 ALPN negotiation, HTTP/3 via quinn, connection pooling, and proxy support out of the box.

### Unix socket over TCP (Linux)

No port conflicts, filesystem permissions control access, no network exposure.

### Token bucket rate limiter

Lock-free implementation using atomics. Connections call `consume()` which async-blocks until tokens are available. The bandwidth scheduler can change the rate at runtime via `set_rate()`.

### Adaptive connection count

Files < 200 MiB use a single connection with no overhead. Larger files start with one connection, measure real speed for 2-3 seconds, then calculate the optimal count from `probe_bandwidth / measured_speed`. This avoids both under-utilization (too few connections) and server overload (too many connections) while eliminating the latency of slow-start batch delays.

Work stealing still redistributes segments from slow to fast connections using a dynamic minimum segment size derived from the optimal connection count.

### Event bus (broadcast channel)

All engine events flow through a single `EventBus` (tokio broadcast, capacity 256). The daemon subscribes and streams events to RPC clients. The TUI subscribes for log capture.

### Shared TaskControl trait

Both `LocalTask` (in-process) and `RemoteTask` (daemon RPC) implement `TaskControl`. The TUI renders them identically — it doesn't know or care whether the download is local or remote.

<!--
title: Documentation
section: Overview
order: 0
desc: Index of all zing documentation — guides, references, and architecture docs.
keywords: zing, docs, documentation, index, reference
-->

# zing Documentation

## User Guides

| Doc | What it covers |
|-----|----------------|
| [Installation](installation.md) | Install from source, pre-built binaries, install script, systemd service |
| [CLI Reference](cli.md) | All commands, flags, URL/file input, progress modes |
| [Configuration](config.md) | Config file, all keys, `zing config` commands |
| [Download Engine](download-engine.md) | Segmented downloads, probing, adaptive connections, end-game, retry, rate limiting, bandwidth scheduling, Metalink, resume |
| [Daemon](daemon.md) | Background downloads, JSON-RPC, task management, scheduled downloads, systemd, session persistence |
| [Terminal UI (TUI)](tui.md) | Layout, keybindings, task table, per-connection view, block map, logs panel |
| [Desktop GUI](gui.md) | eframe/egui interface, IDM-style layout, sidebar filters, speed plot, block grid |
| [Pipe Mode](pipe-mode.md) | Direct piping, script execution, tar extraction, app install |
| [Browser Extension](browser-extension.md) | Native Messaging protocol, message schema, install/uninstall, manifest setup |
| [Architecture](architecture.md) | 5-crate workspace, transport layer, IPC, design rationale |

## Architecture at a glance

```
zing cli ──┬── standalone (in-process download)
            └── daemon mode (RPC over Unix socket / TCP)
                  ├── zing-daemon (background process)
                  ├── zing tui (terminal UI)
                  ├── zing-gui (desktop GUI, eframe)
                  └── zing nm (Native Messaging host for browser extension)
```

## Crate map

| Crate | Path | Role |
|-------|------|------|
| `zing-core` | `core/` | Download engine, probe, segments, adaptive connections, rate limit, storage, transport, RPC client |
| `zing` (cli) | `cli/` | CLI frontend, progress bar, config, event hooks, pipe modes |
| `zing-tui` | `tui/` | Terminal UI: ratatui rendering, task table, per-connection view |
| `zing-gui` | `gui/` | Desktop GUI: eframe/egui, IDM layout, live polling |
| `zing-daemon` | `daemon/` | Background daemon: RPC server, task manager, scheduler |
| `zing-ext` | `ext/` | Utilities: checksum, filename, metalink, bandwidth, digest auth, aria2 import |

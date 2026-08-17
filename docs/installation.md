<!--
title: Installation
section: Getting Started
order: 1
desc: Install zing from source, pre-built binaries, or the install script. Set up systemd service on Linux.
keywords: zing, install, build, cargo, binary, systemd, setup, download, linux, macos, windows
-->

# Installation

## Install script (Linux/macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/TharukRenuja/zing/main/install.sh | sh
```

Installs `zing` and `zing-daemon` to `/usr/local/bin`. Optionally sets up `zing-daemon` as a systemd service.

## Pre-built binaries

Download from [Releases](https://github.com/TharukRenuja/zing/releases/latest):

| Platform | Archive |
|----------|---------|
| Linux (x86_64) | `zing-latest-x86_64-linux.tar.gz` |
| Linux (aarch64) | `zing-latest-aarch64-linux.tar.gz` |
| macOS (Intel) | `zing-latest-x86_64-mac.dmg` |
| macOS (Apple Silicon) | `zing-latest-aarch64-mac.dmg` |
| Windows | `zing-latest-windows.msi` |

Extract the archive and place the binaries somewhere on your `PATH`.

## Build from source

Requires Rust 1.75+.

```bash
git clone https://github.com/TharukRenuja/zing.git
cd zing
cargo build --release
./target/release/zing --help
```

### Build features

The CLI has one optional feature, on by default:

| Feature | Default | What it enables |
|---------|---------|-----------------|
| `tui` | yes | `zing tui` terminal UI |

The desktop GUI is a separate binary. Build it from the workspace root:

```bash
cargo build --release --bin zing-gui
```

`cargo build --release` builds both `zing` and `zing-gui`.

Build the CLI without TUI:

```bash
cargo build --release --bin zing --no-default-features
```

### Workspace members

```
zing/
├── core/       # zing-core: download engine
├── cli/        # zing: CLI frontend
├── tui/        # zing-tui: terminal UI
├── gui/        # zing-gui: desktop GUI
├── daemon/     # zing-daemon: background daemon
└── ext/        # zing-ext: utilities
```

## Systemd service (Linux)

After installing the binaries:

```bash
zing daemon install    # install + start the systemd user service
zing daemon status     # check status
zing daemon uninstall  # remove the service
```

The service runs `zing-daemon` as a user service (`~/.config/systemd/user/zing-daemon.service`).

## Verify installation

```bash
zing --version
zing daemon start      # start the background daemon
zing list              # should return "No downloads."
zing daemon stop
```

## Browser extension

Install the native messaging host so the [zing Interceptor](https://github.com/TharukRenuja/zing-interceptor) browser extension can talk to zing:

```bash
zing extension install    # writes manifests for Chrome, Edge, and Firefox
zing extension uninstall  # removes them
```

Then install the extension in your browser — see the [Browser Extension](browser-extension.md) docs for details.

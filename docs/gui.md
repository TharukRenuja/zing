<!--
title: Desktop GUI
section: Guides
order: 7
desc: Native desktop GUI for zing — eframe/egui IDM-style layout with toolbar, sidebar filters, task table, speed plot, and block grid.
keywords: zing, gui, desktop, eframe, egui, idm, interface, speed plot, block grid, native
-->

# Desktop GUI

`zing-gui` is a standalone native desktop window for managing downloads. Built with [eframe](https://github.com/emilk/egui/tree/main/crates/eframe) / [egui](https://github.com/emilk/egui). It talks to the daemon over RPC and does not depend on the CLI.

## Launch

```bash
zing-gui
```

The GUI auto-starts the daemon if it's not running (waits up to 10 seconds). Build it from the workspace root with `cargo build --release`.

## Layout

```
┌─────────────────────────────────────────────────────────────┐
│ [+ Add URL]  [▶ Resume]  [⏸ Pause]  [■ Stop]  [🗑 Remove] │  toolbar
│                                            15.3 MB/s  v0.2.4│
├────────────┬────────────────────────────────────────────────┤
│            │  Name          Size     Speed   Status Progress │
│  All (3)   │  file.zip      1.2 GB   15MB/s  ███░░  72%   │
│  Download  │  archive.zip   850 MB   —       done   100%  │
│  Paused    │  data.bin      2.1 GB   —       queued  —     │
│  Queued    │                                                │
│  Completed │                                                │
│  Failed    │                                                │
│  Stopped   │                                                │
├────────────┴────────────────────────────────────────────────┤
│ ┌──────────────┬───────────────────┬───────────────────────┐│
│ │ Info         │ Speed             │ Blocks                ││
│ │ File: file.. │    ╱╲  ╱╲        │ ████████████░░░░░░░░░ ││
│ │ URL: https.. │   ╱  ╲╱  ╲╱╲    │                       ││
│ │ Size: 1.2 GB │  ╱        ╲  ╲  │                       ││
│ │ Speed: 15MB/s│ ╱          ╲  ╲ │                       ││
│ │ Peak: 22MB/s │                  │                       ││
│ └──────────────┴───────────────────┴───────────────────────┘│
└─────────────────────────────────────────────────────────────┘
```

### Panels

**Toolbar** (top):
- `+ Add URL` — opens URL input dialog
- `▶ Resume` / `⏸ Pause` / `■ Stop` / `🗑 Remove` — act on selected task
- Aggregate speed display
- Version label

**Sidebar** (left, 190px):
- Category filters with counts: All, Downloading, Paused, Queued, Completed, Failed, Stopped
- Click to filter the task table

**Task table** (center):
- Columns: Name, Size, Speed, Status, Progress
- Click to select a task
- Striped rows, resizable columns

**Detail panel** (bottom, 230px, resizable):
- **Info column**: filename, URL, size, downloaded, speed, peak speed, connections
- **Speed plot**: rolling history graph (egui_plot, last 120 samples)
- **Block grid**: visual grid of completed (green) vs pending (dark) blocks

### Dialogs

**Add URL dialog**: centered overlay with URL text input. Press Enter or click Add to submit. Click Cancel to close.

**Error dialog**: centered overlay showing error messages. Click OK to dismiss.

## Features

- Standalone binary: no dependency on the CLI, connects to the daemon via RPC
- Auto-starts daemon if not running (waits up to 10 seconds)
- Live task updates via background polling (every 500ms)
- Speed plot with rolling 60-second history
- Block grid visualization
- Dark theme with custom styling

## Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| eframe | 0.29 | Application framework |
| egui | 0.29 | Immediate-mode GUI |
| egui_extras | 0.29 | Table widget |
| egui_plot | 0.29 | Speed plot |
| zing-core | — | Daemon RPC client |

## Architecture

```
GUI thread (egui)          Background thread
    │                           │
    ├── read snapshot ──────►   │ polls daemon every 500ms
    │   (Arc<Mutex<Vec>>)       │ via list_tasks RPC
    │                           │
    ├── add_uri ───────────────►│ send_request("zing.addUri")
    ├── pause(id) ─────────────►│ send_request("zing.pause")
    ├── resume(id) ────────────►│ send_request("zing.resume")
    ├── stop(id) ──────────────►│ send_request("zing.stop")
    └── remove(id) ────────────►│ send_request("zing.remove")
```

The GUI uses a dedicated tokio runtime (via `GuiClient`) separate from the egui rendering thread. All RPC calls are synchronous on this runtime.

## Limitations

- No in-progress connection detail view (unlike TUI)
- Speed plot samples from aggregate speed, not per-connection
- Block grid shows fraction completed, not per-block bitfield
- No log panel
- No dark/light theme toggle

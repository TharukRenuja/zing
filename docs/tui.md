<!--
title: Terminal UI (TUI)
section: Guides
order: 6
desc: Interactive terminal UI for managing downloads — layout, keybindings, task table, per-connection view, block map, logs panel, and daemon integration.
keywords: zing, tui, terminal, interface, ratatui, keybindings, task table, progress, block map, logs
-->

# Terminal UI (TUI)

`zing tui` launches a full-screen terminal interface for managing multiple concurrent downloads.

## Launch

```bash
zing tui https://example.com/file1.zip https://example.com/file2.zip

# With download options
zing tui -d downloads/ -n 8 -r 2MB https://example.com/file.zip

# Standalone (skip daemon)
zing tui --standalone https://example.com/file.zip
```

The TUI accepts all the same download flags as `zing download` (see [CLI Reference](cli.md)).

## Layout

```
┌─────────────────────────────────────────────────────────┐
│  ZING │ 3 tasks │ 1.2 GB downloaded │ 15.3 MB/s        │  title bar
├─────────────────────────────────────────────────────────┤
│  ▶ file.zip            ████████████░░░░░  72.3%         │  header (selected task)
│    850 MB / 1.2 GB     15.3 MB/s    ETA 25s            │
├──────────────────────┬──────────────────────────────────┤
│  Downloaded  850 MB  │  Block Map  ████████░░░  78%    │
│  Total       1.2 GB  │  In-flight: 3     Endgame: off  │
│  Speed   15.3 MB/s   │  Speed: 15.3 MB/s  ETA: 25s    │
│  Peak    22.1 MB/s   │                                  │
│  ETA        25s      │                                  │
│  Blocks     78/100   │                                  │
├──────────────────────┼──────────────────────────────────┤
│  Connections          │  Tasks                          │
│  #  ADDR     SPEED   │  #  FILE         STATUS  SPEED  │
│  0  1.2.3.4  5.1MB/s │  0  file.zip     done    —      │
│  1  1.2.3.4  4.8MB/s │  1  archive.zip  72%    15MB/s  │
│  2  1.2.3.4  5.4MB/s │  2  data.bin     queued  —      │
├──────────────────────┴──────────────────────────────────┤
│  Logs                                                   │
│  INFO  Probed server: HTTP/2, 1.2 GB, range support     │
│  INFO  Segmented download: 4 connections, end-game on   │
├─────────────────────────────────────────────────────────┤
│  q quit  j/k select  p pause  P pause all  x stop       │  footer
└─────────────────────────────────────────────────────────┘
```

### Layout modes

The TUI adapts to terminal width:

- **Columns mode** (≥ 120 cols): left panel (stats + connections), right panel (block map + tasks)
- **Stacked mode** (< 120 cols): everything stacked vertically

### Minimum size

The TUI requires at least 40×20 characters. If the terminal is smaller, a "Terminal too small" message is shown.

## Keybindings

| Key | Action |
|-----|--------|
| `q` | Quit |
| `Esc` | Quit |
| `Ctrl+C` | Quit |
| `j` / `↓` | Select next task |
| `k` / `↑` | Select previous task |
| `p` / `Space` | Pause / resume selected task |
| `P` | Pause / resume all tasks |
| `x` / `s` | Stop selected task |
| `r` | Remove selected task |
| `a` | Add a URL (opens input prompt) |

### Add URL mode

Press `a` to enter URL input mode. Type a URL and press Enter to add it. Press Esc to cancel.

## Panels

### Title bar

Shows the "ZING" logo, total task count, total bytes downloaded, and aggregate speed.

### Header

Displays the selected task's filename, status, progress bar, downloaded/total bytes, speed, and ETA.

### Stats

Two columns of statistics for the selected task:

| Stat | Description |
|------|-------------|
| Downloaded | Bytes received so far |
| Total | File size (if known) |
| Speed | Current download speed |
| Peak | Highest speed observed |
| ETA | Estimated time remaining |
| Blocks | Completed / total blocks |
| In-flight | Number of active connections |
| Endgame | Whether end-game mode is active |

### Connections

Table showing each active connection:

| Column | Description |
|--------|-------------|
| # | Connection ID |
| ADDR | Remote IP address |
| SPEED | Current speed |
| BYTES | Bytes downloaded |
| TIME | Time since connection started |
| STATE | Current state (downloading, retrying, etc.) |

### Block map

Visual gauge showing block completion percentage, in-flight count, end-game status, speed, and ETA.

### Task table

List of all downloads:

| Column | Description |
|--------|-------------|
| # | Task ID |
| FILE | Filename |
| STATUS | queued / downloading / paused / done / failed / stopped |
| PROGRESS | Progress bar with percentage |
| SPEED | Current speed (or `—` if not active) |
| SIZE | File size (or `—` if unknown) |

The table auto-scrolls to keep the selected row visible.

### Logs panel

Recent log lines from the download engine, styled by level:
- **ERROR**: red, bold
- **WARN**: yellow
- **INFO**: default
- **DEBUG/TRACE**: dark gray

ANSI escape codes and timestamps are stripped for readability.

## Daemon integration

When a daemon is running, the TUI operates in daemon mode:
- Tasks are managed via RPC (`tellStatus` polling every 250ms)
- Pause/resume/stop/remove are sent via RPC
- New URLs are added via `addUri`
- A `TaskFactory` closure handles interactive URL addition

When no daemon is running (or `--standalone` is set), tasks run in-process with full control.

## Auto-exit

When all tasks reach a terminal state (completed, failed, or stopped), the TUI waits 30 frames (~1.5 seconds) then exits automatically. This prevents the TUI from hanging after a batch download completes.

## Logs capture

The TUI captures all `tracing` log output in a ring buffer (2000 lines max). This includes:
- Server probe results
- Connection events
- Segment allocation
- Retry attempts
- Error messages

The log panel shows the most recent lines, updating in real time.

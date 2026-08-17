<!--
title: Pipe Mode
section: Guides
order: 8
desc: Stream downloads to stdout — pipe to scripts, extract archives, install binaries, and pipe to any command.
keywords: zing, pipe, stdout, streaming, bash, python, tar, app install, script, exec
-->

# Pipe Mode

Pipe mode (`-p` / `--pipe`) streams download content to stdout, optionally piping it to a command. All log output is suppressed in pipe mode.

## Synopsis

```
zing -p [MODE] URL
```

When `MODE` is omitted, raw bytes are written to stdout.

## Modes

### Raw pipe

```bash
zing -p https://example.com/file.zip > file.zip
zing -p https://example.com/file.zip | sha256sum
```

Downloads the file and writes raw bytes to stdout. No progress bar, no logs.

### Script execution

```bash
zing -p=sh     https://example.com/install.sh       # sh -s
zing -p=bash   https://example.com/install.sh       # bash -s
zing -p=python https://example.com/script.py        # python3
zing -p=node   https://example.com/script.js        # node
```

Downloads the script and pipes it directly to the interpreter via stdin.

### Archive extraction

```bash
zing -p=tar https://example.com/archive.tar.gz | tar -xzf -
```

Downloads and pipes to `tar -xzf -`. Works with any archive format that tar supports.

### App install

```bash
zing -p=app https://example.com/tool.AppImage
```

Downloads the file to `~/.local/bin/<filename>` and makes it executable. No extraction — just places the binary.

### Generic install

```bash
zing -p=install https://example.com/tool.tar.gz
```

Downloads and extracts the archive to `~/.local/bin/`. Expects the archive to contain a single executable at the top level.

## How it works

1. The download runs in streaming mode (single connection, no segments)
2. Each chunk of data is written to stdout as it arrives
3. The `-p` flag sets `to_stdout = true` on the download task
4. All tracing/log output is suppressed
5. The progress bar is replaced with a byte counter (if `--progress bar`)

## Examples

```bash
# Pipe a remote script to bash
zing -p=sh https://get.example.com/install.sh

# Download and verify hash
zing -p https://example.com/file.zip | sha256sum

# Stream a video to mpv
zing -p https://example.com/video.mp4 | mpv -

# Extract a tarball
zing -p=tar https://example.com/data.tar.xz | tar -xJf -

# Install a binary
zing -p=app https://example.com/zing-linux-amd64
```

## Limitations

- Only works with single-file downloads (not batch)
- No resume support (streaming mode)
- No progress display in raw mode (bytes go to stdout)
- The `--output` flag is ignored (content goes to stdout)

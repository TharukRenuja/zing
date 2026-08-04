# Browser Extension → zing Native Messaging API

This document specifies the Native Messaging protocol between a browser
extension and the `zing` download manager. It is intended for the authors
of the companion browser extension (separate repo).

## Overview

The browser extension communicates with `zing` over [Native Messaging](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging):

- The extension launches `zing nm` as a native host process.
- Messages are length-prefixed JSON on stdin/stdout.
- The host reads the daemon socket + auth token automatically (same path as the CLI), so **no pairing or manual token copy is required**.

## Setup

Run once per user to install the native host manifest for all browsers:

```
zing extension install
```

This writes a manifest to each browser's native messaging directory.
The host name is `com.zing.native_host`.

To remove:

```
zing extension uninstall
```

### Manifest locations

| Browser  | Path (Linux)                                                            |
|----------|-------------------------------------------------------------------------|
| Chrome   | `~/.config/google-chrome/NativeMessagingHosts/com.zing.native_host.json`|
| Edge     | `~/.config/microsoft-edge/NativeMessagingHosts/com.zing.native_host.json`|
| Firefox  | `~/.mozilla/native-messaging-hosts/com.zing.native_host.json`          |

## Wire protocol

Each message is sent as:

```
[4 bytes: length in little-endian][payload: UTF-8 JSON]
```

- The length prefix is a `uint32` in little-endian byte order.
- The payload is valid JSON (no trailing newline).
- Maximum message size: 1 MB.

### Reading a message (host → extension)

```python
import struct, sys, json
raw_length = sys.stdin.buffer.read(4)
length = struct.unpack('<I', raw_length)[0]
payload = sys.stdin.buffer.read(length)
return json.loads(payload)
```

### Writing a message (extension → host)

```python
import struct, sys, json
def send(obj):
    body = json.dumps(obj).encode('utf-8')
    sys.stdout.buffer.write(struct.pack('<I', len(body)))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()
```

## Request format

All requests share:

```json
{
  "action": "<string>",
  "id": <number>,       // optional, required for per-task actions
  "params": { ... }     // optional, varies by action
}
```

## Actions

### `ping`

Health check. Returns `{"ok": true}` immediately.

**Request:**
```json
{ "action": "ping" }
```

**Response:**
```json
{ "ok": true }
```

---

### `addUri`

Add a download task. Returns the new task's `id`.

**Request:**
```json
{
  "action": "addUri",
  "params": {
    "url": "https://example.com/file.zip",
    "filename": "file.zip",        // optional override
    "dir": "/home/user/Downloads", // optional override
    "connections": 8               // optional
  }
}
```

**Response:**
```json
{ "ok": true, "result": { "id": 1 } }
```

---

### `list`

List all tasks. Returns an array of task snapshots.

**Request:**
```json
{ "action": "list" }
```

**Response:**
```json
{
  "ok": true,
  "result": {
    "tasks": [
      {
        "id": 1,
        "url": "https://example.com/file.zip",
        "filename": "file.zip",
        "total_bytes": 1048576,
        "downloaded": 524288,
        "speed": 1048576,
        "peak_speed": 2097152,
        "paused": false,
        "done": false,
        "error": null,
        "status": "Downloading",
        "connections": 4,
        "completed_blocks": 32,
        "total_blocks": 64
      }
    ]
  }
}
```

---

### `tellStatus`

Get detailed status for a single task.

**Request:**
```json
{ "action": "tellStatus", "id": 1 }
```

**Response:** Same shape as a single entry in `list`.

---

### `pause`

Pause a task.

**Request:**
```json
{ "action": "pause", "id": 1 }
```

**Response:**
```json
{ "ok": true, "result": { "id": 1, "status": "paused" } }
```

---

### `resume`

Resume a paused task.

**Request:**
```json
{ "action": "resume", "id": 1 }
```

**Response:**
```json
{ "ok": true, "result": { "id": 1, "status": "resumed" } }
```

---

### `stop`

Stop a task permanently (it cannot be resumed).

**Request:**
```json
{ "action": "stop", "id": 1 }
```

**Response:**
```json
{ "ok": true, "result": { "ok": true } }
```

---

### `remove`

Remove a task from the list (must be stopped or completed first).

**Request:**
```json
{ "action": "remove", "id": 1 }
```

**Response:**
```json
{ "ok": true, "result": { "ok": true } }
```

---

### `version`

Returns the daemon version string.

**Request:**
```json
{ "action": "version" }
```

**Response:**
```json
{ "ok": true, "result": "0.2.4" }
```

## Error responses

When an action fails, the host returns:

```json
{
  "ok": false,
  "error": "Human-readable error message"
}
```

Common causes:
- `id` not found (task doesn't exist or was already removed).
- `url` missing or invalid in `addUri`.
- Daemon not running (host will try to connect and report the failure).

## Extension ID

The native host manifest's `allowed_origins` (Chromium) or
`allowed_extensions` (Firefox) contains a placeholder extension ID that
**must be replaced** with your actual extension ID before publishing:

- Chromium: `chrome-extension://REPLACE_WITH_YOUR_EXTENSION_ID/`
- Firefox: `chrome-extension://REPLACE_WITH_YOUR_EXTENSION_ID/`

After publishing your extension, update the manifests by running:

```
zing extension install
```

and then manually editing the manifest file to set the correct extension ID.
(This is a one-time setup step; the extension ID is baked into the manifest.)

## Daemon lifecycle

The host assumes the daemon is running. If `ping` fails:

1. The extension can ask the user to run `zing daemon start`, or
2. The extension can attempt to spawn `zing-daemon` from the same directory
   as the host binary (not implemented in the host yet — future work).

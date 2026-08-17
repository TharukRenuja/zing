<!--
title: Browser Extension
section: Guides
order: 9
desc: Native Messaging protocol for browser extensions — wire format, message schema, manifest setup, and security model.
keywords: zing, browser, extension, chrome, firefox, edge, native messaging, nm, manifest, wire protocol
-->

# Browser Extension

zing communicates with browser extensions via [Native Messaging](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging). The `zing nm` command runs as a native host process, and `zing extension install/uninstall` manages the manifest files.

The companion extension is available at **<https://github.com/TharukRenuja/zing-interceptor>**.

## Setup

The extension works out of the box once the native host manifests are installed. The zing installer (`install.sh`) runs this automatically; you can also do it by hand:

```bash
# Install native host manifests for all browsers
zing extension install

# Remove manifests
zing extension uninstall
```

Then install the companion extension in your browser:

- **Chrome / Edge**: Load unpacked from the `dist/chrome/` directory (or build with `./build.sh`)
- **Firefox**: Install from [AMO](https://addons.mozilla.org) or load temporarily from `dist/firefox/`

No extension ID editing is needed — the manifest lists the real, deterministic IDs.

### Manifest locations

| Browser | Path (Linux) |
|---------|-------------|
| Chrome | `~/.config/google-chrome/NativeMessagingHosts/oss.zing.intercept.json` |
| Edge | `~/.config/microsoft-edge/NativeMessagingHosts/oss.zing.intercept.json` |
| Firefox | `~/.mozilla/native-messaging-hosts/oss.zing.intercept.json` |

On Windows, manifests are written under `%LOCALAPPDATA%\zing\native-messaging-hosts\` and registered in the registry (`HKCU\Software\{Google\Chrome, Microsoft\Edge, Mozilla}\NativeMessagingHosts\`).

### Manifest content

Chrome/Edge:

```json
{
  "name": "oss.zing.intercept",
  "description": "zing download manager",
  "path": "/usr/bin/zing",
  "type": "stdio",
  "allowed_origins": [
    "chrome-extension://bcpghfjbokiclpfonepejdcndaoomcpf/"
  ]
}
```

Firefox:

```json
{
  "name": "oss.zing.intercept",
  "description": "zing download manager",
  "path": "/usr/bin/zing",
  "type": "stdio",
  "allowed_extensions": [
    "oss.zing.intercept@tharukrj"
  ]
}
```

**IDs are deterministic** — no manual replacement needed. The Chromium ID (`bcpghfjbokiclpfonepejdcndaoomcpf`) is derived from the public key baked into the extension's `manifest.json`, and the gecko ID (`oss.zing.intercept@tharukrj`) is declared there as `browser_specific_settings.gecko.id`. If you regenerate the extension keypair, update both the extension manifest and the `CHROMIUM_ID` constant in `cli/src/extension.rs` together.

## Wire protocol

Each message is:

```
[4 bytes: length in little-endian][payload: UTF-8 JSON]
```

- Length is a `uint32` in little-endian byte order
- Payload is valid JSON (no trailing newline)
- Maximum message size: 1 MB

### Reading messages (host → extension)

```python
import struct, sys, json
raw_length = sys.stdin.buffer.read(4)
length = struct.unpack('<I', raw_length)[0]
payload = sys.stdin.buffer.read(length)
return json.loads(payload)
```

### Writing messages (extension → host)

```python
import struct, sys, json
def send(obj):
    body = json.dumps(obj).encode('utf-8')
    sys.stdout.buffer.write(struct.pack('<I', len(body)))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()
```

## Message reference

### `ping`

Health check.

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

Add a download. Returns the task ID.

**Request:**
```json
{
  "action": "addUri",
  "params": {
    "url": "https://example.com/file.zip",
    "filename": "file.zip",
    "dir": "/home/user/Downloads",
    "connections": 8
  }
}
```

**Response:**
```json
{ "ok": true, "result": { "id": 1 } }
```

---

### `list`

List all tasks.

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

Get status for a single task.

**Request:**
```json
{ "action": "tellStatus", "id": 1 }
```

**Response:** Same shape as a single entry in `list`.

---

### `pause`

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

**Request:**
```json
{ "action": "version" }
```

**Response:**
```json
{ "ok": true, "result": "0.2.4" }
```

## Error responses

```json
{
  "ok": false,
  "error": "Human-readable error message"
}
```

Common errors:
- Task ID not found
- URL missing or invalid
- Daemon not running

## Daemon lifecycle

The host assumes the daemon is running. If `ping` fails, the extension should:
1. Ask the user to run `zing daemon start`, or
2. Spawn `zing-daemon` from the same directory as the host binary (future work).

## Security

- No token in logs
- Auth token file has `0o600` permissions
- Token rotates on each daemon restart
- No listening port exposed (Unix socket only on Linux)

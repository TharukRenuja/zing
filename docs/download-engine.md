<!--
title: Download Engine
section: Internals
order: 4
desc: How zing downloads files — segmented concurrent downloads, server probing, adaptive connection count, end-game mode, retry, rate limiting, Metalink, resume, and storage I/O.
keywords: zing, download, engine, segmented, probing, adaptive, end-game, retry, rate limit, metalink, resume, pwrite, bitfield
-->

# Download Engine

The download engine (`zing-core`) handles the core logic of fetching files over HTTP. It supports segmented concurrent downloads, server probing, adaptive connection tuning, and resume.

## How a download works

```
1. Probe the server
   ├── HEAD/GET Range: bytes=0-65535
   ├── Detect protocol (HTTP/1.1, HTTP/2, HTTP/3)
   ├── Measure RTT and bandwidth
   ├── Check range support and total file size
   └── Decide: streaming vs segmented, connection count

2. Resolve conflict (if file exists)
   ├── Overwrite (default)
   ├── Auto-rename (file-1.ext, file-2.ext, ...)
   └── Ask (interactive callback)

3. Resume check
   ├── Load .zing control file if it exists
   ├── Verify bitfield against actual file on disk
   └── Skip completed blocks

4. Download
   ├── Segmented: multiple connections, each downloads a range
   │   ├── Adaptive: small files use 1 connection, large files measure then decide
   │   ├── Work stealing: fast connections take work from slow ones
   │   └── End-game: all connections race for remaining blocks
   └── Streaming: single connection for unknown-size servers

5. Verify (optional)
   ├── Checksum (MD5/SHA-1/SHA-256/SHA-512, auto-detected by length)
   └── Metalink per-block hash validation

6. Cleanup
   ├── Remove .zing control file
   └── Run on-download-complete hook
```

## Segmented downloads

The engine splits a file into byte-range segments, each assigned to a connection. As a connection finishes its segment, it claims the next pending one.

### Segment sizes

| Constant | Value | Purpose |
|----------|-------|---------|
| `SEGMENT_MIN_SIZE` | 512 KiB | Legacy minimum; used for initial split only |
| `MIN_SEGMENT_BYTES` | 4 MiB | Dynamic minimum after adaptive count is determined |
| `SMALL_FILE_THRESHOLD` | 200 MiB | Files below this use 1 connection, skip measurement |

### Adaptive connection spawning

Files are split into two categories based on size:

| Threshold | Behavior |
|-----------|----------|
| < 200 MiB (`SMALL_FILE_THRESHOLD`) | 1 connection, no measurement, no delays |
| ≥ 200 MiB | Measure real speed, then calculate optimal count |

For large files:

1. Spawn connection 0 with the entire file
2. Wait 3 seconds (`MEASURE_DURATION_SECS`) to measure real per-connection speed
3. Calculate: `optimal = ceil(probe_bandwidth / measured_speed)`, capped at max_connections
4. If single connection already at 80%+ of probe bandwidth (`SINGLE_CONN_THRESHOLD`) → stay at 1
5. Spawn remaining connections in one shot (no batch delays)

Dynamic minimum segment size: `max(4 MiB, total_size / optimal_connections)` replaces the fixed 512 KiB floor. This ensures segments are large enough to be meaningful while still allowing work stealing between connections.

## Work stealing

When a fast connection finishes its segment early, it can "steal" remaining work from the slowest connection. This happens when:

1. The fast connection will finish its current segment in < 3 seconds
2. The slow connection has ≥ `min_segment_size` remaining (dynamic: `max(4 MiB, total/conns)`)

The slow connection's segment is split: the completed portion is marked done, and the remaining bytes become a new pending segment for the fast connection.

## End-game mode

When remaining blocks drop below a threshold (≤ 8 blocks or ≤ `2 × num_connections`), all connections stop taking sequential segments and instead race to download individual blocks. Each block is claimed atomically — a connection skips a block if another connection already completed it.

This minimizes tail latency when connections have varying speeds.

## Server probing

The probe sends `GET Range: bytes=0-65535` and analyzes the response:

| Check | What it determines |
|-------|--------------------|
| Response status | 206 = range support; 200 = no range support |
| `Content-Range` header | Total file size |
| `Content-Type` / protocol version | HTTP/1.1, HTTP/2, or HTTP/3 |
| Download time of 64 KiB | Bandwidth estimate |

The `decide_strategy` heuristic then picks:
- **Streaming mode** if no size or no range support
- **1 connection** if file is small (< 200 MiB)
- **N connections** based on measured speed vs probe bandwidth

### Mirror probing

`probe_mirrors` sends HEAD requests to all mirror URLs, measures RTT, and returns them sorted fastest-first. The download engine tries mirrors in order on failure.

## Rate limiting

A lock-free token-bucket rate limiter (`TokenBucket`) controls download speed:

- Capacity = `max(bytes_per_sec, 65536)`
- Refill every 10 ms
- Connections call `consume(amount)` which async-blocks until tokens are available
- The bandwidth scheduler can call `set_rate()` at scheduled times to change the limit dynamically

## Bandwidth scheduling

The `--bwlimit` flag accepts a time-of-day schedule:

```
"08:00,500KB 18:00,2MB"
```

A background task sleeps until each transition point, then calls `TokenBucket::set_rate()`. The initial rate is set from the first entry.

## Retry and backoff

Failed connections retry with exponential backoff + jitter:

| Parameter | Default |
|-----------|---------|
| Max retries | 5 |
| Base delay | 500 ms |
| Max delay | 10 s |
| Jitter | ±10% |

After exhausting retries on one URL, the engine rotates to the next mirror (if available).

### Retryable errors

- I/O errors (timeout, connection reset, broken pipe)
- HTTP 408, 429, 5xx
- reqwest connection/timeout errors
- Metalink hash mismatch (re-downloads the block)

## Resume

State is saved to a `.zing` control file (binary format):

```
Magic:   0x5A49 ("ZI")
Version: u16 (currently 2)
Total:   u64 (file size)
Block size: u32 (64 KiB)
Num blocks: u32
Bitfield length: u32
Bitfield: [N bytes]  (1 bit per block, 1 = complete)
```

The control file is saved periodically (every 2-5 seconds) and on clean shutdown. On resume, the engine verifies each bit against the actual file on disk using `read_at`.

### Block bitfield

Each bit represents a 64 KiB block. `missing_ranges()` coalesces consecutive incomplete blocks into `(offset, length)` ranges for efficient gap-filling.

## Storage I/O

All disk I/O uses position-based writes (`pwrite`/`pread`) to avoid seek+read races between concurrent connections:

| Function | Platform | Syscall |
|----------|----------|---------|
| `write_at` | Unix | `pwrite` |
| `write_at` | Windows | `seek_write` |
| `read_at` | Unix | `pread` |
| `read_at` | Windows | `seek_read` |
| `preallocate` | Linux | `fallocate` (fallback: `set_len`) |
| `preallocate` | macOS | `F_PREALLOCATE` (fallback: `set_len`) |

## Connection pool

The `ConnectionPool` wraps `reqwest::Client` with:

- Protocol detection (H1/H2/H3) from response version
- Custom headers, proxy, TLS client certificates
- DNS overrides
- Cookie jar support (Netscape format)
- Request metrics (total requests, H2 streams created)
- Event emission (`ConnectionCreated`, `ConnectionClosed`)

### Happy Eyeballs DNS

`resolve_host` queries the system resolver and returns IPv6 addresses first, then IPv4, preserving system order within each family. This follows RFC 8305 Happy Eyeballs for preferring IPv6.

## Metalink support

`.meta4` files provide:
- Multiple mirror URLs (tried in RTT-sorted order)
- Per-file checksums (MD5, SHA-1, SHA-256, SHA-512)
- Per-block hash validation during download (64 KiB pieces)

The engine validates each block's hash as it's written to disk. A mismatch causes that block to be re-downloaded.

## Throttle detection

When `--throttle-reprobe` is enabled, if a connection's speed drops below the threshold for too long, the engine:

1. Re-probes the server to check if bandwidth has changed
2. If the probe shows better speed, fails over to a mirror
3. Logs the reprobe result

This handles cases where the ISP or server throttles after detecting bulk transfers.

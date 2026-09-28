<!--
title: Coming from curl
section: Guides
order: 7
desc: A curl-to-zing migration guide — the one rule that picks download or request mode, a side-by-side command table, flag-letter collisions, and behavioural differences.
keywords: zing, curl, migration, http, request mode, headers, methods, api, cli, parity, differences
-->

# Coming from curl

zing is a downloader first and an HTTP client second. Most of the friction in
switching from curl comes from one rule, so read that section before anything
else.

## The one rule: how you ask decides what happens

A bare URL is a **file download**. It gets a progress bar, is written to disk,
resumes from a control file, and asks before replacing an existing file.

```bash
zing https://example.com/ubuntu.iso            # saved to disk as ./ubuntu.iso
zing -W ~/Movies https://example.com/video     # saved to ~/Movies/
```

Passing a **method-shaping flag** turns the same URL into an **HTTP request**:
the response body goes to stdout and nothing is written to disk.

The flags that switch to request mode are exactly these five:

| Flag | Meaning |
|------|---------|
| `-X`, `--method` | Explicit HTTP method |
| `-d`, `--data` | Request body |
| `-T`, `--upload-file` | Body from a file |
| `-G`, `--get` | Move `--data` into the query string |
| `-I`, `--head` | HEAD request |

**Headers do not switch modes.** `-H`, `-A`, and `-e` are just headers, so
`zing -H "X-Api-Key: secret" https://api.example.com/thing` is still a file
download and will try to save the response under a name taken from the URL path.
Add a method flag when you mean "call this API":

```bash
# a download
zing -H "X-Api-Key: secret" https://example.com/dataset.csv

# a request
zing -X GET -H "X-Api-Key: secret" https://api.example.com/thing
```

An explicit destination always wins, so a method flag plus `-o` or `-W` still
saves to disk instead of printing:

```bash
zing -X POST --data @query.sql --content-type application/sql \
    -o result.json https://api.example.com/db
```

## Side by side

### Fetching

| curl | zing |
|------|------|
| `curl -O https://example.com/f.iso` | `zing https://example.com/f.iso` |
| `curl -o out.iso https://example.com/f` | `zing -o out.iso https://example.com/f` |
| `curl -J https://example.com/download?id=1` | `zing https://example.com/download?id=1` |
| `curl -s https://example.com/f.iso` | `zing -q https://example.com/f.iso` |
| `curl -C - -o f.iso https://example.com/f.iso` | `zing -o f.iso https://example.com/f.iso` |

zing uses the server-provided `Content-Disposition` filename by default, so
`-J` is the default behaviour. Pass `--no-content-disposition` to turn that off.
Resume is automatic: re-running the same command continues from a `.zing`
control file, so there is no `-C -` equivalent to remember. Files above 200 MB
are fetched with several connections; below that, one.

### Requests

| curl | zing |
|------|------|
| `curl https://api.example.com/items` | `zing -X GET https://api.example.com/items` |
| `curl -I https://example.com/f` | `zing -I https://example.com/f` |
| `curl -X DELETE https://api.example.com/t/1` | `zing -X DELETE https://api.example.com/t/1` |
| `curl -d 'a=1&b=2' https://api.example.com/s` | `zing -X POST -d 'a=1&b=2' https://api.example.com/s` |
| `curl -d @body.json -H 'Content-Type: application/json' URL` | `zing -T body.json --content-type application/json URL` |
| `curl -G -d 'q=hello' https://api.example.com/s` | `zing -G -d 'q=hello' https://api.example.com/s` |
| `curl -T file.bin https://example.com/upload` | `zing -T file.bin https://example.com/upload` |
| `curl -X QUERY --data-raw '...' https://api.example.com/q` | `zing -X QUERY --data-raw '...' https://api.example.com/q` |

`-d` implies `POST`, repeated `-d` values are joined with `&`, and `-d @path`
reads the body from a file. `-T` implies `PUT`. `-G` moves `--data` into the
query string and requires `--data` alongside it. Any RFC 9110 method token is
accepted by `-X`.

### Auth, headers, and TLS

| curl | zing |
|------|------|
| `curl -u user:pass URL` | `zing -u user:pass URL` |
| `curl --digest -u user:pass URL` | `zing --digest -u user:pass URL` |
| `curl --netrc URL` | `zing -N URL` |
| `curl -H 'X-Foo: bar' URL` | `zing -H 'X-Foo: bar' URL` |
| `curl -A 'MyApp/1.0' URL` | `zing -A 'MyApp/1.0' URL` |
| `curl -e https://ref.example URL` | `zing -R https://ref.example URL` |
| `curl -k URL` | `zing -k URL` |
| `curl -x http://proxy:8080 URL` | `zing -x http://proxy:8080 URL` |
| `curl --cert c.pem --key k.pem URL` | `zing --cert c.pem --cert-key k.pem URL` |
| `curl -b cookies.txt URL` | `zing -L cookies.txt URL` |
| `curl -c cookies.txt URL` | `zing -s cookies.txt URL` |
| `curl -b 'name=value' URL` | *(not supported — use `-H` or a cookie file)* |
| `curl -L URL` *(follow redirects)* | *(not needed — see below)* |

### Limits and transfer control

| curl | zing |
|------|------|
| `curl --max-time 60 URL` | `zing --max-time 60 URL` |
| `curl --connect-timeout 10 URL` | `zing --connect-timeout 10 URL` |
| `curl --retry 3 URL` | `zing --retry 3 URL` |
| `curl --retry-delay 1 URL` | `zing --retry-wait 1000 URL` |
| `curl --limit-rate 2M URL` | `zing -r 2MB URL` |
| `curl --max-filesize 500M URL` | `zing -S 500MB URL` |
| `curl -# URL` | `zing URL` *(the bar is the default)* |

## A worked example

Given this curl command:

```bash
curl -s -H "X-SyncLRC-Secret: testsecret" \
  "http://localhost:8000/lyrics?track=Never%20Gonna%20Give%20You%20Up&artist=Rick%20Astley"
```

the equivalent is:

```bash
zing -X GET -H "X-SyncLRC-Secret: testsecret" \
  "http://localhost:8000/lyrics?track=Never%20Gonna%20Give%20You%20Up&artist=Rick%20Astley"
```

`-X GET` is what makes this a request rather than a download, and `-q` replaces
`-s`:

```bash
zing -q -X GET -H "X-SyncLRC-Secret: testsecret" \
  "http://localhost:8000/lyrics?track=Never%20Gonna%20Give%20You%20Up&artist=Rick%20Astley"
```

You do not need `--standalone`. Request mode never proxies to the background
daemon, so a secret header is used in-process and never written into a daemon
task. Without `-q` one informational line is printed, but it goes to **stderr**,
so `zing -X GET ... | jq` is safe either way.

## Flag letters that mean something else

The flags zing shares with curl are not always the same flag. These collisions
are the most common source of silently wrong commands:

| Letter | curl meaning | zing meaning |
|--------|--------------|--------------|
| `-m` | `--max-time` | `--mirror` (a mirror URL) |
| `-r` | `--range` | `--max-download-rate` |
| `-c` | `--cookie-jar` | `--checksum` |
| `-n` | `--netrc` | `--connections` |
| `-b` | `--cookie` | `--bwlimit` (bandwidth schedule) |
| `-L` | `--location` (follow redirects) | `--load-cookies` |
| `-s` | `--silent` | `--save-cookies` |
| `-N` | `--no-buffer` | `--netrc` |
| `-C` | `--continue-at` | `--content-disposition` |
| `-e` | `--referer` | `--referer` *(same)* |
| `-o` | `--output` | `--output` *(same)* |
| `-d` | `--data` | `--data` *(same)* |

The safe habit is to spell long flags out when porting a command, or to re-read
`zing --help` for any letter you are unsure of.

## Behavioural differences

**Redirects are always followed.** zing follows up to 10 redirects on its own.
curl does not follow them unless you pass `-L`, so dropping `-L` during a port
can silently change where you end up.

**A non-2xx response in request mode is an error and the body is dropped.** zing
prints `ERROR <path>: HTTP 404 Not Found` to stderr, writes nothing to stdout,
and — this is the part that matters for scripts — **still exits 0**. There is no
`--fail` flag and no way to make it non-zero, so neither `set -e` nor an `&&`
chain will notice that the call failed. curl exits 22 for the same situation if
you pass `--fail`, and exposes `%{http_code}` through `-w`.

If you need to branch on the result, check stderr rather than the exit status:

```bash
out=$(zing -X GET -H "X-Api-Key: $KEY" "$URL" 2>&1 >/dev/null) || true
case "$out" in
  *HTTP\ 2*) echo "ok" ;;
  *)         echo "failed: $out" ;;
esac
```

Passing `-o` does not rescue the body either: a failed request leaves the output
file empty. Reach for curl when you need the error body, the status code, or a
meaningful exit code.

**You cannot see the status line or response headers.** There is no `-i`,
`-v`, or `-w`. Request mode writes the body and nothing else, so debugging an
API means looking at the server logs or using `curl`.

**Redirect, form, and encoding helpers are absent.** There is no `-F` for
multipart forms and no `--data-urlencode`. zing's `-G -d` sends
`application/x-www-form-urlencoded`, so it will reproduce `curl -d` but not
`curl --data-urlencode`'s character-by-character encoding.

**A request body disables segmented downloads.** A body on a method that
supports ranges falls back to a single connection and prints a warning, since
there is nothing to segment. Use `-o` when you want the multi-connection
behaviour for a file.

## Things curl cannot do

| zing flag | What it does |
|-----------|--------------|
| `-n`, `--connections N` | Max parallel connections. Default is adaptive, capped at 8. |
| `--max-concurrent N` | Run several downloads at once (default 3). |
| `-r`, `--max-download-rate 2MB` | Cap throughput. |
| `-b`, `--bwlimit '08:00,500KB 18:00,2MB'` | Bandwidth schedule over the day. |
| `-m`, `--mirror URL` | Mirror list for failover. |
| `--end-game` | All connections race for the last blocks to close out a file. |
| `-M`, `--metalink FILE` | Read mirrors, checksums, and filename from a `.meta4`. |
| `-S`, `--max-filesize 500MB` | Skip a download whose `Content-Length` is too large. |
| `-c`, `--checksum HASH` | Verify the finished file. |
| `--low-speed-limit` / `--low-speed-time` | Abort a stalled transfer. |
| `-p`, `--pipe=sh\|python\|tar\|app` | Pipe straight into a script, interpreter, archive, or installer. |
| `zing tui URL` | Interactive task UI with `p` to pause. |
| `zing daemon start` | Hand the download to a background service. |

More on each in the [CLI reference](cli.md), the [download engine
notes](download-engine.md), and [pipe mode](pipe-mode.md).

## Small things that surprise people

- **Compression is off.** zing does not request `Accept-Encoding` and does not
  decode gzip, brotli, or deflate, so `--compressed` has no equivalent and is
  unnecessary.
- **The default User-Agent is `zing/0.1.0`.** Set it with `-A` if a server
  cares.
- **`--data` sends `application/x-www-form-urlencoded`, `-T` sends no
  `Content-Type` at all.** The second one is deliberate: it matches what curl
  does with `-T`, so it will not add a header you did not ask for. Use
  `--content-type` when you need one.
- **`-I` and `-X` are mutually exclusive**, and `-G` requires `--data`.
- **The progress bar is the default**, so there is no `-#` equivalent to add.
  `--progress json` gives machine-readable progress instead.

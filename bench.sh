#!/usr/bin/env bash
#
# zing vs aria2c vs curl — sequential benchmark.
#
# Downloads the same set of files with each tool, one after another, and
# reports median wall-times / throughput plus, per download: peak MB/s,
# max parallel TCP connections, retries/errors, CPU time and peak RSS.
# Also verifies sha256 integrity across tools. Meant to be run during
# low-traffic hours (e.g. midnight free-data time).
#
# Usage:
#   ./bench.sh                full run
#   ./bench.sh --self-test    quick flag smoke-test with a tiny file
#   ./bench.sh --fresh        delete previous results before running
#
# Outputs are stored under the OS Downloads dir in zing-test/:
#   ~/Downloads/zing-test/<test>/<tool>/download.bin
#   ~/Downloads/zing-test/results/results.csv
#
set -uo pipefail

# --------------------------------------------------------------------------
# Config — edit freely
# --------------------------------------------------------------------------
ZING_BIN="${ZING_BIN:-zing}"          # use the installed binary on PATH
CONNECTIONS="${CONNECTIONS:-4}"       # segments for zing / aria2c
ROUNDS="${ROUNDS:-3}"                 # passes over every tool (median)
PAUSE="${PAUSE:-3}"                   # seconds between downloads
SELF_TEST_BYTES=5242880               # 5 MB for --self-test

# name|bytes|url   NOTE: on the free-data network this host returns HTTP 403
# for single downloads above ~90 MB, so keep sizes under that cap.
URLS=(
  "50MB|52428800|https://speed.cloudflare.com/__down?bytes=52428800"
  "80MB|83886080|https://speed.cloudflare.com/__down?bytes=83886080"
  "90MB|94371840|https://speed.cloudflare.com/__down?bytes=94371840"
)
# Override the test set (e.g. for loopback validation): BENCH_URLS="a|n|http://..."
if [ -n "${BENCH_URLS:-}" ]; then
    mapfile -t URLS <<< "$(printf '%s\n' $BENCH_URLS)"
fi
# --------------------------------------------------------------------------

SELF_TEST_URL="https://speed.cloudflare.com/__down?bytes=${SELF_TEST_BYTES}"

DOWNLOADS="${DOWNLOADS:-}"
if [ -z "$DOWNLOADS" ] && command -v xdg-user-dir >/dev/null 2>&1; then
    DOWNLOADS="$(xdg-user-dir DOWNLOAD 2>/dev/null || true)"
fi
[ -n "$DOWNLOADS" ] || DOWNLOADS="$HOME/Downloads"

TEST_ROOT="$DOWNLOADS/zing-test"
RESULTS_DIR="$TEST_ROOT/results"
OUTFILE="download.bin"

die() { echo "error: $*" >&2; exit 1; }

command -v "$ZING_BIN" >/dev/null 2>&1 || die "$ZING_BIN not found on PATH"
for t in aria2c curl python3 sha256sum shuf ss; do
    command -v "$t" >/dev/null 2>&1 || die "required tool '$t' not installed"
done
[ -x /usr/bin/time ] || die "/usr/bin/time (GNU time) missing; needed for CPU/RAM stats"

# --------------------------------------------------------------------------
# Tool runners: $1=url $2=outdir  -> writes $2/$OUTFILE, exit code captured.
# Functions are exported so the timed child shell can call them.
# aria2c runs at --console-log-level=notice (no -q) so retries appear in log.
# --------------------------------------------------------------------------
# Each runner writes its own PID to $PIDFILE then execs the tool, so the
# sampler can count exactly which TCP connections belong to this download.
run_zing()    { echo $$ > "$PIDFILE"; exec "$ZING_BIN" "$1" -o "$2/$OUTFILE" \
                    -n "$CONNECTIONS" --progress none --allow-overwrite --standalone; }
run_zing1()   { echo $$ > "$PIDFILE"; exec "$ZING_BIN" "$1" -o "$2/$OUTFILE" \
                    -n 1 --progress none --allow-overwrite --standalone; }
run_aria2c()  { echo $$ > "$PIDFILE"; exec aria2c -x "$CONNECTIONS" -s "$CONNECTIONS" \
                    -d "$2" -o "$OUTFILE" --file-allocation=none \
                    --allow-overwrite=true --auto-file-renaming=false \
                    --summary-interval=0 --console-log-level=notice "$1"; }
run_curl()    { echo $$ > "$PIDFILE"; exec curl -sS -o "$2/$OUTFILE" "$1"; }

TOOLS=(zing zing-n1 aria2c curl)

export -f run_zing run_zing1 run_aria2c run_curl 2>/dev/null
export ZING_BIN CONNECTIONS OUTFILE PIDFILE

clean_outdir() {
    rm -f "$2/$OUTFILE" "$2/.$OUTFILE.aria2" "$2"/"$OUTFILE".*
}

dispatch() { # $1=tool $2=url $3=outdir
    case "$1" in
        zing)    run_zing "$2" "$3" ;;
        zing-n1) run_zing1 "$2" "$3" ;;
        aria2c)  run_aria2c "$2" "$3" ;;
        curl)    run_curl "$2" "$3" ;;
        *)       return 127 ;;
    esac
}
export -f dispatch 2>/dev/null

SAMPLER_PID=""
# Records "ts size conns bytes_recv" every 0.2 s. Connections are matched by
# the download's PID (ss -tnp), which is robust against DNS/anycast rotation.
# bytes_recv = total TCP bytes received on those connections (ss -tinp), i.e.
# the real transfer rate even when a tool preallocates the output file.
start_sampler() { # $1=outfile $2=pidfile $3=samples_file (background loop)
    (
        while :; do
            size=$(stat -c %s "$1" 2>/dev/null || echo 0)
            pid=$(cat "$2" 2>/dev/null || true)
            if [ -n "$pid" ]; then
                conns=$(ss -tnp 2>/dev/null | awk -v pid="$pid" '
                    $1 == "ESTAB" && index($0, "pid=" pid ",") { n++ }
                    END { print n + 0 }')
                recv=$(ss -tinp 2>/dev/null | awk -v pid="$pid" '
                    $1 == "ESTAB" && index($0, "pid=" pid ",") { on = 1; next }
                    $1 == "ESTAB" { on = 0; next }
                    on && /bytes_received:/ { for (i = 1; i <= NF; i++)
                        if ($i ~ /^bytes_received:/) { split($i, a, ":"); tot += a[2] } }
                    END { print tot + 0 }')
            else
                conns=0; recv=0
            fi
            printf '%s %s %s %s\n' "$(date +%s.%N)" "$size" "$conns" "$recv" >> "$3"
            sleep 0.2
        done
    ) &
    SAMPLER_PID=$!
}
stop_sampler() { [ -n "$SAMPLER_PID" ] && kill "$SAMPLER_PID" 2>/dev/null; SAMPLER_PID=""; }

peak_metrics() { # $1=samples_file ; prints "peak_mbps max_conns"
    python3 - "$1" <<'PY'
import sys
samples = []
for line in open(sys.argv[1]):
    p = line.split()
    if len(p) >= 4:
        try:
            samples.append((float(p[0]), float(p[1]), int(p[2]), int(p[3])))
        except ValueError:
            pass

def peak(sel):
    best = 0.0
    prev = None
    for s in samples:
        if prev:
            dt = s[0] - prev[0]
            if dt >= 0.1:
                dv = sel(s) - sel(prev)
                if dv > 0 and (dv / dt / 1e6) > best:
                    best = dv / dt / 1e6
        prev = s
    return best

using_recv = any(s[3] > 0 for s in samples)
pk = peak(lambda s: s[3]) if using_recv else peak(lambda s: s[1])
maxc = max((s[2] for s in samples), default=0)
print("%.2f %d" % (pk, maxc))
PY
}

parse_retries() { # $1=tool $2=run.log ; sets RETRIES ERRORS
    RETRIES=0; ERRORS=0
    case "$1" in
        zing|zing-n1)
            RETRIES=$(grep -ciE 'retry|retrying|resum(e|ing)' "$2" 2>/dev/null)
            ERRORS=$(grep -ciE '\berror\b|failed to|connection (reset|refused)' "$2" 2>/dev/null)
            ;;
        aria2c)
            RETRIES=$(grep -cE 'Retry\.' "$2" 2>/dev/null)
            ERRORS=$(grep -ciE 'error|exception|failed' "$2" 2>/dev/null)
            ;;
        curl)
            ERRORS=$(grep -c '^curl: (' "$2" 2>/dev/null)
            ;;
    esac
    RETRIES=${RETRIES:-0}; ERRORS=${ERRORS:-0}
}

one_run() { # $1=tool $2=url $3=outdir $4=expected_bytes
    # sets: ELAPSED RC SHA PEAK_MBPS MAX_CONNS USER_CPU SYS_CPU MAX_RSS RETRIES ERRORS
    clean_outdir "$2" "$3"
    sync
    samples="$3/samples.log"; : > "$samples"
    pidfile="$3/pid"; rm -f "$pidfile"
    start_sampler "$3/$OUTFILE" "$pidfile" "$samples"
    start=$(date +%s.%N)
    (
        export PIDFILE="$pidfile"
        /usr/bin/time -o "$3/time.log" -v bash -c 'dispatch "$1" "$2" "$3"' \
            _ "$1" "$2" "$3" >"$3/run.log" 2>&1
    )
    rc=$?
    end=$(date +%s.%N)
    stop_sampler
    ELAPSED=$(awk "BEGIN{printf \"%.3f\", $end-$start}")
    RC=$rc
    SHA=""
    if [ -f "$3/$OUTFILE" ]; then
        SHA=$(sha256sum "$3/$OUTFILE" | awk '{print $1}')
        actual=$(stat -c %s "$3/$OUTFILE" 2>/dev/null || echo 0)
        if [ "$RC" -eq 0 ] && [ "$actual" != "$4" ]; then
            echo "  !!! $1: size mismatch (got $actual bytes, expected $4) -> marked failed" >&2
            RC=99
        fi
    else
        if [ "$RC" -eq 0 ]; then
            echo "  !!! $1: no output file written -> marked failed" >&2
            RC=99
        fi
    fi
    read -r PEAK_MBPS MAX_CONNS <<< "$(peak_metrics "$samples")"
    USER_CPU=$(awk '/User time/ {print $4}' "$3/time.log" 2>/dev/null)
    SYS_CPU=$(awk '/System time/ {print $4}' "$3/time.log" 2>/dev/null)
    MAX_RSS=$(awk '/Maximum resident/ {print $6}' "$3/time.log" 2>/dev/null)
    USER_CPU=${USER_CPU:-0}; SYS_CPU=${SYS_CPU:-0}; MAX_RSS=${MAX_RSS:-0}
    parse_retries "$1" "$3/run.log"
}

self_test() {
    echo "== self-test: one small download with each tool =="
    for tool in "${TOOLS[@]}"; do
        outdir="$TEST_ROOT/_self-test/$tool"
        mkdir -p "$outdir"
        one_run "$tool" "$SELF_TEST_URL" "$outdir" "$SELF_TEST_BYTES"
        if [ "$RC" -eq 0 ] && [ -s "$outdir/$OUTFILE" ]; then
            size=$(stat -c %s "$outdir/$OUTFILE" 2>/dev/null || echo 0)
            echo "  $tool: OK (${size} bytes, ${ELAPSED}s)"
        else
            echo "  $tool: FAILED (exit $RC)"
        fi
    done
    echo "self-test done"
}

estimate() {
    bytes=0
    for e in "${URLS[@]}"; do
        b=${e#*|}; b=${b%%|*}
        bytes=$((bytes + b))
    done
    awk "BEGIN{printf \"%.1f GB across %d tools x %d rounds\n\", $bytes*${#TOOLS[@]}*$ROUNDS/1e9, ${#TOOLS[@]}, $ROUNDS}"
}

# --------------------------------------------------------------------------
main() {
    if [ "${1:-}" = "--self-test" ]; then self_test; exit 0; fi
    if [ "${1:-}" = "--fresh" ]; then rm -rf "$RESULTS_DIR"; fi
    [ -d "$RESULTS_DIR" ] || mkdir -p "$RESULTS_DIR"

    echo "zing benchmark"
    echo "  zing:   $("$ZING_BIN" --version 2>/dev/null | head -1) ($ZING_BIN)"
    echo "  aria2c: $(aria2c --version | head -1)"
    echo "  curl:   $(curl --version | head -1)"
    echo "  rounds: $ROUNDS, connections: $CONNECTIONS, pause: ${PAUSE}s"
    echo "  data:   $(estimate)"
    echo "  output: $TEST_ROOT"

    # Pre-flight: every URL must answer 200/206, or the run would silently
    # produce garbage (e.g. Cloudflare returning 403 above the size cap).
    echo "-- checking URLs respond 200..."
    bad=0
    for e in "${URLS[@]}"; do
        name=${e%%|*}; url=${e#*|*|}
        code=$(curl -s -o /dev/null -w "%{http_code}" --max-time 20 \
            -H "Range: bytes=0-1023" "$url")
        if [ "$code" = "200" ] || [ "$code" = "206" ]; then
            echo "  $name OK (HTTP $code)"
        else
            echo "  $name FAILED (HTTP $code)"
            bad=1
        fi
    done
    [ "$bad" -eq 0 ] || die "one or more test URLs are not reachable; fix URLS and rerun"

    CSV="$RESULTS_DIR/results.csv"
    echo "tool,test,bytes,round,elapsed_sec,mbps,peak_mbps,max_conns,retries,user_cpu_s,sys_cpu_s,max_rss_kb,sha256,exit" > "$CSV"

    declare -A ref_sha

    for round in $(seq 1 "$ROUNDS"); do
        order=("${TOOLS[@]}")
        mapfile -t order < <(printf '%s\n' "${order[@]}" | shuf)
        echo "-- round $round/$ROUNDS (order: ${order[*]})"

        for tool in "${order[@]}"; do
            for e in "${URLS[@]}"; do
                name=${e%%|*}; rest=${e#*|}
                bytes=${rest%%|*}; url=${rest#*|}
                outdir="$TEST_ROOT/$name/$tool"
                mkdir -p "$outdir"

                printf '  %-8s %-5s ... ' "$tool" "$name"
                one_run "$tool" "$url" "$outdir" "$bytes"

                mbps=$(awk "BEGIN{printf \"%.2f\", $bytes/$ELAPSED/1e6}")
                status=$([ "$RC" -eq 0 ] && echo ok || echo "exit $RC")
                echo "$status in ${ELAPSED}s (${mbps} MB/s, peak ${PEAK_MBPS} MB/s, ${MAX_CONNS} conns, ${RETRIES} retries, ${MAX_RSS} KB rss)"

                echo "$tool,$name,$bytes,$round,$ELAPSED,$mbps,$PEAK_MBPS,$MAX_CONNS,$RETRIES,$USER_CPU,$SYS_CPU,$MAX_RSS,$SHA,$RC" >> "$CSV"

                # integrity: compare every tool's sha against the first one
                key="$name:$round"
                if [ -z "${ref_sha[$key]:-}" ]; then
                    ref_sha[$key]="$SHA"
                elif [ -n "$SHA" ] && [ "$SHA" != "${ref_sha[$key]}" ]; then
                    echo "  !!! INTEGRITY MISMATCH: $name round $round $tool differs"
                fi
                sleep "$PAUSE"
            done
        done
    done

    echo
    echo "== summary =="
    python3 - "$CSV" "${TOOLS[@]}" <<'PY'
import csv, statistics, sys
path, tools = sys.argv[1], sys.argv[2:]
rows = [r for r in csv.DictReader(open(path)) if r["exit"] == "0"]
tests = sorted({r["test"] for r in rows})

def med(field, test, tool):
    vals = [float(r[field]) for r in rows
            if r["test"] == test and r["tool"] == tool and r[field] not in ("", "0")]
    return statistics.median(vals) if vals else None

print("== per-round detail (elapsed s) ==")
for test in tests:
    rounds = sorted({int(r["round"]) for r in rows if r["test"] == test})
    header = f"{'test':<8}{'tool':<9}" + "".join(f"r{r:<10}" for r in rounds)
    print(header)
    for tool in tools:
        line = f"{test:<8}{tool:<9}"
        for r in rounds:
            v = [float(x["elapsed_sec"]) for x in rows
                 if x["test"] == test and x["tool"] == tool and int(x["round"]) == r]
            line += f"{v[0]:<10.2f}" if v else f"{'--':<10}"
        print(line)
    print()

print("== median summary ==")
print(f"{'test':<8}{'tool':<9}{'med s':>8}{'med MB/s':>10}{'peak MB/s':>10}"
      f"{'conns':>7}{'RSS MB':>8}{'vs curl':>9}{'vs aria2':>9}")
for test in tests:
    curl_med = med("elapsed_sec", test, "curl")
    aria_med = med("elapsed_sec", test, "aria2c")
    for tool in tools:
        m = med("elapsed_sec", test, tool)
        if m is None:
            print(f"{test:<8}{tool:<9}{'n/a':>8}")
            continue
        mb = med("mbps", test, tool) or 0.0
        pk = med("peak_mbps", test, tool) or 0.0
        conn = med("max_conns", test, tool) or 0.0
        rss = med("max_rss_kb", test, tool) or 0.0
        vc = f"{100*(curl_med-m)/curl_med:+.0f}%" if curl_med else ""
        va = f"{100*(aria_med-m)/aria_med:+.0f}%" if aria_med else ""
        print(f"{test:<8}{tool:<9}{m:>8.2f}{mb:>10.2f}{pk:>10.2f}{conn:>7.0f}"
              f"{rss/1024:>8.1f}{vc:>9}{va:>9}")
print()
print("medians of wall time; '% vs curl/aria2' = faster/slower than that tool")
print("conns    = max parallel TCP connections to the server (sampled every 0.2 s)")
print("peak     = fastest TCP transfer rate seen (ss bytes_received, sampled)")
print("RSS      = peak resident set from /usr/bin/time -v")
PY

    echo "results: $CSV"
    echo "files:   $TEST_ROOT"
}

main "$@"

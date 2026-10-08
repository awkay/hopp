#!/usr/bin/env bash
# Profiles hopp_core under a steady emulated call and writes a flamegraph.
#
#   core/tests/profile.sh viewer                  # core watches a fake 3K share and two cameras
#   core/tests/profile.sh sharer                  # core shares this screen; a fake viewer draws
#   core/tests/profile.sh viewer --seconds 30 --net bad --label before-fix
#   core/tests/profile.sh viewer --energy-seconds 0   # skip the energy phase
#
# Builds the release core (as shipped, symbols kept) and the harness, starts
# `livekit-server --dev` when nothing answers at LIVEKIT_URL, runs the load (src/profile.rs),
# measures core's CPU, wake-ups and energy with no profiler attached (energy.py), then records
# it with Instruments' Time Profiler and writes out/profile/<label>-<role>.{trace,folded,svg,txt}.
# The .txt ends with the energy line and core's FramePacing lines (what the screen-share window
# presented). Compare two labels' .txt for before/after.
#
# The sharer run measures whatever changes on screen: ScreenCaptureKit sends no frames for a
# static screen, so play a video or scroll something during it. Its fake viewer only moves a
# cursor and draws on the overlay; it never clicks, so the real pointer is never moved.
# Needs Microphone and (sharer) Screen Recording access for the terminal, like smoke.sh.
set -uo pipefail

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CORE_DIR="$(dirname "$TESTS_DIR")"
OUT_DIR="$TESTS_DIR/out/profile"
WARMUP=5

export LIVEKIT_URL="${LIVEKIT_URL:-ws://localhost:7880}"
export LIVEKIT_API_KEY="${LIVEKIT_API_KEY:-devkey}"
export LIVEKIT_API_SECRET="${LIVEKIT_API_SECRET:-secret}"
export RUST_LOG="${RUST_LOG:-hopp_core=info}"

usage() { sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; }

role=""
seconds=60
energy_seconds=20
network_profile=""
label="$(date +%Y%m%d-%H%M%S)"
while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --seconds) seconds="$2"; shift 2 ;;
    --energy-seconds) energy_seconds="$2"; shift 2 ;;
    --net) network_profile="$2"; shift 2 ;;
    --label) label="$2"; shift 2 ;;
    viewer|sharer) role="$1"; shift ;;
    *) usage >&2; exit 1 ;;
  esac
done
[ -z "$role" ] && { usage >&2; exit 1; }
name="$label-$role"

livekit_pid=""
core_pid=""
harness_pid=""
socket="${TMPDIR:-/tmp}"
socket="${socket%/}/hopp-profile-$$.sock"
ready_file="$OUT_DIR/.ready-$$"
stop_file="$OUT_DIR/.stop-$$"
cleanup() {
  [ -n "$harness_pid" ] && kill "$harness_pid" 2>/dev/null
  [ -n "$core_pid" ] && kill "$core_pid" 2>/dev/null
  # Wait for it to exit: a back-to-back run would otherwise find the port still open, skip
  # starting its own server and fail to connect once this one is gone.
  [ -n "$livekit_pid" ] && kill "$livekit_pid" 2>/dev/null && wait "$livekit_pid" 2>/dev/null
  [ -n "$network_profile" ] && sudo -n "$TESTS_DIR/netem.sh" off > /dev/null
  rm -f "$socket" "$ready_file" "$stop_file"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# CPU time a process has used so far, in seconds (ps prints [[hh:]mm:]ss.cc).
cpu_seconds() {
  ps -o time= -p "$1" | awk -F: '{ s = 0; for (i = 1; i <= NF; i++) s = s * 60 + $i; print s }'
}

mkdir -p "$OUT_DIR"
xcrun -f xctrace > /dev/null 2>&1 || { echo "xctrace not found: install Xcode" >&2; exit 1; }
# A display that sleeps mid-run stops capture and throttles rendering; keep it awake until exit.
caffeinate -d -w $$ &
[ -n "$network_profile" ] && { echo "netem.sh needs root."; sudo -v || exit 1; }

# ── LiveKit ──────────────────────────────────────────────────────────────────
host_port="${LIVEKIT_URL#*://}"
host_port="${host_port%%/*}"
host="${host_port%%:*}"
port="${host_port##*:}"
if ! nc -z "$host" "$port" 2>/dev/null; then
  if ! command -v livekit-server > /dev/null; then
    echo "No LiveKit server at $LIVEKIT_URL and livekit-server is not installed (brew install livekit)" >&2
    exit 1
  fi
  echo "Starting livekit-server --dev (log: $OUT_DIR/livekit.log)"
  # Loopback only: everything runs on this Mac, and ICE checks over the LAN and IPv6 addresses
  # LiveKit advertises by default can time out (a second connection from one process never
  # connected; core took 13 s).
  livekit-server --dev --node-ip 127.0.0.1 --rtc.enable_loopback_candidate \
    --config-body "$(printf 'rtc:\n  interfaces:\n    includes: [lo0]\n')" \
    > "$OUT_DIR/livekit.log" 2>&1 &
  livekit_pid=$!
  for _ in $(seq 100); do nc -z "$host" "$port" 2>/dev/null && break; sleep 0.1; done
  nc -z "$host" "$port" 2>/dev/null || { echo "livekit-server did not start" >&2; exit 1; }
fi

# ── Build ────────────────────────────────────────────────────────────────────
host_triple="$(rustc -vV | awk '/host:/ {print $2}')"
echo "Building release core (the first build takes several minutes)..."
if ! (cd "$CORE_DIR" && cargo build --release --target "$host_triple") > "$OUT_DIR/build.log" 2>&1; then
  echo "core build failed, see $OUT_DIR/build.log" >&2
  exit 1
fi
echo "Building the harness..."
if ! (cd "$TESTS_DIR" && cargo build --release) >> "$OUT_DIR/build.log" 2>&1; then
  echo "harness build failed, see $OUT_DIR/build.log" >&2
  exit 1
fi
CORE_BIN="$CORE_DIR/target/release/hopp_core-$host_triple"
HARNESS_BIN="$TESTS_DIR/target/release/hopp_core_tests"

if [ -n "$network_profile" ]; then
  if [[ "$host" != "localhost" && "$host" != "127.0.0.1" ]]; then
    sudo -n "$TESTS_DIR/netem.sh" "$network_profile" --remote "$host" || exit 1
  else
    sudo -n "$TESTS_DIR/netem.sh" "$network_profile" || exit 1
  fi
fi

# ── Load ─────────────────────────────────────────────────────────────────────
echo "Starting core and the $role load (logs: $OUT_DIR/$name.log, .core.log)"
rm -f "$socket" "$ready_file" "$stop_file"
"$CORE_BIN" --socket-path "$socket" > "$OUT_DIR/$name.core.log" 2>&1 &
core_pid=$!
for _ in $(seq 100); do [ -S "$socket" ] && break; sleep 0.1; done
[ -S "$socket" ] || { echo "core did not open its socket" >&2; exit 1; }

"$HARNESS_BIN" --socket-path "$socket" profile "$role" \
  --ready-file "$ready_file" --stop-file "$stop_file" > "$OUT_DIR/$name.log" 2>&1 &
harness_pid=$!
for _ in $(seq 1200); do
  [ -e "$ready_file" ] && break
  kill -0 "$harness_pid" 2>/dev/null || break
  sleep 0.1
done
if [ ! -e "$ready_file" ]; then
  echo "the load did not start:" >&2
  cat "$OUT_DIR/$name.log" >&2
  exit 1
fi
sleep "$WARMUP"

# ── Energy ───────────────────────────────────────────────────────────────────
# Before the trace, so the profiler's sampling doesn't count against core.
energy="energy: not measured"
if [ "$energy_seconds" != 0 ]; then
  echo "Measuring core's CPU, wake-ups and energy for $energy_seconds s..."
  energy=$(python3 "$TESTS_DIR/energy.py" "$core_pid" "$energy_seconds") || energy="energy: failed"
fi

# ── Record ───────────────────────────────────────────────────────────────────
trace="$OUT_DIR/$name.trace"
rm -rf "$trace"
echo "Recording core (pid $core_pid) for $seconds s..."
core_cpu_before=$(cpu_seconds "$core_pid")
load_cpu_before=$(cpu_seconds "$harness_pid")
started=$(date +%s)
xcrun xctrace record --template 'Time Profiler' --attach "$core_pid" \
  --time-limit "${seconds}s" --output "$trace" --no-prompt > "$OUT_DIR/$name.xctrace.log" 2>&1
recorded=$(( $(date +%s) - started ))
core_cpu=$(echo "$(cpu_seconds "$core_pid") $core_cpu_before $recorded" | awk '{ printf "%.0f", 100 * ($1 - $2) / $3 }')
load_cpu=$(echo "$(cpu_seconds "$harness_pid") $load_cpu_before $recorded" | awk '{ printf "%.0f", 100 * ($1 - $2) / $3 }')

touch "$stop_file"
for _ in $(seq 300); do kill -0 "$harness_pid" 2>/dev/null || break; sleep 0.1; done
wait "$harness_pid" 2>/dev/null
harness_pid=""
cat "$OUT_DIR/$name.log"

if [ ! -d "$trace" ]; then
  echo "xctrace failed, see $OUT_DIR/$name.xctrace.log" >&2
  exit 1
fi
xcrun xctrace export --input "$trace" \
  --xpath '/trace-toc/run[@number="1"]/data/table[@schema="time-profile"]' > "$OUT_DIR/$name.xml"
echo
python3 "$TESTS_DIR/flamegraph.py" "$OUT_DIR/$name.xml" "$OUT_DIR/$name" --seconds "$seconds" \
  --demangler "'$HARNESS_BIN' demangle" \
  --title "hopp_core, $role run '$label': ${core_cpu}% of one core (load generator ${load_cpu}%)" \
  || exit 1
rm -f "$OUT_DIR/$name.xml"
{
  echo
  echo "$energy"
  echo
  echo "FramePacing (core's screen-share window, one line per 10 s):"
  grep -o 'FramePacing .*' "$OUT_DIR/$name.core.log" || echo "  none (no remote share shown)"
} | tee -a "$OUT_DIR/$name.txt"
echo
echo "Flamegraph: $OUT_DIR/$name.svg (open in a browser)"

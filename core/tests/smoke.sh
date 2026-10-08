#!/usr/bin/env bash
# Runs the self-checking smoke scenarios (src/smoke.rs), each against a fresh hopp_core, and
# prints a summary. Exit status: the number of failed scenarios.
#
#   core/tests/smoke.sh                        # every scenario except network-drop
#   core/tests/smoke.sh bandwidth screenshare  # just these
#   core/tests/smoke.sh --net bad              # every scenario except network-drop, over a bad connection (netem.sh)
#   core/tests/smoke.sh network-drop           # opt-in: cuts LiveKit traffic mid-call
#   core/tests/smoke.sh --list
#
# Needs a LiveKit server: LIVEKIT_URL if it answers, otherwise starts `livekit-server --dev`
# (brew install livekit). Logs: core/tests/out/<scenario>.log (harness) and .core.log (core).
# --net and network-drop ask for your password once (sudo, for netem.sh).
# CARGO_TOOLCHAIN=<name> builds with that rustup toolchain instead of the default.
set -uo pipefail

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CORE_DIR="$(dirname "$TESTS_DIR")"
OUT_DIR="$TESTS_DIR/out"
ALL_SCENARIOS=(call-lifecycle call-restart call-end-race stale-call-end bandwidth viewer-hang screenshare)

export LIVEKIT_URL="${LIVEKIT_URL:-ws://localhost:7880}"
export LIVEKIT_API_KEY="${LIVEKIT_API_KEY:-devkey}"
export LIVEKIT_API_SECRET="${LIVEKIT_API_SECRET:-secret}"
export RUST_LOG="${RUST_LOG:-hopp_core=info}"
CARGO=(cargo ${CARGO_TOOLCHAIN:+"+$CARGO_TOOLCHAIN"})

network_profile=""
scenarios=()
while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    --list) printf '%s\n' "${ALL_SCENARIOS[@]}" network-drop; exit 0 ;;
    --net) network_profile="$2"; shift 2 ;;
    *) scenarios+=("$1"); shift ;;
  esac
done
[ ${#scenarios[@]} -eq 0 ] && scenarios=("${ALL_SCENARIOS[@]}")
if [ "$network_profile" = "drop" ]; then
  echo "--net drop would cut every call; use the network-drop scenario" >&2
  exit 1
fi

needs_sudo=""
[ -n "$network_profile" ] && needs_sudo=1
[[ " ${scenarios[*]} " == *" network-drop "* ]] && needs_sudo=1

livekit_pid=""
core_pid=""
sudo_keepalive_pid=""
cleanup() {
  [ -n "$core_pid" ] && kill "$core_pid" 2>/dev/null
  [ -n "$livekit_pid" ] && kill "$livekit_pid" 2>/dev/null
  [ -n "$network_profile" ] && sudo -n "$TESTS_DIR/netem.sh" off > /dev/null
  [ -n "$sudo_keepalive_pid" ] && kill "$sudo_keepalive_pid" 2>/dev/null
}
trap cleanup EXIT
trap 'exit 130' INT TERM

mkdir -p "$OUT_DIR"

if [ -n "$needs_sudo" ]; then
  echo "netem.sh needs root; caching sudo for this run."
  sudo -v || exit 1
  # Keep the credentials fresh: the harness runs `sudo -n netem.sh` for network-drop.
  while true; do sudo -n true; sleep 60; done 2>/dev/null &
  sudo_keepalive_pid=$!
fi

# ── LiveKit ──────────────────────────────────────────────────────────────────
host_port="${LIVEKIT_URL#*://}"
host_port="${host_port%%/*}"
host="${host_port%%:*}"
port="${host_port##*:}"
if ! nc -z "$host" "$port" 2>/dev/null; then
  if [[ "$host" != "localhost" && "$host" != "127.0.0.1" ]]; then
    echo "LiveKit is not answering at $LIVEKIT_URL" >&2
    exit 1
  fi
  if ! command -v livekit-server >/dev/null; then
    echo "No LiveKit server at $LIVEKIT_URL and livekit-server is not installed (brew install livekit)" >&2
    exit 1
  fi
  echo "Starting livekit-server --dev (log: $OUT_DIR/livekit.log)"
  livekit-server --dev > "$OUT_DIR/livekit.log" 2>&1 &
  livekit_pid=$!
  for _ in $(seq 100); do nc -z "$host" "$port" 2>/dev/null && break; sleep 0.1; done
  if ! nc -z "$host" "$port" 2>/dev/null; then
    echo "livekit-server did not start, see $OUT_DIR/livekit.log" >&2
    exit 1
  fi
fi

# ── Build (same as `task build_dev`, plus the harness) ───────────────────────
host_triple="$(rustc -vV | awk '/host:/ {print $2}')"
echo "Building core..."
if ! (cd "$CORE_DIR" && "${CARGO[@]}" build --target "$host_triple") > "$OUT_DIR/build.log" 2>&1; then
  echo "core build failed, see $OUT_DIR/build.log" >&2
  exit 1
fi
echo "Building the harness..."
if ! (cd "$TESTS_DIR" && "${CARGO[@]}" build) >> "$OUT_DIR/build.log" 2>&1; then
  echo "harness build failed, see $OUT_DIR/build.log" >&2
  exit 1
fi
CORE_BIN="$CORE_DIR/target/debug/hopp_core-$host_triple"
HARNESS_BIN="$TESTS_DIR/target/debug/hopp_core_tests"

# ── Network ──────────────────────────────────────────────────────────────────
if [ -n "$network_profile" ]; then
  if [[ "$host" != "localhost" && "$host" != "127.0.0.1" ]]; then
    sudo -n "$TESTS_DIR/netem.sh" "$network_profile" --remote "$host" || exit 1
  else
    sudo -n "$TESTS_DIR/netem.sh" "$network_profile" || exit 1
  fi
fi

# ── Scenarios ────────────────────────────────────────────────────────────────
summary=()
failures=0
for scenario in "${scenarios[@]}"; do
  echo
  echo "== $scenario"
  socket="${TMPDIR:-/tmp}"
  socket="${socket%/}/hopp-smoke-$$.sock"
  rm -f "$socket"
  "$CORE_BIN" --socket-path "$socket" > "$OUT_DIR/$scenario.core.log" 2>&1 &
  core_pid=$!
  for _ in $(seq 100); do [ -S "$socket" ] && break; sleep 0.1; done

  started=$SECONDS
  if [ -S "$socket" ]; then
    "$HARNESS_BIN" --socket-path "$socket" smoke --core-pid "$core_pid" "$scenario" 2>&1 \
      | tee "$OUT_DIR/$scenario.log"
    status=${PIPESTATUS[0]}
  else
    echo "core did not open its socket" | tee "$OUT_DIR/$scenario.log"
    status=1
  fi
  elapsed=$((SECONDS - started))

  # Core exits once the harness disconnects; give it a moment, then make sure.
  for _ in $(seq 50); do kill -0 "$core_pid" 2>/dev/null || break; sleep 0.1; done
  kill "$core_pid" 2>/dev/null
  wait "$core_pid" 2>/dev/null
  core_pid=""
  rm -f "$socket"

  if [ "$status" -eq 0 ]; then
    summary+=("$(printf 'PASS  %-16s %4ss' "$scenario" "$elapsed")")
  else
    failures=$((failures + 1))
    note="see out/$scenario.log, out/$scenario.core.log"
    grep -q 'panicked' "$OUT_DIR/$scenario.core.log" && note="core panicked; $note"
    summary+=("$(printf 'FAIL  %-16s %4ss  %s' "$scenario" "$elapsed" "$note")")
  fi
done

echo
echo "== Summary${network_profile:+ (network: $network_profile)}"
printf '%s\n' "${summary[@]}"
exit "$failures"

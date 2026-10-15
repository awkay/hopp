#!/usr/bin/env bash
# Measures how sharp text stays in a screen share, H.264 against HEVC (spec 0007, B7).
#
#   core/tests/clarity.sh                    # normal mode, both codecs, 3 runs each
#   core/tests/clarity.sh --low-bandwidth    # core's low-bandwidth encoding
#   core/tests/clarity.sh --runs 1 --label try
#
# Renders an editor page with real code (clarity/render.swift: light and dark panes, Menlo
# 13 pt at 2x, 3024x1964 like a 14" MacBook Pro; with --low-bandwidth, scaled down to the
# 1660x1080 that mode encodes, and scored at that size), then for each codec has the harness
# (src/clarity.rs) share it the way core's capture would: one frame when the share starts, one
# per keystroke while a line is typed, one per caret blink, then nothing. A fake viewer decodes
# it and scores every frame against the frame sent (luma PSNR of each pane, chroma PSNR). No
# core runs: the fake sharer uses core's SDK, WebRTC build and encoding settings.
#
# Writes out/clarity/<label>/report.txt (one line per phase and run, then a summary) and
# compare-<phase>-<light|dark>.png from the first run: 1:1 crops around the typed line, top to
# bottom the frame sent, H.264, HEVC; zoom-*.png: the line's start at 3x. Needs `brew install livekit ffmpeg` and Xcode's swift.
set -uo pipefail

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CORE_DIR="$(dirname "$TESTS_DIR")"
WIDTH=3024
HEIGHT=1964

export LIVEKIT_URL="${LIVEKIT_URL:-ws://localhost:7880}"
export LIVEKIT_API_KEY="${LIVEKIT_API_KEY:-devkey}"
export LIVEKIT_API_SECRET="${LIVEKIT_API_SECRET:-secret}"

usage() { sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; }

low_bandwidth=""
runs=3
label="$(date +%Y%m%d-%H%M%S)"
while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --low-bandwidth) low_bandwidth="--low-bandwidth"; shift ;;
    --runs) runs="$2"; shift 2 ;;
    --label) label="$2"; shift 2 ;;
    *) usage >&2; exit 1 ;;
  esac
done
[ -n "$low_bandwidth" ] && label="$label-low-bandwidth"
OUT_DIR="$TESTS_DIR/out/clarity/$label"
PAGE_DIR="$TESTS_DIR/out/clarity/page${low_bandwidth:+-low-bandwidth}"
mkdir -p "$OUT_DIR" "$PAGE_DIR"
report="$OUT_DIR/report.txt"
: > "$report"

livekit_pid=""
cleanup() {
  [ -n "$livekit_pid" ] && kill "$livekit_pid" 2>/dev/null && wait "$livekit_pid" 2>/dev/null
  rm -f "$OUT_DIR"/*.yuv
}
trap cleanup EXIT

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
  # Loopback only, as in profile.sh.
  livekit-server --dev --node-ip 127.0.0.1 --rtc.enable_loopback_candidate \
    --config-body "$(printf 'rtc:\n  interfaces:\n    includes: [lo0]\n')" \
    > "$OUT_DIR/livekit.log" 2>&1 &
  livekit_pid=$!
  for _ in $(seq 100); do nc -z "$host" "$port" 2>/dev/null && break; sleep 0.1; done
  nc -z "$host" "$port" 2>/dev/null || { echo "livekit-server did not start" >&2; exit 1; }
fi

# ── Page ─────────────────────────────────────────────────────────────────────
echo "Rendering the page..."
cat "$CORE_DIR/src/screen_share_codec.rs" "$CORE_DIR/src/room_service.rs" > "$PAGE_DIR/code.txt"
swift "$TESTS_DIR/clarity/render.swift" "$PAGE_DIR" "$PAGE_DIR/code.txt" "$WIDTH" "$HEIGHT" || exit 1
if [ -n "$low_bandwidth" ]; then
  # Low-bandwidth mode encodes the share scaled down to fit 1920x1080 (1662x1080 here). Scaling
  # the page beforehand (area filter, like WebRTC's) and scoring at that size measures what the
  # codec loses, not what the scaling loses, which is the same for both codecs.
  scaled="$(python3 - "$PAGE_DIR/layout.json" <<'PY'
import json, sys
path = sys.argv[1]
layout = json.load(open(path))
scale = max(layout["width"] / 1920, layout["height"] / 1080, 1)
# WebRTC trims the width and height to multiples of 4 (1662 → 1660).
even = lambda n: int(n / scale) // 4 * 4
for key in ("width", "height"):
    layout[key] = even(layout[key])
for key in ("line_top", "line_height", "status_bar"):
    layout[key] = round(layout[key] / scale)
layout["char_x"] = [round(x / scale) for x in layout["char_x"]]
json.dump(layout, open(path, "w"))
print(layout["width"], layout["height"])
PY
)" || exit 1
  WIDTH="${scaled% *}"
  HEIGHT="${scaled#* }"
  for name in base full; do
    ffmpeg -loglevel error -y -i "$PAGE_DIR/$name.png" -vf "scale=$WIDTH:$HEIGHT:flags=area" \
      "$PAGE_DIR/$name-scaled.png" && mv "$PAGE_DIR/$name-scaled.png" "$PAGE_DIR/$name.png" || exit 1
  done
fi
for name in base full; do
  ffmpeg -loglevel error -y -i "$PAGE_DIR/$name.png" -pix_fmt yuv420p -f rawvideo "$PAGE_DIR/$name.yuv" || exit 1
done
line_top="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["line_top"])' "$PAGE_DIR/layout.json")"

echo "Building the harness..."
if ! (cd "$TESTS_DIR" && cargo build --release) > "$OUT_DIR/build.log" 2>&1; then
  echo "harness build failed, see $OUT_DIR/build.log" >&2
  exit 1
fi
HARNESS_BIN="$TESTS_DIR/target/release/hopp_core_tests"

# 1:1 crops around the typed line, from the sent and decoded frames of the first run.
crop_w=$(( WIDTH / 2 < 1200 ? WIDTH / 2 : 1200 ))
crop_h=480
crop_y=$(( line_top - crop_h / 2 ))
crop() { # <yuv> <x> <png>
  ffmpeg -loglevel error -y -f rawvideo -pix_fmt yuv420p -s "${WIDTH}x$HEIGHT" -i "$1" \
    -vf "crop=$crop_w:$crop_h:$2:$crop_y" "$3"
}

# ── Runs ─────────────────────────────────────────────────────────────────────
for run in $(seq "$runs"); do
  # Alternate which codec goes first, so warm-up effects don't favour one.
  if [ $(( run % 2 )) -eq 1 ]; then codecs=(h264 h265); else codecs=(h265 h264); fi
  for codec in "${codecs[@]}"; do
    echo "Run $run, $codec" | tee -a "$report"
    if ! "$HARNESS_BIN" clarity --codec "$codec" $low_bandwidth --page-dir "$PAGE_DIR" \
      --out-dir "$OUT_DIR" --label "$codec-$run" 2>> "$OUT_DIR/harness.log" | tee -a "$report"; then
      echo "the $codec run failed, see $OUT_DIR/harness.log" >&2
      exit 1
    fi
    if [ "$run" -eq 1 ]; then
      for phase in static typing blinking idle; do
        [ -f "$OUT_DIR/$codec-1-$phase.yuv" ] || continue
        crop "$OUT_DIR/$codec-1-$phase.yuv" 0 "$OUT_DIR/$codec-$phase-light.png"
        crop "$OUT_DIR/$codec-1-$phase.yuv" $(( WIDTH / 2 )) "$OUT_DIR/$codec-$phase-dark.png"
        if [ "$codec" = h264 ]; then
          crop "$OUT_DIR/$codec-1-$phase-sent.yuv" 0 "$OUT_DIR/sent-$phase-light.png"
          crop "$OUT_DIR/$codec-1-$phase-sent.yuv" $(( WIDTH / 2 )) "$OUT_DIR/sent-$phase-dark.png"
        fi
      done
      rm -f "$OUT_DIR"/*.yuv
    fi
  done
done

for phase in static typing blinking idle; do
  for pane in light dark; do
    inputs=("$OUT_DIR/sent-$phase-$pane.png" "$OUT_DIR/h264-$phase-$pane.png" "$OUT_DIR/h265-$phase-$pane.png")
    ls "${inputs[@]}" > /dev/null 2>&1 || continue
    ffmpeg -loglevel error -y -i "${inputs[0]}" -i "${inputs[1]}" -i "${inputs[2]}" \
      -filter_complex vstack=inputs=3 "$OUT_DIR/compare-$phase-$pane.png"
    # The typed line's start, 3x with no smoothing.
    ffmpeg -loglevel error -y -i "${inputs[0]}" -i "${inputs[1]}" -i "${inputs[2]}" \
      -filter_complex "[0]crop=400:80:200:$(( crop_h / 2 - 20 )),scale=iw*3:ih*3:flags=neighbor[a];[1]crop=400:80:200:$(( crop_h / 2 - 20 )),scale=iw*3:ih*3:flags=neighbor[b];[2]crop=400:80:200:$(( crop_h / 2 - 20 )),scale=iw*3:ih*3:flags=neighbor[c];[a][b][c]vstack=inputs=3" \
      "$OUT_DIR/zoom-$phase-$pane.png"
    rm -f "${inputs[@]}"
  done
done

# ── Summary ──────────────────────────────────────────────────────────────────
# Means over the runs of each phase's last frame PSNR, QP and bytes.
{
  echo
  echo "Summary (mean over $runs runs; PSNR of each phase's last decoded frame, dB)"
  awk '
    /^Run / { codec = $3 }
    /^  - (static|typing|blinking|idle):/ {
      phase = $2; sub(":", "", phase)
      key = phase " " codec
      for (i = 1; i <= NF; i++) {
        if ($i == "light" && $(i+1) == "last") light[key] += $(i+2)
        if ($i == "dark" && $(i+1) == "last") dark[key] += $(i+2)
        if ($i == "PSNR-UV" && $(i+1) == "last") uv[key] += $(i+2)
        if ($i == "enc") qp[key] += $(i+1)
        if ($i == "kB;") kb[key] += $(i-1)
      }
      n[key]++
    }
    END {
      printf "%-9s %-5s %8s %8s %8s %8s %8s\n", "phase", "codec", "light", "dark", "chroma", "QP enc", "kB"
      split("static typing blinking idle", phases, " ")
      split("h264 h265", codecs, " ")
      for (p = 1; p <= 4; p++) for (c = 1; c <= 2; c++) {
        key = phases[p] " " codecs[c]
        if (!n[key]) continue
        printf "%-9s %-5s %8.2f %8.2f %8.2f %8.1f %8.0f\n", phases[p], codecs[c],
          light[key] / n[key], dark[key] / n[key], uv[key] / n[key], qp[key] / n[key], kb[key] / n[key]
      }
    }' "$report"
} | tee -a "$report"
echo "Report: $report"

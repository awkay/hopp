# Hopp Core Tests

This directory contains integration tests for the Hopp Core application, focusing on testing remote control functionality through LiveKit.

## Overview

The test suite provides automated testing for:

- **Cursor functionality** - Testing remote cursor movement, clicks, scrolling, and multi-participant scenarios
- **Keyboard functionality** - Testing remote keyboard input and character transmission
- **Clipboard functionality** - Testing clipboard operations including copy, cut, and paste with single and multiple payloads
- **Screenshare functionality** - Testing screen sharing capabilities via socket communication

## Smoke scenarios (self-checking)

`smoke.sh` runs scenarios that check their own results: each one drives a fresh core over its
socket the way the Tauri app does, joins the LiveKit room as other participants where needed, and
prints `PASS` or `FAIL` with the reason. Nobody has to watch the screen.

```bash
./smoke.sh                        # every scenario except network-drop (or: task smoke)
./smoke.sh bandwidth screenshare  # just these
./smoke.sh --net bad              # every scenario over a bad connection
./smoke.sh network-drop           # opt-in: cuts LiveKit traffic for 10 s mid-call
./smoke.sh --list
```

| Scenario | Checks |
| --- | --- |
| `call-lifecycle` | Core joins the room (seen by another participant), reports itself and the others, leaves on `CallEnd`, goes idle (CPU) |
| `call-restart` | Five calls back to back, each joined and ended |
| `call-end-race` | Calls ended before their room connects don't break the next call |
| `stale-call-end` | A late `CallEnd` for the previous call is only acknowledged; the current call stays up |
| `bandwidth` | Low-bandwidth mode: local toggle, request sent to others, remote request, requester leaving |
| `viewer-hang` | Core views a fake sharer through a mute/unmute storm, keeps answering, goes idle after the call |
| `screenshare` | Core shares the screen, another participant receives frames, the share stops cleanly |
| `hevc-fallback` | Core shares in HEVC; a watcher without HEVC joins and within 2 s the share is H.264 and the viewer receives it (needs an M3 or newer) |
| `track-replace` | A fake sharer replaces its HEVC screen track with an H.264 one, as core does on a codec switch; core subscribes to it and keeps the share open |
| `network-drop` | The call survives a 10 s outage of all LiveKit traffic (opt-in, needs sudo) |

One-time setup:

- `brew install livekit`. The script starts `livekit-server --dev` when nothing answers at
  `LIVEKIT_URL` (default `ws://localhost:7880`, key `devkey` / `secret`).
- Stable Rust (`CARGO_TOOLCHAIN=<name> ./smoke.sh` builds with another rustup toolchain).
- The terminal running the script needs Microphone access (every call starts the mic) and, for
  `screenshare` and `hevc-fallback`, Screen Recording. macOS asks on the first run; re-run after
  granting.

The fake participants announce HEVC support the way core does (the `hopp_hevc` attribute, spec
0007), so core shares HEVC to them; `Participant::join_without_hevc` joins like an older client or
a browser viewer. Like the backend's, the harness's audio tokens may set their own attributes.

### Network conditions

`netem.sh` throttles traffic with macOS's built-in shaper (dummynet), so calls run over an emulated
connection. It needs root and only touches its own pf anchor and pipes.

```bash
sudo ./netem.sh bad                   # good | medium | bad | 3g | drop (see the table in the script)
sudo ./netem.sh 3g --remote <host>    # throttle a real call through <host> instead of local LiveKit
sudo ./netem.sh drop --for 10         # 10 s outage, then back to the previous profile
sudo ./netem.sh status
sudo ./netem.sh off
```

Locally it shapes LiveKit's ports on `lo0`: upload (to the server) is mostly the sharer's video,
download is what viewers receive. Switching profiles takes effect immediately, also mid-call, so it
works for manual testing with the real app too. `smoke.sh --net <profile>` runs the suite with a
profile applied and turns it off at the end.

### Notes

`viewer-hang` opens core's screen-share window for about half a minute. Logs land in `out/`:
`<scenario>.log` (harness) and `<scenario>.core.log` (core). The exit status is the number of
failed scenarios. The scenarios live in `src/smoke.rs`; the socket client they share (request ids,
call ids, keepalive) is `src/ipc.rs`.

## Text clarity

`clarity.sh` measures how sharp text stays in a screen share, H.264 against HEVC (spec 0007).
It renders an editor page with real code, shares it through the fake participants with core's
encoding settings (no core runs), one frame per change as core's capture would, and scores each
decoded frame against the one sent. Needs `brew install livekit ffmpeg` and Xcode's `swift`.

```sh
./clarity.sh                   # normal mode, 3 rounds per codec
./clarity.sh --low-bandwidth   # 900 kbps, 15 fps, scaled to 1660x1080
```

Output: `out/clarity/<label>/report.txt` and 1:1 / 3x crops (sent, H.264, HEVC).

## Profiling

`profile.sh` keeps an emulated call busy and records core with Instruments' Time Profiler, so CPU
work can be compared before and after a change without a second person.

```bash
./profile.sh viewer                         # core watches a fake share and two fake cameras
./profile.sh sharer                         # core shares this screen; a fake viewer draws on it
./profile.sh viewer --seconds 30 --label before-fix --net bad
./profile.sh sharer --codec h264                # force core's codec (default: core picks)
./profile.sh sharer --switch-after 30           # a watcher without HEVC joins 30 s into the load
./profile.sh viewer --codec h265 --switch-after 30  # the fake sharer switches to H.264 mid-run
```

| Role | Load |
| --- | --- |
| `viewer` | A fake participant shares a scrolling text page at 3024x1964 and 40 fps (H.264, or `--codec h265`; 12 Mbps, core's own maximums), as a screencast source that keeps its resolution, like core's own share. It and a second fake participant send 720p cameras at 30 fps and quiet audio. Core opens its screen-share and camera windows. |
| `sharer` | Core joins, waits 7 s (past the 5 s after which it stops its muted tracks' keepalive frames), then shares the main display (or `HOPP_TEST_SCREEN_ID`) at the app's default 4K setting. A fake viewer, which supports HEVC, watches it, reports the size, frame intervals, time to first frame and the codec it received, and sends camera and audio. It alternates 4 s of cursor movement and 4 s of drawing on the overlay, at 60 events/s. |

- **What you get:** the release core is built as shipped, with symbols. After a 5 s warm-up,
  `energy.py` reads core's CPU time, wake-ups and energy from the kernel for 20 s
  (`--energy-seconds`, 0 skips it) with no profiler attached; then the Time Profiler records.
  Results go to `out/profile/<label>-<role>.*`:
  - `.svg`: the flamegraph; open it in a browser and hover a frame for its share.
  - `.txt`: CPU per thread, the functions with the most self time, core's own functions by total
    time, the energy line, core's `FramePacing` lines (every 10 s: the size the screen-share
    window showed, presented fps, present intervals, render time and frame age, frames never
    shown), the fake share's encoder (viewer run) or what the fake viewer received and its
    decoder (sharer run), any codec switch, and core's `VideoHealthSummary` (codecs and
    encoder/decoder implementations). Diff two labels' `.txt` for a before/after comparison.
  - `.folded`: stacks for other flamegraph tools (inferno, speedscope).
  - `.trace`: the raw recording; open it in Instruments.
  - `.log` and `.core.log`: the harness's and core's logs. The sharer run's `.log` ends with the
    fake viewer's report (also printed at the end of the run, and copied into the `.txt`).
- **Codec switches:** with `--switch-after`, the sharer run's report has a `fake viewer switched`
  line (first frame of the new track after the watcher joined, and the gap since the previous
  frame); in the viewer run, core's log shows `replaced screen track` and its `FramePacing` lines
  carry on.
- **Check the size in `FramePacing`:** WebRTC's own stats count decoder drops only, and a share
  that WebRTC scaled down still looks healthy there. With the harness on crates.io livekit, the
  "3K" share arrived at 1128x732.
- **A baseline needs the old core:** `profile.sh` builds core from the working tree, so check out
  the old `core/src` for a baseline run. You can restore it once the run prints "Starting core".
- **Runs are noisy:** two viewer runs of the same core used 461 and 533 mW. Repeat a run before
  trusting one number.
- **Sharer runs measure what's on screen:** ScreenCaptureKit sends no frames for a static screen,
  so play a video or scroll something during the run, and compare sharer runs only roughly. The
  capture rate also depends on the refresh rate of the shared display, or of the monitor a shared
  window is on (40 fps at 120 Hz, 60 fps at 60 or 144 Hz: `bandwidth_mode::capture_framerate`),
  so compare runs on the same display.
- **The fake viewer never clicks or types:** core would replay those on the real pointer and
  keyboard. Its cursor and drawings only appear on the overlay.
- **Audio:** the fake participants send noise at about -60 dBFS. That keeps core's remote-audio
  path busy, with no silence for DTX to skip, while staying close to inaudible.
- **Not covered:** input from core's own user (mouse over the screen-share window, local
  drawing). Core's own camera stays off.
- **Needs** LiveKit, Xcode's `xctrace`, and Microphone access for the terminal (plus Screen
  Recording for `sharer`), like the smoke scenarios. The load lives in `src/profile.rs`, the report
  in `flamegraph.py`.

## Manual scenarios

The commands below drive core and fake participants, but most of them need a person watching the
screen to judge the result.

## Prerequisites

- Rust (latest stable version)
- LiveKit server instance with API credentials
- Core process running (use `task dev` from the core directory). Core exits when the test disconnects, so restart it between runs

## Setup

### 1. Install Dependencies

```bash
cargo build
```

### 2. Environment Variables

Set the following environment variables:

- `LIVEKIT_URL`: The WebSocket URL of your LiveKit server
- `LIVEKIT_API_KEY`: Your LiveKit API key
- `LIVEKIT_API_SECRET`: Your LiveKit API secret
- `HOPP_TEST_SCREEN_ID`: CoreGraphics id of the display to share (defaults to the main display)

## Usage

Run tests using cargo:

```bash
cargo run -- <command> [options]
```

### Available Commands

#### Cursor Tests

Test various cursor functionalities:

```bash
# Basic cursor tests
cargo run -- cursor complete          # Run complete cursor test for single cursor
cargo run -- cursor click             # Test cursor clicking
cargo run -- cursor move              # Test cursor movement
cargo run -- cursor scroll            # Test cursor scrolling

# Multi-participant tests
cargo run -- cursor multiple-participants    # Test multiple participants
cargo run -- cursor cursor-control          # Test multiple cursors with control handoff
cargo run -- cursor staggered-joining       # Test staggered participant joining
cargo run -- cursor same-first-name-participants  # Test participants with same first names

# Advanced cursor behavior tests
cargo run -- cursor hide-on-inactivity      # Test cursor hiding after inactivity
cargo run -- cursor concurrent-scrolling    # Test concurrent scrolling scenarios
```

#### Keyboard Tests

Test keyboard input functionality:

```bash
# Test keyboard character input (lowercase, uppercase, numbers, symbols)
cargo run -- keyboard
```

#### Clipboard Tests

Test clipboard functionality:

```bash
# Test paste with single payload
cargo run -- clipboard paste-single

# Test paste with multiple payloads
cargo run -- clipboard paste-multiple

# Test add to clipboard (copy)
cargo run -- clipboard add-copy

# Test add to clipboard (cut)
cargo run -- clipboard add-cut
```

**Note:** The `add-copy` and `add-cut` tests require user interaction. You will be prompted to select text in any application before the test executes the copy/cut operation.

#### Screenshare Tests

Test screen sharing functionality:

```bash
# Test screenshare capabilities via socket communication
cargo run -- screenshare
```

### Help

Get help for available commands:

```bash
# General help
cargo run -- --help

# Help for specific commands
cargo run -- cursor --help
```

## License

This project follows the same license as the parent Hopp Core project.
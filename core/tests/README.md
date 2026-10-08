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
| `network-drop` | The call survives a 10 s outage of all LiveKit traffic (opt-in, needs sudo) |

One-time setup:

- `brew install livekit`. The script starts `livekit-server --dev` when nothing answers at
  `LIVEKIT_URL` (default `ws://localhost:7880`, key `devkey` / `secret`).
- Stable Rust (`CARGO_TOOLCHAIN=<name> ./smoke.sh` builds with another rustup toolchain).
- The terminal running the script needs Microphone access (every call starts the mic) and, for
  `screenshare`, Screen Recording. macOS asks on the first run; re-run after granting.

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
- `HOPP_TEST_SCREEN_ID`: Display to share (defaults to `0`)

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
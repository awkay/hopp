# Core and Tauri audit: nightly clippy, lock scope, allocator

- **Commit audited:** `d9ed322` (branch `test/harness-smoke-ci`), 2026-10-08.
- **Scope:** the macOS / Apple Silicon build of `core/` (workspace, including `socket_lib` and
  `sentry_utils`) and the Tauri backend `tauri/src-tauri/`. The Windows and Linux code is upstream's
  and was not reviewed.
- **Companion:** [core-perf-hot-paths-and-libraries.md](core-perf-hot-paths-and-libraries.md) covers
  loops, vectorization, frame copies and dependencies. Where this file overlaps, it links there.
- **Method:** three passes.
  1. Nightly clippy with every lint group except `restriction`, on both crates.
  2. A code read of every `significant_drop_tightening` hit from pass 1, plus the same files for lock
     scopes clippy can't see.
  3. Allocator benchmarks: core's hot-path code copied into a scratch crate outside the repo and run
     under the system allocator, mimalloc and jemalloc, with a counting wrapper.

  The app was not run, so nothing here is a whole-app profile.

## Summary

- **Nightly clippy:** switching CI to nightly with only the default lints would add one failure, a
  rustc warning. Adding `pedantic`, `nursery` and `cargo` reports 1,860 warnings, mostly mechanical or
  noise for an app. See [Nightly clippy](#nightly-clippy).
- **Lock scope:** two real problems in core, both on LiveKit's `room` lock: it is held across
  `publish_data().await`, and the stats loop holds it across a `get_stats().await` per track. Both
  stall remote-control input. Nothing real in Tauri. 13 hits must not be tightened. See
  [Lock scope](#lock-scope).
- **Allocator:** don't switch to mimalloc or jemalloc. It would save well under 1% of a core, the
  largest buffers are allocated in C++ where a Rust allocator has no effect, and it has real downsides
  on macOS. Fix the hot-path allocations instead, and measure a real call first. See
  [Allocator](#allocator).
- **Found twice:** the perf review (finding 2) and the allocator review both found that
  `capture/macos_stream.rs:446-474` overwrites an `NV12Buffer` that libwebrtc may still be encoding on
  another thread. Two independent reads agree, so it's worth fixing.

## Nightly clippy

### How it was run

Nightly 1.101.0 (2026-10-07), with all four groups as warnings so the totals are complete:

```sh
# core/ (--keep-going: --all-targets also builds tests/src/main.rs as an integration test of
# hopp_core, which fails to compile; see "Known noise" below)
cargo +nightly clippy --keep-going --workspace --all-targets --all-features \
  --target aarch64-apple-darwin -- \
  -W clippy::all -W clippy::pedantic -W clippy::nursery -W clippy::cargo

# tauri/src-tauri/ (needs tauri/dist/ and core/target/debug/hopp_core-<triple>, as in CI)
SENTRY_DSN_RUST="" cargo +nightly clippy --all-targets --all-features -- \
  -W clippy::all -W clippy::pedantic -W clippy::nursery -W clippy::cargo
```

`clippy::restriction` was left out on purpose. It is a menu of opinionated lints, some of which
contradict each other, and clippy itself warns against enabling it wholesale
(`blanket_clippy_restriction_lints`).

### Results

Counts are deduplicated: the lib and its test target report the same hit once each.

| | Default lints, nightly | All four groups | Auto-fixable (`--fix`) | Distinct lints |
|---|---|---|---|---|
| core (workspace) | 1 | 1,571 | 947 | 75 clippy + 1 rustc |
| Tauri backend | 0 | 290 | 124 | 41 |

**Default lints only:** the one hit is a rustc warning, not clippy:
`recursion_depth_exceeding_limit` at `core/src/graphics/yuv_renderer.rs:382`, while proving
`YuvPipeline: Sync` through wgpu's types. With `-D warnings` it fails the build on nightly.

**Core, by kind:**

- **Mechanical, mostly auto-fixable:** `use_self` 266, `uninlined_format_args` 107,
  `cast_lossless` 97, `doc_markdown` 89, `missing_const_for_fn` 78, `redundant_closure_for_method_calls`
  29, `manual_let_else` 28.
- **Numeric casts, manual review, mostly intentional:** `cast_possible_truncation` 130,
  `cast_precision_loss` 51, `cast_sign_loss` 43, `cast_possible_wrap` 37. They sit in graphics and
  audio code.
- **Library-API docs, noise for an app:** `must_use_candidate` 59, `missing_panics_doc` 46,
  `missing_errors_doc` 36.
- **Worth reading:** `significant_drop_tightening` 54. See [Lock scope](#lock-scope).
- **Not fixable by us:** `multiple_crate_versions` (duplicate versions in the dependency tree) and
  `cargo_common_metadata` (package metadata we don't publish).
- **By file:** `src/windows/colors.rs` 258 (almost all `use_self`), `src/room_service.rs` 126,
  `src/lib.rs` 111.

**Tauri, by kind:**

- `needless_pass_by_value` 73: almost all are `#[tauri::command]` functions taking `AppHandle` or
  `String` by value, which Tauri requires. Noise.
- `missing_panics_doc` 27, `must_use_candidate` 27, `missing_errors_doc` 14: library-API docs, noise.
- `significant_drop_tightening` 13. See [Lock scope](#lock-scope).

### Known noise

`cargo clippy --all-targets` in `core/` compiles `core/tests/src/main.rs` as an integration test named
`src` (Cargo's `tests/<name>/main.rs` autodiscovery) and fails with 9 errors (missing `rand`,
`futures`, `ctrlc`, `livekit_api`, and API drift). These aren't lint findings. Core's CI clippy step
doesn't pass `--all-targets`, so it doesn't hit this.

### If this goes into CI

1. Pin a dated nightly (e.g. `nightly-2026-10-07`). A floating nightly can break CI on any day.
2. Put the groups in a `[lints.clippy]` table in each `Cargo.toml`, so local runs and CI agree.
3. Allow the noise: the library-API doc lints, `needless_pass_by_value` in Tauri,
   `multiple_crate_versions` and `cargo_common_metadata`.
4. Run `cargo clippy --fix`, then fix the rest by hand. None of the `significant_drop_tightening`
   suggestions are machine-applicable, so `--fix` won't touch the locks in
   [Don't tighten](#dont-tighten).

## Lock scope

`clippy::significant_drop_tightening` (nursery) flags a lock guard held past its last use. Every hit
was read in context and classified:

- **Real:** held across something slow or blocking, with a contender that suffers.
- **Harmless:** trivial code after the last use, or an uncontended lock.
- **Must stay held:** the guard deliberately covers a read-modify-write or check-then-act; tightening
  would add a race.
- **False positive:** the guard is still borrowed or moved, or a `break` / return follows.

| | Real | Must stay held | Harmless | False positive | Total |
|---|---|---|---|---|---|
| core | 21 | 3 | 16 | 14 | 54 |
| Tauri | 0 | 10 | 3 | 0 | 13 |

### Real problems, ranked

#### 1. `room` lock held across `publish_data().await`

- **Where:** `core/src/room_service.rs`, 21 hits: 1562, 1586, 1611, 1632, 1654, 1677, 1700, 1723,
  1753, 1778, 1803, 1936, 1960, 1984, 2008, 2034, 2062, 2088, 2114, 2154 (the command task) and
  2530 (`publish_bandwidth_mode_request`).
- **What's slow:** in our LiveKit fork (`rtc_session.rs`), `publish_data` first waits out any
  reconnect (`wait_reconnection()`), then waits up to `ICE_CONNECT_TIMEOUT` (15 s) for the publisher
  connection, then waits until the reliable data channel's buffered amount drops below threshold.
  That last wait is backpressure, so it bites exactly on congested and low-bandwidth calls.
- **Who waits:** the `handle_room_events` task, which takes the lock through
  `update_camera_quality` / `apply_bandwidth_mode` on camera publish, mute, unmute, subscribe and
  unpublish, on `ParticipantDisconnected`, and on remote bandwidth requests; and `stats_loop`. While
  a publish is stuck, the room-event loop stops at that event, and everything queued behind it
  waits: `DataReceived` (remote-control input on the sharer), screen-share `TrackSubscribed`,
  snapshots, `CloseCameraWindow`.
- **The lock buys nothing here:** every publish runs on the one sequential command task, so ordering
  is already guaranteed, and the other lockers only read.
- **Fix:** `room.local_participant()` returns an owned clone. Take it and drop the guard before
  awaiting:

  ```rust
  let Some(local_participant) = inner.room.lock().await.as_ref().map(Room::local_participant) else {
      log::warn!("room_service_commands: Room doesn't exist");
      continue;
  };
  ```

  Clippy's suggested drop point, right after `let room = inner_room.as_ref().unwrap()`, doesn't
  compile: `room` borrows the guard. At 2154 (App Veil), the later write to
  `published_app_veil_snapshot` needs no room lock; only the command task writes it.

#### 2. `stats_loop` holds both room locks across every `get_stats().await` (clippy can't see it)

- **Where:** `core/src/livekit/stats.rs:503-511`.
- **What happens:** once a second it locks `room`, then `video_room`, and holds both while it awaits
  `track.get_stats()` for every local and remote video track in turn. Each call is a round-trip to
  libwebrtc's signaling thread: milliseconds normally, more when the encoder is busy.
- **Consequence:** once a second, every command-task publish waits for the whole stats pass. That
  covers the controller's mouse, click, key, wheel and draw events, and the sharer's cursor position
  (up to 100 Hz), so it shows up as a periodic input-latency spike during remote control.
  `handle_room_events` blocks too if it needs the lock then.
- **Fix:** under the locks, take `room.local_participant()`, `room.remote_participants()` and
  `video_room.map(Room::local_participant)` (owned clones, and all `collect_stats` uses), drop both
  guards, then run `collect_stats` on those.

#### 3. Low: room teardown awaits under the lock

- **Where:** `core/src/room_service.rs:339-353` (`clear`), DestroyRoom (`:1530-1552`) and
  UnpublishAudioTrack (around `:1852-1858`).
- **What happens:** `room` / `video_room` stay locked across `room.close().await`,
  `publisher.unpublish(room).await` and `unpublish_track().await`. Contenders are mostly dying tasks,
  plus the previous call's stats task during CreateRoom's `clear()`, since that task is aborted only
  after the new connect.
- **Fix:** `.take()` the `Room` out under the lock and close it after dropping the guard.

#### 4. Low, rare: capture restart under the `screen_capturer` lock

- **Where:** `core/src/capture/capturer.rs:497-499`.
- **What happens:** the `poll_stream` thread holds `screen_capturer` across `restart_stream()`: a
  200 ms sleep, a blocking ScreenCaptureKit stop and start, up to 10 retries with 100 ms sleeps, and
  a 2 s Sentry flush before `exit(2)`. The event loop, on the macOS main thread that drives every core
  window, locks `screen_capturer` on `CaptureFrameChanged`, `stop_screenshare`,
  `SetAppVeilBundleIds`, `RefreshAppVeilFilter` and `CallEnd`, so it freezes for the whole restart.
  This only happens after a capture failure.
- **No deadlock:** ScreenCaptureKit's completions don't need the main thread.
- **Fix (not a one-liner):** take the stream out, restart it unlocked, and use a generation check
  against a concurrent stop.

### Tauri: no real problems

- Every lock is `std::sync`, and Tauri command futures must be `Send`, so no guard can be held across
  `.await`.
- Lock order is clean: `call` is never nested with `settings`. `call` leads to `sleep_prevention`,
  then `pending`, and `run_on_main_thread` posts don't block.
- **Design note, not a bug:** settings setters write a small JSON file (no fsync) under `settings`,
  and the core-event dispatcher locks `settings` on every `ParticipantsSnapshot`
  (`core_events.rs:54`). A slow disk write delays core events. Holding it is intentional: it keeps
  saves in the same order as messages to core.

### Every hit

**Tauri** (`tauri/src-tauri/src/`):

| Hit | Class | Why |
|---|---|---|
| `call_state.rs:77` `begin_call` | must stay held | `current_call_id` mirror written under the call lock so mirror writes follow transition order. Clippy's suggestion deletes the publish line. |
| `call_state.rs:209` `on_call_ended` | must stay held | transition, mirror and `apply_call_ended_effects` posted in transition order; only a log follows |
| `call_state.rs:232` `reset_for_core_restart` | must stay held | the suggestion moves publish and effects out of the lock |
| `main.rs:83` `get_available_content` | must stay held (through the first send) | reading `remote_control_enabled` then sending `ControllerCursorEnabled` must not interleave with `set_remote_control_enabled`'s save and send, or core keeps a stale value |
| `main.rs:156` `stop_sound` | harmless | unbounded mpsc send and a debug log |
| `main.rs:358` `set_last_used_camera` | must stay held | settings rule: save and enqueue in one critical section |
| `main.rs:408` `set_sharer_draw_persist` | must stay held | same rule |
| `main.rs:475` `set_livekit_url` | must stay held | compare, set, send; a core restart re-sends the URL under the same lock |
| `main.rs:497` `set_sentry_metadata` | must stay held | same rule |
| `main.rs:704` `set_app_veil_applications` | must stay held (through the send) | App Veil list ordering; only the error-path `map_err` could move out |
| `main.rs:782` `update_user_setting_and_send` | must stay held | the documented helper for the settings rule (`lib.rs:59-64`) |
| `main.rs:1129` single-instance `location_set` | harmless | tokio task; window setters are fire-and-forget |
| `main.rs:1479` `reopen_requested` | harmless | main thread only; `window.hide()` and a log |

**Core** (`core/`):

| Hit | Class | Why |
|---|---|---|
| `socket_lib/src/lib.rs:382` | must stay held (and a false positive) | `write_all` must hold the stream mutex so frames don't interleave; only `Ok(())` follows |
| `src/audio/mixer.rs:55` | harmless | nothing after the cap loop |
| `src/audio/mixer.rs:156` | false positive | `mixed` borrows the guard through APM and resampling |
| `src/audio/mixer.rs:242` | false positive | guard used later; order is `inner` then `mixer` |
| `src/audio/mixer.rs:257` `reconnect` | harmless (rare) | held across the CoreAudio device open and old stream drop; only `add_source` waits |
| `src/camera/capturer.rs:144` | false positive | the binding is the `LockResult`, moved by `unwrap` |
| `src/camera/stream_macos.rs:178` | must stay held | `stop_capture` clears the state under this lock; holding it for the whole frame guarantees no frame is delivered after stop |
| `src/capture/capturer.rs:475` | false positive | `LockResult`, shadowed |
| `src/capture/capturer.rs:480` | harmless | `rx` is used only by the `poll_stream` thread |
| `src/capture/macos_stream.rs:387` | harmless | compare-and-set of the resize target; block ends right after |
| `src/capture/macos_stream.rs:399` | harmless | frame compare and update plus an unbounded send |
| `src/input/mouse.rs:700` | harmless | unbounded redraw send and a log |
| `src/input/mouse.rs:798` | false positive | `break` follows |
| `src/input/mouse.rs:820` | harmless | `sharer` guard across a non-blocking `send_event` |
| `src/input/mouse.rs:873` | false positive | `break` follows |
| `src/input/mouse.rs:894` | harmless | same as 820 |
| `src/input/mouse.rs:922` | false positive | only the return value follows; the block exists to release `ctrls` before taking `sharer` |
| `src/input/mouse.rs:954` | false positive | same |
| `src/input/mouse.rs:995` `update_cursors` | harmless | `sharer` held while taking `ctrls` matches the documented order |
| `src/input/mouse_macos.rs:155` | harmless | tap thread holds `sharer` across `CGWarpMouseCursorPosition`; could drop before the warp |
| `src/input/mouse_macos.rs:174` | harmless | `hide(true)` must stay under `sharer` (it takes `ctrls` inside, the documented order) |
| `src/lib.rs:1050` | false positive | `LockResult` shadowed; the real guard is dropped at 1057 |
| `src/lib.rs:1069` | harmless | `camera_capturer` is locked only on the event-loop thread |
| `src/lib.rs:1381` | false positive | `Drop` impl; last use is the last statement |
| `src/room_service.rs:990` | false positive | `info` borrows the guard; `write()` where `read()` would do |
| `src/room_service.rs:1457` | harmless | moves the `Room` in; no await after |
| `src/room_service.rs:1562` … `:2154` (20 hits) | real | problem 1 |
| `src/room_service.rs:2410` | false positive | `info` borrows the guard; block ends |
| `src/room_service.rs:2530` | real | problem 1 |
| `src/room_service.rs:2564` | harmless (minor) | std mutex across the synchronous `set_encoding_parameters` and a log; rare path |
| `src/room_service.rs:2633` | false positive | no await inside; block ends |
| `src/room_service.rs:3096` | must stay held (and a false positive) | used in the speaker loop to the end of the block; clearing then setting the speaking flags must be one critical section |
| `src/snapshot_sender.rs:50` | harmless | only `collect()` follows; the socket send happens after |
| `src/window/camera_window.rs:1492` | false positive | `sorted` borrows the guard for the whole render; read lock, CPU only |
| `src/window/screensharing_window.rs:2458` | harmless (rare) | on a size change, `latest_frame` is held across winit resize calls; copying the size and dropping is safe |

### Don't tighten

Hand-applying clippy's suggestion to these adds a race:

- **Tauri `call_state.rs:77`, `:209`, `:232`:** the call transition, the `current_call_id` mirror and
  the main-thread effects must update together and in order. At 77 and 232 the suggestion moves
  `publish_current_call` / `apply_call_ended_effects` out of the lock.
- **Tauri `main.rs:83`, `:358`, `:408`, `:475`, `:497`, `:704`, `:782`:** the settings rule
  "save and enqueue in one critical section" (`lib.rs:59-64`). 83 is the easy one to miss: dropping
  right after the read lets a concurrent remote-control toggle be overwritten in core by the stale
  value.
- **Core `camera/stream_macos.rs:178`:** `stop_capture` exclusion.
- **Core `room_service.rs:3096`:** speaking flags must update atomically.
- **Core `socket_lib/src/lib.rs:382`:** frame atomicity on the IPC socket.
- **Core `input/mouse.rs`, every hit:** shortening a scope is fine, reordering is not. Never take
  `sharer_cursor` while holding `controllers_cursors`: that's an ABBA deadlock with the event-tap
  thread (documented at `mouse.rs:516-522` and `mouse_macos.rs:44-49`).

## Allocator

**Recommendation: don't switch to mimalloc or jemalloc. Fix the hot-path allocations, and measure a
real call before revisiting.** Confidence is high.

### Measured

Apple M4, macOS 15.7.9. Core's code copied into a scratch crate, run once per allocator with a
counting wrapper:

| Scenario | allocs/op | System | mimalloc v3 | jemalloc |
|---|---|---|---|---|
| DTLN denoiser (`audio/denoiser.rs` feed), per 8 ms block | 473 (152 KB) | 107–112 µs | 96–98 µs | 98–101 µs |
| Output-callback pattern (`audio/mixer.rs:141-208`) | 2 | 160–230 ns | 136–145 ns | 137–143 ns |
| 320 B `Vec` allocated on one thread, freed on another | 1 | 218 ns | 65 ns | 44–58 ns |
| Small alloc + free, 16 B–4 KB | 1 | 18 ns | 6 ns | 9 ns |
| 3 MB buffer per frame, fully written | 1 | 143 µs | 45 µs | 44 µs |

- **Denoiser:** the largest Rust allocation source, about 59k allocations/s and 19 MB/s whenever the
  mic is on (noise cancellation defaults to on, `core/src/lib.rs:519`). A swap saves about 12 µs per
  block, roughly 0.15% of one core.
- **Fixing it in our code doesn't help:** reusing tract's `SimpleState` gives bit-identical output but
  removes only 10 of the 473 allocations. The rest are per-op output tensors inside tract
  (`tract-core-0.21.4/src/plan.rs:121`).
- **3 MB per frame:** `sample` shows libmalloc's `free_medium` calling `madvise` on every free, so the
  next frame re-faults its pages. In core this pattern is in libwebrtc (C++), which a Rust allocator
  doesn't reach.
- **Binary size:** mimalloc adds about 194 KB, jemalloc about 466 KB, against a 61.7 MB `hopp_core`.
- **Tooling:** with mimalloc, `heap` saw 182 nodes where the system allocator showed 1,000,189 for the
  same live data. The Rust heap disappears from `heap`, `leaks` and Instruments Allocations.
- **Idle `hopp_core` today (`vmmap`):** 20.4 MB footprint on classic libmalloc (nano zone plus helper
  zone, not xzone), 40k allocations, 32% fragmentation (3.9 MB).

### Where the allocations are

**Per frame** (bytes are mostly in C++ and IOSurface memory, which a Rust allocator never sees):

- `core/src/livekit/video.rs:204`: `to_i420()` on every decoded viewer frame. VideoToolbox outputs a
  `CVPixelBuffer`, so libwebrtc allocates a new full-frame I420 buffer each time (about 3 MB at
  1080p, up to 40 fps), then `:211` copies it again. The largest byte volume in the process. Same as
  perf finding 3.
- `core/src/camera/stream_macos.rs:320`: `i420.scale()` allocates a new C++ buffer per camera frame
  when downscaling. Same as perf finding 5.
- `core/src/capture/macos_stream.rs:446-474`: the `NV12Buffer` is reused, so no Rust allocation per
  frame; ScreenCaptureKit buffers are IOSurface memory.
- libwebrtc's frame wrapper: about 2 tiny Rust allocations per frame
  (rust-sdks `native/video_stream.rs:136`).
- `core/src/graphics/yuv_renderer.rs:262-353` already reuses its staging buffers; what's left is
  wgpu-core bookkeeping.
- The iced UI tree is rebuilt on every redraw (`window/screensharing_window.rs:2567-2592`, and the
  overlay at `graphics/iced_renderer.rs:151-171` every 16 ms while active). Many small Rust
  allocations, not counted without running the app.

**Per audio buffer** (every 10 ms):

- `core/src/audio/denoiser.rs:111`, `:118-121`, `:158`, `:165-168`: the 473 per block.
- `core/src/audio/capturer.rs:326`: `to_vec`.
- `core/src/livekit/audio.rs:206` and `core/src/audio/mixer.rs:56`: the same remote frame copied twice
  more after libwebrtc already copied it (rust-sdks `audio_stream.rs:93`).
- `core/src/audio/mixer.rs:93-99`: those `Vec`s are freed on the CoreAudio real-time thread.
- `core/src/audio/mixer.rs:144` (`split_off`) and `:186-189` (`collect`): allocations inside the
  real-time callback. Same as perf finding 8.
- rust-sdks `audio_source.rs:150-160`: a oneshot channel and a `Box` per frame.

**Per event:** cursor publish (`room_service.rs:1556-1583`: `serde_json::to_vec`, the topic `String`,
a `DataPacket`; capped at 100/s by the throttle), receive (`room_service.rs:2849-2870`: deserialize
plus an identity `String`), IPC (`socket_lib/src/lib.rs:396`, `:421-423`). Low rate.

**Cold:** stats (1 Hz), snapshots, device lists.

**Rust vs everything else:** by count, Rust probably dominates (denoiser plus UI churn). By bytes,
libwebrtc's per-frame buffers dominate, and IOSurface and Metal memory sit outside malloc entirely.

### Why not switch

- **Throughput:** about 12 ns saved per small allocation. Even at a few hundred thousand allocations a
  second, that's a fraction of a percent of one core.
- **Contention:** low. The allocation-heavy Rust threads are the main/render thread, the audio runtime
  thread and the CoreAudio I/O thread.
- **Memory:** Zed (a Rust GPU app with LiveKit calls) switched to mimalloc and back in PR #11293:
  peak 1.6 GB against 991 MB with the system allocator, about 1.5 GB held after the work was done,
  and no better frame times.
- **jemalloc on macOS:** no background purge threads (its `configure.ac` disables them for Mach-O),
  4 × ncpu arenas. Its upstream repo was archived in June 2025 and unarchived in March 2026.
- **Freeing across allocators:** a Rust-only allocator is safe only while nothing frees memory from
  the other side. Our code and the cxx bridges are fine, but the risk is real: screencapturekit 8.0.1
  (`src/cm/audio.rs:196-203`) carries a fix for a Swift-allocated buffer freed through the Rust
  allocator, which "crashes when a custom allocator like mimalloc is active".
- **Override mode** (to cover libwebrtc too): mimalloc's static override defines `malloc` in the
  executable and swaps the default malloc zone; jemalloc's uses the same `zone_register` trick. Ruled
  out with ScreenCaptureKit, CoreAudio and VideoToolbox in the process.
- **mimalloc v3 on macOS:** uses private TLS slots. v3.4.0 and v3.4.1 corrupted the heap at thread exit
  for arm64 binaries linking CoreFoundation (issue #1333). Crate 0.1.52 bundles v3.3.2, which predates
  it, but a crate bump could pull in a bad version.
- **Signing:** no effect on hardened runtime or notarization. Both are static C, add no dylib (the
  `build-macos.sh` system-library guard stays green), and need no entitlement.
- **Tauri `hopp`:** low allocation rate, and the WebView's memory lives in a separate process. No
  benefit.

### Fix the causes instead

1. **Real-time audio callback** (`audio/mixer.rs:144`, `:186-189`, `:56`, `:99`): stop allocating and
   freeing on the CoreAudio thread. Keep a buffer with a read offset and refill it with `clear()` +
   `extend()`; give each source a ring buffer (`rtrb`, see perf L2). The reason is glitch safety, since
   malloc can block, not throughput.
2. **Viewer `to_i420`** (`livekit/video.rs:204`): take `NativeBuffer::get_cv_pixel_buffer()` and
   convert NV12 to I420 straight into the reused `VideoBuffer` with `yuv_sys::rs_NV12ToI420`, as the
   camera already does at `stream_macos.rs:219`. Better: upload NV12 to the GPU (perf finding 3).
   Removes the per-frame C++ allocation, its roughly 100 µs `madvise` and re-fault cost, and one
   full-frame copy.
3. **Camera scaling** (`camera/stream_macos.rs:320`): `rs_I420Scale` straight into
   `stream_frame.buffer`.
4. **Trivial copies:** pass `frame.data.into_owned()` at `livekit/audio.rs:206` instead of copying, and
   drop the `to_vec` at `audio/capturer.rs:326`.
5. **Denoiser:** leave it; its allocations are inside tract.
6. **Swapping the allocator** comes last, as a complement, not a substitute. After 2 and 3 it would only
   speed up tract and iced churn.

### How to measure, and what would change the answer

Run a real two-person call with a screen share and the mic on (camera optional), on the dev build so
the process can be attached. The smoke scenarios last only seconds.

- **CPU, on both sharer and viewer:**
  `xcrun xctrace record --template 'Time Profiler' --attach hopp_core --time-limit 60s --output core-cpu.trace`.
  Invert the call tree and sum `libsystem_malloc`.
- **Volume and the Rust vs C++ split:**
  `xcrun xctrace record --template 'Allocations' --attach hopp_core --time-limit 60s --output core-alloc.trace`.
  Group by caller: Rust symbols vs `webrtc::` / libyuv vs Apple frameworks. Only works on the system
  allocator.
- **Footprint over a 1–2 hour call:**

  ```sh
  while true; do
    date
    vmmap --summary "$(pgrep -x hopp_core)" | grep -E 'Physical footprint|DefaultMallocZone|MallocHelperZone|^TOTAL  '
    sleep 300
  done | tee footprint.log
  ```

- **Optional:** a counting `#[global_allocator]` behind an `alloc-stats` feature in `core/src/main.rs`
  that logs allocations/s, bytes/s and live bytes every 10 s. `dhat` is heavier and writes its report
  only at exit.

**Thresholds:**

- malloc under 2% of `hopp_core`'s samples: leave the allocator alone.
- Over 5%, with Rust callers dominating: A/B test mimalloc.
- Footprint growing more than about 100 MB/hour while allocated bytes stay flat and fragmentation
  rises past 50%: an allocator problem. If allocated bytes grow instead, it's a leak, and no allocator
  fixes that.

### The change, if measurements ever justify it

mimalloc rather than jemalloc: faster in nearly every benchmark above, smaller, no autoconf, and it
doesn't rely on background purge threads, which macOS doesn't get.

```toml
# core/Cargo.toml
[features]
mimalloc = ["dep:mimalloc"]

[target.'cfg(target_os = "macos")'.dependencies]
mimalloc = { version = "=0.1.52", optional = true, default-features = false }
```

```rust
// core/src/main.rs: binary only, so lib tests and the test harness keep the system allocator
#[cfg(all(target_os = "macos", feature = "mimalloc"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
```

- Pin the version exactly: the bundled C library changes with crate bumps, which is how #1333 shipped.
- Don't enable `override` or `secure`.
- Leave Tauri alone.
- Enable the feature in packaging and add a line to `DATAICO.md`.

### Sources

zed-industries/zed PR #11293 and #7140; microsoft/mimalloc readme and issues #1333 and #1327; Meta's
March 2026 jemalloc post (engineering.fb.com); the FreeBSD list thread on the June 2025 jemalloc
archive; Apple's xzone_malloc notes (cs.iossec.tech); the source of libmimalloc-sys 0.1.49 `build.rs`,
tikv-jemalloc-sys 0.7.1 `build.rs`, jemalloc `configure.ac`, and screencapturekit 8.0.1.

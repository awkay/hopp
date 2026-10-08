# Core profile: viewer run against the audit

- **Code profiled:** core at `d9ed322`, release build as shipped (LTO, symbols kept). The profiling
  tooling (`core/tests/profile.sh`, `src/profile.rs`, `flamegraph.py`) was added on top in the same
  working tree; core itself was unchanged.
- **When and where:** 2026-10-08, 14:07, Apple M4, macOS 15.7.9, display awake.
- **Companions:** [core-perf-hot-paths-and-libraries.md](core-perf-hot-paths-and-libraries.md)
  (perf findings, numbered #1–#9 and L1–L7) and [clippy-locks-and-allocator.md](clippy-locks-and-allocator.md)
  (lock and allocator findings). This file checks those findings against a measured profile.
- **Data:** everything is in [`profile/`](profile/). See [Data and how to reproduce](#data-and-how-to-reproduce).

## Method

`core/tests/profile.sh viewer --seconds 30`:

- **Room:** a local `livekit-server --dev`, restricted to loopback.
- **Core** joins a call as a viewer, with its mic on (the default) and its camera off.
- **Fake participants:**
  - "Profile Sharer" shares a scrolling text page at 3024x1964, 40 fps, H.264 at 12 Mbps (core's
    own maximums). It also sends a 720p/30 fps camera and quiet audio.
  - "Profile Camera" sends another 720p camera and quiet audio.
- **Core's windows:** core opened its screen-share window and its camera window, as it does in a
  real call.
- **Recording:** after a 5 s warm-up, Instruments' Time Profiler records core for 30 s. It samples
  only threads that are on a CPU, every 1 ms, so every number below is CPU time.
- **Result:** core used **28% of one core** on average. The `ps` CPU time and the sampled total agree.
  The fake participants ran in a separate process (36% of one core) and aren't counted.

## Where core's CPU goes

Shares come from [`profile/buckets.py`](profile/buckets.py) on the folded stacks: each stack goes
to the first matching rule.

| Where | % of one core | Audit finding | Verdict |
|---|---|---|---|
| libwebrtc network thread | 4.8 | none | libwebrtc internals: `WaitPoll` alone is 2% of a core, plus `sendto` / `recvmsg` and SRTP |
| Rendering: iced, wgpu, Metal, all windows | 4.3 | perf #4 | Not judged: the content was moving, so redraws were justified. Needs a static-content run. |
| Noise filter (DTLN in tract) | **3.7** | perf #7, alloc | **Confirmed, at 3× the audit's estimate** (see new finding 4) |
| libwebrtc worker and signaling threads | 3.7 | none | libwebrtc internals |
| Unattributed | 2.6 | | Mostly dispatch threads whose stacks Instruments couldn't unwind |
| Remote audio: decode, mix, playout | 1.6 | perf #8 | Low CPU, as the audit said; #8 is about glitches on the real-time thread |
| Viewer: `to_i420` + `copy_from_i420` | 1.4 | perf #3, alloc #2 | **Confirmed** |
| Video receive and decode (VideoToolbox) | 1.3 | none | Normal |
| Viewer: YUV upload in `prepare`, main thread | 1.2 | perf #3 | **Confirmed** |
| Mic: Opus encode and send | 1.1 | none | Normal while unmuted |
| Echo cancellation (APM) | 1.1 | perf #7 | Normal while unmuted |
| Black-frame keepalive encoding | **1.0** | **none** | New finding 1 |
| Mic capture (rodio, cpal) | 0.3 | perf L1 | Small |
| Stats loop | ~0 | lock #2 | Lock #2 is about latency, not CPU; a profile can't show it |

CPU by thread, as a share of core's samples (from [`profile/viewer.txt`](profile/viewer.txt)):

| Thread | Share |
|---|---|
| Unnamed (GCD dispatch: WebRTC task queues, Metal submission, VideoToolbox) | 29.0% |
| `hopp-audio` (audio processing: noise filter, APM) | 18.8% |
| libwebrtc `network_thread` | 17.2% |
| Main thread (event loop, rendering) | 12.5% |
| libwebrtc `worker_thread` | 11.6% |
| `tokio-rt-worker` (frame conversion, `copy_from_i420`) | 5.6% |
| CoreMedia, CoreAudio I/O | 5.1% |

Almost all samples ran on performance cores ([`profile/cores.py`](profile/cores.py)): `hopp-audio`
17.7% P against 1.1% E.

## Audit findings, checked

- **perf #3 (viewer copies) is confirmed.**
  - The two parts add up to 2.6% of one core for one 3K/40 fps share and two cameras, against the
    audit's estimate of about 3%.
  - 1.2 of it runs in `YuvVideoPrimitive::prepare` on the main thread. The rest is `to_i420`
    (libyuv `CopyRow` / `SplitUVRow`) and `copy_from_i420` on a tokio worker.
- **perf #7 (noise filter) is confirmed, and bigger than the audit said.** It runs on every 8 ms
  block while the mic is on. Skipping it while muted helps muted listeners only; the 3× gap is
  covered in new finding 4.
- **perf #8, alloc #1 (the real-time audio callback):** low CPU, as both reviews said. They argued
  for fixing it to avoid glitches, which this profile doesn't measure.
- **Not measured by this run:**
  - **perf #4 (timer redraws):** needs a run where the content stands still: cameras off, or a
    static shared screen.
  - **perf #1, #2, #5, #6 and #9:** these are sharer-side or join-time costs. #6 (font reload)
    happens when a participant joins, which was before recording started.
  - **lock #1 and #2:** they cause input latency, not CPU use. Measuring them needs a latency probe,
    such as the time from the fake viewer's cursor event to core's overlay update.
- **Allocator:** `nanov2_malloc_type` and `_nanov2_free` are 1.2% of samples together. That agrees
  with the allocator review: the allocator itself isn't the cost.

## New findings

### 1. Black frames are encoded for the whole call (about 1% of one core per participant)

- **Where:** our LiveKit fork, `libwebrtc/src/native/video_source.rs`, `NativeVideoSource::new`.
  It calls `new_inner(.., raw_keepalive: true)`, which spawns a task that captures a black I420
  frame every 100 ms until the source's first real `capture_frame`.
- **Why it runs all call:** core creates both of its video sources muted at call start: the camera
  (`core/src/room_service.rs:1180`, 1280x720, simulcast) and the screen (`:1257`, 1920x1080). A
  participant who never turns on their camera, or never shares, never captures a real frame, so
  both keepalives run for the whole call.
- **Evidence:** `VideoStreamEncoder::OnFrame` → `SimulcastEncoderAdapter` → `RTCVideoEncoderH264`
  takes 2.9% of core's samples. Of that, `I420ToNV12` is 1.3% and `VTCompressionSessionEncodeFrame`
  0.7%. The encoded packets are sent as well.
- **Fix (small):** stop the keepalive once the track is muted or after a few frames, in the fork.
  Alternatively, core could create the camera and screen sources only when they're first used.
- **Impact:** about 1% of one core, plus network traffic, for nearly every participant in every call.

### 2. Core crashes if a share starts while the display is asleep

- **Where:** `core/src/lib.rs:1716` passes `event_loop.available_monitors()` to `screenshare()`.
  `core/src/capture/capturer.rs:403` then reaches `core/src/capture/macos.rs:28`, which takes
  `monitors[0]` without checking the list is non-empty.
- **What happens:** with the display asleep, the list is empty. The main thread panics with
  "index out of bounds: the len is 0", which poisons the `screen_capturer` lock, and core exits.
- **Evidence:** [`profile/sharer-display-asleep-panic.core.log`](profile/sharer-display-asleep-panic.core.log).
  At that moment `CGGetActiveDisplayList` returned 0 displays while `CGGetOnlineDisplayList`
  returned 1.
- **Fix (small):** if the list is empty, fail the share with an error instead of indexing it.

### 3. Core panics at shutdown, which Sentry probably reports

- **Where:** our LiveKit fork, `livekit-runtime/src/tokio.rs:53`, calls
  `expect("Tasks should not panic")` on a `JoinError::Cancelled`.
- **What happens:** when core terminates (its client disconnects), the tokio runtime cancels
  LiveKit tasks that are still pending. The wrapper treats the cancellation as a panic.
- **Evidence:** [`profile/viewer.core.log`](profile/viewer.core.log), line 265, right after
  "Client disconnected, terminating".
- **Impact:** harmless for the call. But `sentry_utils` initializes Sentry 0.35 with default
  features, which include the panic integration, and its `before_send` uploads logs. So this likely
  files a crash event and a log upload on many core exits. Not verified against Sentry: search it
  for "Tasks should not panic".
- **Fix (small):** treat `Cancelled` as a normal end in the fork's join-handle wrapper.

### 4. The noise filter costs 3× what perf-audit benchmarked

- **Measured:** 3.7% of one core, about 300 µs per 8 ms block. perf-audit's standalone benchmark
  of the same models measured 97 µs per block (1.2% of one core).
- **Where:** most of it is in tract's Apple AMX matrix kernel (`apple_amx_mmm_f32_32x1`, 8.7% of
  samples), under `process_audio_samples` → `SimplePlan::run`.
- **Ruled out:** efficiency cores. The thread runs on performance cores.
- **Unchecked:** whether core feeds it more than 125 blocks per second (sample rate or channel
  count), contention for the AMX unit with other threads, and the cost of the per-op allocations
  inside tract.
- **Why it matters:** it's the largest single cost in our own code for anyone who's unmuted.

## Where to attack

1. **Black-frame keepalive** (new finding 1): a small fork change that saves about 1% of a core,
   plus traffic, for nearly every participant.
2. **Crash on an empty monitor list** (new finding 2): small, and core currently crashes.
3. **Shutdown panic** (new finding 3): small; first check Sentry for how often it fires.
4. **Viewer copies** (perf #3): 2.6% of one core per 3K share, partly on the main thread. Medium
   effort.
5. **Noise filter** (new finding 4, perf #7): find the cause of the 3× gap first, then skip it while
   muted.
6. **After the sharer run:** perf #1 (capture fps) and #2 (zero-copy send), ordered by what that run
   measures.

## Still to measure

- **Sharer run** (`profile.sh sharer`): needs the display awake and something moving on screen. It
  covers perf #1, #2, #4 (overlay redraws) and #9.
- **Static-content viewer run:** for perf #4's redraw timers. That needs a new profile role, with
  the cameras off and a share that stops changing.
- **Lock latency:** a probe from the fake viewer's cursor event to core's overlay update, with and
  without the lock fixes.
- **Join cost:** perf #6's font reload, measured by having a participant join during recording.

## Fixes made to the test tooling along the way

- **LiveKit ICE:** `livekit-server --dev` advertised LAN, public IPv6 and STUN candidates whose
  checks got no answers. A second connection from one process never connected, and core took 13 s.
  `profile.sh` and `smoke.sh` now start it on loopback only (`--node-ip 127.0.0.1`, loopback
  candidate, `lo0`).
- **Display id:** the harness defaulted `HOPP_TEST_SCREEN_ID` to 0, but core matches CoreGraphics
  display ids, so 0 never matched. It now defaults to `CGMainDisplayID()`. This also affects
  `smoke.sh screenshare`.
- **Display sleep:** `profile.sh` keeps the display awake while it runs. The sharer role refuses to
  start if the display is already asleep.

## Data and how to reproduce

Files in [`profile/`](profile/):

| File | What |
|---|---|
| `viewer.svg` | Flamegraph; open in a browser and hover for shares |
| `viewer.txt` | CPU per thread, top self-time functions, core's own functions by total time |
| `viewer.folded.gz` | Demangled folded stacks, in microseconds (input for the scripts, inferno, speedscope) |
| `viewer.trace.tar.gz` | The raw Instruments recording; untar and open in Instruments |
| `viewer.core.log`, `viewer.harness.log` | Core's log and the load generator's log for the run |
| `sharer-display-asleep-panic.core.log` | Core's log from the failed sharer run (new finding 2) |
| `buckets.py` | The table above: `python3 buckets.py viewer.folded.gz 0.28` |
| `drill.py` | The tree below a frame: `python3 drill.py viewer.folded.gz "process_audio_samples" 3` |
| `cores.py` | E/P core split per thread, from the raw export (see its docstring) |

To record a new run: `core/tests/profile.sh viewer --seconds 30 --label <name>`. It writes to
`core/tests/out/profile/`. The setup is described in `core/tests/README.md` under Profiling.

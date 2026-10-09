# Changes waiting on the LiveKit fork

Core and the `core/tests` harness build against `awkay/rust-sdks`, branch `hopp-encoding-params`
(see `DATAICO.md` for what the fork adds and how to rebase it). Only the fork's owner can push
there, so the changes below wait for them. Ask for them together: each one needs a fork commit,
then `cargo update -p livekit` in `core/` and `core/tests/` (it moves every crate from the fork's
git source together). If the 0006 SDK bump rebases the fork first, these commits have to be
carried along.

## 1. Shutdown panic (patch ready)

Audit `d9ed322`, [profile-viewer-vs-audit.md](../audit/d9ed322/profile-viewer-vs-audit.md) "New
findings" #3; tracker row 0005. Deferred on 2026-10-09: it needs fork access, and its only effect
is Sentry noise.

- **Symptom:** core logs `Tasks should not panic: JoinError::Cancelled(Id(..))` from a
  `tokio-rt-worker` thread on exit, which fires the panic hook (Sentry, with its log upload).
  Search Sentry for "Tasks should not panic" to see how often.
- **Cause:**
  - `livekit-runtime/src/tokio.rs`: `TokioJoinHandle::poll` does `value.expect("Tasks should not
    panic")` on the `JoinError`, including `Cancelled`.
  - Core drops its main runtime (`_async_runtime` in `core/src/room_service.rs`) right after
    queueing the room close (`DestroyRoom`, then "Client disconnected, terminating"). The close
    awaits LiveKit handles on that same runtime: `Room::close`'s 7, `RtcEngine::close`'s
    `engine_task`, `RtcSession::close`'s 4, `SignalClient::close`, `SignalStream::close`'s
    read/write tasks.
  - In tokio 1.53.1's multi-thread shutdown, each worker notices on its own (next maintenance
    tick, every 61 tasks, or when it parks). One worker can already be cancelling idle tasks while
    another still polls tasks, including stolen ones. That one sees `Cancelled` and the `expect`
    panics.
  - Nothing exposes `abort()` / `abort_handle()` on the inner handle, so `Cancelled` only comes
    from runtime shutdown. Every awaiter in the fork runs on the runtime that spawned the task (no
    `block_on`, no std-thread awaiter), so it's dropped by the same shutdown.
- **Fix:** [0001-fix-livekit-runtime-don-t-panic-when-the-runtime-can.patch](0001-fix-livekit-runtime-don-t-panic-when-the-runtime-can.patch),
  made on top of `hopp-encoding-params` at `2ce5cf0`. On `Cancelled` the handle stays `Pending`
  and remembers it (tokio's `JoinHandle` panics if polled again after `Ready`); a real task panic
  is resumed with `resume_unwind(err.into_panic())`. Two unit tests in `livekit-runtime`; a
  scratch stress repro (busy awaiters on a 4-worker runtime, then drop) panicked 288 of 300 runs
  before, 0 of 300 after. clippy and fmt clean.
- **To ship:** `git am` the patch on `hopp-encoding-params` and push; re-pin core and the harness;
  run core's checks; then a `core/tests/profile.sh` viewer run, whose core log should no longer
  have the panic after "Client disconnected, terminating".

## 2. Audio mixer source that fills a buffer

Removes the audio output callback's last allocation on the real-time thread. Details and the
proposed `fill_audio_frame(&mut self, ...)` signature: [../unsafe.md](../unsafe.md), "Audio output
callback".

## 3. Safe CVPixelBuffer APIs for GPU-resident video

From the [§GPU-RESIDENT-VIDEO audit](../audit/6ac5333/gpu-resident-video.md). Core can't use the
fork's pixel-buffer APIs without `unsafe` (§NO-UNSAFE), so none of the zero-copy routes can start
without these:

- **Sharer:** a safe `NativeBuffer` constructor that takes an owned, retained buffer
  (`apple_cf::cv::CVPixelBuffer`, which screencapturekit 8 already hands core, or
  `CFRetained<objc2_core_video::CVPixelBuffer>`), wrapping the existing `unsafe fn
  from_cv_pixel_buffer` inside the fork. Removes the sharer's copy and two conversions (~9.5% of a
  core) and the buffer-reuse race. The camera can use it too.
- **Viewer:** a safe accessor for a decoded frame's pixel buffer: return a retained buffer, or a
  plane-lock guard, instead of `get_cv_pixel_buffer()`'s non-retained `*mut c_void`.
- **Doc fix:** `from_cv_pixel_buffer` (`libwebrtc/src/video_frame.rs:575`) says it doesn't touch
  the reference count, but `objc_video_frame_buffer.mm:31` releases one, so callers must pass a
  retained (+1) buffer.
- **Optional:** report whether VideoToolbox really runs in hardware (its
  `UsingHardwareAcceleratedVideoEncoder` property), or require it. Today a software fallback is
  invisible.

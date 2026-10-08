# Core performance audit: hot paths and libraries

- **Commit audited:** `d9ed322` (branch `test/harness-smoke-ci`), 2026-10-08.
- **Scope:** the macOS / Apple Silicon build of `core/`. The Windows and Linux code (`capture/stream.rs`,
  `camera/stream_nokhwa.rs`, `*_windows.rs`, `*_linux.rs`) is upstream's and was not reviewed.
- **Questions:**
  1. Do the per-frame, per-pixel, per-sample and per-event loops vectorize? Are there per-frame
     allocations, needless frame copies, or CPU work the GPU should do?
  2. Does the code hand-roll things a mature crate already does, or carry dependencies it doesn't need?
- **Method:** a code read of `graphics/`, `capture/`, `camera/`, `audio/`, `effects*`, `livekit/`,
  `window/` and everything they call, plus the pinned LiveKit/libwebrtc sources
  (`awkay/rust-sdks`, branch `hopp-encoding-params`, rev `2ce5cf0`). The app was not run, so there is
  no whole-app profile. Claims about vectorization come from emitted assembly. Costs come from
  scratch benchmarks on an Apple M4 (warm cache), built outside the repo. See
  [Reproducing the measurements](#reproducing-the-measurements).

## Summary

Nothing vectorizes badly where it matters: almost every per-pixel operation goes through libyuv's
NEON code or `memcpy`, and the audio conversion loops vectorize. The waste is in the pipeline around
the loops:

- whole frames converted and copied on the CPU when VideoToolbox could take or give them directly;
- screen capture running faster than the encoder accepts;
- windows that redraw on a timer with nothing new to show.

| # | Finding | How hot | Effort |
|---|---------|---------|--------|
| 1 | Screen capture runs at 60 fps and full size; the encoder takes 40 fps (15 in low-bandwidth) | per frame, sharer | small |
| 2 | Sending the screen does three full-frame CPU passes VideoToolbox doesn't need; likely buffer-reuse race | per frame, sharer | medium |
| 3 | The viewer converts and copies each decoded frame three times, partly on the main thread | per frame, every incoming stream | medium |
| 4 | Camera, overlay, drawing and screen-share windows redraw on timers with no new content | per frame, whole call | medium |
| 5 | The camera converts at full resolution, scales on the CPU, then copies again | per frame, camera on | small to medium |
| 6 | Every cursor badge reloads all system fonts on the main thread | per participant join | small |
| 7 | The noise filter and echo cancellation keep running while muted | per 8 ms audio block | small |
| 8 | The real-time audio callback allocates memory and shares a lock with a tokio thread | per audio callback | small |
| 9 | The effect premultiply loop doesn't vectorize | per pixel, while an effect plays | small |

Measured costs:

| Work | Size | Time |
|---|---|---|
| Row copy of an NV12 frame (`macos_stream.rs:459`) | 3024x1964 | 0.28 ms |
| NV12 to I420 (libyuv) | 3024x1964 | 0.18 ms |
| I420 to NV12 (libyuv) | 3024x1964 | 0.17 ms |
| Same three at 1080p | 1920x1080 | 0.04 / 0.05 / 0.05 ms |
| I420 downscale, box filter | 1920x1080 to 1280x720 | 0.45 ms |
| `fontdb::Database::load_system_fonts` (1389 faces) | per call | 330 ms first call, 22–30 ms after |
| DTLN noise filter, one block | 512 samples in, 128 out | 97 µs (about 1.2% of one core at 125 blocks/s) |

3024x1964 is what the default "4K" screen-share setting gives on a 14" MacBook Pro: the setting is
an upper bound, and the capture is fitted to the display's native size.

## Ranked findings

### 1. Screen capture runs at 60 fps and full size; the encoder takes 40 fps (15 in low-bandwidth)

- **Where:** `core/src/capture/macos_stream.rs:284` and `:498` (`with_fps(60)`), against
  `core/src/bandwidth_mode.rs:10` (`MAX_FRAMERATE = 40`) and `:17` (`LOW_BANDWIDTH_FRAMERATE = 15`).
- **How hot:** per frame on the sharer.
- **What happens now:** the capture rate is fixed and never follows the encoding. Every frame
  ScreenCaptureKit delivers is copied into an `NV12Buffer` and handed to libwebrtc. libwebrtc's
  frame adapter then drops whatever exceeds the encoder's maximum frame rate: a third of all
  frames normally, three quarters in low-bandwidth mode.
- **Low-bandwidth size:** low-bandwidth mode also asks the encoder to shrink the frame
  (`scale_down_by`, `bandwidth_mode.rs:121`). libwebrtc does that with a CPU scale of each frame.
  That last step is from reading libwebrtc, not measured.
- **Fix:** set ScreenCaptureKit's fps from the current encoding (40 or 15). When low-bandwidth turns
  on or off, call the existing `Stream::reconfigure()` with the scaled size and the new fps, so
  ScreenCaptureKit scales on the GPU.
- **Impact:** up to 75% less capture work for a change of a few lines.

### 2. Sending the screen does three full-frame CPU passes VideoToolbox doesn't need

- **Where:** `core/src/capture/macos_stream.rs:446-474`.
- **How hot:** per frame, for every screen share.
- **What happens now:**
  1. Each frame is copied row by row from the IOSurface into a reused `NV12Buffer` (`:459-471`).
  2. libwebrtc's macOS H.264 encoder (VideoToolbox) uses a frame directly only when it is a native
     pixel buffer. For any other buffer type, the encoder wrapper calls `ToI420()`: an NV12 to I420
     conversion into a newly allocated buffer.
  3. The encoder then copies I420 back to NV12 into a pixel buffer from its own pool.
- **Cost:** about 0.63 ms per frame at 3K (copy + both conversions, warm cache), roughly 2.5–4% of a
  core at 40–60 fps, plus memory bandwidth. Expect more with a cold IOSurface.
- **Fix:** hand the capture buffer to libwebrtc directly with
  `livekit::webrtc::video_frame::native::NativeBuffer::from_cv_pixel_buffer` (rust-sdks
  `libwebrtc/src/video_frame.rs:575`). VideoToolbox then reads the IOSurface with no CPU work.
  - Display capture needs no crop, so it can go zero-copy as is.
  - Window capture crops to `content_rect`. Keep the copy for that case, or add a cropped
    constructor to our rust-sdks fork. `RTCCVPixelBuffer` already has
    `initWithPixelBuffer:adaptedWidth:adaptedHeight:cropWidth:cropHeight:cropX:cropY:`.
  - Holding pixel buffers in the encoder keeps them out of ScreenCaptureKit's pool. Raise the
    stream's `queueDepth` if frames start dropping.
- **Ownership gotcha:** the Rust doc comment says `from_cv_pixel_buffer` "does not bump the reference
  count". But the C++ wrapper calls `CVPixelBufferRelease` on the pointer it is given
  (rust-sdks `webrtc-sys/src/objc_video_frame_buffer.mm:31`). The caller must hand over an extra
  retained (+1) reference, or the buffer is released one time too many.
- **Likely bug fixed along the way:** `capture_frame` passes the reference-counted buffer to
  libwebrtc without copying it (`webrtc-sys/src/video_frame.cpp:62-63`). The encoder reads it later
  on its own task queue. Meanwhile our handler overwrites the same `NV12Buffer` on the next frame
  (`:446-457`), so the encoder can read half of one frame and half of the next. Moderately
  confident: found by reading the code, not reproduced.

### 3. The viewer converts and copies each decoded frame three times, partly on the main thread

- **Where:** `core/src/livekit/video.rs:204` and `:211`, `core/src/graphics/yuv_renderer.rs:222-379`.
- **How hot:** per frame of every incoming video stream (the screen share and each remote camera).
- **What happens now:**
  1. VideoToolbox decodes into NV12 native pixel buffers. LiveKit registers its decoder first on
     Apple (rust-sdks `webrtc-sys/src/video_decoder_factory.cpp:46`).
  2. `to_i420()` allocates a fresh I420 buffer and converts into it (0.18 ms at 3K).
  3. `VideoBuffer::copy_from_i420` copies that into a `Vec` whose rows are padded to 256 bytes.
  4. `YuvVideoPrimitive::prepare` copies it again into a `StagingBelt` slice. This runs inside iced's
     `present`, on the winit main thread.
  5. Each video tile creates its own command encoder and calls `queue.submit` and
     `device.poll` (`yuv_renderer.rs:248`, `:349`, `:378`).
- **Fix:**
  - Store the decoded frame itself in the double buffer (it is reference-counted on the C++ side)
    instead of a converted copy.
  - At upload, lock the decoder's pixel buffer
    (`frame.buffer.as_native()` → `get_cv_pixel_buffer()`) and copy its Y and UV planes straight
    into an `R8Unorm` and an `Rg8Unorm` texture. The shader samples NV12 and drops its third
    texture.
  - Upload with `queue.write_texture`, as `graphics/effect_renderer.rs:287` already does. It has no
    256-byte row rule, so the padding in `VideoBuffer`, the padded texture widths and the shader's
    UV adjustment (`shaders/yuv_shader.wgsl:114-116`) all go away, and so do the per-tile encoder,
    submit and poll.
  - Later: import the IOSurface as a Metal texture through wgpu's hal layer for zero CPU copies.
- **Check while doing it:** whether the decoder outputs full range (`420f`) or video range (`420v`).
  The shader assumes limited-range BT.709 (`yuv_shader.wgsl:62-73`).
- **Impact:** two CPU passes and one allocation removed per frame, and about 0.2 ms per frame at 3K
  taken off the main thread. Roughly 3% of a core at 40 fps.

### 4. Windows redraw on timers with no new content

- **Camera window:** rebuilds the whole iced interface and presents at a fixed 30 fps (15 while
  sharing) for the whole call, even when every camera is off (`core/src/window/camera_window.rs:67-104`,
  redraw at `:890`). `skip_upload` only skips the texture upload.
- **Sharer overlay:** after any remote activity it redraws a full-screen transparent window at
  60 fps for 15 s (`core/src/graphics/graphics_context.rs:46`). Remote cursors hide after 5 s
  (`graphics/participant.rs:14`) and click animations last 0.8 s (`graphics/click_animation.rs:9`),
  so about 10 s of every burst draws nothing new.
- **Idle wake-ups:** once inactive, the same thread keeps waking every 16 ms for as long as it
  exists (`graphics_context.rs:69-72`). `core/src/window/drawing_window.rs:139-170` copies the
  pattern.
- **Screen-share window:** a 10 Hz timer (`core/src/window/screensharing_window.rs:286`, `:329-343`)
  redraws even without a new frame, and each such redraw logs at WARN (`:2439-2451`). A static
  shared screen makes ScreenCaptureKit send no frames, so that is about 10 warnings per second in
  user logs.
- **Fix:**
  - Redraw on new frames (`process_video_stream` already sends `ForceRedraw`) and on UI events.
  - Schedule one redraw at the next known deadline: cursor hide, animation end, effect end.
  - Block on the channel when idle instead of waking every 16 ms.
  - Lower the "dropping redraw" line to debug.
- **Impact:** not measured. Probably as large as finding 2 in energy use, because every participant
  pays it for the whole call. An Instruments trace would settle the order between 2 and 4.

### 5. The camera converts at full resolution, scales on the CPU, then copies again

- **Where:** `core/src/camera/stream_macos.rs:216-345`.
- **How hot:** per frame at 30 fps (15 while screen sharing), for the whole call while the camera is
  on.
- **What happens now:**
  - No capture size is chosen (the TODO at `:472`), so a 1080p camera is converted from NV12 to I420
    at 1080p.
  - `I420Buffer::scale` allocates a new buffer every frame (`:320`), 0.45 ms for 1080p to 720p.
  - The scaled buffer is copied into `stream_frame` (`:322-326`), then copied again for the local
    preview (`:301-306`), then the encoder converts I420 back to NV12.
  - The same buffer-reuse race as finding 2 applies: `state.i420` and `stream_frame` are overwritten
    after `capture_frame` (`:341-345`).
- **Fix:**
  - Choose the device `activeFormat` closest to the target size, or request width and height in the
    output's `videoSettings`. The SDK header documents those keys only for iOS 16+, with the rules
    "same aspect ratio as the active format, not larger than it". Follow those rules, test on a Mac,
    and keep it inside the existing `exception::catch`.
  - Pass `scaled` straight to `capture_frame` and drop `stream_frame`: one copy fewer and no reuse.
  - Ultimately, send the camera's pixel buffer zero-copy as in finding 2.
- **Impact:** about 1.5% of a core at 30 fps, plus the allocations and copies.

### 6. Every cursor badge reloads all system fonts on the main thread

- **Where:** `core/src/utils/svg_renderer.rs:135-136` (`fontdb.load_system_fonts()`), called three
  times per participant from `core/src/graphics/cursor.rs:51-53`.
- **How hot:** cold (once per participant join, per window), but it runs on the winit main thread
  (`lib.rs:1206`, `:1887`, `:1898`).
- **Cost:** 330 ms the first time and 22–30 ms after (fontdb 0.23.0, 1389 faces on the test Mac).
  That is roughly 150 ms (warm) to over 1 s (cold) of main-thread stall per join.
- **Also:** each badge is encoded to PNG, decoded once just to read its size (`cursor.rs:55-63`), then
  decoded again by iced.
- **Fix:** load the font database once into a `static OnceLock<Arc<fontdb::Database>>`, optionally
  warmed on a background thread at startup. Render to RGBA from the tiny-skia pixmap (demultiplied)
  and build the handle with `image::Handle::from_rgba` and the known size. No PNG at all.

### 7. The noise filter and echo cancellation keep running while muted

- **Where:** muting only mutes the LiveKit track (`core/src/lib.rs:2291-2302`). Mic capture
  continues. Echo cancellation (`process_stream`, `core/src/livekit/audio.rs:153`) and the DTLN
  noise filter (`:155-157`) run on every block regardless. Noise cancellation is on by default
  (`lib.rs:519`).
- **How hot:** every 8 ms block, for the whole call.
- **Cost:** 97 µs per block, about 1.2% of one core, nonstop. Breakdown (same tract 0.21.4, the repo's
  models): model 1 37 µs, model 2 54 µs, both FFTs 0.9 µs, atan2/sin/cos phase math 5.2 µs.
- **Fix:** skip `denoiser.process` while the track is muted. On unmute, reset its LSTM state tensors
  and its input and output queues. Echo cancellation could also be skipped; it re-converges within
  about a second of unmuting.
- **Related, in `core/src/audio/denoiser.rs`:**
  - Rebuilding the spectrum from magnitude and phase (`:103-106`, `:133-141`) is mathematically the
    same as `masked[i] = spectrum[i] * mask[i]`. That removes the atan2/sin/cos calls (scalar libm
    calls in the assembly) and lets the loop vectorize. The gain is small; the code gets simpler.
  - The special case for the first frequency bin (`:138`) uses the magnitude `|re|`, so it drops the
    sign of the DC term. The reference DTLN keeps it, and so does the multiply form. I haven't
    checked whether it's audible.
- **Not worth doing:** keeping one tract `SimpleState` across blocks instead of calling
  `SimplePlan::run` measured 51.8 µs against 53.0 µs per run.

### 8. The real-time audio callback allocates memory and shares a lock with a tokio thread

- **Where:** `core/src/audio/mixer.rs`:
  - `:143-146`: `buf.split_off` allocates a new tail vector on every callback.
  - `:186-189`: `collect()` allocates a new vector for every 10 ms mix.
  - Remote audio frames are vectors allocated on a tokio thread (`push_samples`, `:56`) and freed
    on the audio thread after mixing (`Cow::Owned`, `:99`).
  - The audio-processing mutex (`:161`) is also taken every 10 ms by `process_stream` on a tokio
    worker, so the real-time thread can end up waiting on a lower-priority thread.
- **How hot:** every callback (about 10 ms) on CoreAudio's real-time thread.
- **Fix:** keep one `f32` buffer with a read position and refill it with `clear()` + `extend()`, so the
  callback never allocates. Use `rtrb` for the per-source buffers (see L2).
- **Impact:** fewer audio glitches rather than CPU saved.

### 9. The effect premultiply loop doesn't vectorize

- **Where:** `core/src/effects/manifest.rs:545-555` (`premultiply_rgba`).
- **How hot:** every pixel of every decoded effect frame (up to 1280x720), on the effect-decode worker
  thread, only while an effect plays.
- **Evidence (assembly, rustc 1.99, `aarch64-apple-darwin`, release):**
  - The current loop is scalar, with a compare-and-branch per pixel (`cmp w12, #255` / `b.eq`) and
    scalar `madd`/`mul`/`lsr` per channel.
  - Removing the branch vectorizes the arithmetic (`ld4.8b`) but LLVM still stores byte by byte.
  - Treating each pixel as a `u32` compiles to four pixels per iteration, one 16-byte load and store
    (`ldr q` / `str q`), no branches. Output is identical, because alpha 255 already maps each
    channel to itself:

    ```rust
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let v = u32::from_le_bytes(*pixel);
        let a = v >> 24;
        let mul = |shift: u32| ((((v >> shift) & 0xff) * a + 127) / 255) << shift;
        *pixel = (mul(0) | mul(8) | mul(16) | (a << 24)).to_le_bytes();
    }
    ```

- **Impact:** low. Roughly 1–2 ms down to about 0.2 ms per frame (estimated, not timed), on a worker
  thread.

## Libraries and dependencies

### L1. rodio carries a lot of unused weight

- `core/Cargo.toml:92` keeps rodio's default features: flac, mp3, mp4 and vorbis decoders (symphonia),
  wav (hound), dither/noise (rand, rand_distr). Only `rodio::microphone` is used
  (`core/src/audio/capturer.rs:2-3`).
- Fix: `default-features = false, features = ["recording"]` (not built yet). Or drop rodio and
  record with `cpal::build_input_stream` + `rtrb`: cpal 0.17.3 is already a direct dependency used
  for output, and this also removes a pinned git dependency. The per-sample iterator pull in the
  capture loop (`capturer.rs:301`) would become slice reads, which vectorize.

### L2. A hand-made queue for audio between threads

- `core/src/audio/mixer.rs` uses `Arc<Mutex<VecDeque<Vec<i16>>>>` per remote source.
- Use `rtrb` 0.3.4, a lock-free single-producer single-consumer ring buffer built for real-time
  audio. It is already in `Cargo.lock` through livekit's libwebrtc and rodio, so adding it costs no
  build time.

### L3. Two copies of libyuv

- `yuv-sys` (`core/Cargo.toml:91`) builds its own libyuv, with symbols renamed so it can live next to
  libwebrtc's copy (its `build.rs` says so). Core uses it only for the camera
  (`stream_macos.rs:221-281`).
- LiveKit's `livekit::webrtc::native::yuv_helper` already has `nv12_to_i420` and `argb_to_i420`.
  Only the YUY2 and UYVY camera fallbacks need `yuv-sys`.
- AVFoundation converts to NV12 when asked, and NV12 is already first in `SUPPORTED_PIXEL_FORMATS`.
  If the camera always requests NV12, or goes zero-copy (finding 5), drop `yuv-sys` and its C++
  build.

### L4. Unused or redundant dependencies

- `cgmath` (`core/Cargo.toml:69`): no uses, and nothing else depends on it, so removing it removes the
  crate from the build.
- `dirs` (`:79`) and `sysinfo` (`:83`): no uses in core. They stay in the build because `sentry_utils`
  uses them, so this is manifest hygiene only.
- `realfft` + `rustfft` (`:99-100`): not two crates doing the same job; realfft is built on rustfft,
  and tract-core pulls rustfft in as well. The direct `rustfft` line exists only for
  `rustfft::num_complex::Complex` (`denoiser.rs:2`), which realfft re-exports as
  `realfft::num_complex`. Drop the line.

### L5. Hand-rolled code a dependency already covers

- `core/src/graphics/click_animation.rs:139` hand-codes cubic ease-out (`1 - (1 - t)^3`). Use
  `simple_easing::cubic_out`, already a dependency and used in `components/toast.rs:62` and
  `components/segmented_control.rs:181`.
- `ColorToken::to_color` (`core/src/windows/colors.rs:506-536`) parses hex strings at runtime on every
  `view()` call (57 call sites). Tiny but avoidable: return constant colours (`Color::from_rgb8` is
  `const`, or use `iced::color!`).

### L6. GPU uploads

The hand-managed `StagingBelt` in `yuv_renderer.rs`, with a command encoder, submit and poll per tile,
could be wgpu's `queue.write_texture`, which does its own staging and batching and has no 256-byte row
rule. `effect_renderer.rs` already uses it. See finding 3.

### L7. Keep as is

- **tract-onnx / tract-core:** heavy, but they run DTLN at a reasonable measured cost. Switching to
  WebRTC's built-in noise suppression would be a product decision, not a drop-in swap.
- **parking_lot:** used in only two files, with `std::sync::Mutex` everywhere else. It is already in
  the build through wgpu, so consolidating gains nothing.
- **Used correctly:** LiveKit's `AudioResampler`, libyuv, `image` and `realfft`.
- **Not fixable from core alone:** `image`'s default codecs are also enabled by iced's `image`
  feature, so `default-features = false` in core would not drop them.

## Cold-path and minor items

- `core/src/input/mouse_macos.rs:444`, `:529`, `:561`: a new `CGEventSource` for every simulated
  mouse event. Create it once.
- `core/src/graphics/draw.rs:354` and `:388`: text being typed in drawing mode is laid out every frame
  just to measure it, then laid out again by `fill_text`. In-progress strokes are tessellated twice
  per frame (outline and fill, `:327-330`), fine at typical lengths. `graphics/marker.rs` rebuilds its
  border every frame; a `canvas::Cache` would keep it.
- `core/src/graphics/graphics_window_context.rs:228-292`: four separate wgpu instance, device and iced
  engine sets (five with the stats window), each with 4x multisampling (`:109`). On the full-screen
  overlay the multisample target could take 100 MB or more of GPU memory. Worth checking.
- `core/src/audio/denoiser.rs:98` and `:145`: the FFT plan is looked up in the planner's map every
  block, and `:111` / `:158` copy inputs with `to_vec` / `clone`. Negligible next to inference.
- `core/src/effects/player.rs:321`: every effect frame is a fresh 3.6 MB vector from `into_frames`.
  `image_webp::WebPDecoder::read_frame` can decode into a reused buffer. Effects only.
- `core/src/capture/macos_stream.rs:330-454`: four uncontended mutexes locked per captured frame
  (`failures_count`, `resize_target`, `frame`, `output_extent`). Atomics would do.
- `core/src/window/drawing_helpers.rs:47-55`: un-premultiplying is hand-written in the cursor
  rasterizer; tiny-skia's `PremultipliedColorU8::demultiply` does it. Cold.

## Checked and found fine

- **Pixel conversion and scaling:** all done by libyuv's NEON code (through `yuv-sys` or libwebrtc's
  `yuv_helper`). The only hand-written per-pixel loops are premultiply (finding 9) and the cold
  demultiply above.
- **Row copies:** `macos_stream.rs:459-471` and `livekit/video.rs:63-81` copy each row with
  `copy_from_slice`, which compiles to `memcpy`, with bounds checks once per row and none in the inner
  loop (checked in the assembly). The problem is that the copies exist, not how they are written.
- **GPU work already on the GPU:** YUV to RGB, crop and sharpening in `yuv_shader.wgsl`. The effect
  quad uploads only when its frame changes. Effect thumbnails and cursor image handles are built
  once, so iced doesn't re-upload them every frame.
- **Audio sample conversions vectorize** (checked in the assembly): `i16` to `f32` uses `scvtf.4s` +
  `fdiv.4s`, `f32` to `i16` uses `fmul.4s` + `fcvtzs.4s` + `sqxtn`, and the division by 512 becomes
  `fmul.4s`.
- **Noise-filter FFT:** `realfft`, with scratch and spectrum buffers reused across blocks.
- **Not hot enough to matter:** input events (JSON per event at human input rates), stats polling
  (1 Hz), display enumeration (setup only).

## Reproducing the measurements

The benchmarks were throwaway crates outside the repo and were not kept. To redo them:

- **Assembly checks:** copy the loop into a scratch `rlib` crate and run
  `cargo rustc --release --target aarch64-apple-darwin -- --emit=asm`. The default CPU for that target
  is `apple-m1`, the same as core's build.
- **libyuv timings:** depend on `yuv-sys = "=0.3.14"` and time `rs_NV12ToI420`, `rs_I420ToNV12` and
  `rs_I420Scale` (`FilterMode_kFilterBox`) on buffers of the sizes above, plus the row-copy loop from
  `macos_stream.rs`.
- **Noise filter:** depend on `tract-onnx = "=0.21.4"`, `tract-core = "=0.21.4"` and `realfft`. Load
  `core/resources/models/dtln_model_{1,2}.onnx` exactly as `DtlnEngine::new` does, and time each stage
  of `DtlnEngine::feed` over a few thousand blocks.
- **Font loading:** time `fontdb::Database::new()` + `load_system_fonts()` (fontdb `=0.23.0`) several
  times in a row; the first call is cold.

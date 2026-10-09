# §GPU-RESIDENT-VIDEO audit, `perf/lightweight` at 6ac5333 (2026-10-09)

Read-only audit of the code at `6ac5333` against §GPU-RESIDENT-VIDEO (`CLAUDE.md`). No new runs:
the figures come from the spec 0005 step 1–2 profiles in [profile/](profile/), made on 2026-10-08
with `core/tests/profile.sh`. The `baseline-*` runs built core without the step 1–2 fixes, the
`after-*` runs with them; `after2-viewer` and `baseline2-viewer` repeat the viewer runs, because
viewer energy readings are noisy. Each run keeps its summary (`.txt`), flame graph (`.svg`),
folded stacks (`.folded.gz`), core log and `profile.sh` output (`-run.out`, with the energy and
`FramePacing` lines). The Instruments traces (13–16 MB each) were not kept.

**Verdict:** the invariant isn't met anywhere. Capture and VideoToolbox (VT) encode/decode stay
GPU-backed, but core takes every frame out to the CPU at both ends: 3 full-frame CPU passes per
frame on the sharer, 3 on the viewer (one on the main thread). None is documented or flagged as a
fallback; they are the only path. The GPU-resident fix for both ends needs new safe APIs in
`awkay/rust-sdks`, which only its owner can push (see [../../livekit-fork/](../../livekit-fork/)).
The last viewer step, importing the IOSurface into wgpu, has no safe API in wgpu 27, so it needs a
decision on `unsafe` (§NO-UNSAFE).

## Violations (largest first)

### 1. Viewer: decode → I420 → padded Vec → staging belt → texture
- `core/src/livekit/video.rs:218`: `to_i420()` allocates a new I420 buffer, NV12→I420, every frame.
- `video.rs:225` (code `:33-81`): `copy_from_i420` copies into a Vec padded to 256 bytes.
- `core/src/graphics/yuv_renderer.rs:222-378`: copies into the staging belt inside `prepare` on the
  main thread, encoder + submit + poll per tile (`:248`, `:349`, `:378`).
- Normal path, every incoming stream (screen share and remote cameras).
- Cost ~8.7% of a core at 3024x1964 ~38 fps: `to_i420` 3.5%, `copy_from_i420` 2.7%, `prepare` 2.6%.
- Audit #3 still open (lines moved from 204/211).
- Route: keep the decoded frame (refcounted) in the double buffer, then
  (a) transitional: lock the decoder's CVPixelBuffer and `write_texture` Y→R8, UV→Rg8 (one copy
  remains, still a violation); (b) full: import IOSurface planes as Metal textures.
- Unsafe: (a) needs a fork API: `NativeBuffer::get_cv_pixel_buffer()` returns a non-retained
  `*mut c_void`; the fork should return a retained buffer or a safe plane-lock guard.
  (b) no safe route: wgpu 27 `Device::as_hal`, `create_texture_from_hal`,
  `wgpu_hal::metal::Device::texture_from_raw` are all `unsafe fn`; Metal backend has no NV12
  format. `objc2-metal` `newTextureWithDescriptor_iosurface_plane` and screencapturekit's
  `IOSurfaceMetalExt::create_metal_textures` make the texture safely, but handing it to wgpu is
  `unsafe`.

### 2. Sharer: copy, NV12→I420, I420→NV12
- `core/src/capture/macos_stream.rs:477-505` copies each row from the IOSurface into a reused
  `NV12Buffer`. libwebrtc's ObjC encoder wrapper calls `ToI420()` on the non-native buffer, then
  `RTCVideoEncoderH264` converts I420→NV12 into a VT pool buffer.
- Normal path. ~9.5% of a core (3440x1440): copy 3.2%, NV12→I420 4.3%, I420→NV12 1.9%.
- Audit #2 still open (lines moved from 446-474).
- **Buffer-reuse race:** the same `NV12Buffer` is overwritten while libwebrtc may still hold the
  previous frame's reference.
- Route: pass ScreenCaptureKit's CVPixelBuffer straight to the encoder. Display shares need no crop.
  Window shares crop to `content_rect`; a cropped native buffer makes the encoder crop on the CPU,
  so match SCK's output to the window (resizes arrive via `FrameChanged`) and keep an explicit,
  logged, counted copy for the mismatch case. Raise SCK `queueDepth`.
- Unsafe: fork `NativeBuffer::from_cv_pixel_buffer` (`libwebrtc/src/video_frame.rs:575`) is
  `pub unsafe fn(*mut c_void)`. Its doc says it doesn't touch the refcount, but
  `objc_video_frame_buffer.mm:31` releases one, so the caller must pass +1. Fix: a safe fork
  constructor taking an owned retained buffer (`apple_cf::cv::CVPixelBuffer` from screencapturekit 8,
  whose `Clone` retains, or `CFRetained<objc2_core_video::CVPixelBuffer>`). Core then needs no
  `unsafe`. Zero-copy also fixes the reuse race.

### 3. Low-bandwidth mode scales on the CPU
- `room_service.rs:2567` sets `scale_resolution_down_by` (from `bandwidth_mode.rs`
  `screen_encoding`); libwebrtc scales each non-native NV12 frame on the CPU. From reading
  libwebrtc, not measured; ≤15 fps.
- Explicit mode, but its CPU scale isn't documented as a fallback.
- Audit #1: fps half fixed (4df658a, d0ca389); size half open.
- Route, no `unsafe`: call `Stream::reconfigure` (`macos_stream.rs:520`) with the scaled size so SCK
  scales on the GPU, and send `scale_down_by = 1`.

### 4. Software codecs can be used silently
- Encoder: fork factory tries VT first; OpenH264 compiled in (`rtc_use_h264 = true`), used when no
  VT format matches (fork `webrtc-sys/src/video_encoder_factory.cpp:567-583`).
- Decoder: VT first; H.264 falls back to `webrtc::H264Decoder::Create()`
  (`video_decoder_factory.cpp:146`). VP8, VP9, AV1 always decode in software.
- `RTCVideoEncoderH264` only enables hardware, doesn't require it.
- VT's "hw accl disabled" line is forwarded as `log::debug!` target `libwebrtc`
  (`libwebrtc/src/native/peer_connection_factory.rs:43`); core's `RUST_LOG`
  (`tauri/src-tauri/src/lib.rs:561-566`) excludes it, so it never appears.
- Only trace today: end-of-call `VideoHealthSummary ... implementations=` (`core/src/livekit/stats.rs:59`,
  `:398`, `:407`). `power_efficient_encoder/decoder` never read. The ObjC wrapper reports hardware
  regardless, so a software fallback inside VT is invisible.
- Fix, no `unsafe`: warn/alert when an implementation isn't VideoToolbox; in the fork, require
  hardware or read VT's `UsingHardwareAcceleratedVideoEncoder` and report it.

### Camera (same pattern, outside the invariant's letter)
- `core/src/camera/stream_macos.rs`: `:221` NV12→I420 at full camera size (no capture size chosen,
  TODO `:472`); `:320` CPU scale, new buffer per frame; `:324-326` copy into `stream_frame`;
  `:303` copy for local preview; encoder converts I420→NV12.
- ~1.5% of a core at 30 fps (audit #5).
- Reuse race on `state.i420` and `stream_frame` (`:206`, `:333`, `:345`).
- Remote cameras go through violation 1.
- Route: choose capture size (`activeFormat`/`videoSettings`), send the CVPixelBuffer zero-copy via
  the same fork API, draw the preview from it.

### Not violations
- Black keepalive frames: synthetic I420, ≤5 s per muted track (`room_service.rs:2490-2507`); a
  §CHANGE-DRIVEN-VIDEO concern.
- Overlay and drawing windows never read captured frames. Core has no GPU readback anywhere (no
  `map_async`, no `copy_texture_to_buffer`); the only mapped range is the staging upload at
  `yuv_renderer.rs:280`.

## Already GPU-resident
- SCK captures IOSurface-backed `420v` at the target size, scaling on the GPU; capture rate follows
  the encoder.
- VT encodes once frames reach its pool; VT decodes into IOSurface-backed native buffers, wrapped
  without conversion (fork `libwebrtc/src/native/video_stream.rs:134-139`).
- YUV→RGB, crop, sharpening, rounded corners in `yuv_shader.wgsl`; present via iced/wgpu/Metal.
- Screen-share and camera windows upload only new frames (`screensharing_window.rs:2511`,
  `camera_window.rs:1611`).
- Non-IOSurface capture frames are dropped with a warning (`macos_stream.rs:373`).

## Open questions
- **Decoder color range:** upstream `RTCVideoDecoderH264` asks VT for full range (`420f`). If so,
  the shader's limited-range BT.709 (`yuv_shader.wgsl:62-73`) stretches the image. Check a decoded
  native buffer's pixel format.
- **Codec switch:** a VT encode failure may make libwebrtc switch to software VP8; needs libwebrtc
  source to settle.
- **Zero-copy pool depth:** whether SCK's default queue depth starves while the encoder holds buffers.
- **`unsafe` for the full viewer route:** `docs/unsafe.md` entry, a vetted crate, or present outside
  wgpu (e.g. `AVSampleBufferDisplayLayer`, a large change).
- **Fork access:** every safe route needs new `awkay/rust-sdks` APIs; its owner has to land them.

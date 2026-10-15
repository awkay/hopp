# 0007: Discoveries and limitations

Drafting research, then the viewer-direction spike (2026-10-09, M4, `profile.sh viewer`, local
LiveKit `--dev`).

- Spike: the harness's fake share (3024x1964, 40 fps, 12 Mbps cap, scrolling synthetic text)
  published as H.265 through our SDK fork is encoded by VideoToolbox; core decodes it with
  VideoToolbox (`VideoHealthSummary codecs={"H265"} implementations={"VideoToolbox"}`) and its
  window presents ~40 fps with 0 dropped frames and no freezes. No SDK change was needed for this
  direction. (The sharer direction was tested next; see below.)
- Spike numbers, two runs per codec (core only; trace CPU / energy-phase power):
  H.264 34–38% of a core, 323 and 436 mW; HEVC 33% and 33%, 467 and 481 mW. CPU is the same or
  lower with HEVC; power was higher in both pairs but H.264 alone varied by 113 mW, so B7 needs
  more runs and real content before concluding. On this synthetic content HEVC sent ~45% more
  bytes than H.264 (0.60 vs 0.41 Mbps), both far under the cap; quality wasn't measured, so this
  says nothing about bitrate at equal quality.
- Fixed: core's `VideoHealthSummary` logged empty inbound `codecs` and `implementations`, because
  it read them only from the first stats sample, before libwebrtc fills them in.
- Sharer spike (2026-10-09, M4, 3440x1440 display, `profile.sh sharer --codec`, which sets the
  `HOPP_SCREEN_SHARE_CODEC` dev override): core encodes its real ScreenCaptureKit capture as
  H.265 in hardware (WebRTC logs `RTCVideoEncoderH265.mm:433: Compression session created with hw
  accl enabled`), and the fake viewer decodes `video/H265` with VideoToolbox: ~39.6 fps, first
  frame 442 ms after the share starts (H.264: 439 ms), 1 keyframe, 0 PLIs, no quality limitation.
  One run each with uncontrolled screen content: H.264 1.70 Mbps, 236 mW; HEVC 1.31 Mbps,
  385 mW. HEVC drew more power in all three pairs so far (two viewer, one sharer).
- Our SDK fork pin (`2ce5cf0`) already has upstream's three HEVC follow-ups (`6857edc` "modified
  h265", `73f614c` "Fix mixed-codec publisher preferences", `d3442fb` "Keep VideoToolbox for
  selected H264 profiles"); upstream's reverts each flip one codec line. No SDK change was needed
  in either direction. Core publishes the share alone on the `:video` connection, so the
  mixed-codec case `73f614c` fixed (camera H.264 and share H.265 on one connection) can't arise.
- `implementation=VideoToolbox` in WebRTC stats doesn't prove hardware: the Apple wrappers report
  it either way (the decoder even logs `is_hardware_accelerated = false`). The proof is the "hw
  accl enabled" line, logged with `RUST_LOG=hopp_core=info,libwebrtc=debug`. B6 needs a check
  that doesn't rely on the stats name.
- Risk for B7 (text clarity): our WebRTC binary's HEVC screen-share encoder sets VideoToolbox
  `minQP: 23` (maxQP 51, screen-sharing mode); the H.264 screen share sets only maxQP 51. A QP
  floor may stop HEVC getting as sharp as H.264 on static text. Changing it is tuning inside the
  WebRTC binary, which this spec puts out of scope.
- Core feeds ScreenCaptureKit frames to WebRTC as CPU `NV12Buffer`s (`capture/stream.rs`), for
  both codecs.

- Apple hardware (VideoToolbox probe on an M4, macOS 15.7.9): hardware encoders for H.264 and
  HEVC only, no AV1 encoder at all; hardware decode for H.264, HEVC and AV1, not VP9. Apple's M5
  spec also lists AV1 as decode-only. Our WebRTC uses libaom and dav1d (CPU) for AV1.
- The shipped WebRTC binary (`webrtc-9c9f414`, gethopp's M144 build) contains
  `RTCVideoEncoderH265` and `RTCVideoDecoderH265`, and its macOS build sets `rtc_use_h265=true`.
  Our SDK pin has `VideoCodec::H265`. Current core already decodes HEVC (spike above).
- Upstream history: H.265 screen sharing shipped 2026-08-01 inside #363, reverted the next day
  ("m1 doesn't share anything", `3cad4d9`), re-added after gethopp/rust-sdks commits "modified
  h265", "Fix mixed-codec publisher preferences" and "Keep VideoToolbox for selected H264
  profiles", then reverted for good 2026-08-07 (`01e59cf`) with no reason recorded.
- LiveKit's codec doc still lists only H.264, VP8, VP9 and AV1, and says H.265 isn't supported
  because browsers don't support it. The server parses H.265 since at least v1.10.1, and the JS
  client added it in 2.15.0. Our self-hosted server's version and config are in a separate infra
  repo, not this one.
- Browser support for H.265 over WebRTC: Safari 18 and Chrome 136 (hardware decode only), not
  Firefox.
- Neither our SDK pin nor upstream livekit 0.9.4 has a backup codec in `TrackPublishOptions`; our
  pin only reads `subscribed_codecs` from the server's quality updates to pick dynacast layers.
- Participant attributes: `LocalParticipant::set_attributes`, `RemoteParticipant::attributes` and
  `RoomEvent::ParticipantAttributesChanged` exist in our SDK pin.
- Viewers: `TrackSubscribed` for an already unmuted screen track starts the stream;
  `TrackUnpublished` handles only cameras, so a replaced screen track needs new handling.
- Offline benchmark (ffmpeg `h264_videotoolbox` vs `hevc_videotoolbox`, realtime, no B-frames, on
  a synthetic 2560x1600 split dark/light editor): equal PSNR needs 24–35% less bitrate with HEVC
  when scrolling, ~28–29% in low-bandwidth mode. At 1.5 Mbps scrolling: 38.8 dB H.264 vs 44.7 dB
  HEVC. Near-static typing: H.264 stays at 47–49 dB PSNR even with a 12 Mbps budget (using under
  1 Mbps) while HEVC reaches 54–56 dB. VideoToolbox undershoots targets on screen content
  (12 Mbps target, 6.3 Mbps used by H.264 while scrolling). VMAF saturates at 100 on this content.
  ffmpeg's settings are not WebRTC's, and our binary carries extra H.264 screen-share tuning, so
  these numbers need confirming in a real call.

## Implementation (2026-10-09)

- **Joins arrive before announcements.** The LiveKit server batches the participant updates of
  participants that publish nothing and sends them every 3 s (`subscriberUpdateInterval`), but
  sends attribute and metadata changes, and publishers' updates, at once (`pkg/rtc/room.go`). So
  `set_attributes` after connecting is not delivered "with the join info", as the spec's first
  draft assumed. Core waits `HEVC_ANNOUNCEMENT_GRACE` (750 ms) for an unannounced watcher before
  counting it as H.264-only (`ScreenShareCodecChoice::watcher_unannounced` /
  `announcement_overdue`); a watcher that publishes a track without announcing counts at once.
- **Only the audio token may set attributes.** The backend's audio token has
  `CanUpdateOwnMetadata`, the video and camera tokens don't, so the announcement goes on the
  `:audio` participant. The web app's Room page joins with the audio token too, so browser
  viewers are `:audio` identities without the attribute. The harness tokens mirror this
  (`livekit_utils::generate_participant_token`).
- **The sharer's own main participant is a remote one** on the `:video` connection that publishes
  the screen track. It is left out of the watchers and follows the local setting; counting it
  would make a setting change round-trip through the server.
- **A full reconnect drops our attributes.** The SDK's "restart" joins as a new server session and
  republishes every local track (new sids) before `Reconnected`, when core announces again. A
  sharer sees the reconnecting watcher's tracks first and switches to H.264 until its next share.
  Viewers see a sharer's restart as a track replacement, which keeps their window open.
- **Unpublish first, then publish.** Master LiveKit's `AddTrack` has no duplicate-source check
  (keyed by cid; read in the source, not tried), so publishing the new track first would be
  allowed, but an older Hopp viewer closes its window on the old track's unsubscribe and reopens
  it only for a track that arrives after that.
- **Hardware isn't required by WebRTC.** `RTCVideoEncoderH265.mm` only *prefers* VideoToolbox's
  hardware encoder (`EnableHardwareAcceleratedVideoEncoder`), so VideoToolbox may silently use
  its software HEVC encoder. libwebrtc has no software HEVC encoder of its own, so the encoder
  factory can't fall back to one; session creation failure returns `WEBRTC_VIDEO_CODEC_ERROR`.
  The only proof of hardware is the log line "Compression session created with hw accl
  enabled" (the H.264 encoder logs the same text; the file name tells them apart). All WebRTC
  logs reach Rust as `log::debug!(target: "libwebrtc")` from a sink registered at LS_VERBOSE,
  visible only when the global max level is Debug, so core's logger (`webrtc_log.rs`) raises it.
  If a WebRTC update changes that text, every HEVC share falls back to H.264 (logged).
- **Static screens send nothing.** ScreenCaptureKit sends no frames while the screen doesn't
  change, so a newly published track had nothing to encode and a viewer got no key frame until
  the screen changed. Core resends the capture's last frame while a new track warms up
  (`check_screen_share_encoder`, `Capturer::resend_last_frame`).
- **Viewer frame ids continue across streams** (`process_video_stream`), so a replacement
  stream's first frames count as new at once; the window's "stream restart" path needed a drop
  of more than 40 ids.
- Session tooling: the `svg_renderer` unit tests write `*_badge.png` files into `core/`; delete
  them after `cargo test --workspace --lib`.

## Resolved bugs

- Core's `VideoHealthSummary` logged empty inbound `codecs` and `implementations` (above).
- `profile.sh` printed its `set -uo pipefail` line as part of the usage text (range off by one).

## Remaining limitations

- B3's 2 s can only be measured from when the sharer learns of a join: a browser viewer, which
  publishes nothing, can reach the sharer up to 3 s after it joined.
- After a full reconnect of a watcher, a sharer may fall back to H.264 for the rest of its share.
- B7 isn't measured yet; HEVC drew more power in every pair of runs so far, and the binary's
  minQP 23 may cap text sharpness (above).

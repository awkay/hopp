# 0007: Discoveries and limitations

Drafting research, then the viewer-direction spike (2026-10-09, M4, `profile.sh viewer`, local
LiveKit `--dev`).

- Spike: the harness's fake share (3024x1964, 40 fps, 12 Mbps cap, scrolling synthetic text)
  published as H.265 through our SDK fork is encoded by VideoToolbox; core decodes it with
  VideoToolbox (`VideoHealthSummary codecs={"H265"} implementations={"VideoToolbox"}`) and its
  window presents ~40 fps with 0 dropped frames and no freezes. No SDK change was needed for this
  direction. Core encoding HEVC (the sharer direction) is still untested.
- Spike numbers, two runs per codec (core only; trace CPU / energy-phase power):
  H.264 34–38% of a core, 323 and 436 mW; HEVC 33% and 33%, 467 and 481 mW. CPU is the same or
  lower with HEVC; power was higher in both pairs but H.264 alone varied by 113 mW, so B7 needs
  more runs and real content before concluding. On this synthetic content HEVC sent ~45% more
  bytes than H.264 (0.60 vs 0.41 Mbps), both far under the cap; quality wasn't measured, so this
  says nothing about bitrate at equal quality.
- Fixed: core's `VideoHealthSummary` logged empty inbound `codecs` and `implementations`, because
  it read them only from the first stats sample, before libwebrtc fills them in.

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

No bugs have been resolved yet.

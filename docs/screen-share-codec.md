# Screen share codec (HEVC or H.264)

Spec 0007. Each share is HEVC when the sharer's Mac is an M3 or newer, the sharer's "Use HEVC for
screen sharing" setting is on, and every participant who can watch it announces HEVC support;
otherwise H.264. The decision is the pure `ScreenShareCodecChoice` (`core/src/screen_share_codec.rs`,
unit tested); `core/src/room_service.rs` feeds it and republishes the track.

## Announcing support

- Core sets the participant attribute `hopp_hevc` on its main participant (the `:audio` one, which
  subscribes to shares): `1` with the setting on, `0` with it off. Only `1` counts; a missing
  attribute (older Hopp, upstream clients, the web app's Room page) counts as H.264-only. Key and
  parsing live in `socket_lib::screen_share_hevc`, shared with the harness.
- Setting attributes needs the `CanUpdateOwnMetadata` grant. The backend gives it to the audio
  token only (`backend/internal/handlers/utils.go`); the harness tokens mirror that.
- Core announces right after its main room connects (not awaited), again when the setting changes,
  and after every `Reconnected`: a full reconnect joins as a new server session without our
  attributes.

## Who counts as a watcher

The screen track is published by the `:video` participant on its own connection, so watchers are
tracked from that room's events (`handle_video_room_events`). Every `:video` participant is left
out (they never subscribe to shares), and so is our own main participant, which shows up there as
a remote one and follows our own setting.

**LiveKit doesn't deliver a join together with attributes set after connecting.** The server
batches participant updates of participants that publish nothing and sends them every 3 s
(`subscriberUpdateInterval`), but sends attribute changes and publishers' updates at once. A
sharer can therefore see a new Hopp client join before its announcement. An unannounced watcher
doesn't count until it announces, publishes a track (a Hopp client announces before it
publishes, so it counts as H.264-only at once), or `HEVC_ANNOUNCEMENT_GRACE` (750 ms) passes
(H.264-only too). A browser viewer that publishes nothing may reach the sharer up to 3 s after it
joined, so B3's 2 s can only be met from the moment the sharer learns of it.

A full reconnect (the SDK's "restart") joins as a new server session without our attributes,
and the SDK republishes our tracks before `Reconnected`, when core announces again. A sharer
sees those tracks first and switches its share to H.264 until its next share.

## Switching codec

- A published track can't change codec. A switch unpublishes the screen track and publishes a new
  one from the same `NativeVideoSource`, so the capture carries on untouched
  (`republish_screen_share_track`). It happens when a share starts with a different pick than the
  muted track's, or mid-share when a watcher without HEVC joins, a watcher or the user turns HEVC
  off, or HEVC encoding fails. Mid-share it only goes HEVC → H.264; HEVC comes back at the next
  share.
- **The old track goes first.** An older Hopp viewer closes its window when the shown track is
  unsubscribed and reopens it for the new one; publishing the new track first, it would close it
  for good. Publishing first would be allowed: LiveKit's `AddTrack` has no duplicate-source
  check (read in the server's source, not tried).
- A viewer on this version keeps its window: it remembers the shown track's sid, ignores the
  unsubscribe of a track it no longer shows, and when the shown track of a `:video` participant
  is unpublished while unmuted, it stops the stream but keeps the last frame up for up to 5 s
  (`SCREEN_SHARE_REPLACEMENT_GRACE`). The new track's stream then feeds the same buffer and redraw
  channel (`replace_remote_screen_share_stream`): no `OpenScreenShareWindow`, so no refocus
  (§TILING-WM) and no reset of drawings. A share still ends with a mute; a browser sharer's
  unpublish ends it at once.
- **Static screens.** ScreenCaptureKit sends no frames while nothing changes, so a new track
  would have nothing to encode, and a viewer subscribing to it would get no key frame until the
  screen changed. For 3 s after a republish, and until the first frame is encoded after a share
  starts, the capture resends its last frame every 250 ms (`check_screen_share_encoder`,
  `Capturer::resend_last_frame`).
- A new track starts with the publish defaults, so low-bandwidth mode's encoding is applied again
  right after the republish (`apply_screen_share_encoding`).

## Hardware HEVC (B6)

WebRTC's `RTCVideoEncoderH265` only *prefers* VideoToolbox's hardware encoder
(`EnableHardwareAcceleratedVideoEncoder`, never `Require…`). libwebrtc has no software HEVC
encoder of its own, but VideoToolbox may fall back to its software one without telling. The stats
report `implementation=VideoToolbox` either way; only WebRTC's log says which:
`(RTCVideoEncoderH265.mm:433): Compression session created with hw accl enabled` (or
`disabled`, or `Failed to create compression session`). The H.264 encoder logs the same text
from `RTCVideoEncoderH264.mm`.

- Core's logger (`core/src/webrtc_log.rs`, installed by `main.rs`) wraps env_logger and remembers
  what the last of those H.265 lines said. Our SDK forwards every libwebrtc message as
  `log::debug!(target: "libwebrtc")`, so the logger raises the global max level to Debug;
  env_logger still decides what is printed. WebRTC formats all those messages anyway (its sink is
  registered at the most verbose level, ~430 a second in a sharer run, mostly
  `text_pcap_packet_observer.cc`), about 0.3% of core's CPU samples; the watch adds a substring
  search per message.
- After an HEVC share starts or its track is republished, core checks the outbound stats
  (`check_screen_share_encoder`): within 5 s the track must encode a frame as H265, and the last
  H.265 session must have been logged as hardware. Otherwise (nothing encoded, WebRTC switched to
  another codec after the encoder failed, a software session, or no session line at all) the
  share republishes as H.264 for the rest of the call. If a WebRTC update changes that log text,
  every HEVC share falls back to H.264 (logged), which `profile.sh sharer` and the smoke
  `hevc-fallback` scenario show.

## Testing

- `HOPP_SCREEN_SHARE_CODEC=h264|h265` forces core's codec whoever watches (a dev override, logged
  as a warning); `core/tests/profile.sh sharer --codec` sets it.
- `core/tests/profile.sh sharer --switch-after N` has a watcher without HEVC join mid-run;
  `viewer --switch-after N` has the fake sharer replace its track the way core does. The run's
  `.txt` reports the switch.
- `core/tests/smoke.sh hevc-fallback track-replace`: the B3 switch, sharer and viewer side.

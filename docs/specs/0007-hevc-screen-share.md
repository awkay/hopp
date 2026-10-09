# 0007: HEVC screen sharing by default

## Goal

Screen shares between our Macs use HEVC (H.265) instead of H.264 by default. HEVC needs about
25–35% less bitrate than H.264 for the same picture and keeps text sharper while typing, which
helps most in low-bandwidth mode and on poor connections. Apple Silicon encodes and decodes it in
hardware. Anyone who can't decode HEVC still sees the share, because the call falls back to H.264.

## Behavior

- **B1.** When someone on an M3-or-newer Mac shares their screen and every participant who can
  watch it supports HEVC, core shall publish the share as HEVC, encoded and decoded in hardware.
- **B2.** When any watcher doesn't support HEVC (an older Hopp, a browser viewer, or someone who
  turned HEVC off), or the sharer's Mac is older than M3, the share shall use H.264.
- **B3.** When such a watcher joins during an HEVC share, the share shall switch to H.264 for
  everyone within 2 seconds, without ending the share or closing viewer windows. A brief freeze is
  acceptable. The share returns to HEVC only when the next share starts.
- **B4.** Settings shall have a "Use HEVC for screen sharing" option, on by default. When it is
  off, the user's own shares use H.264 and the user counts as a watcher without HEVC support (B2,
  B3). Turning it off during a call takes effect at once, like a joining watcher in B3. Turning it
  back on during a share takes effect at the next share.
- **B5.** Low-bandwidth mode shall keep changing bitrate, frame rate and resolution live with both
  codecs, including after a B3 switch.
- **B6.** If hardware HEVC encoding can't start, core shall use H.264 for that share and log why.
  It shall never fall back to a software HEVC encoder.
- **B7.** Compared with H.264 on the same Mac and content, HEVC shares shall not regress CPU,
  energy, latency or delivered frame rate, and text shall be at least as sharp. Results are recorded
  in the completion notes.

Older clients (ours and upstream's; calls mix versions): they don't announce HEVC support, so they
count as H.264-only and calls with them use H.264 (B2, B3). No minimum app version. Browser viewers
on the web app's Room page count as H.264-only the same way.

Out of scope: HEVC for cameras (they stay H.264, which the web app also publishes); AV1;
per-viewer codecs (LiveKit's backup codec); switching back to HEVC mid-share; a remote switch to
turn HEVC off for everyone; tuning the HEVC encoder inside the WebRTC binary; HEVC bitrate tuning
beyond today's caps.

## Design

**Announcing support.** Core sets a participant attribute (e.g. `hopp_hevc=1`) on its main
participant, the one that subscribes to screen shares, with `LocalParticipant::set_attributes` when
it connects, unless the user turned HEVC off. Attributes arrive with the participant's join info, so
there is no race. Changes come through `RoomEvent::ParticipantAttributesChanged`. Our SDK pin
already has both. Each user also has a `:video` participant that never subscribes to shares; it
doesn't count as a watcher.

**Choosing the codec.** A new pure module, like `BandwidthNegotiation` in `bandwidth_mode.rs`,
decides from: the sharer's chip (M3 or newer, else H.264), the sharer's setting, and the HEVC
support of the current watchers. It has unit tests and no LiveKit types. The chip comes from
`sysctl machdep.cpu.brand_string` ("Apple M4"); anything unrecognized counts as older than M3.

**Publishing.** Today the screen track is published muted with H.264 when the call connects
(`room_service.rs`, screen-share publish around `VideoCodec::H264`), and sharing unmutes it. A
track's codec can't change while it is published, so a codec change means unpublishing the track
and publishing a new one from the same `NativeVideoSource`. At connect, the track is published with
the codec the module picks for the participants present. When a share starts and the pick differs,
or a B3 switch happens mid-share, the track is republished. A republish re-applies low-bandwidth
settings to the new track (B5).

**Watching.** Viewers handle a replaced screen track: the unpublish of the current share (today
`RoomEvent::TrackUnpublished` ignores screen shares) must not close the viewer window when a
replacement follows, and `TrackSubscribed` for an already unmuted screen track already starts a
stream. Confirm it replaces the old stream cleanly.

**Setting.** A new persisted setting in the Tauri app (`app_state.rs`, settings window), sent to
core with the startup config and when changed. That is an IPC change: read `docs/ipc.md` first,
and include the setting in the config re-sent after a core restart.

**Server.** Our LiveKit server is self-hosted on AWS and deployed from a separate infra repo. The
user confirmed on 2026-10-09 that it accepts H.265.

**Bitrate.** Keep today's caps for HEVC (12 Mbps normal, 900 kbps low-bandwidth). VideoToolbox
stays well under the normal cap on screen content, so the gain shows up as sharper text and lower
use, not as a higher cap.

**Where it lands.** This branch builds on tracker 0005's call-cost work (its steps 1–2 are on
`main`), which changed `room_service.rs`, `bandwidth_mode.rs` and the harness, and points the
harness at our LiveKit fork. Spec 0006 (SDK bump) isn't needed: the current SDK has
`VideoCodec::H265`, and the shipped WebRTC binary has Apple's H.265 encoder and decoder
(`RTCVideoEncoderH265`, `RTCVideoDecoderH265`).

**Upstream files changed:** `core/src/room_service.rs`, `core/src/bandwidth_mode.rs`, Tauri
settings and app state, `core/tests` harness. Record the divergence in `DATAICO.md`.

Alternatives rejected:
- LiveKit's backup codec, which sends an extra H.264 copy only to viewers who need it: our Rust
  SDK doesn't implement it (neither does upstream 0.9.4), and the second encode costs CPU.
- HEVC always, with a minimum app version: breaks browser viewers.
- Opt-in switch: the user wants HEVC as the default (2026-10-09).

## Tasks

- [x] Agree on Behavior; tracker stays `spec` until approval. (Approved 2026-10-09.)
- [x] Before any code: confirm the production LiveKit server accepts H.265 (user confirmed
      2026-10-09).
- [x] Spike: the harness shares in HEVC to core with `profile.sh`; core receives, decodes and shows
      it; record codec, encoder implementation and CPU. (2026-10-09, results in the notes.)
- [ ] Codec decision module with unit tests.
- [ ] Announce support with the participant attribute; react to joins, leaves and attribute changes.
- [ ] Publish with the chosen codec; republish on a change; re-apply low-bandwidth settings.
- [ ] Viewer handling of a replaced screen track.
- [ ] Setting in Tauri and its IPC to core (`docs/ipc.md`).
- [ ] B6: require hardware HEVC; fall back to H.264 with a log line.
- [ ] Harness: selectable codec for the fake share; smoke scenario with a watcher without HEVC.
- [ ] `DATAICO.md` divergence row; anything that stays true after shipping moved into a `docs/`
      topic doc.
- [ ] Preserve discoveries from `workingcontext.md` in `docs/specs/0007-hevc-screen-share/notes.md`
      and link below before marking complete, merging or removing the worktree. Include notes,
      quirks, awkward behavior, gotchas and bugs (resolved vs. remaining); explicitly say if none
      were found.

## Verification

Automated, from `core/`: `cargo build`, `cargo test --workspace --lib` (decision module: B1, B2,
B4) and `cargo clippy --all-features -- -D warnings`; Tauri checks from `CLAUDE.md` for the setting.

Harness: `core/tests/profile.sh` with the fake share in HEVC and in H.264 (B1, B7): the codec and
encoder implementation in core's `VideoHealthSummary` log, CPU, energy and presented frames. The
user runs `core/tests/smoke.sh` with a new scenario: a watcher without HEVC joins mid-share, the
share switches to H.264 within 2 s and the viewer window stays open (B3).

Manual, by the user, on two M3-or-newer Macs:
1. Share: the other side receives HEVC (codec in `hopp.log`); text stays sharp while typing (B1).
2. Turn the setting off on the viewer during the share: switch to H.264 within 2 s (B3, B4).
3. Open the web app's Room page in a browser during the share: switch to H.264 (B2, B3).
4. Toggle low-bandwidth mode during an HEVC share and after a switch (B5).
5. Share from an M1 or M2, if one is available: H.264 (B2).
6. Compare a typing session and a low-bandwidth session in HEVC and H.264 (B7).

## Decisions

- **Q:** Opt-in or opt-out? **A:** HEVC on by default with a setting to turn it off. (2026-10-09)
- **Q:** Why M3 or newer for sharers, when every Apple Silicon chip encodes HEVC in hardware?
  **A:** Upstream shipped HEVC sharing on 2026-08-01 and reverted it the next day because M1 Macs
  shared nothing (cause never recorded), then reverted again for good on 2026-08-07 (`01e59cf`).
  The team uses M3 to M5, so the gate avoids that unknown at no cost. (2026-10-09)
- **Q:** Per-viewer codecs or everyone falls back? **A:** Everyone falls back; per-viewer needs
  backup-codec support our SDK lacks. (2026-10-09)
- **Q:** Switch back to HEVC when the H.264-only watcher leaves? **A:** Only at the next share
  start, so joins and leaves don't cause repeated freezes. (2026-10-09)
- **Q:** A remote switch to turn HEVC off for everyone if it breaks in the field? **A:** No. Each
  user's setting plus a fast in-app update (spec 0004) is enough. (2026-10-09)
- **Q:** Does turning the setting back on mid-share switch to HEVC? **A:** No, it takes effect at
  the next share, like B3. (2026-10-09)

## Completion notes

[Notes, quirks, awkward behavior, gotchas and bugs](0007-hevc-screen-share/notes.md)

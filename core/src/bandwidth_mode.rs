//! Low-bandwidth mode: negotiation between call participants and the screen
//! share encoding it implies. Kept free of LiveKit types so it can be unit tested.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::room_service::canonical_participant_identity;

pub(crate) const MAX_FRAMERATE: f64 = 40.0;

// Bitrate constants (in bits per second)
pub(crate) const AV1_BITRATE_DEFAULT: u64 = 5_000_000; // 5 Mbps
pub(crate) const H264_BITRATE_DEFAULT: u64 = 12_000_000; // 12 Mbps

pub(crate) const LOW_BANDWIDTH_BITRATE: u64 = 900_000;
pub(crate) const LOW_BANDWIDTH_FRAMERATE: f64 = 15.0;
const LOW_BANDWIDTH_MAX_WIDTH: f64 = 1920.0;
const LOW_BANDWIDTH_MAX_HEIGHT: f64 = 1080.0;

/// The screen share is published as AV1 on Windows and H.264 elsewhere.
pub(crate) const SCREEN_SHARE_USES_AV1: bool = cfg!(target_os = "windows");

pub(crate) fn default_screen_bitrate() -> u64 {
    if SCREEN_SHARE_USES_AV1 {
        AV1_BITRATE_DEFAULT
    } else {
        H264_BITRATE_DEFAULT
    }
}

/// Payload of the `bandwidth_mode` data topic.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct BandwidthModeRequest {
    pub low_bandwidth: bool,
}

/// Tracks who is requesting low-bandwidth mode. The mode is active while
/// anyone (local or remote) requests it.
#[derive(Debug, Default)]
pub(crate) struct BandwidthNegotiation {
    local: bool,
    /// Keyed by canonical participant identity.
    remote: HashMap<String, bool>,
}

impl BandwidthNegotiation {
    pub fn local(&self) -> bool {
        self.local
    }

    pub fn effective(&self) -> bool {
        self.local || self.remote.values().any(|requested| *requested)
    }

    /// Canonical identities of the remote participants requesting the mode, sorted.
    pub fn requesters(&self) -> Vec<String> {
        let mut requesters: Vec<String> = self
            .remote
            .iter()
            .filter(|(_, requested)| **requested)
            .map(|(identity, _)| identity.clone())
            .collect();
        requesters.sort();
        requesters
    }

    /// Returns whether the effective mode changed.
    pub fn set_local(&mut self, requested: bool) -> bool {
        let before = self.effective();
        self.local = requested;
        before != self.effective()
    }

    /// Returns whether the effective mode changed.
    pub fn set_remote(&mut self, identity: &str, requested: bool) -> bool {
        let before = self.effective();
        self.remote.insert(
            canonical_participant_identity(identity).to_string(),
            requested,
        );
        before != self.effective()
    }

    /// Returns whether the effective mode changed.
    pub fn remove(&mut self, identity: &str) -> bool {
        let before = self.effective();
        self.remote.remove(canonical_participant_identity(identity));
        before != self.effective()
    }

    /// Returns whether the effective mode changed.
    pub fn clear(&mut self) -> bool {
        let before = self.effective();
        self.local = false;
        self.remote.clear();
        before != self.effective()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ScreenEncoding {
    pub max_bitrate: u64,
    pub max_framerate: f64,
    pub scale_down_by: f64,
}

/// Encoding for the screen share given the mode and the captured frame size.
/// In low-bandwidth mode the frame is scaled down to fit 1920x1080, never up.
pub(crate) fn screen_encoding(
    low: bool,
    capture_width: u32,
    capture_height: u32,
) -> ScreenEncoding {
    if !low {
        return ScreenEncoding {
            max_bitrate: default_screen_bitrate(),
            max_framerate: MAX_FRAMERATE,
            scale_down_by: 1.0,
        };
    }
    let scale_down_by = (capture_width as f64 / LOW_BANDWIDTH_MAX_WIDTH)
        .max(capture_height as f64 / LOW_BANDWIDTH_MAX_HEIGHT)
        .max(1.0);
    ScreenEncoding {
        max_bitrate: LOW_BANDWIDTH_BITRATE,
        max_framerate: LOW_BANDWIDTH_FRAMERATE,
        scale_down_by,
    }
}

/// What ScreenCaptureKit captures before frames reach the encoder when the display's refresh
/// rate is unknown, or when the encoder's rate doesn't divide it.
const CAPTURE_FALLBACK_FRAMERATE: u32 = 60;

/// Capture rate for a share encoded at `encoder_fps` from a display refreshing at `refresh_hz`
/// (`None`: unknown, e.g. a window share, assumed 60 Hz).
///
/// Capturing faster than the encoder takes only makes libwebrtc drop the extra frames after
/// they were copied. But ScreenCaptureKit delivers frames on display refreshes only, so a rate
/// that doesn't divide the refresh rate comes out lower and uneven: 40 fps on a 60 Hz display
/// would capture 30. Those cases keep the 60 fps capture and the encoder's dropping.
pub(crate) fn capture_framerate(encoder_fps: f64, refresh_hz: Option<u32>) -> u32 {
    let fps = encoder_fps.round() as u32;
    let refresh_hz = refresh_hz.unwrap_or(60);
    if fps > 0 && refresh_hz.is_multiple_of(fps) {
        fps
    } else {
        CAPTURE_FALLBACK_FRAMERATE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_matches_the_encoder_when_the_display_allows_it() {
        // Low-bandwidth mode: 15 divides 60 and 120.
        assert_eq!(capture_framerate(LOW_BANDWIDTH_FRAMERATE, Some(60)), 15);
        assert_eq!(capture_framerate(LOW_BANDWIDTH_FRAMERATE, Some(120)), 15);
        assert_eq!(capture_framerate(LOW_BANDWIDTH_FRAMERATE, None), 15);
        // ProMotion displays refresh at 120 Hz.
        assert_eq!(capture_framerate(MAX_FRAMERATE, Some(120)), 40);
        // 40 fps can't be captured evenly at 60 Hz: keep capturing 60.
        assert_eq!(capture_framerate(MAX_FRAMERATE, Some(60)), 60);
        assert_eq!(capture_framerate(MAX_FRAMERATE, None), 60);
        assert_eq!(capture_framerate(MAX_FRAMERATE, Some(144)), 60);
    }

    const ALICE: &str = "room:1:alice:audio";
    const BOB: &str = "room:1:bob:audio";

    #[test]
    fn local_request_only() {
        let mut negotiation = BandwidthNegotiation::default();
        assert!(!negotiation.effective());
        assert!(negotiation.set_local(true));
        assert!(negotiation.effective());
        assert!(negotiation.local());
        assert!(negotiation.requesters().is_empty());
        assert!(negotiation.set_local(false));
        assert!(!negotiation.effective());
    }

    #[test]
    fn remote_request_only() {
        let mut negotiation = BandwidthNegotiation::default();
        assert!(negotiation.set_remote(ALICE, true));
        assert!(negotiation.effective());
        assert!(!negotiation.local());
        assert_eq!(negotiation.requesters(), vec!["room:1:alice".to_string()]);
        assert!(negotiation.set_remote(ALICE, false));
        assert!(!negotiation.effective());
        assert!(negotiation.requesters().is_empty());
    }

    #[test]
    fn several_requesters() {
        let mut negotiation = BandwidthNegotiation::default();
        assert!(negotiation.set_remote(BOB, true));
        assert!(!negotiation.set_remote(ALICE, true));
        assert!(!negotiation.set_local(true));
        assert_eq!(
            negotiation.requesters(),
            vec!["room:1:alice".to_string(), "room:1:bob".to_string()]
        );
        assert!(negotiation.effective());
    }

    #[test]
    fn leaving_requester_turns_mode_off_only_when_nobody_else_asks() {
        let mut negotiation = BandwidthNegotiation::default();
        negotiation.set_remote(ALICE, true);
        negotiation.set_remote(BOB, true);
        assert!(!negotiation.remove(ALICE));
        assert!(negotiation.effective());
        assert!(negotiation.remove(BOB));
        assert!(!negotiation.effective());
        assert!(!negotiation.remove("room:1:unknown:audio"));
    }

    #[test]
    fn withdrawing_local_does_not_override_remote_request() {
        let mut negotiation = BandwidthNegotiation::default();
        negotiation.set_remote(ALICE, true);
        assert!(!negotiation.set_local(true));
        assert!(!negotiation.set_local(false));
        assert!(negotiation.effective());
    }

    #[test]
    fn changed_flag_reports_only_effective_transitions() {
        let mut negotiation = BandwidthNegotiation::default();
        assert!(!negotiation.set_local(false));
        assert!(!negotiation.set_remote(ALICE, false));
        assert!(negotiation.set_remote(ALICE, true));
        assert!(!negotiation.set_remote(ALICE, true));
        assert!(!negotiation.set_local(true));
        assert!(!negotiation.set_remote(ALICE, false));
        assert!(negotiation.clear());
        assert!(!negotiation.clear());
        assert!(!negotiation.local());
    }

    #[test]
    fn identities_are_canonicalized() {
        let mut negotiation = BandwidthNegotiation::default();
        negotiation.set_remote("room:1:alice:audio", true);
        negotiation.set_remote("room:1:alice:video", true);
        assert_eq!(negotiation.requesters(), vec!["room:1:alice".to_string()]);
        assert!(negotiation.set_remote("room:1:alice:video", false));
        assert!(!negotiation.effective());
        negotiation.set_remote("room:1:alice:audio", true);
        assert!(negotiation.remove("room:1:alice:video"));
    }

    #[test]
    fn low_mode_scales_retina_capture_to_1080p() {
        let encoding = screen_encoding(true, 3024, 1964);
        assert_eq!(encoding.max_bitrate, 900_000);
        assert_eq!(encoding.max_framerate, 15.0);
        assert!((encoding.scale_down_by - 1964.0 / 1080.0).abs() < 1e-9);
        assert!((encoding.scale_down_by - 1.818).abs() < 1e-3);
    }

    #[test]
    fn low_mode_keeps_1080p_capture() {
        assert_eq!(screen_encoding(true, 1920, 1080).scale_down_by, 1.0);
    }

    #[test]
    fn low_mode_never_upscales_small_capture() {
        assert_eq!(screen_encoding(true, 1280, 800).scale_down_by, 1.0);
    }

    #[test]
    fn normal_mode_restores_defaults() {
        let encoding = screen_encoding(false, 3024, 1964);
        assert_eq!(
            encoding,
            ScreenEncoding {
                max_bitrate: default_screen_bitrate(),
                max_framerate: MAX_FRAMERATE,
                scale_down_by: 1.0,
            }
        );
        #[cfg(target_os = "macos")]
        assert_eq!(encoding.max_bitrate, H264_BITRATE_DEFAULT);
    }

    #[test]
    fn bandwidth_mode_request_round_trip() {
        let request = BandwidthModeRequest {
            low_bandwidth: true,
        };
        let payload = serde_json::to_vec(&request).unwrap();
        assert_eq!(
            std::str::from_utf8(&payload).unwrap(),
            r#"{"low_bandwidth":true}"#
        );
        let decoded: BandwidthModeRequest = serde_json::from_slice(&payload).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn bandwidth_mode_state_message_round_trip() {
        let message = socket_lib::Message::BandwidthModeState(socket_lib::BandwidthModeState {
            active: true,
            local_requested: false,
            requested_by: vec!["Alice".to_string()],
        });
        let json = serde_json::to_string(&message).unwrap();
        let decoded: socket_lib::Message = serde_json::from_str(&json).unwrap();
        match decoded {
            socket_lib::Message::BandwidthModeState(state) => {
                assert!(state.active);
                assert!(!state.local_requested);
                assert_eq!(state.requested_by, vec!["Alice".to_string()]);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }
}

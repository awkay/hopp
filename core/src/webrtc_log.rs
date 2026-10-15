//! Core's logger: env_logger, plus a watch on libwebrtc's log for the one fact only that log
//! reports: whether VideoToolbox encodes the screen share's HEVC in hardware (spec 0007 B6).
//!
//! WebRTC's `RTCVideoEncoderH265` asks VideoToolbox for a hardware encoder but doesn't require
//! one, so VideoToolbox may fall back to its software encoder without telling. Each time it
//! creates a compression session it logs `Compression session created with hw accl enabled` (or
//! `disabled`), or `Failed to create compression session`. Its stats report
//! `implementation=VideoToolbox` either way.
//!
//! Our LiveKit SDK forwards every libwebrtc log message as `log::debug!(target: "libwebrtc")`,
//! from a sink registered at WebRTC's most verbose level, so WebRTC formats them all anyway
//! (hundreds per second in a call). They reach a logger only when the global max level is at
//! least Debug, so [`init_logger`] raises it; env_logger still filters what is printed.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU8, Ordering};

use log::{LevelFilter, Log, Metadata, Record};

const WEBRTC_TARGET: &str = "libwebrtc";
/// Source file of WebRTC's HEVC encoder, in the location prefix of its log lines. The H.264
/// encoder logs the same messages from `RTCVideoEncoderH264.mm`.
const HEVC_ENCODER_FILE: &str = "RTCVideoEncoderH265";

/// What WebRTC logged about the last HEVC compression session it tried to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HevcSession {
    /// None logged since core started.
    Unknown = 0,
    Hardware = 1,
    Software = 2,
    Failed = 3,
}

static LAST_HEVC_SESSION: AtomicU8 = AtomicU8::new(HevcSession::Unknown as u8);

pub(crate) fn last_hevc_session() -> HevcSession {
    match LAST_HEVC_SESSION.load(Ordering::Acquire) {
        1 => HevcSession::Hardware,
        2 => HevcSession::Software,
        3 => HevcSession::Failed,
        _ => HevcSession::Unknown,
    }
}

/// Installs env_logger (`RUST_LOG`, `warn` by default) behind the libwebrtc watch.
pub fn init_logger() {
    let inner =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).build();
    let max_level = inner.filter().max(LevelFilter::Debug);
    if log::set_boxed_logger(Box::new(Logger { inner })).is_ok() {
        log::set_max_level(max_level);
    }
}

struct Logger {
    inner: env_logger::Logger,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if record.target() == WEBRTC_TARGET {
            let mut scan = SessionScan::default();
            // The SDK logs each message as a single `{}` argument, so it arrives in one piece.
            let _ = write!(scan, "{}", record.args());
            if let Some(session) = scan.session {
                LAST_HEVC_SESSION.store(session as u8, Ordering::Release);
            }
        }
        // env_logger prints only what `RUST_LOG` enables.
        self.inner.log(record);
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

/// Reads a libwebrtc message without copying it.
#[derive(Default)]
struct SessionScan {
    session: Option<HevcSession>,
}

impl std::fmt::Write for SessionScan {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if self.session.is_none() {
            self.session = hevc_session(text);
        }
        Ok(())
    }
}

fn hevc_session(message: &str) -> Option<HevcSession> {
    if !message.contains(HEVC_ENCODER_FILE) {
        return None;
    }
    if message.contains("Compression session created with hw accl enabled") {
        Some(HevcSession::Hardware)
    } else if message.contains("Compression session created with hw accl disabled") {
        Some(HevcSession::Software)
    } else if message.contains("Failed to create compression session") {
        Some(HevcSession::Failed)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hevc_session_lines() {
        assert_eq!(
            hevc_session(
                "(RTCVideoEncoderH265.mm:433): Compression session created with hw accl enabled"
            ),
            Some(HevcSession::Hardware)
        );
        assert_eq!(
            hevc_session(
                "(RTCVideoEncoderH265.mm:435): Compression session created with hw accl disabled"
            ),
            Some(HevcSession::Software)
        );
        assert_eq!(
            hevc_session(
                "(RTCVideoEncoderH265.mm:420): Failed to create compression session: -12902"
            ),
            Some(HevcSession::Failed)
        );
    }

    #[test]
    fn other_lines_say_nothing_about_hevc() {
        for message in [
            "(RTCVideoEncoderH264.mm:744): Compression session created with hw accl enabled",
            "(RTCVideoEncoderH264.mm:746): Compression session created with hw accl disabled",
            "(RTCVideoEncoderH264.mm:700): Failed to create compression session: -12902",
            "(RTCVideoEncoderH265.mm:500): h265 encoder dropped a frame.",
            "(video_stream_encoder.cc:1234): Encoder settings changed",
            "",
        ] {
            assert_eq!(hevc_session(message), None, "{message:?}");
        }
    }

    #[test]
    fn a_scan_keeps_the_first_verdict() {
        let mut scan = SessionScan::default();
        // As the SDK logs it.
        let message =
            "(RTCVideoEncoderH265.mm:433): Compression session created with hw accl disabled"
                .to_string();
        write!(scan, "{message}").unwrap();
        scan.write_str(
            "(RTCVideoEncoderH265.mm:433): Compression session created with hw accl enabled",
        )
        .unwrap();
        assert_eq!(scan.session, Some(HevcSession::Software));
    }
}

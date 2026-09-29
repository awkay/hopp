//! The effect data packet: `{"v":1,"id":"thumbs_up","at":null}` on topic `effect`.
//!
//! Published lossy (`reliable: false`): a lost effect is harmless, and lossy
//! packets never queue in front of keystrokes on the reliable channel. Only the
//! id travels; labels and assets are local. Receivers validate everything.

use serde::{Deserialize, Serialize};

use super::{find, manifest, EffectDef};

/// LiveKit data topic. Never reuse `participant_location` (web viewers parse it).
pub const TOPIC_EFFECT: &str = "effect";
/// Packets larger than this are dropped before parsing.
pub const MAX_PAYLOAD_BYTES: usize = 256;
/// Packet format version.
pub const WIRE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WirePoint {
    pub x: f64,
    pub y: f64,
}

/// On-the-wire form. Unknown fields are tolerated for forward compatibility.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectPacket {
    pub v: u32,
    pub id: String,
    /// Normalized position on the shared content. v1 always sends `None` and
    /// receivers centre the effect; the field is kept for placement later.
    #[serde(default)]
    pub at: Option<WirePoint>,
}

/// A validated trigger.
#[derive(Debug, Clone, Copy)]
pub struct EffectTrigger {
    pub effect: &'static EffectDef,
    /// Clamped to 0..=1. Ignored by v1 renderers (kept for placement later).
    #[allow(dead_code)]
    pub at: Option<WirePoint>,
}

/// Encodes a trigger for `id` (v1: no position).
pub fn encode_effect_packet(id: &str) -> Vec<u8> {
    serde_json::to_vec(&EffectPacket {
        v: WIRE_VERSION,
        id: id.to_string(),
        at: None,
    })
    .unwrap_or_default()
}

/// Validates a received payload. Returns `None` (drop) for oversized payloads,
/// malformed JSON, another version, a malformed id, or an id this build does not
/// have. A non-finite point is dropped (the trigger is kept); others are clamped.
pub fn parse_effect_packet(payload: &[u8]) -> Option<EffectTrigger> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return None;
    }
    let packet: EffectPacket = serde_json::from_slice(payload).ok()?;
    if packet.v != WIRE_VERSION || !manifest::is_valid_id(&packet.id) {
        return None;
    }
    let effect = find(&packet.id)?;
    let at = packet
        .at
        .filter(|point| point.x.is_finite() && point.y.is_finite())
        .map(|point| WirePoint {
            x: point.x.clamp(0.0, 1.0),
            y: point.y.clamp(0.0, 1.0),
        });
    Some(EffectTrigger { effect, at })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::EFFECTS;

    fn known_id() -> &'static str {
        EFFECTS[0].id
    }

    #[test]
    fn round_trips_a_known_id_without_a_point() {
        let trigger = parse_effect_packet(&encode_effect_packet(known_id())).unwrap();
        assert_eq!(trigger.effect.id, known_id());
        assert!(trigger.at.is_none());
    }

    #[test]
    fn accepts_a_point_and_clamps_it() {
        let payload = format!(
            r#"{{"v":1,"id":"{}","at":{{"x":0.4,"y":0.6}}}}"#,
            known_id()
        );
        let at = parse_effect_packet(payload.as_bytes()).unwrap().at.unwrap();
        assert_eq!(at, WirePoint { x: 0.4, y: 0.6 });

        let payload = format!(
            r#"{{"v":1,"id":"{}","at":{{"x":-5,"y":1e308}}}}"#,
            known_id()
        );
        let at = parse_effect_packet(payload.as_bytes()).unwrap().at.unwrap();
        assert_eq!(at, WirePoint { x: 0.0, y: 1.0 });

        let payload = format!(
            r#"{{"v":1,"id":"{}","at":{{"x":-1e308,"y":2}}}}"#,
            known_id()
        );
        let at = parse_effect_packet(payload.as_bytes()).unwrap().at.unwrap();
        assert_eq!(at, WirePoint { x: 0.0, y: 1.0 });
    }

    #[test]
    fn a_number_json_cannot_represent_drops_the_packet() {
        // serde_json rejects 1e999 (out of f64 range), so the whole packet goes.
        let payload = format!(
            r#"{{"v":1,"id":"{}","at":{{"x":1e999,"y":0}}}}"#,
            known_id()
        );
        assert!(parse_effect_packet(payload.as_bytes()).is_none());
    }

    #[test]
    fn tolerates_unknown_fields() {
        let payload = format!(r#"{{"v":1,"id":"{}","at":null,"extra":[1,2]}}"#, known_id());
        assert!(parse_effect_packet(payload.as_bytes()).is_some());
    }

    #[test]
    fn drops_oversized_payloads_before_parsing() {
        let padding = " ".repeat(MAX_PAYLOAD_BYTES);
        let payload = format!(r#"{{"v":1,"id":"{}"{padding}}}"#, known_id());
        assert!(payload.len() > MAX_PAYLOAD_BYTES);
        assert!(parse_effect_packet(payload.as_bytes()).is_none());
    }

    #[test]
    fn drops_bad_ids() {
        for id in [
            "",
            "Thumbs_Up",
            "thumbs-up",
            "thumbs up",
            "../x",
            "a_very_long_identifier_over_32_chars",
            "no_such_effect",
        ] {
            let payload = format!(r#"{{"v":1,"id":"{id}"}}"#);
            assert!(parse_effect_packet(payload.as_bytes()).is_none(), "{id}");
        }
    }

    #[test]
    fn drops_other_versions_and_malformed_json() {
        for payload in [
            format!(r#"{{"v":2,"id":"{}"}}"#, known_id()),
            format!(r#"{{"v":0,"id":"{}"}}"#, known_id()),
            format!(r#"{{"id":"{}"}}"#, known_id()),
            format!(r#"{{"v":"1","id":"{}"}}"#, known_id()),
            r#"{"v":1}"#.to_string(),
            r#"{"v":1,"id":7}"#.to_string(),
            "not json".to_string(),
            String::new(),
        ] {
            assert!(
                parse_effect_packet(payload.as_bytes()).is_none(),
                "{payload}"
            );
        }
        assert!(parse_effect_packet(&[0xff, 0xfe, 0x00]).is_none());
    }
}

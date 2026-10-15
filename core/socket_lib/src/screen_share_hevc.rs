//! How a participant announces that it can watch HEVC (H.265) screen shares (spec 0007), shared
//! by core and its test harness.
//!
//! Core sets [`ATTRIBUTE`] on its main participant, the one that subscribes to screen shares.
//! A sharer counts a watcher as supporting HEVC only when the attribute is exactly
//! [`SUPPORTED`]: older clients and browser viewers never set it, and a user who turned the
//! setting off announces [`UNSUPPORTED`].

use std::collections::HashMap;

/// LiveKit participant attribute key.
pub const ATTRIBUTE: &str = "hopp_hevc";
pub const SUPPORTED: &str = "1";
pub const UNSUPPORTED: &str = "0";

/// The attribute value announcing `supported`.
pub fn attribute_value(supported: bool) -> &'static str {
    if supported {
        SUPPORTED
    } else {
        UNSUPPORTED
    }
}

/// The attributes to set on a participant announcing `supported`.
pub fn announcement(supported: bool) -> HashMap<String, String> {
    HashMap::from([(
        ATTRIBUTE.to_string(),
        attribute_value(supported).to_string(),
    )])
}

/// The participant's announcement, `None` when it has none (it may not have announced yet).
pub fn announced(attributes: &HashMap<String, String>) -> Option<bool> {
    attributes.get(ATTRIBUTE).map(|value| value == SUPPORTED)
}

/// Whether a participant with these attributes can watch HEVC shares.
pub fn supports_hevc(attributes: &HashMap<String, String>) -> bool {
    announced(attributes).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attributes(value: &str) -> HashMap<String, String> {
        HashMap::from([(ATTRIBUTE.to_string(), value.to_string())])
    }

    #[test]
    fn only_1_counts_as_support() {
        assert!(supports_hevc(&attributes("1")));
        for value in ["0", "", "true", "yes", " 1", "1 ", "2"] {
            assert!(!supports_hevc(&attributes(value)), "{value:?}");
            assert_eq!(announced(&attributes(value)), Some(false), "{value:?}");
        }
    }

    #[test]
    fn a_missing_attribute_is_no_announcement() {
        assert_eq!(announced(&HashMap::new()), None);
        assert!(!supports_hevc(&HashMap::new()));
        let other = HashMap::from([("other".to_string(), "1".to_string())]);
        assert_eq!(announced(&other), None);
    }

    #[test]
    fn announcements_round_trip() {
        for supported in [true, false] {
            assert_eq!(announced(&announcement(supported)), Some(supported));
        }
    }
}

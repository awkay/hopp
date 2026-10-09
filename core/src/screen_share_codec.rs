//! Screen share codec: HEVC (H.265) when this Mac and every watcher can use it, H.264 otherwise
//! (spec 0007). Kept free of LiveKit types so it can be unit tested.
#![cfg_attr(
    not(test),
    expect(dead_code, reason = "wired into room_service in spec 0007's next step")
)]

use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScreenShareCodec {
    H264,
    H265,
}

/// Picks the screen track's codec and remembers the codec it is published with.
///
/// HEVC needs all three: this Mac may share it ([`this_mac_may_share_hevc`]), the user's "Use
/// HEVC for screen sharing" setting is on, and every watcher supports it. With no watchers
/// nothing stands in the way, so the pick is HEVC; a watcher without HEVC who joins later is
/// handled like any other join.
///
/// Once a share has started, its codec only goes from HEVC to H.264 (a watcher without HEVC
/// joins, a watcher turns HEVC off, or the user turns the setting off), never back: each switch
/// freezes the viewers for a moment, so the share returns to HEVC only when the next one starts.
/// Between shares the muted track keeps its codec, and changes are applied when a share starts.
///
/// The caller reports:
/// - Call connected: [`Self::set_watcher`] for the watchers present, then publish the muted
///   screen track with [`Self::connected`]'s codec.
/// - A watcher joins, or its HEVC support changes: [`Self::set_watcher`]. It leaves:
///   [`Self::remove_watcher`].
/// - The setting changes: [`Self::set_setting`].
/// - A share starts: [`Self::share_started`], before unmuting the track. It ends:
///   [`Self::share_ended`].
/// - The call ends: [`Self::clear`].
///
/// A method returning `Some(codec)` means the track must be republished with `codec` now: a
/// published track can't change codec, so unpublish it and publish a new one from the same
/// source. The codec counts as published from then on.
///
/// Watchers are the participants that can subscribe to the share, keyed by identity: the other
/// users' main participants, and browser viewers. The caller leaves out every `:video`
/// participant, which never subscribes to shares; it may also leave out its own main participant,
/// whose announcement follows the same setting. A watcher supports HEVC only if it announces it
/// with its participant attribute. Older clients and browser viewers don't, so the caller passes
/// `false` when the attribute is missing.
#[derive(Debug)]
pub(crate) struct ScreenShareCodecChoice {
    /// This Mac may share HEVC: M3 or newer.
    chip_allows_hevc: bool,
    /// The user's "Use HEVC for screen sharing" setting.
    setting_allows_hevc: bool,
    /// Whether each watcher supports HEVC, keyed by participant identity.
    watchers: HashMap<String, bool>,
    /// Codec of the published screen track, `None` while none is published.
    published: Option<ScreenShareCodec>,
    sharing: bool,
}

impl ScreenShareCodecChoice {
    pub fn new(chip_allows_hevc: bool, setting_allows_hevc: bool) -> Self {
        Self {
            chip_allows_hevc,
            setting_allows_hevc,
            watchers: HashMap::new(),
            published: None,
            sharing: false,
        }
    }

    /// Codec of the published screen track, `None` before [`Self::connected`] and after
    /// [`Self::clear`].
    pub fn published(&self) -> Option<ScreenShareCodec> {
        self.published
    }

    /// The call connected: the codec to publish the muted screen track with.
    #[must_use]
    pub fn connected(&mut self) -> ScreenShareCodec {
        let codec = self.pick();
        self.published = Some(codec);
        codec
    }

    /// A share starts. Returns the codec to republish the track with before unmuting it, when
    /// the pick differs from the published codec. `None` too when no track is published.
    #[must_use = "the screen track must be republished with the returned codec"]
    pub fn share_started(&mut self) -> Option<ScreenShareCodec> {
        self.sharing = true;
        let codec = self.pick();
        if self.published? == codec {
            return None;
        }
        self.published = Some(codec);
        Some(codec)
    }

    pub fn share_ended(&mut self) {
        self.sharing = false;
    }

    /// A watcher joined, or announced a change in its HEVC support. Returns `Some(H264)` when
    /// this downgrades the share in progress.
    #[must_use = "the screen track must be republished with the returned codec"]
    pub fn set_watcher(&mut self, identity: &str, supports_hevc: bool) -> Option<ScreenShareCodec> {
        self.watchers.insert(identity.to_string(), supports_hevc);
        self.downgrade_if_needed()
    }

    /// A watcher left. Never switches codec: a share that fell back to H.264 stays there.
    pub fn remove_watcher(&mut self, identity: &str) {
        self.watchers.remove(identity);
    }

    /// The user changed the setting. Turning it off returns `Some(H264)` when this downgrades the
    /// share in progress; turning it back on takes effect at the next share.
    #[must_use = "the screen track must be republished with the returned codec"]
    pub fn set_setting(&mut self, allows_hevc: bool) -> Option<ScreenShareCodec> {
        self.setting_allows_hevc = allows_hevc;
        self.downgrade_if_needed()
    }

    /// The call ended: forgets the watchers, the published track and the share, but keeps the
    /// chip and the setting.
    pub fn clear(&mut self) {
        self.watchers.clear();
        self.published = None;
        self.sharing = false;
    }

    /// The codec the current inputs call for, whatever is published.
    fn pick(&self) -> ScreenShareCodec {
        let watchers_allow_hevc = self.watchers.values().all(|supports_hevc| *supports_hevc);
        if self.chip_allows_hevc && self.setting_allows_hevc && watchers_allow_hevc {
            ScreenShareCodec::H265
        } else {
            ScreenShareCodec::H264
        }
    }

    /// Switches a share in progress from HEVC to H.264 when the pick calls for it, never back.
    fn downgrade_if_needed(&mut self) -> Option<ScreenShareCodec> {
        if !self.sharing
            || self.published != Some(ScreenShareCodec::H265)
            || self.pick() != ScreenShareCodec::H264
        {
            return None;
        }
        self.published = Some(ScreenShareCodec::H264);
        Some(ScreenShareCodec::H264)
    }
}

/// The first Apple Silicon generation allowed to share HEVC. Upstream's HEVC sharing shared
/// nothing on M1 (cause unknown) and the team uses M3 to M5, so older chips keep H.264.
const FIRST_HEVC_GENERATION: u32 = 3;

/// Whether this Mac may share in HEVC: Apple M3 or newer. A chip it doesn't recognize counts as
/// older. Reads `machdep.cpu.brand_string` once per process.
pub(crate) fn this_mac_may_share_hevc() -> bool {
    static MAY_SHARE_HEVC: OnceLock<bool> = OnceLock::new();
    *MAY_SHARE_HEVC.get_or_init(|| {
        let brand = cpu_brand_string();
        let allowed = brand.as_deref().is_some_and(brand_may_share_hevc);
        log::info!("screen_share_codec: CPU {brand:?}, may share HEVC: {allowed}");
        allowed
    })
}

#[cfg(target_os = "macos")]
fn cpu_brand_string() -> Option<String> {
    let output = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .inspect_err(|e| log::warn!("screen_share_codec: failed to run sysctl: {e}"))
        .ok()?;
    if !output.status.success() {
        log::warn!("screen_share_codec: sysctl exited with {}", output.status);
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|brand| brand.trim().to_string())
}

#[cfg(not(target_os = "macos"))]
fn cpu_brand_string() -> Option<String> {
    None
}

fn brand_may_share_hevc(brand: &str) -> bool {
    apple_silicon_generation(brand).is_some_and(|generation| generation >= FIRST_HEVC_GENERATION)
}

/// The Apple Silicon generation in a `machdep.cpu.brand_string`: 4 for "Apple M4" or
/// "Apple M4 Pro". `None` for anything else, Intel included.
fn apple_silicon_generation(brand: &str) -> Option<u32> {
    let rest = brand.trim().strip_prefix("Apple M")?;
    let digits_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (digits, variant) = rest.split_at(digits_end);
    if !variant.is_empty() && !variant.starts_with(' ') {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::ScreenShareCodec::{H264, H265};
    use super::*;

    const ALICE: &str = "room:1:alice:audio";
    const BOB: &str = "room:1:bob:audio";
    const BROWSER: &str = "room:1:carol";

    /// A call with one watcher, sharing in HEVC.
    fn sharing_hevc() -> ScreenShareCodecChoice {
        let mut choice = ScreenShareCodecChoice::new(true, true);
        assert_eq!(choice.set_watcher(ALICE, true), None);
        assert_eq!(choice.connected(), H265);
        assert_eq!(choice.share_started(), None);
        choice
    }

    #[test]
    fn hevc_when_chip_setting_and_every_watcher_allow_it() {
        let mut choice = ScreenShareCodecChoice::new(true, true);
        assert_eq!(choice.set_watcher(ALICE, true), None);
        assert_eq!(choice.set_watcher(BOB, true), None);
        assert_eq!(choice.connected(), H265);
        assert_eq!(choice.share_started(), None);
        assert_eq!(choice.published(), Some(H265));
    }

    #[test]
    fn h264_when_any_watcher_lacks_hevc() {
        let mut choice = ScreenShareCodecChoice::new(true, true);
        assert_eq!(choice.set_watcher(ALICE, true), None);
        assert_eq!(choice.set_watcher(BROWSER, false), None);
        assert_eq!(choice.connected(), H264);
        assert_eq!(choice.share_started(), None);
        assert_eq!(choice.published(), Some(H264));
    }

    #[test]
    fn hevc_with_no_watchers() {
        let mut choice = ScreenShareCodecChoice::new(true, true);
        assert_eq!(choice.connected(), H265);
        assert_eq!(choice.share_started(), None);
    }

    #[test]
    fn older_mac_always_shares_h264() {
        let mut choice = ScreenShareCodecChoice::new(false, true);
        assert_eq!(choice.connected(), H264);
        assert_eq!(choice.set_watcher(ALICE, true), None);
        assert_eq!(choice.share_started(), None);
        assert_eq!(choice.set_watcher(BOB, false), None);
        choice.remove_watcher(BOB);
        assert_eq!(choice.set_setting(false), None);
        assert_eq!(choice.set_setting(true), None);
        choice.share_ended();
        assert_eq!(choice.share_started(), None);
        assert_eq!(choice.published(), Some(H264));
    }

    #[test]
    fn setting_off_shares_h264() {
        let mut choice = ScreenShareCodecChoice::new(true, false);
        assert_eq!(choice.set_watcher(ALICE, true), None);
        assert_eq!(choice.connected(), H264);
        assert_eq!(choice.share_started(), None);
    }

    #[test]
    fn watcher_without_hevc_joining_mid_share_downgrades() {
        let mut choice = sharing_hevc();
        assert_eq!(choice.set_watcher(BROWSER, false), Some(H264));
        assert_eq!(choice.published(), Some(H264));
        // Already H.264: a second one changes nothing.
        assert_eq!(choice.set_watcher(BOB, false), None);
    }

    #[test]
    fn share_stays_h264_after_that_watcher_leaves_until_the_next_share() {
        let mut choice = sharing_hevc();
        assert_eq!(choice.set_watcher(BROWSER, false), Some(H264));
        choice.remove_watcher(BROWSER);
        assert_eq!(choice.set_watcher(BOB, true), None);
        assert_eq!(choice.published(), Some(H264));
        choice.share_ended();
        assert_eq!(choice.share_started(), Some(H265));
        assert_eq!(choice.published(), Some(H265));
    }

    #[test]
    fn watcher_turning_hevc_off_mid_share_downgrades() {
        let mut choice = sharing_hevc();
        assert_eq!(choice.set_watcher(ALICE, false), Some(H264));
        // Turning it back on waits for the next share.
        assert_eq!(choice.set_watcher(ALICE, true), None);
        assert_eq!(choice.published(), Some(H264));
        choice.share_ended();
        assert_eq!(choice.share_started(), Some(H265));
    }

    #[test]
    fn own_setting_off_mid_share_downgrades_at_once() {
        let mut choice = sharing_hevc();
        assert_eq!(choice.set_setting(false), Some(H264));
        assert_eq!(choice.set_setting(false), None);
        // Back on mid-share: no change until the next share.
        assert_eq!(choice.set_setting(true), None);
        assert_eq!(choice.published(), Some(H264));
        choice.share_ended();
        assert_eq!(choice.share_started(), Some(H265));
    }

    #[test]
    fn own_setting_on_mid_h264_share_waits_for_the_next_share() {
        let mut choice = ScreenShareCodecChoice::new(true, false);
        assert_eq!(choice.set_watcher(ALICE, true), None);
        assert_eq!(choice.connected(), H264);
        assert_eq!(choice.share_started(), None);
        assert_eq!(choice.set_setting(true), None);
        assert_eq!(choice.published(), Some(H264));
        choice.share_ended();
        assert_eq!(choice.share_started(), Some(H265));
    }

    #[test]
    fn changes_between_shares_apply_at_share_start() {
        let mut choice = ScreenShareCodecChoice::new(true, true);
        assert_eq!(choice.connected(), H265);
        // The muted track isn't republished while nobody shares.
        assert_eq!(choice.set_watcher(BROWSER, false), None);
        assert_eq!(choice.set_setting(false), None);
        assert_eq!(choice.published(), Some(H265));
        assert_eq!(choice.share_started(), Some(H264));
        choice.share_ended();
        choice.remove_watcher(BROWSER);
        assert_eq!(choice.set_setting(true), None);
        assert_eq!(choice.published(), Some(H264));
        assert_eq!(choice.share_started(), Some(H265));
        choice.share_ended();
        // Same pick as published: nothing to republish.
        assert_eq!(choice.share_started(), None);
    }

    #[test]
    fn nothing_to_republish_without_a_published_track() {
        let mut choice = ScreenShareCodecChoice::new(true, true);
        assert_eq!(choice.share_started(), None);
        assert_eq!(choice.set_watcher(BROWSER, false), None);
        assert_eq!(choice.set_setting(false), None);
        assert_eq!(choice.published(), None);
    }

    #[test]
    fn watchers_are_keyed_by_identity() {
        let mut choice = ScreenShareCodecChoice::new(true, true);
        assert_eq!(choice.set_watcher(ALICE, false), None);
        assert_eq!(choice.set_watcher(ALICE, true), None);
        assert_eq!(choice.connected(), H265);
        assert_eq!(choice.set_watcher(BOB, false), None);
        choice.remove_watcher(BOB);
        choice.remove_watcher("room:1:unknown:audio");
        assert_eq!(choice.share_started(), None);
        assert_eq!(choice.published(), Some(H265));
    }

    #[test]
    fn clear_forgets_the_call_but_keeps_chip_and_setting() {
        let mut choice = sharing_hevc();
        assert_eq!(choice.set_watcher(BROWSER, false), Some(H264));
        assert_eq!(choice.set_setting(false), None);
        choice.clear();
        assert_eq!(choice.published(), None);
        // Setting still off.
        assert_eq!(choice.connected(), H264);
        choice.clear();
        assert_eq!(choice.set_setting(true), None);
        // The watcher without HEVC is forgotten.
        assert_eq!(choice.connected(), H265);
        // The share is forgotten too: no downgrade until a share starts.
        assert_eq!(choice.set_watcher(BROWSER, false), None);
    }

    #[test]
    fn apple_silicon_generations() {
        assert_eq!(apple_silicon_generation("Apple M1"), Some(1));
        assert_eq!(apple_silicon_generation("Apple M2 Ultra"), Some(2));
        assert_eq!(apple_silicon_generation("Apple M3"), Some(3));
        assert_eq!(apple_silicon_generation("Apple M3 Pro"), Some(3));
        assert_eq!(apple_silicon_generation("Apple M3 Max"), Some(3));
        assert_eq!(apple_silicon_generation("Apple M4\n"), Some(4));
        assert_eq!(apple_silicon_generation("Apple M5"), Some(5));
        assert_eq!(apple_silicon_generation("Apple M12 Max"), Some(12));
        // GitHub's macOS runners are virtual machines.
        assert_eq!(apple_silicon_generation("Apple M1 (Virtual)"), Some(1));
    }

    #[test]
    fn unrecognized_brands_have_no_generation() {
        for brand in [
            "",
            "\n",
            "Intel(R) Core(TM) i9-9980HK CPU @ 2.40GHz",
            "VirtualApple @ 2.50GHz processor",
            "Apple",
            "Apple M",
            "Apple M Pro",
            "Apple Mx",
            "Apple M3X",
            "Apple M-3",
            "Apple A18 Pro",
            "apple m4",
            "Apple M99999999999",
        ] {
            assert_eq!(apple_silicon_generation(brand), None, "{brand:?}");
        }
    }

    #[test]
    fn m3_or_newer_may_share_hevc() {
        for brand in [
            "Apple M3",
            "Apple M3 Pro",
            "Apple M3 Max",
            "Apple M4",
            "Apple M5",
        ] {
            assert!(brand_may_share_hevc(brand), "{brand:?}");
        }
        for brand in ["Apple M1", "Apple M1 Max", "Apple M2 Pro", "", "garbage"] {
            assert!(!brand_may_share_hevc(brand), "{brand:?}");
        }
        assert!(!brand_may_share_hevc(
            "Intel(R) Core(TM) i7-1068NG7 CPU @ 2.30GHz"
        ));
    }

    #[test]
    fn this_mac_reads_its_chip_once() {
        let allowed = this_mac_may_share_hevc();
        #[cfg(target_os = "macos")]
        {
            let brand = cpu_brand_string().expect("sysctl reports the CPU brand string");
            assert_eq!(allowed, brand_may_share_hevc(&brand));
        }
        #[cfg(not(target_os = "macos"))]
        assert!(!allowed);
        assert_eq!(this_mac_may_share_hevc(), allowed);
    }
}

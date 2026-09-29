//! One-at-a-time effect playback with frames streamed from a worker thread.
//!
//! Only the compressed WebP bytes live in memory while idle (they are compiled
//! in). `try_play` spawns a worker that decodes frames lazily
//! (`AnimationDecoder::into_frames`) and hands them over a bounded channel, so it
//! runs at most one or two frames ahead of what is shown. The render thread calls
//! `tick` every redraw: it picks the frame for the elapsed time from the
//! cumulative per-frame delays, takes the newest decoded frame that is due, and
//! skips frames the decoder delivered late. It never blocks.
//!
//! When the effect ends, is stopped, or the player is dropped, the channel is
//! dropped; the worker's next send fails and it exits. Nothing is retained.

use std::collections::HashSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use parking_lot::Mutex;

use super::{manifest, EffectDef};
use crate::utils::clock::Clock;

/// One decoded frame, premultiplied RGBA (gamma space), `width * height * 4` bytes.
pub struct DecodedFrame {
    /// `loop_index * frame_count + frame_index`.
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for DecodedFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedFrame")
            .field("seq", &self.seq)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

enum WorkerMessage {
    Frame(DecodedFrame),
    Failed(String),
}

/// Result of a play request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayOutcome {
    Started,
    /// Another effect is playing; the request was dropped.
    Busy,
    /// This effect failed to decode before; it is never retried.
    Broken,
}

/// What `tick` found.
#[derive(Debug)]
pub enum Tick {
    /// Nothing playing.
    Idle,
    /// The effect just finished (or failed); release what was shown.
    Ended,
    Playing {
        effect: &'static EffectDef,
        /// A newly due frame to upload, if the shown frame changed.
        new_frame: Option<DecodedFrame>,
        /// True once any frame has been delivered (something can be drawn).
        has_frame: bool,
    },
}

/// Picks the frame sequence number for `elapsed_ms`, or `None` once all loops
/// have played. `ends` are the cumulative frame end times of one loop.
pub fn frame_at(ends: &[u64], loops: u32, elapsed_ms: u64) -> Option<u64> {
    let loop_ms = *ends.last()?;
    if loop_ms == 0 || loops == 0 {
        return None;
    }
    if elapsed_ms >= loop_ms.saturating_mul(loops as u64) {
        return None;
    }
    let loop_index = elapsed_ms / loop_ms;
    let within = elapsed_ms % loop_ms;
    let frame = ends
        .partition_point(|&end| end <= within)
        .min(ends.len() - 1);
    Some(loop_index * ends.len() as u64 + frame as u64)
}

/// Cumulative end time of each frame.
pub fn frame_ends(delays_ms: &[u32]) -> Vec<u64> {
    delays_ms
        .iter()
        .scan(0u64, |total, &delay| {
            *total += delay as u64;
            Some(*total)
        })
        .collect()
}

fn broken_ids() -> &'static Mutex<HashSet<&'static str>> {
    static BROKEN: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    BROKEN.get_or_init(|| Mutex::new(HashSet::new()))
}

fn is_broken(id: &'static str) -> bool {
    broken_ids().lock().contains(id)
}

fn mark_broken(id: &'static str, reason: &str) {
    log::error!("effects: {id} failed to decode and is disabled: {reason}");
    broken_ids().lock().insert(id);
}

struct Playback {
    effect: &'static EffectDef,
    started: Instant,
    ends: Vec<u64>,
    receiver: Receiver<WorkerMessage>,
    /// Decoded but not yet due.
    pending: Option<DecodedFrame>,
    has_frame: bool,
    cancel: Arc<AtomicBool>,
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

pub struct EffectPlayer {
    clock: Arc<dyn Clock>,
    playback: Option<Playback>,
    live_workers: Arc<AtomicUsize>,
}

impl std::fmt::Debug for EffectPlayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectPlayer")
            .field("playing", &self.playback.as_ref().map(|p| p.effect.id))
            .finish()
    }
}

impl EffectPlayer {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            playback: None,
            live_workers: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn elapsed_ms(&self, playback: &Playback) -> u64 {
        self.clock
            .now()
            .saturating_duration_since(playback.started)
            .as_millis() as u64
    }

    /// True while an effect is on screen (not yet past its total play time).
    pub fn is_playing(&self) -> bool {
        self.playback
            .as_ref()
            .is_some_and(|playback| self.elapsed_ms(playback) < playback.effect.total_ms())
    }

    /// When the current effect ends.
    pub fn deadline(&self) -> Option<Instant> {
        self.playback.as_ref().map(|playback| {
            playback.started + std::time::Duration::from_millis(playback.effect.total_ms())
        })
    }

    /// Starts `effect` unless another one is playing (then it is dropped).
    pub fn try_play(&mut self, effect: &'static EffectDef) -> PlayOutcome {
        if self.is_playing() {
            return PlayOutcome::Busy;
        }
        self.playback = None;
        if is_broken(effect.id) {
            return PlayOutcome::Broken;
        }

        let (sender, receiver) = sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let live = Arc::clone(&self.live_workers);
        live.fetch_add(1, Ordering::AcqRel);
        let spawned = std::thread::Builder::new()
            .name("effect-decode".to_string())
            .spawn(move || decode_worker(effect, sender, worker_cancel, live));
        if let Err(e) = spawned {
            // The closure (and its live-count guard) was never run.
            self.live_workers.fetch_sub(1, Ordering::AcqRel);
            log::error!("effects: cannot spawn decoder for {}: {e}", effect.id);
            return PlayOutcome::Busy;
        }

        self.playback = Some(Playback {
            effect,
            started: self.clock.now(),
            ends: frame_ends(effect.delays_ms),
            receiver,
            pending: None,
            has_frame: false,
            cancel,
        });
        PlayOutcome::Started
    }

    /// Stops and frees the current effect (cancel, call end, window hidden).
    pub fn stop(&mut self) {
        self.playback = None;
    }

    /// Advances playback to now. See `Tick`.
    pub fn tick(&mut self) -> Tick {
        let Some(playback) = self.playback.as_ref() else {
            return Tick::Idle;
        };
        let elapsed = self.elapsed_ms(playback);
        let Some(target) = frame_at(&playback.ends, playback.effect.loops, elapsed) else {
            self.playback = None;
            return Tick::Ended;
        };

        let playback = self.playback.as_mut().unwrap();
        let mut newest: Option<DecodedFrame> = None;
        loop {
            if playback.pending.is_none() {
                match playback.receiver.try_recv() {
                    Ok(WorkerMessage::Frame(frame)) => playback.pending = Some(frame),
                    Ok(WorkerMessage::Failed(reason)) => {
                        mark_broken(playback.effect.id, &reason);
                        self.playback = None;
                        return Tick::Ended;
                    }
                    Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                }
            }
            match playback.pending.as_ref() {
                Some(frame) if frame.seq <= target => newest = playback.pending.take(),
                _ => break,
            }
        }
        if newest.is_some() {
            playback.has_frame = true;
        }
        Tick::Playing {
            effect: playback.effect,
            new_frame: newest,
            has_frame: playback.has_frame,
        }
    }

    /// Decoder threads still running (for tests and diagnostics).
    pub fn live_workers(&self) -> usize {
        self.live_workers.load(Ordering::Acquire)
    }
}

struct LiveGuard(Arc<AtomicUsize>);

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn decode_worker(
    effect: &'static EffectDef,
    sender: SyncSender<WorkerMessage>,
    cancel: Arc<AtomicBool>,
    live: Arc<AtomicUsize>,
) {
    let _guard = LiveGuard(live);
    let result = catch_unwind(AssertUnwindSafe(|| stream_frames(effect, &sender, &cancel)));
    let failure = match result {
        Ok(Ok(())) => None,
        Ok(Err(reason)) => Some(reason),
        Err(_) => Some("decoder panicked".to_string()),
    };
    if let Some(reason) = failure {
        if !cancel.load(Ordering::Acquire) {
            let _ = sender.send(WorkerMessage::Failed(reason));
        }
    }
}

fn stream_frames(
    effect: &'static EffectDef,
    sender: &SyncSender<WorkerMessage>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let frame_count = effect.delays_ms.len() as u64;
    for loop_index in 0..effect.loops as u64 {
        let (width, height, frames) = manifest::open_animation(effect.bytes)?;
        for (index, frame) in frames.enumerate() {
            if cancel.load(Ordering::Acquire) {
                return Ok(());
            }
            if index as u64 >= frame_count {
                break;
            }
            let frame = frame.map_err(|e| format!("frame {index}: {e}"))?;
            let mut rgba = frame.into_buffer().into_raw();
            if rgba.len() != (width as usize) * (height as usize) * 4 {
                return Err(format!("frame {index}: unexpected buffer size"));
            }
            manifest::premultiply_rgba(&mut rgba);
            let message = WorkerMessage::Frame(DecodedFrame {
                seq: loop_index * frame_count + index as u64,
                width,
                height,
                rgba,
            });
            if sender.send(message).is_err() {
                return Ok(()); // player gone
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::EFFECTS;
    use crate::utils::clock::TestClock;
    use std::time::Duration;

    fn player() -> (EffectPlayer, Arc<TestClock>) {
        let clock = Arc::new(TestClock::new());
        (EffectPlayer::new(clock.clone()), clock)
    }

    fn leak(def: EffectDef) -> &'static EffectDef {
        Box::leak(Box::new(def))
    }

    fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }

    #[test]
    fn frame_selection_follows_uneven_delays() {
        let ends = frame_ends(&[40, 200, 40]);
        assert_eq!(ends, vec![40, 240, 280]);
        assert_eq!(frame_at(&ends, 1, 0), Some(0));
        assert_eq!(frame_at(&ends, 1, 39), Some(0));
        assert_eq!(frame_at(&ends, 1, 40), Some(1));
        assert_eq!(frame_at(&ends, 1, 239), Some(1));
        assert_eq!(frame_at(&ends, 1, 240), Some(2));
        assert_eq!(frame_at(&ends, 1, 279), Some(2));
        assert_eq!(frame_at(&ends, 1, 280), None);
    }

    #[test]
    fn frame_selection_wraps_loops_and_expires() {
        let ends = frame_ends(&[40, 200, 40]);
        assert_eq!(frame_at(&ends, 2, 280), Some(3));
        assert_eq!(frame_at(&ends, 2, 320), Some(4));
        assert_eq!(frame_at(&ends, 2, 559), Some(5));
        assert_eq!(frame_at(&ends, 2, 560), None);
        assert_eq!(frame_at(&ends, 3, 840), None);
    }

    #[test]
    fn frame_selection_never_panics() {
        assert_eq!(frame_at(&[], 1, 0), None);
        assert_eq!(frame_at(&[0], 1, 0), None);
        assert_eq!(frame_at(&[10], 0, 0), None);
        let ends = frame_ends(&[20, 1000, 20, 500]);
        for elapsed in (0..10_000).step_by(7) {
            if let Some(seq) = frame_at(&ends, 3, elapsed) {
                assert!(seq < 12);
            }
        }
        assert_eq!(frame_at(&ends, u32::MAX, u64::MAX), None);
    }

    #[test]
    fn drops_triggers_while_busy_then_accepts_after_expiry() {
        let (mut player, clock) = player();
        let first = &EFFECTS[0];
        let other = EFFECTS.last().unwrap();
        assert_eq!(player.try_play(first), PlayOutcome::Started);
        assert!(player.is_playing());
        assert_eq!(player.try_play(other), PlayOutcome::Busy);
        assert_eq!(player.try_play(first), PlayOutcome::Busy);
        clock.advance(Duration::from_millis(first.total_ms() - 1));
        assert_eq!(player.try_play(other), PlayOutcome::Busy);
        clock.advance(Duration::from_millis(1));
        assert!(!player.is_playing());
        assert_eq!(player.try_play(other), PlayOutcome::Started);
        player.stop();
    }

    #[test]
    fn deadline_is_start_plus_total_play_time() {
        let (mut player, clock) = player();
        assert!(player.deadline().is_none());
        let effect = &EFFECTS[0];
        let start = clock.now();
        player.try_play(effect);
        assert_eq!(
            player.deadline(),
            Some(start + Duration::from_millis(effect.total_ms()))
        );
        player.stop();
        assert!(player.deadline().is_none());
    }

    #[test]
    fn streams_due_frames_and_ends_on_expiry() {
        let (mut player, clock) = player();
        let effect = EFFECTS.iter().max_by_key(|e| e.delays_ms.len()).unwrap();
        assert_eq!(player.try_play(effect), PlayOutcome::Started);

        // Frame 0 is due at t=0; wait for the worker to deliver it.
        let mut first = None;
        assert!(wait_until(|| match player.tick() {
            Tick::Playing {
                new_frame: Some(frame),
                ..
            } => {
                first = Some(frame);
                true
            }
            _ => false,
        }));
        let first = first.unwrap();
        assert_eq!(first.seq, 0);
        assert_eq!((first.width, first.height), (effect.width, effect.height));
        assert_eq!(
            first.rgba.len(),
            (effect.width * effect.height * 4) as usize
        );

        // Jump past several frames: the newest due frame is shown, late ones skipped.
        let ends = frame_ends(effect.delays_ms);
        let jump_to = ends[3];
        clock.advance(Duration::from_millis(jump_to));
        let target = frame_at(&ends, effect.loops, jump_to).unwrap();
        let mut shown = 0;
        assert!(wait_until(|| {
            if let Tick::Playing {
                new_frame: Some(frame),
                ..
            } = player.tick()
            {
                assert!(frame.seq <= target, "showed a frame before it was due");
                shown = frame.seq;
            }
            shown == target
        }));

        // Nothing newer is handed out until it is due.
        match player.tick() {
            Tick::Playing {
                new_frame,
                has_frame,
                ..
            } => {
                assert!(new_frame.is_none());
                assert!(has_frame);
            }
            other => panic!("unexpected {other:?}"),
        }

        clock.advance(Duration::from_millis(effect.total_ms()));
        assert!(matches!(player.tick(), Tick::Ended));
        assert!(matches!(player.tick(), Tick::Idle));
        assert!(wait_until(|| player.live_workers() == 0));
    }

    #[test]
    fn stop_frees_the_worker() {
        let (mut player, _clock) = player();
        assert_eq!(player.try_play(&EFFECTS[0]), PlayOutcome::Started);
        assert_eq!(player.live_workers(), 1);
        player.stop();
        assert!(!player.is_playing());
        assert!(matches!(player.tick(), Tick::Idle));
        assert!(wait_until(|| player.live_workers() == 0));
    }

    #[test]
    fn dropping_the_player_frees_the_worker() {
        let (mut player, _clock) = player();
        player.try_play(&EFFECTS[0]);
        let live = Arc::clone(&player.live_workers);
        drop(player);
        assert!(wait_until(|| live.load(Ordering::Acquire) == 0));
    }

    #[test]
    fn corrupt_bytes_end_playback_and_are_never_retried() {
        let mut corrupt = b"RIFF\x20\x00\x00\x00WEBPVP8X".to_vec();
        corrupt.extend_from_slice(&[0xAB; 64]);
        for (id, bytes) in [
            (
                "test_corrupt_riff",
                Box::leak(corrupt.into_boxed_slice()) as &[u8],
            ),
            ("test_not_webp", b"GIF89a not a webp at all" as &[u8]),
            ("test_empty", b"" as &[u8]),
        ] {
            let effect = leak(EffectDef {
                id,
                label: "Corrupt",
                loops: 1,
                height_fraction: 0.3,
                width: 64,
                height: 64,
                delays_ms: &[100, 100],
                bytes,
                thumbnail: &[],
            });
            let (mut player, _clock) = player();
            assert_eq!(player.try_play(effect), PlayOutcome::Started);
            assert!(wait_until(|| matches!(player.tick(), Tick::Ended)), "{id}");
            assert!(!player.is_playing());
            assert!(wait_until(|| player.live_workers() == 0));
            assert_eq!(player.try_play(effect), PlayOutcome::Broken);
            assert_eq!(player.live_workers(), 0);
        }
    }

    #[test]
    fn premultiply_is_exact_at_the_ends() {
        let mut pixels = [255, 128, 0, 255, 255, 128, 10, 0, 200, 100, 50, 128];
        manifest::premultiply_rgba(&mut pixels);
        assert_eq!(pixels, [255, 128, 0, 255, 0, 0, 0, 0, 100, 50, 25, 128]);
    }
}

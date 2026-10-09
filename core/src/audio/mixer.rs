use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::StreamConfig;
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::native::apm::AudioProcessingModule;
use livekit::webrtc::native::audio_mixer::{self, AudioMixer};
use livekit::webrtc::native::audio_resampler::AudioResampler;
use livekit::webrtc::RtcError;
use log::{error, info};
use parking_lot::Mutex;
use rtrb::{Consumer, Producer, RingBuffer};
use std::borrow::Cow;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::Instant;

pub type SharedProcessor = Arc<Mutex<AudioProcessor>>;

pub const MIXER_SAMPLE_RATE: u32 = 16000;
pub const MIXER_NUM_CHANNELS: u32 = 1;
const MIXER_FRAME_SAMPLES: usize = (MIXER_SAMPLE_RATE / 100 * MIXER_NUM_CHANNELS) as usize;

/// Hard cap on per-source buffered frames (10ms each).
///
/// LiveKit's internal receive task runs on the main runtime and bursts frames
/// to us when it's CPU-starved. We absorb bursts with headroom; if we blow
/// past this cap we crash back by dropping from the front.
const HARD_CAP_FRAMES: usize = 80; // 800ms
const TARGET_DELAY: usize = 20;
/// Only the reading end of a ring can drop its oldest audio, so the mixer enforces
/// `HARD_CAP_FRAMES` and each source's ring has room for a burst on top of it.
const SOURCE_QUEUE_FRAMES: usize = 2 * HARD_CAP_FRAMES;
/// Played audio waiting for the capture task, well past the 200 ms of microphone audio that
/// task keeps before it drops some.
const FAR_END_QUEUE_FRAMES: usize = 50; // 500ms

/// WebRTC's audio processing (echo cancellation and gain control) for the microphone.
///
/// Echo cancellation needs the far end, the audio we play. The output callback runs on
/// CoreAudio's real-time thread and must not wait for the capture task, which holds this
/// processor's lock every 10 ms, so it never touches the APM: it queues each mixed frame in a
/// lock-free ring, and `process_stream` feeds the queued frames to the APM right before each
/// microphone frame. WebRTC expects render and capture calls from separate threads, in bursts,
/// and a frame is queued before it plays, so it always reaches the APM before the microphone
/// frame that can hold its echo.
pub struct AudioProcessor {
    apm: AudioProcessingModule,
    far_end: Consumer<i16>,
    far_end_frame: [i16; MIXER_FRAME_SAMPLES],
}

impl AudioProcessor {
    fn new(far_end: Consumer<i16>) -> Self {
        let mut apm = AudioProcessingModule::new(true, true, false, false);
        let _ = apm.set_stream_delay_ms(50);
        Self {
            apm,
            far_end,
            far_end_frame: [0; MIXER_FRAME_SAMPLES],
        }
    }

    /// Feeds the APM the audio played since the last call, then processes `data`, a multiple of
    /// 10 ms of microphone audio, in place.
    pub fn process_stream(
        &mut self,
        data: &mut [i16],
        sample_rate: i32,
        num_channels: i32,
    ) -> Result<(), RtcError> {
        self.feed_far_end();
        self.apm.process_stream(data, sample_rate, num_channels)
    }

    fn feed_far_end(&mut self) {
        let queued = self.far_end.slots();
        if queued == self.far_end.buffer().capacity() {
            // Nothing drained it for longer than it holds (the microphone isn't published yet,
            // or the capture task stalled), so the output callback is dropping played frames
            // and what's queued is stale. Start again from the next frame and let echo
            // cancellation re-converge.
            discard_oldest(&mut self.far_end, queued);
            log::debug!(
                "AudioProcessor: far-end queue was full, dropped {}ms",
                queued / MIXER_FRAME_SAMPLES * 10
            );
            return;
        }
        while self
            .far_end
            .pop_entire_slice(&mut self.far_end_frame)
            .is_ok()
        {
            let _ = self.apm.process_reverse_stream(
                &mut self.far_end_frame,
                MIXER_SAMPLE_RATE as i32,
                MIXER_NUM_CHANNELS as i32,
            );
        }
    }
}

fn far_end_queue() -> (Producer<i16>, Consumer<i16>) {
    RingBuffer::new(FAR_END_QUEUE_FRAMES * MIXER_FRAME_SAMPLES)
}

/// Drops the `samples` oldest queued samples, or all of them if fewer are queued.
fn discard_oldest(queue: &mut Consumer<i16>, samples: usize) {
    if let Ok(chunk) = queue.read_chunk(samples.min(queue.slots())) {
        chunk.commit_all();
    }
}

struct MixerInner {
    _stream: cpal::Stream,
    mixer: Arc<Mutex<AudioMixer>>,
    processor: SharedProcessor,
    next_ssrc: i32,
}

#[derive(Clone)]
pub struct MixerHandle {
    inner: Arc<Mutex<MixerInner>>,
}

impl std::fmt::Debug for MixerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MixerHandle").finish()
    }
}

/// The receive task's end of a remote participant's audio queue.
pub struct AudioSource {
    queue: Producer<i16>,
    stale_gap_ms: Arc<AtomicU32>,
}

impl AudioSource {
    /// Queues a decoded frame for the mixer, or drops it whole if the queue is full. That only
    /// happens when the output stopped pulling for longer than the cap, and the mixer flushes
    /// everything queued when it resumes anyway.
    pub fn push_samples(&mut self, samples: &[i16]) {
        let _ = self.queue.push_entire_slice(samples);
        // The mixer flushes on the real-time thread, where it can't log.
        let gap_ms = self.stale_gap_ms.swap(0, Ordering::Relaxed);
        if gap_ms > 0 {
            log::warn!("AudioSource: {gap_ms}ms gap, flushed stale frames");
        }
    }
}

/// The mixer's end of a remote participant's audio queue, read on the real-time thread.
struct SourceQueue {
    samples: Consumer<i16>,
    frame_samples: usize,
    last_mix: Option<Instant>,
    stale_gap_ms: Arc<AtomicU32>,
}

fn source_queue(frame_samples: usize) -> (AudioSource, SourceQueue) {
    let (producer, consumer) = RingBuffer::new(SOURCE_QUEUE_FRAMES * frame_samples);
    let stale_gap_ms = Arc::new(AtomicU32::new(0));
    let source = AudioSource {
        queue: producer,
        stale_gap_ms: stale_gap_ms.clone(),
    };
    let queue = SourceQueue {
        samples: consumer,
        frame_samples,
        last_mix: None,
        stale_gap_ms,
    };
    (source, queue)
}

impl SourceQueue {
    /// The next 10 ms frame, or `None` if the queue ran dry; the mixer plays that as silence.
    fn next_frame(&mut self, now: Instant) -> Option<Vec<i16>> {
        let frame_samples = self.frame_samples;
        if let Some(prev) = self.last_mix {
            let gap = now.duration_since(prev);
            if gap.as_millis() > 100 {
                discard_oldest(&mut self.samples, usize::MAX);
                self.stale_gap_ms
                    .store(gap.as_millis() as u32, Ordering::Relaxed);
            }
        }
        self.last_mix = Some(now);

        let queued = self.samples.slots() / frame_samples;
        if queued > HARD_CAP_FRAMES {
            discard_oldest(
                &mut self.samples,
                (queued - HARD_CAP_FRAMES) * frame_samples,
            );
        }
        if queued.min(HARD_CAP_FRAMES) > TARGET_DELAY {
            discard_oldest(&mut self.samples, frame_samples);
        }
        // A frame can wrap around the ring's end, so it comes out in two slices.
        let chunk = self.samples.read_chunk(frame_samples).ok()?;
        let (first, second) = chunk.as_slices();
        let frame = [first, second].concat();
        chunk.commit_all();
        Some(frame)
    }
}

/// A remote participant's source, at `MIXER_SAMPLE_RATE`, so the mix is at that rate too.
struct MixerSource {
    ssrc: i32,
    num_channels: u32,
    // Only `AudioMixer::mix` locks this, under the mixer lock, so it never waits. The
    // participant's `MixerSourceGuard` holds the other reference.
    queue: Arc<Mutex<SourceQueue>>,
}

impl audio_mixer::AudioMixerSource for MixerSource {
    fn ssrc(&self) -> i32 {
        self.ssrc
    }

    fn preferred_sample_rate(&self) -> u32 {
        MIXER_SAMPLE_RATE
    }

    fn get_audio_frame_with_info(&self, _target_sample_rate: u32) -> Option<AudioFrame<'_>> {
        // The fork's mixer asks through `&self` for a frame that borrows from `self`. Lending a
        // reused buffer that changes every call would need `unsafe`, so each frame is a small
        // allocation the mixer frees after copying it. A `&mut self` (or fill-this-buffer) API
        // in the fork would remove it; see `docs/unsafe.md`.
        let data = self.queue.lock().next_frame(Instant::now())?;
        Some(AudioFrame {
            data: Cow::Owned(data),
            sample_rate: MIXER_SAMPLE_RATE,
            num_channels: self.num_channels,
            samples_per_channel: MIXER_SAMPLE_RATE / 100,
        })
    }
}

/// Keeps a remote participant's source in the mixer; dropping it removes the source, so the
/// output callback stops mixing it.
pub struct MixerSourceGuard {
    mixer: Weak<Mutex<AudioMixer>>,
    ssrc: i32,
    // Removing the source only drops the mixer's reference to the queue under the mixer lock.
    // This one goes after the lock is released, so the queue and its ring (unless the receive
    // task still holds the ring's other end) are freed outside it.
    _queue: Arc<Mutex<SourceQueue>>,
}

impl Drop for MixerSourceGuard {
    fn drop(&mut self) {
        // `remove_source` and `mix` both take the mixer lock, and libwebrtc unlinks the source
        // under its own lock before destroying it, so the output callback never sees a removed
        // source; it waits at most for this brief removal, as for `add_source`. The mixer is
        // already gone if the call ended first.
        if let Some(mixer) = self.mixer.upgrade() {
            mixer.lock().remove_source(self.ssrc);
        }
    }
}

fn add_mixer_source(
    mixer: &Arc<Mutex<AudioMixer>>,
    ssrc: i32,
    channels: u16,
) -> (AudioSource, MixerSourceGuard) {
    // Allocate before taking the lock the output callback waits on.
    let (source, queue) = source_queue((MIXER_SAMPLE_RATE / 100) as usize * channels as usize);
    let queue = Arc::new(Mutex::new(queue));
    let mixer_source = MixerSource {
        ssrc,
        num_channels: channels as u32,
        queue: queue.clone(),
    };
    mixer.lock().add_source(mixer_source);
    let guard = MixerSourceGuard {
        mixer: Arc::downgrade(mixer),
        ssrc,
        _queue: queue,
    };
    (source, guard)
}

/// The last mixed frame in the device's format, until the device has taken all of it.
struct PendingOutput {
    samples: Vec<f32>,
    read: usize,
    /// 10 ms of the device's audio, the length of every refill.
    frame_samples: usize,
}

impl PendingOutput {
    fn new(frame_samples: usize) -> Self {
        Self {
            samples: Vec::with_capacity(frame_samples),
            read: 0,
            frame_samples,
        }
    }

    /// Copies as much pending audio as fits into `out` and returns how many samples it wrote.
    fn drain_into(&mut self, out: &mut [f32]) -> usize {
        let pending = &self.samples[self.read..];
        let count = pending.len().min(out.len());
        out[..count].copy_from_slice(&pending[..count]);
        self.read += count;
        count
    }

    /// Replaces the pending audio with the resampler's output, in place.
    fn refill(&mut self, sampled: &[i16], output_channels: u16) {
        self.samples.clear();
        self.read = 0;
        let normalize = |sample: i16| sample as f32 / i16::MAX as f32;
        if output_channels <= 2 {
            self.samples
                .extend(sampled.iter().map(|&sample| normalize(sample)));
        } else {
            for &sample in sampled {
                let normalized_sample = normalize(sample);
                self.samples.extend((0..output_channels).map(|channel| {
                    if channel < 2 {
                        normalized_sample
                    } else {
                        0.0
                    }
                }));
            }
        }
        debug_assert_eq!(
            self.samples.len(),
            self.frame_samples,
            "a mixed frame must be 10 ms of device audio"
        );
    }
}

/// Fills the output device's buffers from the mixer.
///
/// `render` runs on CoreAudio's real-time thread, where any wait is an audible glitch, so in
/// steady state it must not wait on a lock another thread holds, and it keeps heap use to the
/// one frame per participant the mixer needs (see `MixerSource`): it reuses its buffers, reads
/// participants' audio from lock-free rings, writes the far end for echo cancellation to
/// another, and only shares the mixer lock with adding and removing sources.
struct OutputRenderer {
    mixer: Arc<Mutex<AudioMixer>>,
    resampler: AudioResampler,
    far_end: Producer<i16>,
    pending: PendingOutput,
    output_sample_rate: u32,
    output_channels: u16,
}

impl OutputRenderer {
    fn new(
        mixer: Arc<Mutex<AudioMixer>>,
        far_end: Producer<i16>,
        output_sample_rate: u32,
        output_channels: u16,
    ) -> Self {
        let frame_samples = output_sample_rate as usize / 100 * output_channels as usize;
        Self {
            mixer,
            resampler: AudioResampler::default(),
            far_end,
            pending: PendingOutput::new(frame_samples),
            output_sample_rate,
            output_channels,
        }
    }

    // Buffer draining pattern adapted from Zed's audio playback implementation:
    // https://github.com/zed-industries/zed/blob/main/crates/audio/src/audio.rs
    fn render(&mut self, data: &mut [f32]) {
        let mut written = self.pending.drain_into(data);
        while written < data.len() {
            self.mix_next_frame();
            written += self.pending.drain_into(&mut data[written..]);
        }
    }

    /// Mixes a new 10ms frame from all sources (mono at MIXER_SAMPLE_RATE).
    fn mix_next_frame(&mut self) {
        let mut mixer = self.mixer.lock();
        let mixed = mixer.mix(MIXER_NUM_CHANNELS as usize);
        // Every source is at MIXER_SAMPLE_RATE, so the mix is one frame at that rate, except
        // while there are no sources: then libwebrtc mixes silence at 48 kHz, and this takes
        // one 16 kHz frame's worth. A full queue drops it; the capture side then starts over.
        if let Some(frame) = mixed.get(..MIXER_FRAME_SAMPLES) {
            let _ = self.far_end.push_entire_slice(frame);
        }
        // WebRTC only upmixes mono to stereo. Keep that path for
        // mono/stereo devices and handle wider outputs ourselves.
        let resampler_channels = if self.output_channels > 2 {
            MIXER_NUM_CHANNELS
        } else {
            self.output_channels as u32
        };
        let sampled = self.resampler.remix_and_resample(
            mixed,
            MIXER_SAMPLE_RATE / 100,
            MIXER_NUM_CHANNELS,
            MIXER_SAMPLE_RATE,
            resampler_channels,
            self.output_sample_rate,
        );
        self.pending.refill(sampled, self.output_channels);
    }
}

fn open_output_stream(
    mixer: Arc<Mutex<AudioMixer>>,
    far_end: Producer<i16>,
) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or("No default output device")?;
    let cfg = device
        .default_output_config()
        .map_err(|e| format!("Failed to get output config: {e}"))?;

    let output_sample_rate = cfg.sample_rate();
    let output_channels = cfg.channels();
    let config = StreamConfig {
        channels: output_channels,
        sample_rate: output_sample_rate,
        buffer_size: cpal::BufferSize::Default,
    };

    info!(
        "cpal output: {}Hz {}ch",
        output_sample_rate, output_channels
    );

    let mut renderer = OutputRenderer::new(mixer, far_end, output_sample_rate, output_channels);
    let stream = device
        .build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| renderer.render(data),
            |err| error!("cpal stream error: {err}"),
            None,
        )
        .map_err(|e| format!("Failed to build output stream: {e}"))?;

    stream
        .play()
        .map_err(|e| format!("Failed to start stream: {e}"))?;

    Ok(stream)
}

impl MixerHandle {
    pub fn new() -> Result<(Self, SharedProcessor), String> {
        let (far_end_tx, far_end_rx) = far_end_queue();
        let processor = Arc::new(Mutex::new(AudioProcessor::new(far_end_rx)));
        let mixer = Arc::new(Mutex::new(AudioMixer::new()));
        let stream = open_output_stream(mixer.clone(), far_end_tx)?;
        let handle = Self {
            inner: Arc::new(Mutex::new(MixerInner {
                _stream: stream,
                mixer,
                processor: processor.clone(),
                next_ssrc: 1,
            })),
        };
        Ok((handle, processor))
    }

    /// Adds a remote participant's source, which takes 10 ms frames at `MIXER_SAMPLE_RATE`. It
    /// stays in the mixer until the guard is dropped.
    pub fn add_source(&self, channels: u16) -> (AudioSource, MixerSourceGuard) {
        let mut inner = self.inner.lock();
        let ssrc = inner.next_ssrc;
        inner.next_ssrc += 1;
        add_mixer_source(&inner.mixer, ssrc, channels)
    }

    pub fn reconnect(&self) -> Result<(), String> {
        let mut inner = self.inner.lock();
        // Each stream writes its own far-end queue (a ring has one writer). The old stream
        // keeps writing to the one it had until it is dropped below.
        let (far_end_tx, far_end_rx) = far_end_queue();
        let stream = open_output_stream(inner.mixer.clone(), far_end_tx)?;
        inner.processor.lock().far_end = far_end_rx;
        inner._stream = stream;
        info!("Audio output reconnected");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A 10 ms frame at the mixer's rate, every sample `value`.
    fn frame(value: i16) -> [i16; MIXER_FRAME_SAMPLES] {
        [value; MIXER_FRAME_SAMPLES]
    }

    fn normalized(sample: i16) -> f32 {
        sample as f32 / i16::MAX as f32
    }

    #[test]
    fn plays_nothing_until_a_whole_frame_is_queued() {
        let (mut source, mut queue) = source_queue(4);
        let now = Instant::now();
        assert_eq!(queue.next_frame(now), None);
        source.push_samples(&[1, 2]);
        assert_eq!(queue.next_frame(now), None);
        source.push_samples(&[3, 4, 5, 6]);
        assert_eq!(queue.next_frame(now), Some(vec![1, 2, 3, 4]));
        assert_eq!(queue.next_frame(now), None);
    }

    #[test]
    fn drops_one_extra_frame_per_mix_above_the_target_delay() {
        let (mut source, mut queue) = source_queue(1);
        for value in 0..TARGET_DELAY as i16 + 2 {
            source.push_samples(&[value]);
        }
        let now = Instant::now();
        let mut played = Vec::new();
        while let Some(frame) = queue.next_frame(now) {
            played.push(frame[0]);
        }
        assert_eq!(played, (1..=TARGET_DELAY as i16 + 1).collect::<Vec<_>>());
    }

    #[test]
    fn drops_the_oldest_frames_past_the_hard_cap() {
        let (mut source, mut queue) = source_queue(1);
        for value in 0..HARD_CAP_FRAMES as i16 + 20 {
            source.push_samples(&[value]);
        }
        // 20 frames over the cap, then one more to catch up towards the target delay.
        assert_eq!(queue.next_frame(Instant::now()), Some(vec![21]));
        assert_eq!(queue.samples.slots(), HARD_CAP_FRAMES - 2);
    }

    #[test]
    fn a_full_queue_drops_incoming_frames() {
        let (mut source, mut queue) = source_queue(1);
        for value in 0..SOURCE_QUEUE_FRAMES as i16 + 10 {
            source.push_samples(&[value]);
        }
        assert_eq!(queue.samples.slots(), SOURCE_QUEUE_FRAMES);
        let newest_kept = SOURCE_QUEUE_FRAMES as i16 - 1;
        let first_played = newest_kept - HARD_CAP_FRAMES as i16 + 2;
        assert_eq!(queue.next_frame(Instant::now()), Some(vec![first_played]));
    }

    #[test]
    fn flushes_after_a_gap_and_logs_it_from_the_receive_side() {
        let (mut source, mut queue) = source_queue(1);
        source.push_samples(&[1]);
        source.push_samples(&[2]);
        let start = Instant::now();
        assert_eq!(queue.next_frame(start), Some(vec![1]));
        assert_eq!(queue.next_frame(start + Duration::from_millis(150)), None);
        assert_eq!(source.stale_gap_ms.load(Ordering::Relaxed), 150);

        source.push_samples(&[3]);
        assert_eq!(source.stale_gap_ms.load(Ordering::Relaxed), 0);
        assert_eq!(
            queue.next_frame(start + Duration::from_millis(160)),
            Some(vec![3])
        );
    }

    #[test]
    fn mixes_every_source_across_short_callbacks() {
        let mixer = Arc::new(Mutex::new(AudioMixer::new()));
        let (mut first, _first_guard) = add_mixer_source(&mixer, 1, 1);
        let (mut second, _second_guard) = add_mixer_source(&mixer, 2, 1);
        for _ in 0..3 {
            first.push_samples(&frame(1000));
            second.push_samples(&frame(2000));
        }
        let (far_end_tx, far_end_rx) = far_end_queue();
        let mut renderer = OutputRenderer::new(mixer, far_end_tx, MIXER_SAMPLE_RATE, 1);

        // Callbacks shorter than a frame, so frames straddle them; the fourth mix underruns.
        let mut out = vec![1.0; 4 * MIXER_FRAME_SAMPLES];
        for callback in out.chunks_mut(100) {
            renderer.render(callback);
        }
        let (played, underrun) = out.split_at(3 * MIXER_FRAME_SAMPLES);
        assert!(played.iter().all(|&sample| sample == normalized(3000)));
        assert!(underrun.iter().all(|&sample| sample == 0.0));
        assert_eq!(far_end_rx.slots(), 4 * MIXER_FRAME_SAMPLES);
    }

    #[test]
    fn wide_outputs_get_the_mix_in_the_first_two_channels() {
        let mixer = Arc::new(Mutex::new(AudioMixer::new()));
        let (mut source, _guard) = add_mixer_source(&mixer, 1, 1);
        source.push_samples(&frame(1000));
        let (far_end_tx, _far_end_rx) = far_end_queue();
        let mut renderer = OutputRenderer::new(mixer, far_end_tx, MIXER_SAMPLE_RATE, 4);

        let mut out = vec![1.0; 4 * MIXER_FRAME_SAMPLES];
        renderer.render(&mut out);
        for sample in out.chunks(4) {
            assert_eq!(sample, [normalized(1000), normalized(1000), 0.0, 0.0]);
        }
    }

    #[test]
    fn dropping_the_guard_removes_the_source() {
        let mixer = Arc::new(Mutex::new(AudioMixer::new()));
        let (mut kept, kept_guard) = add_mixer_source(&mixer, 1, 1);
        // Participants leaving and rejoining.
        for ssrc in 2..100 {
            let (mut source, guard) = add_mixer_source(&mixer, ssrc, 1);
            let queue = Arc::downgrade(&guard._queue);
            kept.push_samples(&frame(1000));
            source.push_samples(&frame(2000));
            drop(guard);
            // The mixer let go of the source and its queue was freed, so only the kept source
            // plays.
            assert!(queue.upgrade().is_none());
            let mut locked = mixer.lock();
            assert_eq!(locked.mix(1), frame(1000));
        }
        // With no sources left, libwebrtc mixes at 48 kHz again.
        drop(kept_guard);
        assert_eq!(mixer.lock().mix(1).len(), 480);

        // A guard that outlives the mixer has nothing to remove.
        let (_source, guard) = add_mixer_source(&mixer, 100, 1);
        drop(mixer);
        drop(guard);
    }

    #[test]
    fn every_mix_is_10ms_of_device_audio() {
        for sample_rate in [16000, 44100, 48000] {
            for channels in [1, 2, 4, 6] {
                let mixer = Arc::new(Mutex::new(AudioMixer::new()));
                let (far_end_tx, _far_end_rx) = far_end_queue();
                let mut renderer =
                    OutputRenderer::new(mixer.clone(), far_end_tx, sample_rate, channels);
                let frame_samples = sample_rate as usize / 100 * channels as usize;
                let check = |renderer: &OutputRenderer, when: &str| {
                    assert_eq!(
                        renderer.pending.samples.len(),
                        frame_samples,
                        "{sample_rate}Hz {channels}ch {when}"
                    );
                };

                // libwebrtc mixes at 48 kHz without sources and at 16 kHz with one.
                renderer.mix_next_frame();
                check(&renderer, "before any source");
                let (mut source, guard) = add_mixer_source(&mixer, 1, 1);
                source.push_samples(&frame(1000));
                renderer.mix_next_frame();
                check(&renderer, "with a source");
                drop(guard);
                renderer.mix_next_frame();
                check(&renderer, "after it left");
            }
        }
    }

    #[test]
    fn rendering_allocates_only_the_frames_lent_to_the_mixer() {
        for (sample_rate, channels) in [(48000, 2), (44100, 1), (48000, 6)] {
            let mixer = Arc::new(Mutex::new(AudioMixer::new()));
            let (far_end_tx, _far_end_rx) = far_end_queue();
            let mut renderer =
                OutputRenderer::new(mixer.clone(), far_end_tx, sample_rate, channels);
            let mut out = vec![0.0; 512 * channels as usize];
            // Before any source, libwebrtc mixes at a different rate.
            renderer.render(&mut out);

            let mut sources = [1, 2].map(|ssrc| add_mixer_source(&mixer, ssrc, 1));
            for (source, _guard) in &mut sources {
                for value in 0..HARD_CAP_FRAMES as i16 + 20 {
                    source.push_samples(&frame(value));
                }
            }
            // Covers the hard cap and catching up. Each source's frame is allocated for the
            // mixer and freed by it, and nothing else touches the heap. Only this thread's Rust
            // allocations are counted, not libwebrtc's C++ ones.
            const MIXES: usize = 20;
            let heap = allocation_counter::measure(|| {
                for _ in 0..MIXES {
                    renderer.mix_next_frame();
                }
            });
            assert_eq!(
                heap.count_total as usize,
                sources.len() * MIXES,
                "{sample_rate}Hz {channels}ch"
            );
            assert_eq!(heap.count_current, 0, "{sample_rate}Hz {channels}ch");

            // Play what's left, then cover underruns, a full far-end queue and, after the
            // sleep, flushing after a gap: no heap at all.
            for _ in 0..200 {
                renderer.render(&mut out);
            }
            let heap = allocation_counter::measure(|| {
                for _ in 0..200 {
                    renderer.render(&mut out);
                }
                std::thread::sleep(Duration::from_millis(110));
                renderer.render(&mut out);
            });
            assert_eq!(heap.count_total, 0, "{sample_rate}Hz {channels}ch");
        }
    }

    /// How much quieter echo cancellation makes a far end that leaks back into the microphone
    /// 30 ms later, measured over the last two of six seconds.
    fn echo_reduction_db(feed_far_end: bool) -> f64 {
        let (mut far_end_tx, far_end_rx) = far_end_queue();
        let mut processor = AudioProcessor::new(far_end_rx);
        let mut seed = 12345u32;
        let far_end: Vec<i16> = (0..6 * MIXER_SAMPLE_RATE)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 16) as i16 / 4
            })
            .collect();
        let echo_delay = 30 * MIXER_FRAME_SAMPLES / 10;
        let frames = far_end.len() / MIXER_FRAME_SAMPLES;
        let energy = |samples: &[i16]| samples.iter().map(|&s| (s as f64).powi(2)).sum::<f64>();
        let (mut echo, mut residual) = (0.0, 0.0);
        for played in 0..frames {
            if feed_far_end {
                let start = played * MIXER_FRAME_SAMPLES;
                far_end_tx
                    .push_entire_slice(&far_end[start..start + MIXER_FRAME_SAMPLES])
                    .unwrap();
            }
            // The capture task gets three microphone frames at a time.
            if played % 3 != 2 {
                continue;
            }
            for captured in played - 2..=played {
                let start = captured * MIXER_FRAME_SAMPLES;
                let mut microphone: Vec<i16> = (start..start + MIXER_FRAME_SAMPLES)
                    .map(|n| n.checked_sub(echo_delay).map_or(0, |n| far_end[n] / 3))
                    .collect();
                let before = energy(&microphone);
                processor
                    .process_stream(&mut microphone, MIXER_SAMPLE_RATE as i32, 1)
                    .unwrap();
                if captured >= frames - 200 {
                    echo += before;
                    residual += energy(&microphone);
                }
            }
        }
        10.0 * (echo / residual.max(1.0)).log10()
    }

    #[test]
    fn cancels_echo_with_the_far_end_fed_in_bursts() {
        // About 32 dB fed this way, the same as one far-end frame before each microphone frame.
        assert!(echo_reduction_db(true) > 20.0);
        assert!(echo_reduction_db(false) < 3.0);
    }
}

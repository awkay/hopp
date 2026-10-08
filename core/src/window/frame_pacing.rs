//! Frame-timing summary for a window that presents a remote video stream.
//!
//! WebRTC's inbound stats (`VideoHealthSummary`) count frames the decoder dropped, not frames the
//! window never showed: `process_video_stream` skips stale frames and a redraw can miss a frame.
//! This logs what the window actually presented, every [`LOG_EVERY`] and when the stream ends:
//!
//! ```text
//! FramePacing screen share 10.0s: 3024x1964 presented=398 (39.8 fps) interval_ms p50=25.0 ...
//! ```
//!
//! - `interval_ms`: time between presents that showed a new stream frame. A paused stream (a
//!   static shared screen sends no frames) shows up as a long `max`.
//! - `render_ms`: from the start of a redraw to `present()`, for every redraw.
//! - `age_ms`: from the frame reaching core (decoded, before conversion) to its present.
//! - `idle_redraws`: redraws that had no new frame to show.
//! - `never_presented`: frames written to the buffer but replaced before any redraw showed them.
//! - `stale_skipped`: decoded frames `process_video_stream` dropped because a newer one was queued.

use std::time::{Duration, Instant};

const LOG_EVERY: Duration = Duration::from_secs(10);

/// A stream frame shown by a present.
pub(crate) struct PresentedFrame {
    pub id: u64,
    pub received_at: Option<Instant>,
    pub width: u32,
    pub height: u32,
}

pub(crate) struct FramePacing {
    label: &'static str,
    window_start: Instant,
    last_presented_at: Option<Instant>,
    last_presented_id: u64,
    presented: u32,
    intervals_ms: Vec<f32>,
    render_ms: Vec<f32>,
    age_ms: Vec<f32>,
    idle_redraws: u32,
    never_presented: u64,
    stale_skipped_at_start: u64,
    resolution: (u32, u32),
}

impl FramePacing {
    pub(crate) fn new(label: &'static str) -> Self {
        Self {
            label,
            window_start: Instant::now(),
            last_presented_at: None,
            last_presented_id: 0,
            presented: 0,
            intervals_ms: Vec::new(),
            render_ms: Vec::new(),
            age_ms: Vec::new(),
            idle_redraws: 0,
            never_presented: 0,
            stale_skipped_at_start: 0,
            resolution: (0, 0),
        }
    }

    /// Records a redraw that started at `render_started` and has just presented. `frame` is the
    /// stream frame it showed, if that frame was new. `stale_skipped` is the stream's running
    /// total of skipped stale frames.
    pub(crate) fn record_present(
        &mut self,
        render_started: Instant,
        frame: Option<PresentedFrame>,
        stale_skipped: u64,
    ) {
        let now = Instant::now();
        self.render_ms.push(millis(now - render_started));
        match frame {
            Some(frame) => {
                if let Some(last) = self.last_presented_at {
                    self.intervals_ms.push(millis(now - last));
                }
                if let Some(received_at) = frame.received_at {
                    self.age_ms
                        .push(millis(now.saturating_duration_since(received_at)));
                }
                if self.last_presented_id > 0 && frame.id > self.last_presented_id {
                    self.never_presented += frame.id - self.last_presented_id - 1;
                }
                self.presented += 1;
                self.last_presented_at = Some(now);
                self.last_presented_id = frame.id;
                self.resolution = (frame.width, frame.height);
            }
            None => self.idle_redraws += 1,
        }
        if now - self.window_start >= LOG_EVERY {
            self.log(now, stale_skipped);
        }
    }

    /// Logs what is left of the current window, then forgets the stream (a new one restarts its
    /// frame ids). Call when the stream ends or the window switches streams.
    pub(crate) fn finish(&mut self, stale_skipped: u64) {
        if !self.render_ms.is_empty() {
            self.log(Instant::now(), stale_skipped);
        }
        *self = Self::new(self.label);
    }

    fn log(&mut self, now: Instant, stale_skipped: u64) {
        let seconds = (now - self.window_start).as_secs_f32();
        log::info!(
            "FramePacing {} {seconds:.1}s: {}x{} presented={} ({:.1} fps) interval_ms {} render_ms {} age_ms {} idle_redraws={} never_presented={} stale_skipped={}",
            self.label,
            self.resolution.0,
            self.resolution.1,
            self.presented,
            self.presented as f32 / seconds.max(f32::EPSILON),
            summary(&mut self.intervals_ms),
            summary(&mut self.render_ms),
            summary(&mut self.age_ms),
            self.idle_redraws,
            self.never_presented,
            stale_skipped.saturating_sub(self.stale_skipped_at_start),
        );
        self.window_start = now;
        self.intervals_ms.clear();
        self.render_ms.clear();
        self.age_ms.clear();
        self.presented = 0;
        self.idle_redraws = 0;
        self.never_presented = 0;
        self.stale_skipped_at_start = stale_skipped;
    }
}

fn millis(duration: Duration) -> f32 {
    duration.as_secs_f32() * 1000.0
}

/// `p50=.. p95=.. p99=.. max=..` of `values`, which it sorts.
fn summary(values: &mut [f32]) -> String {
    if values.is_empty() {
        return "n=0".to_string();
    }
    values.sort_by(f32::total_cmp);
    let percentile = |p: f32| {
        let rank = (p * values.len() as f32).ceil() as usize;
        values[rank.clamp(1, values.len()) - 1]
    };
    format!(
        "p50={:.1} p95={:.1} p99={:.1} max={:.1}",
        percentile(0.50),
        percentile(0.95),
        percentile(0.99),
        values[values.len() - 1]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_reports_nearest_rank_percentiles() {
        let mut values: Vec<f32> = (1..=100).map(|v| v as f32).collect();
        values.reverse();
        assert_eq!(summary(&mut values), "p50=50.0 p95=95.0 p99=99.0 max=100.0");
        assert_eq!(summary(&mut [7.0]), "p50=7.0 p95=7.0 p99=7.0 max=7.0");
        assert_eq!(summary(&mut []), "n=0");
    }

    #[test]
    fn counts_frames_replaced_before_any_present() {
        let mut pacing = FramePacing::new("test");
        let frame = |id| {
            Some(PresentedFrame {
                id,
                received_at: None,
                width: 4,
                height: 2,
            })
        };
        let started = Instant::now();
        pacing.record_present(started, frame(1), 0);
        pacing.record_present(started, None, 0);
        pacing.record_present(started, frame(4), 0);
        assert_eq!(pacing.presented, 2);
        assert_eq!(pacing.never_presented, 2);
        assert_eq!(pacing.idle_redraws, 1);
        assert_eq!(pacing.intervals_ms.len(), 1);
        assert_eq!(pacing.resolution, (4, 2));
    }
}

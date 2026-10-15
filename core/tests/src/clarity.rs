//! Text clarity of the screen share in one codec (spec 0007, B7), run by `core/tests/clarity.sh`.
//!
//! A fake sharer shares an editor page rendered with real text (`clarity/render.swift`) the way
//! core's capture delivers it: ScreenCaptureKit sends a frame only when the screen changes, so a
//! static page is one frame (resent every 250 ms until one is encoded, like core's
//! `check_screen_share_encoder`), typing is one frame per keystroke and a blinking caret one per
//! blink. A fake viewer decodes the share and scores each frame against the frame that was sent
//! (PSNR, matched by a frame id drawn into the status bar). Core doesn't run: the fake sharer
//! publishes through the same LiveKit SDK and WebRTC build with core's encoding settings, so it
//! gets the same encoder.

use crate::profile::Codec;
use crate::smoke::{codec_of, Participant};
use futures::StreamExt;
use livekit::options::{
    DegradationPreference, TrackPublishOptions, VideoEncoding, VideoEncodingUpdate,
};
use livekit::prelude::*;
use livekit::webrtc::prelude::VideoResolution as WebrtcVideoResolution;
use livekit::webrtc::stats::RtcStats;
use livekit::webrtc::video_frame::{I420Buffer, VideoBuffer, VideoFrame, VideoRotation};
use livekit::webrtc::video_source::native::NativeVideoSource;
use livekit::webrtc::video_source::RtcVideoSource;
use livekit::webrtc::video_stream::native::NativeVideoStream;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SHARER: &str = "Clarity Sharer";
const VIEWER: &str = "Clarity Viewer";

/// Core's screen share encoding (`bandwidth_mode::screen_encoding`), for both codecs.
const NORMAL_BITRATE: u64 = 12_000_000;
const NORMAL_FRAMERATE: f64 = 40.0;
const LOW_BANDWIDTH_BITRATE: u64 = 900_000;
const LOW_BANDWIDTH_FRAMERATE: f64 = 15.0;
const LOW_BANDWIDTH_MAX: (f64, f64) = (1920.0, 1080.0);

/// Core resends the capture's last frame this often until a new track encodes one.
const RESEND_INTERVAL: Duration = Duration::from_millis(250);
/// Eight characters a second.
const KEYSTROKE: Duration = Duration::from_millis(125);
/// macOS's caret blink.
const BLINK: Duration = Duration::from_millis(530);
const STATIC_PHASE: Duration = Duration::from_secs(8);
const BLINK_PHASE: Duration = Duration::from_secs(6);
const IDLE_PHASE: Duration = Duration::from_secs(6);

const CARET_WIDTH: usize = 4;
/// The light theme's text colour (#1f1f1f) in limited-range luma.
const CARET_LUMA: u8 = 43;
const ID_BITS: usize = 16;
const ID_BLOCK: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// The share starts on a page that doesn't change.
    Static,
    /// A line is typed, one frame per keystroke.
    Typing,
    /// Typing stopped; the caret blinks.
    Blinking,
    /// Nothing changes (the editor lost focus).
    Idle,
}

const PHASES: [Phase; 4] = [Phase::Static, Phase::Typing, Phase::Blinking, Phase::Idle];

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Static => "static",
            Phase::Typing => "typing",
            Phase::Blinking => "blinking",
            Phase::Idle => "idle",
        }
    }
}

/// What a sent frame shows.
#[derive(Clone, Copy)]
struct Shown {
    phase: Phase,
    typed: usize,
    caret: bool,
    sent_at: Instant,
}

/// The decoded frame's score against the frame that was sent.
struct Sample {
    phase: Phase,
    latency: Duration,
    light: f64,
    dark: f64,
    chroma: f64,
}

#[derive(Default)]
struct Viewed {
    samples: Vec<Sample>,
    /// The latest decoded frame at the page's size, and its id.
    latest: Option<(u16, Vec<u8>)>,
    size: (u32, u32),
}

pub async fn run(
    codec: Codec,
    low_bandwidth: bool,
    page_dir: &Path,
    out_dir: &Path,
    label: &str,
) -> io::Result<()> {
    let page = Arc::new(Page::load(page_dir)?);
    std::fs::create_dir_all(out_dir)?;
    let sent: Arc<Mutex<Vec<Shown>>> = Arc::default();
    let viewed: Arc<Mutex<Viewed>> = Arc::default();

    // No announcement: only the fake sharer and this viewer are in the room.
    let mut viewer = Participant::join_without_hevc(VIEWER).await?;
    let sharer = Participant::join_track(SHARER, "video").await?;
    let source = NativeVideoSource::new(
        WebrtcVideoResolution {
            width: page.width as u32,
            height: page.height as u32,
        },
        true,
    );
    let track =
        LocalVideoTrack::create_video_track("screen_share", RtcVideoSource::Native(source.clone()));
    sharer
        .room
        .local_participant()
        .publish_track(
            LocalTrack::Video(track.clone()),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                video_codec: codec.into(),
                video_encoding: Some(VideoEncoding {
                    max_bitrate: NORMAL_BITRATE,
                    max_framerate: NORMAL_FRAMERATE,
                }),
                simulcast: false,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| io::Error::other(format!("publishing the share failed: {e:?}")))?;
    let (bitrate, framerate, scale_down_by) = if low_bandwidth {
        let scale = (page.width as f64 / LOW_BANDWIDTH_MAX.0)
            .max(page.height as f64 / LOW_BANDWIDTH_MAX.1)
            .max(1.0);
        (LOW_BANDWIDTH_BITRATE, LOW_BANDWIDTH_FRAMERATE, scale)
    } else {
        (NORMAL_BITRATE, NORMAL_FRAMERATE, 1.0)
    };
    track
        .set_encoding_parameters(VideoEncodingUpdate {
            max_bitrate: Some(bitrate),
            max_framerate: Some(framerate),
            scale_resolution_down_by: Some(scale_down_by),
            degradation_preference: Some(DegradationPreference::MaintainResolution),
        })
        .map_err(|e| io::Error::other(format!("setting the encoding failed: {e:?}")))?;

    let remote = viewer
        .wait_event("the share", |event| match event {
            RoomEvent::TrackSubscribed {
                track: RemoteTrack::Video(track),
                publication,
                ..
            } if publication.source() == TrackSource::Screenshare => Some(track),
            _ => None,
        })
        .await?;
    let decoding = tokio::spawn(view(
        remote.clone(),
        page.clone(),
        sent.clone(),
        viewed.clone(),
    ));
    // Let the viewer get the keepalive frames first, so the page is never the stream's start.
    tokio::time::sleep(Duration::from_secs(1)).await;

    println!(
        "  - {label}: {codec:?}, {} at {} kbps, {} fps, scaled down by {scale_down_by:.2}",
        if low_bandwidth {
            "low-bandwidth"
        } else {
            "normal"
        },
        bitrate / 1000,
        framerate
    );
    let push = |phase: Phase, typed: usize, caret: bool| {
        let mut sent = sent.lock().unwrap();
        sent.push(Shown {
            phase,
            typed,
            caret,
            sent_at: Instant::now(),
        });
        let id = sent.len() as u16;
        source.capture_frame(&VideoFrame {
            rotation: VideoRotation::VideoRotation0,
            timestamp_us: 0,
            frame_metadata: None,
            buffer: page.buffer(&page.render(typed, caret, id)),
        });
    };
    let mut before = Counters::read(&track, &remote).await;
    let typed_all = page.char_x.len() - 1;
    for phase in PHASES {
        let started = Instant::now();
        match phase {
            Phase::Static => {
                push(phase, 0, true);
                let encoded = before.frames_encoded;
                while Counters::read(&track, &remote).await.frames_encoded == encoded {
                    tokio::time::sleep(RESEND_INTERVAL).await;
                    // Core resends the same captured frame: same content, same id.
                    let id = sent.lock().unwrap().len() as u16;
                    source.capture_frame(&VideoFrame {
                        rotation: VideoRotation::VideoRotation0,
                        timestamp_us: 0,
                        frame_metadata: None,
                        buffer: page.buffer(&page.render(0, true, id)),
                    });
                }
                sleep_until(started + STATIC_PHASE).await;
            }
            Phase::Typing => {
                for typed in 1..=typed_all {
                    push(phase, typed, true);
                    sleep_until(started + KEYSTROKE * typed as u32).await;
                }
                // Give the last keystroke the same time as the others.
            }
            Phase::Blinking => {
                let mut caret = true;
                let mut blinks = 1;
                while started.elapsed() + BLINK <= BLINK_PHASE {
                    sleep_until(started + BLINK * blinks).await;
                    caret = !caret;
                    push(phase, typed_all, caret);
                    blinks += 1;
                }
                sleep_until(started + BLINK_PHASE).await;
            }
            Phase::Idle => {
                push(phase, typed_all, true);
                sleep_until(started + IDLE_PHASE).await;
            }
        }
        let after = Counters::read(&track, &remote).await;
        let sent_count = sent
            .lock()
            .unwrap()
            .iter()
            .filter(|shown| shown.phase == phase)
            .count();
        println!(
            "  - {}",
            report(phase, sent_count, &before, &after, &viewed.lock().unwrap())
        );
        snapshot(
            &page,
            &sent,
            &viewed,
            out_dir,
            &format!("{label}-{}", phase.name()),
        )?;
        before = after;
    }
    let size = viewed.lock().unwrap().size;
    println!(
        "  - encoder: codec={} implementation={} size={}x{} target_bitrate={:.0} kbps; viewer decoded at {}x{} ({})",
        before.codec,
        before.encoder,
        before.width,
        before.height,
        before.target_bitrate / 1000.0,
        size.0,
        size.1,
        before.decoder,
    );

    decoding.abort();
    sharer.leave().await;
    viewer.leave().await;
    Ok(())
}

async fn sleep_until(deadline: Instant) {
    tokio::time::sleep_until(deadline.into()).await;
}

/// Decodes the share and scores every frame that carries a known id.
async fn view(
    track: RemoteVideoTrack,
    page: Arc<Page>,
    sent: Arc<Mutex<Vec<Shown>>>,
    viewed: Arc<Mutex<Viewed>>,
) {
    let mut stream = NativeVideoStream::new(track.rtc_track());
    while let Some(frame) = stream.next().await {
        let decoded_at = Instant::now();
        let buffer = frame.buffer.to_i420();
        let planes = page.planes_of(&buffer);
        let id = page.read_id(&planes);
        let shown = match id
            .checked_sub(1)
            .and_then(|i| sent.lock().unwrap().get(i as usize).copied())
        {
            Some(shown) => shown,
            None => continue,
        };
        let reference = page.render(shown.typed, shown.caret, id);
        let (light, dark, chroma) = page.psnr(&reference, &planes);
        let mut viewed = viewed.lock().unwrap();
        viewed.samples.push(Sample {
            phase: shown.phase,
            latency: decoded_at - shown.sent_at,
            light,
            dark,
            chroma,
        });
        viewed.size = (buffer.width(), buffer.height());
        viewed.latest = Some((id, planes));
    }
}

/// Writes the latest decoded frame and the frame that was sent as raw I420, for clarity.sh to
/// crop into PNGs.
fn snapshot(
    page: &Page,
    sent: &Mutex<Vec<Shown>>,
    viewed: &Mutex<Viewed>,
    out_dir: &Path,
    name: &str,
) -> io::Result<()> {
    let viewed = viewed.lock().unwrap();
    let Some((id, planes)) = &viewed.latest else {
        return Ok(());
    };
    let shown = sent.lock().unwrap()[*id as usize - 1];
    std::fs::write(out_dir.join(format!("{name}.yuv")), planes)?;
    std::fs::write(
        out_dir.join(format!("{name}-sent.yuv")),
        page.render(shown.typed, shown.caret, *id),
    )
}

/// The encoder's and decoder's running totals.
#[derive(Default)]
struct Counters {
    frames_encoded: u32,
    key_frames_encoded: u32,
    qp_sum_encoded: u64,
    bytes_sent: u64,
    target_bitrate: f64,
    width: u32,
    height: u32,
    frames_decoded: u32,
    codec: String,
    encoder: String,
    decoder: String,
}

impl Counters {
    async fn read(track: &LocalVideoTrack, remote: &RemoteVideoTrack) -> Self {
        let mut counters = Counters::default();
        if let Ok(stats) = track.get_stats().await {
            for stat in &stats {
                if let RtcStats::OutboundRtp(outbound) = stat {
                    if outbound.stream.kind == "video" {
                        counters.frames_encoded = outbound.outbound.frames_encoded;
                        counters.key_frames_encoded = outbound.outbound.key_frames_encoded;
                        counters.qp_sum_encoded = outbound.outbound.qp_sum;
                        counters.bytes_sent = outbound.sent.bytes_sent;
                        counters.target_bitrate = outbound.outbound.target_bitrate;
                        counters.width = outbound.outbound.frame_width;
                        counters.height = outbound.outbound.frame_height;
                        counters.encoder = outbound.outbound.encoder_implementation.clone();
                        counters.codec = codec_of(&stats, &outbound.stream.codec_id);
                    }
                }
            }
        }
        if let Ok(stats) = remote.get_stats().await {
            for stat in &stats {
                if let RtcStats::InboundRtp(inbound) = stat {
                    if inbound.stream.kind == "video" {
                        counters.frames_decoded = inbound.inbound.frames_decoded;
                        counters.decoder = inbound.inbound.decoder_implementation.clone();
                    }
                }
            }
        }
        counters
    }
}

fn report(
    phase: Phase,
    sent: usize,
    before: &Counters,
    after: &Counters,
    viewed: &Viewed,
) -> String {
    let encoded = after.frames_encoded - before.frames_encoded;
    let decoded = after.frames_decoded - before.frames_decoded;
    let average = |sum: u64, frames: u32| {
        if frames == 0 {
            "-".to_string()
        } else {
            format!("{:.1}", sum as f64 / frames as f64)
        }
    };
    let samples: Vec<&Sample> = viewed.samples.iter().filter(|s| s.phase == phase).collect();
    let summary = |pick: fn(&Sample) -> f64| {
        if samples.is_empty() {
            return "-".to_string();
        }
        let values: Vec<f64> = samples.iter().map(|s| pick(s)).collect();
        let min = values.iter().copied().fold(f64::INFINITY, f64::min);
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        format!(
            "last {:.2} min {min:.2} mean {mean:.2}",
            values[values.len() - 1]
        )
    };
    let mut latencies: Vec<f64> = samples
        .iter()
        .map(|s| s.latency.as_secs_f64() * 1000.0)
        .collect();
    latencies.sort_by(f64::total_cmp);
    let median = latencies
        .get(latencies.len() / 2)
        .map_or("-".to_string(), |ms| format!("{ms:.0}"));
    format!(
        "{}: sent {sent}, encoded {encoded} ({} key), decoded {decoded}, scored {}; {} kB; QP enc {}; PSNR-Y light {}; PSNR-Y dark {}; PSNR-UV {}; latency p50 {median} ms",
        phase.name(),
        after.key_frames_encoded - before.key_frames_encoded,
        samples.len(),
        (after.bytes_sent - before.bytes_sent) / 1000,
        average(after.qp_sum_encoded - before.qp_sum_encoded, encoded),
        summary(|s| s.light),
        summary(|s| s.dark),
        summary(|s| s.chroma),
    )
}

/// The rendered page as I420 planes back to back (from `clarity.sh`'s ffmpeg conversion), and
/// where the typed line is.
struct Page {
    width: usize,
    height: usize,
    base: Vec<u8>,
    full: Vec<u8>,
    line_top: usize,
    line_height: usize,
    /// Where the caret sits after 0, 1, ... characters.
    char_x: Vec<usize>,
    status_bar: usize,
}

impl Page {
    fn load(dir: &Path) -> io::Result<Self> {
        let layout: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("layout.json"))?)?;
        let number = |key: &str| {
            layout[key]
                .as_u64()
                .map(|n| n as usize)
                .ok_or_else(|| io::Error::other(format!("layout.json has no {key}")))
        };
        let char_x = layout["char_x"]
            .as_array()
            .and_then(|xs| {
                xs.iter()
                    .map(|x| x.as_u64().map(|x| x as usize))
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or_else(|| io::Error::other("layout.json has no char_x"))?;
        let (width, height) = (number("width")?, number("height")?);
        let read = |name: &str| -> io::Result<Vec<u8>> {
            let path: PathBuf = dir.join(name);
            let bytes = std::fs::read(&path)?;
            if bytes.len() != width * height * 3 / 2 {
                return Err(io::Error::other(format!(
                    "{} is not a {width}x{height} I420 frame",
                    path.display()
                )));
            }
            Ok(bytes)
        };
        Ok(Self {
            width,
            height,
            base: read("base.yuv")?,
            full: read("full.yuv")?,
            line_top: number("line_top")?,
            line_height: number("line_height")?,
            char_x,
            status_bar: number("status_bar")?,
        })
    }

    fn chroma_size(&self) -> (usize, usize) {
        (self.width / 2, self.height / 2)
    }

    /// The page with `typed` characters of the line, the caret, and frame id `id`.
    fn render(&self, typed: usize, caret: bool, id: u16) -> Vec<u8> {
        let (w, h) = (self.width, self.height);
        let (cw, ch) = self.chroma_size();
        let mut frame = self.base.clone();
        let (x0, x1) = (self.char_x[0], self.char_x[typed]);
        for row in self.line_top..self.line_top + self.line_height {
            let at = row * w;
            frame[at + x0..at + x1].copy_from_slice(&self.full[at + x0..at + x1]);
        }
        for plane in 0..2 {
            let offset = w * h + plane * cw * ch;
            for row in self.line_top / 2..(self.line_top + self.line_height) / 2 {
                let at = offset + row * cw;
                frame[at + x0 / 2..at + x1 / 2]
                    .copy_from_slice(&self.full[at + x0 / 2..at + x1 / 2]);
            }
        }
        if caret {
            for row in self.line_top + 4..self.line_top + self.line_height - 4 {
                frame[row * w + x1..][..CARET_WIDTH].fill(CARET_LUMA);
            }
        }
        for bit in 0..ID_BITS {
            let (x, y) = self.id_block(bit);
            let value = if (id >> bit) & 1 == 1 { 235 } else { 16 };
            for row in y..y + ID_BLOCK {
                frame[row * w + x..][..ID_BLOCK].fill(value);
            }
        }
        frame
    }

    /// Top-left corner of the frame id's `bit` block, in the status bar.
    fn id_block(&self, bit: usize) -> (usize, usize) {
        (
            self.width - 32 - (ID_BITS - bit) * ID_BLOCK,
            (self.status_bar - ID_BLOCK) / 2,
        )
    }

    fn read_id(&self, planes: &[u8]) -> u16 {
        let mut id = 0;
        for bit in 0..ID_BITS {
            let (x, y) = self.id_block(bit);
            let middle = ID_BLOCK / 2;
            let mut sum = 0_u32;
            for row in y + middle - 4..y + middle + 4 {
                for col in x + middle - 4..x + middle + 4 {
                    sum += u32::from(planes[row * self.width + col]);
                }
            }
            if sum / 64 > 125 {
                id |= 1 << bit;
            }
        }
        id
    }

    fn buffer(&self, frame: &[u8]) -> I420Buffer {
        let (w, h) = (self.width, self.height);
        let (cw, ch) = self.chroma_size();
        let mut buffer = I420Buffer::new(w as u32, h as u32);
        let (stride_y, stride_u, stride_v) = buffer.strides();
        let (y, u, v) = buffer.data_mut();
        let (luma, chroma) = frame.split_at(w * h);
        let (frame_u, frame_v) = chroma.split_at(cw * ch);
        copy_plane(y, stride_y as usize, luma, w, h);
        copy_plane(u, stride_u as usize, frame_u, cw, ch);
        copy_plane(v, stride_v as usize, frame_v, cw, ch);
        buffer
    }

    /// A decoded frame as planes back to back at the page's size, scaled up (bilinear) when the
    /// encoder scaled it down, as a viewer's window would.
    fn planes_of(&self, buffer: &I420Buffer) -> Vec<u8> {
        let (bw, bh) = (buffer.width() as usize, buffer.height() as usize);
        let (cw, ch) = self.chroma_size();
        let (stride_y, stride_u, stride_v) = buffer.strides();
        let (y, u, v) = buffer.data();
        let mut planes = Vec::with_capacity(self.width * self.height * 3 / 2);
        resample(
            &mut planes,
            y,
            stride_y as usize,
            (bw, bh),
            (self.width, self.height),
        );
        let chroma = (bw.div_ceil(2), bh.div_ceil(2));
        resample(&mut planes, u, stride_u as usize, chroma, (cw, ch));
        resample(&mut planes, v, stride_v as usize, chroma, (cw, ch));
        planes
    }

    /// Luma PSNR of the light and dark panes (below the status bar), and chroma PSNR.
    fn psnr(&self, sent: &[u8], decoded: &[u8]) -> (f64, f64, f64) {
        let w = self.width;
        let half = w / 2;
        let (mut light, mut dark) = ((0_u64, 0_u64), (0_u64, 0_u64));
        for row in self.status_bar..self.height {
            let at = row * w;
            light.0 += sse(&sent[at..at + half], &decoded[at..at + half]);
            light.1 += half as u64;
            dark.0 += sse(&sent[at + half..at + w], &decoded[at + half..at + w]);
            dark.1 += (w - half) as u64;
        }
        let luma = w * self.height;
        let chroma = (
            sse(&sent[luma..], &decoded[luma..]),
            (sent.len() - luma) as u64,
        );
        (to_psnr(light), to_psnr(dark), to_psnr(chroma))
    }
}

fn sse(a: &[u8], b: &[u8]) -> u64 {
    a.iter()
        .zip(b)
        .map(|(&x, &y)| {
            let d = i64::from(x) - i64::from(y);
            (d * d) as u64
        })
        .sum()
}

fn to_psnr((sse, count): (u64, u64)) -> f64 {
    if sse == 0 {
        return 99.0;
    }
    let mse = sse as f64 / count as f64;
    10.0 * (255.0 * 255.0 / mse).log10()
}

fn copy_plane(dst: &mut [u8], stride: usize, src: &[u8], width: usize, rows: usize) {
    for row in 0..rows {
        dst[row * stride..][..width].copy_from_slice(&src[row * width..][..width]);
    }
}

/// Appends plane `src` (`from` pixels, rows `stride` apart) to `out` at size `to`, bilinear.
fn resample(
    out: &mut Vec<u8>,
    src: &[u8],
    stride: usize,
    from: (usize, usize),
    to: (usize, usize),
) {
    if from == to {
        for row in 0..from.1 {
            out.extend_from_slice(&src[row * stride..][..from.0]);
        }
        return;
    }
    let position = |dst: usize, from: usize, to: usize| {
        let at = ((dst as f64 + 0.5) * from as f64 / to as f64 - 0.5).clamp(0.0, (from - 1) as f64);
        let first = at.floor() as usize;
        (first, (first + 1).min(from - 1), at - first as f64)
    };
    let columns: Vec<_> = (0..to.0).map(|x| position(x, from.0, to.0)).collect();
    for y in 0..to.1 {
        let (y0, y1, ty) = position(y, from.1, to.1);
        let (row0, row1) = (&src[y0 * stride..], &src[y1 * stride..]);
        for &(x0, x1, tx) in &columns {
            let top = f64::from(row0[x0]) * (1.0 - tx) + f64::from(row0[x1]) * tx;
            let bottom = f64::from(row1[x0]) * (1.0 - tx) + f64::from(row1[x1]) * tx;
            out.push((top * (1.0 - ty) + bottom * ty).round() as u8);
        }
    }
}

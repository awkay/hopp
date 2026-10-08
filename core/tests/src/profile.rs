//! Steady call load for profiling core (`core/tests/profile.sh`). Not a test: it sets up a call
//! with fake participants, keeps them busy until the stop file appears, then ends the call.
//! Creating the ready file tells profile.sh the load is running, so it can attach a profiler.
//!
//! The fake participants never send clicks, keystrokes or remote-control requests: in the sharer
//! run core would replay them on this Mac's real pointer and keyboard.

use crate::events::{
    ClientEvent, ClientPoint, DrawPathPoint, DrawPoint, DrawSettings, DrawingMode,
};
use crate::ipc::{CoreConn, Step};
use crate::screenshare_client;
use crate::smoke::{Participant, ROOM_EVENT_TIMEOUT};
use clap::ValueEnum;
use futures::StreamExt;
use livekit::options::{
    DegradationPreference, TrackPublishOptions, VideoCodec, VideoEncoding, VideoEncodingUpdate,
};
use livekit::prelude::*;
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_source::{AudioSourceOptions, RtcAudioSource};
use livekit::webrtc::prelude::VideoResolution as WebrtcVideoResolution;
use livekit::webrtc::video_frame::{I420Buffer, VideoFrame, VideoRotation};
use livekit::webrtc::video_source::native::NativeVideoSource;
use livekit::webrtc::video_source::RtcVideoSource;
use livekit::webrtc::video_stream::native::NativeVideoStream;
use socket_lib::Message;
use std::borrow::Cow;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

const CORE_USER: &str = "Profile Core";
const SHARER: &str = "Profile Sharer";
const CAMERA_USER: &str = "Profile Camera";
const VIEWER: &str = "Profile Viewer";

/// What a 14" MacBook Pro shares at the default "4K" setting, at core's maximum frame rate and
/// H.264 bitrate (`bandwidth_mode.rs`).
const SCREEN: (u32, u32) = (3024, 1964);
const SCREEN_FPS: f64 = 40.0;
const SCREEN_BITRATE: u64 = 12_000_000;
/// Core's own camera publish settings (`room_service.rs`).
const CAMERA: (u32, u32) = (1280, 720);
const CAMERA_FPS: f64 = 30.0;
const CAMERA_BITRATE: u64 = 1_700_000;
/// The size the app asks core to share at by default (`ScreenShareResolution::P4K`).
const SHARE_RESOLUTION: (f64, f64) = (4096.0, 2160.0);
/// Gives up if profile.sh never creates the stop file.
const MAX_RUN: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, ValueEnum, Debug)]
pub enum Role {
    /// Core watches: a fake participant shares a scrolling 3K screen at 40 fps, two fake
    /// participants send camera and audio
    Viewer,
    /// Core shares this Mac's screen; a fake viewer watches it, sends camera and audio, and moves
    /// its cursor and draws on the overlay
    Sharer,
}

/// Runs the `role` load against the core at `--socket-path`.
pub async fn run(role: Role, ready_file: &Path, stop_file: &Path) -> io::Result<()> {
    let conn = CoreConn::connect_with_livekit_url()?;
    match role {
        Role::Viewer => viewer(&conn, ready_file, stop_file).await,
        Role::Sharer => sharer(&conn, ready_file, stop_file).await,
    }
}

async fn viewer(conn: &CoreConn, ready_file: &Path, stop_file: &Path) -> io::Result<()> {
    let call_id = conn.join_call(CORE_USER)?;
    let mut tasks = Vec::new();

    step("fake sharer joins with camera, audio and a screen share");
    // Core resolves a sharer's name through its audio identity, so join with both, like a client.
    let sharer = Participant::join(SHARER).await?;
    tasks.push(publish_audio(&sharer).await?);
    tasks.push(publish_camera(&sharer, 3).await?);
    let sharer_video = Participant::join_track(SHARER, "video").await?;
    tasks.push(publish_screen(&sharer_video).await?);

    step("second fake participant joins with camera and audio");
    let camera_user = Participant::join(CAMERA_USER).await?;
    tasks.push(publish_audio(&camera_user).await?);
    tasks.push(publish_camera(&camera_user, 5).await?);

    conn.wait_for(
        ROOM_EVENT_TIMEOUT,
        "core seeing the remote share",
        |message| match message {
            Message::ParticipantsSnapshot(participants)
                if participants
                    .iter()
                    .any(|p| p.identity != "local" && p.is_screensharing) =>
            {
                Step::Done(())
            }
            _ => Step::Skip,
        },
    )?;

    run_until_stopped(ready_file, stop_file).await?;

    for task in tasks {
        task.abort();
    }
    sharer_video.leave().await;
    sharer.leave().await;
    camera_user.leave().await;
    conn.end_call(call_id)
}

async fn sharer(conn: &CoreConn, ready_file: &Path, stop_file: &Path) -> io::Result<()> {
    let display = screenshare_client::screen_id();
    if screenshare_client::display_asleep(display) {
        return Err(io::Error::other(format!(
            "display {display} is asleep: wake it (and keep it awake) for the sharer run"
        )));
    }
    let mut viewer = Participant::join(VIEWER).await?;
    let call_id = conn.join_call(CORE_USER)?;
    let mut tasks = vec![
        publish_audio(&viewer).await?,
        publish_camera(&viewer, 3).await?,
    ];

    step("core shares the screen");
    let (width, height) = SHARE_RESOLUTION;
    conn.start_screenshare(display, width, height)?;

    step("fake viewer watches the share and draws on the overlay");
    let track = viewer
        .wait_event("screen share track", |event| match event {
            RoomEvent::TrackSubscribed {
                track: RemoteTrack::Video(track),
                publication,
                ..
            } if publication.source() == TrackSource::Screenshare => Some(track),
            _ => None,
        })
        .await?;
    let frames = Arc::new(Mutex::new(FrameLog::default()));
    tasks.push(tokio::spawn(log_frames(track, frames.clone())));
    tasks.push(tokio::spawn(drive_overlay(viewer.room.local_participant())));

    let started = Instant::now();
    run_until_stopped(ready_file, stop_file).await?;
    step(&frames.lock().unwrap().report(started.elapsed()));

    for task in tasks {
        task.abort();
    }
    conn.send(Message::StopScreenshare)?;
    viewer.leave().await;
    conn.end_call(call_id)
}

/// Creates `ready_file`, then waits for `stop_file` (or [`MAX_RUN`]).
async fn run_until_stopped(ready_file: &Path, stop_file: &Path) -> io::Result<()> {
    std::fs::write(ready_file, b"")?;
    step("load running; waiting for the stop file");
    let started = Instant::now();
    while !stop_file.exists() {
        if started.elapsed() > MAX_RUN {
            step(&format!("no stop file after {MAX_RUN:?}, stopping"));
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok(())
}

async fn publish_screen(participant: &Participant) -> io::Result<JoinHandle<()>> {
    let picture = ScrollingPicture::new(SCREEN.0, SCREEN.1);
    let source = NativeVideoSource::new(
        WebrtcVideoResolution {
            width: SCREEN.0,
            height: SCREEN.1,
        },
        true,
    );
    let track =
        LocalVideoTrack::create_video_track("screen_share", RtcVideoSource::Native(source.clone()));
    participant
        .room
        .local_participant()
        .publish_track(
            LocalTrack::Video(track.clone()),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                video_codec: VideoCodec::H264,
                video_encoding: Some(VideoEncoding {
                    max_bitrate: SCREEN_BITRATE,
                    max_framerate: SCREEN_FPS,
                }),
                simulcast: false,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| io::Error::other(format!("publishing the fake screen share failed: {e:?}")))?;
    // Like core's own share (`apply_screen_share_encoding`): under CPU or bandwidth pressure, drop
    // frames rather than resolution. WebRTC's default scaled this share down to 1128x732.
    track
        .set_encoding_parameters(VideoEncodingUpdate {
            max_bitrate: Some(SCREEN_BITRATE),
            max_framerate: Some(SCREEN_FPS),
            scale_resolution_down_by: Some(1.0),
            degradation_preference: Some(DegradationPreference::MaintainResolution),
        })
        .map_err(|e| {
            io::Error::other(format!("setting the fake share's encoding failed: {e:?}"))
        })?;
    // Scrolling a document by a few lines per frame.
    Ok(tokio::spawn(push_video(source, picture, SCREEN_FPS, 6)))
}

async fn publish_camera(participant: &Participant, speed: u32) -> io::Result<JoinHandle<()>> {
    let picture = ScrollingPicture::new(CAMERA.0, CAMERA.1);
    let source = NativeVideoSource::new(
        WebrtcVideoResolution {
            width: CAMERA.0,
            height: CAMERA.1,
        },
        false,
    );
    let track =
        LocalVideoTrack::create_video_track("camera", RtcVideoSource::Native(source.clone()));
    participant
        .room
        .local_participant()
        .publish_track(
            LocalTrack::Video(track),
            TrackPublishOptions {
                source: TrackSource::Camera,
                video_codec: VideoCodec::H264,
                video_encoding: Some(VideoEncoding {
                    max_bitrate: CAMERA_BITRATE,
                    max_framerate: CAMERA_FPS,
                }),
                simulcast: true,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| io::Error::other(format!("publishing a fake camera failed: {e:?}")))?;
    Ok(tokio::spawn(push_video(source, picture, CAMERA_FPS, speed)))
}

async fn publish_audio(participant: &Participant) -> io::Result<JoinHandle<()>> {
    const SAMPLE_RATE: u32 = 48_000;
    // A full second of queue: capture_frame waits for room in it, which paces the loop.
    let source = NativeAudioSource::new(AudioSourceOptions::default(), SAMPLE_RATE, 1, 1000);
    let track =
        LocalAudioTrack::create_audio_track("microphone", RtcAudioSource::Native(source.clone()));
    participant
        .room
        .local_participant()
        .publish_track(
            LocalTrack::Audio(track),
            TrackPublishOptions {
                source: TrackSource::Microphone,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| io::Error::other(format!("publishing fake audio failed: {e:?}")))?;
    Ok(tokio::spawn(async move {
        // Noise at about -60 dBFS: enough that the remote-audio path never goes quiet (no
        // DTX), too quiet to hear on the speakers.
        const SAMPLES: u32 = SAMPLE_RATE / 100;
        let mut seed: u32 = 0x9e37_79b9;
        let mut samples = vec![0_i16; SAMPLES as usize];
        loop {
            for sample in &mut samples {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                *sample = (seed % 61) as i16 - 30;
            }
            let frame = AudioFrame {
                data: Cow::Borrowed(&samples),
                sample_rate: SAMPLE_RATE,
                num_channels: 1,
                samples_per_channel: SAMPLES,
            };
            if source.capture_frame(&frame).await.is_err() {
                break;
            }
        }
    }))
}

/// Feeds `source` at `fps`, scrolling `picture` by `rows_per_frame` each frame.
async fn push_video(
    source: NativeVideoSource,
    picture: ScrollingPicture,
    fps: f64,
    rows_per_frame: u32,
) {
    let mut interval = tokio::time::interval(Duration::from_secs_f64(1.0 / fps));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut offset = 0;
    loop {
        interval.tick().await;
        source.capture_frame(&VideoFrame {
            rotation: VideoRotation::VideoRotation0,
            timestamp_us: 0,
            frame_metadata: None,
            buffer: picture.frame(offset),
        });
        offset = (offset + rows_per_frame) % picture.height;
    }
}

/// What the fake viewer received: the decoded size and the time between frames.
#[derive(Default)]
struct FrameLog {
    frames: u32,
    size: (u32, u32),
    last_at: Option<Instant>,
    intervals_ms: Vec<f64>,
}

impl FrameLog {
    fn report(&mut self, elapsed: Duration) -> String {
        let fps = f64::from(self.frames) / elapsed.as_secs_f64();
        let intervals = &mut self.intervals_ms;
        if intervals.is_empty() {
            return format!("fake viewer received {} frames", self.frames);
        }
        intervals.sort_by(f64::total_cmp);
        let percentile = |p: f64| {
            let rank = (p * intervals.len() as f64).ceil() as usize;
            intervals[rank.clamp(1, intervals.len()) - 1]
        };
        format!(
            "fake viewer received {} frames at {}x{}, {fps:.1} fps; interval_ms p50={:.1} p95={:.1} p99={:.1} max={:.1}",
            self.frames,
            self.size.0,
            self.size.1,
            percentile(0.50),
            percentile(0.95),
            percentile(0.99),
            intervals[intervals.len() - 1],
        )
    }
}

/// Decodes the share, as a real viewer would, and logs each frame.
async fn log_frames(track: RemoteVideoTrack, frames: Arc<Mutex<FrameLog>>) {
    let mut stream = NativeVideoStream::new(track.rtc_track());
    while let Some(frame) = stream.next().await {
        let now = Instant::now();
        let mut log = frames.lock().unwrap();
        log.frames += 1;
        log.size = (frame.buffer.width(), frame.buffer.height());
        if let Some(last) = log.last_at.replace(now) {
            log.intervals_ms.push((now - last).as_secs_f64() * 1000.0);
        }
    }
}

/// Alternates 4 s of cursor movement and 4 s of drawing a stroke, both at 60 events per
/// second, the rate a real viewer's mouse produces. Never clicks.
async fn drive_overlay(participant: LocalParticipant) {
    const RATE: Duration = Duration::from_millis(16);
    const PHASE: Duration = Duration::from_secs(4);
    let mut interval = tokio::time::interval(RATE);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let started = Instant::now();
    let position = |t: f64| (0.5 + 0.3 * (t * 1.3).sin(), 0.5 + 0.3 * (t * 1.7).sin());
    let mut path_id = 0;
    loop {
        let phase_start = Instant::now();
        while phase_start.elapsed() < PHASE {
            interval.tick().await;
            let (x, y) = position(started.elapsed().as_secs_f64());
            let event = ClientEvent::MouseMove(ClientPoint {
                x,
                y,
                pointer: false,
            });
            if publish(&participant, &event, false).await.is_err() {
                return;
            }
        }

        let (x, y) = position(started.elapsed().as_secs_f64());
        let start = [
            ClientEvent::DrawingMode(DrawingMode::Draw(DrawSettings { permanent: false })),
            ClientEvent::DrawStart(DrawPathPoint {
                point: DrawPoint { x, y },
                path_id,
            }),
        ];
        for event in &start {
            if publish(&participant, event, true).await.is_err() {
                return;
            }
        }
        let phase_start = Instant::now();
        while phase_start.elapsed() < PHASE {
            interval.tick().await;
            let (x, y) = position(started.elapsed().as_secs_f64());
            if publish(
                &participant,
                &ClientEvent::DrawAddPoint(DrawPoint { x, y }),
                false,
            )
            .await
            .is_err()
            {
                return;
            }
        }
        let (x, y) = position(started.elapsed().as_secs_f64());
        let end = [
            ClientEvent::DrawEnd(DrawPoint { x, y }),
            ClientEvent::DrawingMode(DrawingMode::Disabled),
        ];
        for event in &end {
            if publish(&participant, event, true).await.is_err() {
                return;
            }
        }
        path_id += 1;
    }
}

async fn publish(
    participant: &LocalParticipant,
    event: &ClientEvent,
    reliable: bool,
) -> io::Result<()> {
    participant
        .publish_data(DataPacket {
            payload: serde_json::to_vec(event).map_err(io::Error::other)?,
            reliable,
            ..Default::default()
        })
        .await
        .map_err(io::Error::other)
}

/// A page of text-like lines that scrolls vertically. The texture is twice the frame height, so
/// each frame is a row copy (cheap for the load generator) and the encoder sees real motion.
struct ScrollingPicture {
    width: u32,
    height: u32,
    luma: Vec<u8>,
    chroma_u: Vec<u8>,
    chroma_v: Vec<u8>,
}

impl ScrollingPicture {
    fn new(width: u32, height: u32) -> Self {
        let (w, h) = (width as usize, height as usize);
        let hash = |a: usize, b: usize| {
            let mut x = (a as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ (b as u64);
            x ^= x >> 31;
            x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
            x ^ (x >> 29)
        };
        // Lines of 9 px wide "glyphs", 18 px apart, with a coloured heading every 400 rows.
        let heading = |row: usize| row % 400 < 30;
        let mut luma = vec![0_u8; w * 2 * h];
        for row in 0..2 * h {
            let line = row / 18;
            let line_length = 20 + (hash(line, 0) % (w as u64 / 9)) as usize;
            for col in 0..w {
                let glyph = col / 9;
                let ink = row % 18 < 12
                    && col % 9 < 7
                    && glyph < line_length
                    && hash(line, glyph + 1) % 4 != 0;
                luma[row * w + col] = match (heading(row), ink) {
                    (true, _) => 90,
                    (false, true) => 30,
                    (false, false) => 235,
                };
            }
        }
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let mut chroma_u = vec![128_u8; cw * 2 * ch];
        let mut chroma_v = vec![128_u8; cw * 2 * ch];
        for row in (0..2 * ch).filter(|row| heading(row * 2)) {
            chroma_u[row * cw..][..cw].fill(170);
            chroma_v[row * cw..][..cw].fill(90);
        }
        Self {
            width,
            height,
            luma,
            chroma_u,
            chroma_v,
        }
    }

    /// The frame starting `offset` rows down the page.
    fn frame(&self, offset: u32) -> I420Buffer {
        let (w, h) = (self.width as usize, self.height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let offset = offset as usize % h;
        let mut buffer = I420Buffer::new(self.width, self.height);
        let (stride_y, stride_u, stride_v) = buffer.strides();
        let (y, u, v) = buffer.data_mut();
        copy_rows(y, stride_y as usize, &self.luma, w, offset, h);
        copy_rows(u, stride_u as usize, &self.chroma_u, cw, offset / 2, ch);
        copy_rows(v, stride_v as usize, &self.chroma_v, cw, offset / 2, ch);
        buffer
    }
}

fn copy_rows(dst: &mut [u8], stride: usize, src: &[u8], width: usize, first: usize, rows: usize) {
    for row in 0..rows {
        dst[row * stride..][..width].copy_from_slice(&src[(first + row) * width..][..width]);
    }
}

fn step(description: &str) {
    println!("  - {description}");
}

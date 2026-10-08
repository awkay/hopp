use crate::ipc::CoreConn;
use crate::livekit_utils;
use livekit::options::{TrackPublishOptions, VideoCodec, VideoEncoding};
use livekit::prelude::*;
use livekit::webrtc::prelude::VideoResolution as WebrtcVideoResolution;
use livekit::webrtc::video_source::native::NativeVideoSource;
use livekit::webrtc::video_source::RtcVideoSource;
use std::env;
use std::io;
use std::time::{Duration, Instant};

const H264_BITRATE_DEFAULT: u64 = 12_000_000;
const MAX_FRAMERATE: f64 = 40.0;
const MUTE_UNMUTE_CYCLES: usize = 5;
/// How long core must keep answering after the mute/unmute cycles.
const LIVENESS_WINDOW: Duration = Duration::from_secs(30);
const PROBE_INTERVAL: Duration = Duration::from_secs(2);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn test_screenshare_reconnect_hang() -> io::Result<()> {
    let livekit_url = env::var("LIVEKIT_URL").expect("LIVEKIT_URL environment variable not set");

    // 1. Socket setup — connect to core, send LiveKit URL, start call as viewer
    let conn = CoreConn::connect_with_livekit_url()?;
    println!("Connected to core socket.");

    let call_id = conn.join_call("Viewer")?;
    println!("Viewer call started and core joined the room.");

    // 2. Fake audio participant — presence only for identity resolution
    let audio_token = livekit_utils::generate_participant_token("FakeSharer", "audio");
    let (audio_room, mut audio_rx) =
        Room::connect(&livekit_url, &audio_token, RoomOptions::default())
            .await
            .map_err(io::Error::other)?;
    println!(
        "Fake audio participant connected to room: {}",
        audio_room.name()
    );

    // 3. Fake video participant — publish screen_share track
    let video_token = livekit_utils::generate_participant_token("FakeSharer", "video");
    let (video_room, mut video_rx) =
        Room::connect(&livekit_url, &video_token, RoomOptions::default())
            .await
            .map_err(io::Error::other)?;
    println!(
        "Fake video participant connected to room: {}",
        video_room.name()
    );

    // Verify participants in the room
    let participants = video_room.remote_participants();
    println!(
        "Participants in room: {}",
        participants
            .values()
            .map(|p| p.identity().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let screen_source = NativeVideoSource::new(
        WebrtcVideoResolution {
            width: 1920,
            height: 1080,
        },
        true,
    );
    let screen_track = LocalVideoTrack::create_video_track(
        "screen_share",
        RtcVideoSource::Native(screen_source.clone()),
    );

    // Start muted (mirrors room_service.rs behavior)
    screen_track.mute();

    #[cfg(target_os = "macos")]
    let use_av1 = false;
    #[cfg(target_os = "windows")]
    let use_av1 = true;

    let max_bitrate = if use_av1 {
        5_000_000
    } else {
        H264_BITRATE_DEFAULT
    };
    let video_codec = if use_av1 {
        VideoCodec::AV1
    } else {
        VideoCodec::H264
    };

    // Must use the publication (not the track) for mute/unmute — only publication
    // signals the server, which propagates TrackMuted/TrackUnmuted to remote participants.
    let screen_publication = video_room
        .local_participant()
        .publish_track(
            LocalTrack::Video(screen_track.clone()),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                video_codec,
                video_encoding: Some(VideoEncoding {
                    max_bitrate,
                    max_framerate: MAX_FRAMERATE,
                }),
                simulcast: false,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| io::Error::other(format!("Failed to publish screen track: {e:?}")))?;
    println!("Screen share track published (muted).");

    // Give the viewer time to see the track subscription
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Drain events from both fake participants
    drain_events(&mut audio_rx).await;
    drain_events(&mut video_rx).await;

    // 4. Unmute — viewer sees TrackUnmuted → opens screenshare window + spawns redraw thread
    println!("Unmuting screen track to trigger viewer window open...");
    screen_publication.unmute();

    // Wait for viewer to open the screensharing window and spawn redraw thread
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Drain any room events from the fake sharer side
    drain_events(&mut video_rx).await;

    // 5. Mute/unmute loop — rapid cycles to trigger deadlock
    println!(
        "Starting mute/unmute loop ({} cycles, 300ms per cycle)...",
        MUTE_UNMUTE_CYCLES
    );

    for i in 1..=MUTE_UNMUTE_CYCLES {
        println!("Cycle {}/{}: muting...", i, MUTE_UNMUTE_CYCLES);
        screen_publication.mute();
        tokio::time::sleep(Duration::from_millis(100)).await;

        println!("Cycle {}/{}: unmuting...", i, MUTE_UNMUTE_CYCLES);
        screen_publication.unmute();
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Drain events periodically to keep the room event channel clear
        if i % 10 == 0 {
            drain_events(&mut video_rx).await;
        }
    }

    println!(
        "All {} mute/unmute cycles completed. Probing core for {:?}...",
        MUTE_UNMUTE_CYCLES, LIVENESS_WINDOW
    );
    let probing_since = Instant::now();
    while probing_since.elapsed() < LIVENESS_WINDOW {
        let round_trip = conn
            .probe(PROBE_TIMEOUT)
            .map_err(|e| io::Error::other(format!("core stopped answering (hang?): {e}")))?;
        println!("Core answered in {round_trip:?}.");
        tokio::time::sleep(PROBE_INTERVAL).await;
    }
    println!("No hang detected.");

    // 6. Cleanup
    println!("Cleaning up...");
    screen_publication.mute();
    drop(screen_source);
    drop(audio_room);
    drop(video_room);
    conn.end_call(call_id)?;
    println!("Cleanup complete.");

    Ok(())
}

async fn drain_events(rx: &mut tokio::sync::mpsc::UnboundedReceiver<RoomEvent>) {
    loop {
        match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => break,
        }
    }
}

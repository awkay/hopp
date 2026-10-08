use crate::ipc::CoreConn;
use crate::livekit_utils;
use crate::screenshare_client;
use livekit::prelude::*;
use socket_lib::Message;
use std::io;
use std::time::Duration;

/// Connects and joins a call as `name`.
fn setup_camera(name: &str) -> io::Result<(CoreConn, socket_lib::CallId)> {
    let conn = CoreConn::connect_with_livekit_url()?;
    let call_id = conn.join_call(name)?;
    Ok((conn, call_id))
}

pub fn test_list_cameras() -> io::Result<()> {
    let (conn, _) = setup_camera("Test Camera")?;

    let devices = conn.list_cameras()?;
    println!("Found {} cameras:", devices.len());
    for device in &devices {
        println!("  {}", device.name);
    }
    assert!(!devices.is_empty(), "Expected at least one camera");

    Ok(())
}

pub fn test_camera_30s(camera_name: Option<&str>) -> io::Result<()> {
    let (conn, _) = setup_camera("Test Camera")?;

    let device_name = if let Some(name) = camera_name {
        println!("Using explicitly provided camera: {}", name);
        name.to_string()
    } else {
        let devices = conn.list_cameras()?;
        let device = devices
            .first()
            .ok_or_else(|| io::Error::other("No cameras found"))?;

        println!("Using camera: {}", device.name);
        device.name.clone()
    };

    conn.start_camera(device_name)?
        .map_err(|e| io::Error::other(format!("Camera start failed: {e}")))?;
    println!("Camera started successfully");

    println!("Capturing for 30s...");
    std::thread::sleep(Duration::from_secs(30));

    println!("Stopping camera...");
    conn.send(Message::StopCamera)?;
    std::thread::sleep(Duration::from_secs(1));

    println!("Camera 30s test complete");
    Ok(())
}

pub async fn test_camera_track_subscribe() -> io::Result<()> {
    let token = livekit_utils::generate_token("Test Camera Track");
    let url = std::env::var("LIVEKIT_URL").expect("LIVEKIT_URL environment variable not set");

    let (room, mut rx) = Room::connect(&url, &token, RoomOptions::default())
        .await
        .map_err(io::Error::other)?;

    println!("Connected to room: {}", room.name());
    println!("Waiting for camera tracks to be subscribed...");

    // Listen for track events
    while let Some(event) = rx.recv().await {
        match event {
            RoomEvent::TrackSubscribed {
                track,
                participant,
                publication,
            } => {
                if track.kind() == TrackKind::Video {
                    println!(
                        "Camera track subscribed from participant '{}' (sid: {}): track name '{}', publication name '{}'",
                        participant.identity(),
                        participant.sid(),
                        track.name(),
                        publication.name()
                    );
                }
            }
            RoomEvent::TrackUnsubscribed {
                track, participant, ..
            } => {
                if track.kind() == TrackKind::Video {
                    println!(
                        "Camera track unsubscribed from participant '{}': track name '{}'",
                        participant.identity(),
                        track.name()
                    );
                }
            }
            RoomEvent::TrackUnpublished {
                publication,
                participant,
            } => {
                if publication.kind() == TrackKind::Video {
                    println!(
                        "Camera track unpublished from participant '{}': publication name '{}'",
                        participant.identity(),
                        publication.name()
                    );
                }
            }
            RoomEvent::ParticipantConnected(participant) => {
                println!(
                    "Participant connected: '{}' (sid: {})",
                    participant.identity(),
                    participant.sid()
                );
            }
            RoomEvent::ParticipantDisconnected(participant) => {
                println!(
                    "Participant disconnected: '{}' (sid: {})",
                    participant.identity(),
                    participant.sid()
                );
            }
            _ => {}
        }
    }

    Ok(())
}

/// Joins a call with camera and mic, stays until Ctrl-C.
pub fn test_call(
    camera_name: Option<&str>,
    mic_id: Option<&str>,
    name: &str,
    screenshare: bool,
) -> io::Result<()> {
    println!("\n=== TEST: Call with Camera + Mic ===");

    let (conn, call_id) = setup_camera(name)?;

    // Start camera — validate the name against available devices first
    let mut camera_started = false;

    let devices = conn.list_cameras()?;
    let device_name = if let Some(name) = camera_name {
        if devices.iter().any(|d| d.name == name) {
            println!("Using explicitly provided camera: {}", name);
            Some(name.to_string())
        } else {
            println!(
                "Camera '{}' not found. Available cameras: [{}]. Skipping camera.",
                name,
                devices
                    .iter()
                    .map(|d| d.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            None
        }
    } else {
        println!("Found {:?} cameras:", devices);
        match devices.first() {
            Some(device) => {
                println!("Using camera: {}", device.name);
                Some(device.name.clone())
            }
            None => {
                println!("No cameras found. Skipping camera.");
                None
            }
        }
    };

    if let Some(device_name) = device_name {
        match conn.start_camera(device_name)? {
            Ok(()) => {
                println!("Camera started successfully");
                camera_started = true;
            }
            Err(e) => println!("Camera start failed: {e}. Continuing without camera."),
        }
    }

    // Start mic
    let device_name = if let Some(name) = mic_id {
        println!("Using explicitly provided mic: {}", name);
        name.to_string()
    } else {
        let devices = conn.list_audio_devices()?;
        let device = devices
            .last()
            .ok_or_else(|| io::Error::other("No audio devices found"))?;

        println!("Using mic: {}", device.name);
        device.name.clone()
    };

    conn.start_audio_capture(device_name)?
        .map_err(|e| io::Error::other(format!("Mic start failed: {e}")))?;
    println!("Mic started successfully");

    // Start screen sharing if requested
    if screenshare {
        println!("Starting screen share...");
        let screen_id = screenshare_client::screen_id();
        println!("Using display: {screen_id}");

        // Use default resolution for screenshare
        screenshare_client::request_screenshare(&conn, screen_id, 1920.0, 1080.0)?;
        println!("Screen share started successfully");
    }

    println!("In call with camera and mic. Press Ctrl-C to stop.");

    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();
    ctrlc::set_handler(move || {
        let _ = shutdown_tx.send(());
    })
    .map_err(|e| io::Error::other(format!("Failed to set Ctrl-C handler: {e}")))?;

    shutdown_rx.recv().ok();
    println!("\nCtrl-C received, stopping...");

    if screenshare {
        conn.send(Message::StopScreenshare)?;
    }
    if camera_started {
        conn.send(Message::StopCamera)?;
    }
    conn.send(Message::StopAudioCapture)?;
    conn.end_call(call_id)?;

    println!("Call test complete");
    Ok(())
}

/// Opens the camera window and keeps it alive for manual interaction.
///
/// Usage: `cargo run -- camera open`
///
/// Requires a running core process (`task dev` in core/) and
/// LIVEKIT_URL, LIVEKIT_API_KEY, LIVEKIT_API_SECRET env vars.
pub fn test_open_camera() -> io::Result<()> {
    println!("\n=== TEST: Open Camera Window ===");

    let (conn, _) = setup_camera("Test Camera")?;
    println!("Connected to socket and joined room.");

    screenshare_client::open_camera(&conn)?;
    println!("OpenCamera sent. Camera window should appear.");
    println!("You have 60_000 seconds to interact with the window...");

    std::thread::sleep(Duration::from_secs(60_000));

    println!("Test completed.");
    Ok(())
}

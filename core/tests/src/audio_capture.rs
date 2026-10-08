use crate::ipc::CoreConn;
use crate::screenshare_client::SHARER;
use socket_lib::Message;
use std::io;
use std::time::Duration;

/// Connects and joins a call, the required setup before audio capture.
fn setup_audio() -> io::Result<CoreConn> {
    let conn = CoreConn::connect_with_livekit_url()?;
    conn.join_call(SHARER)?;
    Ok(conn)
}

pub fn test_list_devices() -> io::Result<()> {
    let conn = setup_audio()?;

    let devices = conn.list_audio_devices()?;
    println!("Found {} audio devices:", devices.len());
    for device in &devices {
        println!("  {}", device.name);
    }
    assert!(!devices.is_empty(), "Expected at least one audio device");

    Ok(())
}

pub fn test_capture_all_devices(duration_secs: u64) -> io::Result<()> {
    let conn = setup_audio()?;

    let devices = conn.list_audio_devices()?;
    println!(
        "Testing {} devices for {}s each",
        devices.len(),
        duration_secs
    );

    for device in &devices {
        println!("Capturing from: {}", device.name);

        match conn.start_audio_capture(device.name.clone())? {
            Ok(()) => println!("  Capture started successfully"),
            Err(e) => {
                println!("  Capture failed: {e}, skipping");
                continue;
            }
        }

        std::thread::sleep(Duration::from_secs(duration_secs));

        conn.send(Message::StopAudioCapture)?;
        // Give it a moment to clean up
        std::thread::sleep(Duration::from_secs(1));
        println!("  Capture stopped");
    }

    Ok(())
}

pub fn test_mute_unmute() -> io::Result<()> {
    let conn = setup_audio()?;

    let devices = conn.list_audio_devices()?;
    let device = devices
        .first()
        .ok_or_else(|| io::Error::other("No audio devices found"))?;

    println!("Using device: {}", device.name);

    conn.start_audio_capture(device.name.clone())?
        .map_err(|e| io::Error::other(format!("Capture failed: {e}")))?;
    println!("Capture started");

    println!("Capturing for 2s...");
    std::thread::sleep(Duration::from_secs(2));

    println!("Muting...");
    conn.send(Message::MuteAudio)?;
    std::thread::sleep(Duration::from_secs(2));

    println!("Unmuting...");
    conn.send(Message::UnmuteAudio)?;
    std::thread::sleep(Duration::from_secs(2));

    println!("Stopping capture...");
    conn.send(Message::StopAudioCapture)?;
    std::thread::sleep(Duration::from_secs(1));

    println!("Mute/unmute test complete");
    Ok(())
}

pub fn test_capture_30s(mic_name: Option<&str>) -> io::Result<()> {
    let conn = setup_audio()?;

    let devices = conn.list_audio_devices()?;
    println!("Found {} audio devices:", devices.len());
    for device in &devices {
        println!("  {}", device.name);
    }

    let device_name = if let Some(name) = mic_name {
        println!("Using explicitly provided device: {}", name);
        name.to_string()
    } else {
        let device = devices
            .first()
            .ok_or_else(|| io::Error::other("No audio devices found"))?;

        println!("Using device: {}", device.name);
        device.name.clone()
    };

    conn.start_audio_capture(device_name)?
        .map_err(|e| io::Error::other(format!("Capture failed: {e}")))?;
    println!("Capture started");

    println!("Capturing for 30s...");
    std::thread::sleep(Duration::from_secs(30));

    println!("Stopping capture...");
    conn.send(Message::StopAudioCapture)?;
    std::thread::sleep(Duration::from_secs(1));

    println!("30s capture test complete");
    Ok(())
}

use crate::ipc::CoreConn;
use socket_lib::{CallId, Message};
use std::env;
use std::io;
use std::ops::Deref;

/// User the sharing core joins the call as.
pub const SHARER: &str = "Test Screenshare";

/// Returns the screen content id to capture, read from the `HOPP_TEST_SCREEN_ID`
/// environment variable. Falls back to `0` if unset.
pub fn screen_id() -> u32 {
    env::var("HOPP_TEST_SCREEN_ID")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Shares display `content_id` and waits for core's result.
pub fn request_screenshare(
    conn: &CoreConn,
    content_id: u32,
    width: f64,
    height: f64,
) -> io::Result<()> {
    conn.start_screenshare(content_id, width, height)?;
    Ok(())
}

/// Sends a request to open the camera window.
pub fn open_camera(conn: &CoreConn) -> io::Result<()> {
    conn.send(Message::OpenCamera)
}

/// Sends a request to open the screensharing window.
pub fn open_screensharing(conn: &CoreConn) -> io::Result<()> {
    conn.send(Message::OpenScreensharing)
}

/// Sends a request to stop screen sharing.
pub fn stop_screenshare(conn: &CoreConn) -> io::Result<()> {
    conn.send(Message::StopScreenshare)
}

/// A call in which core shares the screen. Derefs to its connection; dropping it closes the
/// connection, which makes core exit.
pub struct ScreenshareSession {
    pub conn: CoreConn,
    pub call_id: CallId,
}

impl Deref for ScreenshareSession {
    type Target = CoreConn;

    fn deref(&self) -> &CoreConn {
        &self.conn
    }
}

pub fn screenshare_test() -> io::Result<()> {
    let session = start_screenshare_session()?;
    println!("Screen share started.");

    std::thread::sleep(std::time::Duration::from_secs(20)); // Wait for a moment

    stop_screenshare_session(&session)
}

/// Joins a call as [`SHARER`] and shares the `HOPP_TEST_SCREEN_ID` display.
pub fn start_screenshare_session() -> io::Result<ScreenshareSession> {
    let conn = CoreConn::connect_with_livekit_url()?;
    println!("Connected to socket.");

    let call_id = conn.join_call(SHARER)?;
    println!("Call started.");

    println!("Requesting screenshare start...");
    conn.start_screenshare(screen_id(), 1920.0, 1080.0)?;
    println!("Screenshare started.");
    Ok(ScreenshareSession { conn, call_id })
}

pub fn stop_screenshare_session(session: &ScreenshareSession) -> io::Result<()> {
    println!("Stopping screenshare...");
    stop_screenshare(session)?;
    println!("Screenshare stopped.");

    session.end_call(session.call_id)?;
    println!("Call ended.");
    Ok(())
}

pub fn test_every_monitor() -> io::Result<()> {
    let id = screen_id();
    println!("Testing screen ID {id}");

    let session = start_screenshare_session()?;
    println!("Screen share started for screen {id}.");

    std::thread::sleep(std::time::Duration::from_secs(10));

    stop_screenshare_session(&session)?;
    println!("✓ Success: screen tested.");
    Ok(())
}

/// Test call restart cycle: start call, wait 5s, end call, start another call
pub fn test_call_restart_cycle() -> io::Result<()> {
    println!("Testing call restart cycle...");
    let conn = CoreConn::connect_with_livekit_url()?;
    println!("Connected to socket.");

    for call in ["first", "second"] {
        println!("Starting {call} call...");
        let call_id = conn.join_call(SHARER)?;
        println!("{call} call started.");

        println!("Waiting 5 seconds...");
        std::thread::sleep(std::time::Duration::from_secs(5));

        println!("Ending {call} call...");
        conn.end_call(call_id)?;
        println!("{call} call ended.");
    }

    println!("✓ Success: Call restart cycle completed.");
    Ok(())
}

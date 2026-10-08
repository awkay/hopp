use crate::screenshare_client;
use socket_lib::{DrawingEnabled, Message};
use std::{io, time::Duration};

pub fn test_local_drawing_permanent() -> io::Result<()> {
    println!("\n=== TEST: Local Drawing (Permanent) ===");

    // Start screenshare session
    let session = screenshare_client::start_screenshare_session()?;

    // Enable permanent drawing mode
    session.send(Message::DrawingEnabled(DrawingEnabled { permanent: true }))?;
    println!("Permanent drawing enabled. Draw with mouse, press Escape to exit.");
    println!(
        "Type to write text at the cursor (it follows the mouse); Enter or click places it, \
         Backspace edits, Escape cancels pending text."
    );
    println!("You have 15 seconds to test drawing...");

    // Wait for user to test manually
    std::thread::sleep(Duration::from_secs(15));

    // Stop screenshare
    screenshare_client::stop_screenshare_session(&session)?;

    println!("Test completed.");
    Ok(())
}

pub fn test_local_drawing_non_permanent() -> io::Result<()> {
    println!("\n=== TEST: Local Drawing (Non-Permanent) ===");

    // Start screenshare session
    let session = screenshare_client::start_screenshare_session()?;

    // Enable non-permanent drawing mode
    session.send(Message::DrawingEnabled(DrawingEnabled { permanent: false }))?;
    println!("Non-permanent drawing enabled. Draw with mouse, press Escape to exit.");
    println!(
        "Type to write text at the cursor (it follows the mouse); Enter or click places it, \
         Backspace edits, Escape cancels pending text. Placed text fades like strokes."
    );
    println!("You have 15 seconds to test drawing...");

    // Wait for user to test manually
    std::thread::sleep(Duration::from_secs(15));

    // Stop screenshare
    screenshare_client::stop_screenshare_session(&session)?;

    println!("Test completed.");
    Ok(())
}

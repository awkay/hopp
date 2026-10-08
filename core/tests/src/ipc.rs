//! Connection to a running `hopp_core`, speaking the same protocol as Tauri's `CoreClient`:
//! requests carry a request id that core echoes on its response, calls carry a call id, and a
//! periodic `Ping` keeps core from exiting on its idle timeout.

use crate::livekit_utils;
use socket_lib::{
    AudioCaptureMessage, AudioDevice, CallId, CallStartMessage, CallStartResultMessage,
    CameraDevice, CameraStartMessage, Content, ContentType, CoreParticipantState, EventSocket,
    Extent, Frame, Message, RoomConnectionFailedMessage, ScreenShareMessage, SocketSender,
};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Same as Tauri's `REQUEST_TIMEOUT`.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Same as Tauri's `CALL_START_TIMEOUT`.
pub const CALL_START_TIMEOUT: Duration = Duration::from_secs(10);
/// Core gives up on a room after 90 s; a local LiveKit server connects in well under that.
pub const ROOM_READY_TIMEOUT: Duration = Duration::from_secs(30);
/// Ending a call destroys both rooms before core confirms it.
pub const CALL_END_TIMEOUT: Duration = Duration::from_secs(15);
/// Creating the capture stream and publishing the track.
pub const SCREENSHARE_START_TIMEOUT: Duration = Duration::from_secs(15);
/// Tauri pings every 15 s; core exits after 300 s (debug) or 30 s (release) without a message.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// What a wait does with each frame or message it receives.
pub enum Step<T> {
    /// Not what we are waiting for: drop it and keep waiting.
    Skip,
    Done(T),
    Fail(String),
}

pub struct CoreConn {
    sender: SocketSender,
    incoming: Mutex<Receiver<Frame>>,
    // Dropping the EventSocket shuts the socket down, and core exits when its client goes away.
    _event_socket: EventSocket,
    next_request_id: AtomicU64,
}

static LAST_CALL_ID: AtomicU64 = AtomicU64::new(0);

/// A new call id, unique in this process and never 0 (0 means "no call" to core). Same scheme
/// as the frontend's `newCallId`.
pub fn new_call_id() -> CallId {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(1);
    let mut last = LAST_CALL_ID.load(Ordering::Relaxed);
    loop {
        let next = (last + 1).max(now);
        match LAST_CALL_ID.compare_exchange(last, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(actual) => last = actual,
        }
    }
}

impl CoreConn {
    /// Connects to core at `--socket-path` and starts the keepalive.
    pub fn connect() -> io::Result<Self> {
        let socket_path = crate::SOCKET_PATH
            .get()
            .expect("SOCKET_PATH not initialized");
        println!("Connecting to socket: {socket_path}");
        let (sender, mut event_socket) = socket_lib::connect(socket_path)?;
        let incoming = event_socket.take_incoming();

        let keepalive = sender.clone();
        std::thread::spawn(move || {
            // Ends once the socket is shut down (the CoreConn was dropped or core exited).
            while keepalive.send(Message::Ping).is_ok() {
                std::thread::sleep(KEEPALIVE_INTERVAL);
            }
        });

        Ok(Self {
            sender,
            incoming: Mutex::new(incoming),
            _event_socket: event_socket,
            next_request_id: AtomicU64::new(1),
        })
    }

    /// Connects and sends the LiveKit server URL, which core needs before `CallStart`.
    pub fn connect_with_livekit_url() -> io::Result<Self> {
        let conn = Self::connect()?;
        let url = std::env::var("LIVEKIT_URL").expect("LIVEKIT_URL environment variable not set");
        conn.send(Message::LivekitServerUrl(url))?;
        Ok(conn)
    }

    /// Sends an event (no request id).
    pub fn send(&self, message: Message) -> io::Result<()> {
        self.sender.send(message)
    }

    /// Sends `message` as a request and returns core's response to it. Only the variants core
    /// answers by id get a response (`CallStart`, `ListAudioDevices`, `StartAudioCapture`,
    /// `ListCameras`, `StartCamera`, `BringWindowsToFront`); anything else times out.
    pub fn request(&self, message: Message, timeout: Duration) -> io::Result<Message> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let what = format!("response to request {request_id} ({message:?})");
        self.sender.send_with_id(Some(request_id), message)?;
        self.wait_frame(timeout, &what, |frame| {
            if frame.request_id == Some(request_id) {
                Step::Done(frame.message)
            } else {
                Step::Skip
            }
        })
    }

    /// Reads frames until `pick` finishes the wait. Frames it skips are dropped.
    pub fn wait_frame<T>(
        &self,
        timeout: Duration,
        what: &str,
        mut pick: impl FnMut(Frame) -> Step<T>,
    ) -> io::Result<T> {
        let incoming = self.incoming.lock().unwrap();
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match incoming.recv_timeout(remaining) {
                Ok(frame) => match pick(frame) {
                    Step::Skip => {}
                    Step::Done(value) => return Ok(value),
                    Step::Fail(reason) => {
                        return Err(io::Error::other(format!(
                            "while waiting for {what}: {reason}"
                        )))
                    }
                },
                Err(RecvTimeoutError::Timeout) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("timed out after {timeout:?} waiting for {what}"),
                    ))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        format!("core closed the socket (did it exit?) while waiting for {what}"),
                    ))
                }
            }
        }
    }

    /// Reads messages until `pick` finishes the wait. Messages it skips are dropped.
    pub fn wait_for<T>(
        &self,
        timeout: Duration,
        what: &str,
        mut pick: impl FnMut(Message) -> Step<T>,
    ) -> io::Result<T> {
        self.wait_frame(timeout, what, |frame| pick(frame.message))
    }

    /// Watches messages for `duration` and fails if `is_bad` matches one of them.
    pub fn expect_none(
        &self,
        duration: Duration,
        what: &str,
        mut is_bad: impl FnMut(&Message) -> bool,
    ) -> io::Result<()> {
        let result = self.wait_for(duration, what, |message| {
            if is_bad(&message) {
                Step::Fail(format!("unexpected {message:?}"))
            } else {
                Step::Skip
            }
        });
        match result {
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(()),
            other => other,
        }
    }

    /// Starts a call as `user` and returns its id once core accepted it. Core answers before it
    /// has joined the room: use [`Self::wait_room_ready`] before anything that needs the room.
    pub fn start_call(&self, user: &str) -> io::Result<CallId> {
        let call_id = new_call_id();
        let response = self.request(
            Message::CallStart(CallStartMessage {
                call_id,
                audio_token: livekit_utils::generate_participant_token(user, "audio"),
                video_token: livekit_utils::generate_participant_token(user, "video"),
                audio_device_name: String::new(),
                start_mic_on_call: None,
                start_camera_on_call: None,
            }),
            CALL_START_TIMEOUT,
        )?;
        match response {
            Message::CallStartResult(CallStartResultMessage {
                call_id: id,
                result: Ok(()),
            }) if id == call_id => Ok(call_id),
            Message::CallStartResult(CallStartResultMessage { result: Err(e), .. }) => {
                Err(io::Error::other(format!("CallStart failed: {e}")))
            }
            other => Err(io::Error::other(format!(
                "unexpected response to CallStart: {other:?}"
            ))),
        }
    }

    /// Waits until core has joined the room of `call_id`. Core sends a `ParticipantsSnapshot`
    /// once both rooms are connected and its tracks are published; that snapshot is returned.
    pub fn wait_room_ready(&self, call_id: CallId) -> io::Result<Vec<CoreParticipantState>> {
        self.wait_for(ROOM_READY_TIMEOUT, "room ready", |message| match message {
            Message::ParticipantsSnapshot(participants) => Step::Done(participants),
            Message::RoomConnectionFailed(RoomConnectionFailedMessage { call_id: id, reason })
                if id == call_id =>
            {
                Step::Fail(format!("room connection failed: {reason}"))
            }
            Message::CallEnded(id) if id == call_id => Step::Fail("core ended the call".into()),
            _ => Step::Skip,
        })
    }

    /// Starts a call as `user` and waits until core has joined its room.
    pub fn join_call(&self, user: &str) -> io::Result<CallId> {
        let call_id = self.start_call(user)?;
        self.wait_room_ready(call_id)?;
        Ok(call_id)
    }

    /// Ends `call_id` and waits for core to confirm it.
    pub fn end_call(&self, call_id: CallId) -> io::Result<()> {
        self.send(Message::CallEnd(Some(call_id)))?;
        self.wait_for(CALL_END_TIMEOUT, "CallEnded", |message| match message {
            Message::CallEnded(id) if id == call_id => Step::Done(()),
            _ => Step::Skip,
        })
    }

    /// Shares display `content_id` and waits for core's result. The room must be ready.
    /// Returns the last `ParticipantsSnapshot` core sent before the result, if any (on success
    /// core sends one with the local share before the result).
    pub fn start_screenshare(
        &self,
        content_id: u32,
        width: f64,
        height: f64,
    ) -> io::Result<Option<Vec<CoreParticipantState>>> {
        self.send(Message::StartScreenShare(ScreenShareMessage {
            content: Content {
                content_type: ContentType::Display,
                id: content_id,
            },
            resolution: Extent { width, height },
        }))?;
        let mut last_snapshot = None;
        // The result is an event (no request id); core has no other pending share request.
        self.wait_for(
            SCREENSHARE_START_TIMEOUT,
            "StartScreenShareResult",
            |message| match message {
                Message::ParticipantsSnapshot(participants) => {
                    last_snapshot = Some(participants);
                    Step::Skip
                }
                Message::StartScreenShareResult(Ok(())) => Step::Done(()),
                Message::StartScreenShareResult(Err(e)) => {
                    Step::Fail(format!("StartScreenShare failed: {e}"))
                }
                _ => Step::Skip,
            },
        )?;
        Ok(last_snapshot)
    }

    pub fn list_audio_devices(&self) -> io::Result<Vec<AudioDevice>> {
        match self.request(Message::ListAudioDevices, REQUEST_TIMEOUT)? {
            Message::AudioDeviceList(devices) => Ok(devices),
            other => Err(unexpected("ListAudioDevices", other)),
        }
    }

    pub fn list_cameras(&self) -> io::Result<Vec<CameraDevice>> {
        match self.request(Message::ListCameras, REQUEST_TIMEOUT)? {
            Message::CameraList(devices) => Ok(devices),
            other => Err(unexpected("ListCameras", other)),
        }
    }

    /// Starts the microphone; the outer error is the IPC failing, the inner one core's answer.
    pub fn start_audio_capture(&self, device_name: String) -> io::Result<Result<(), String>> {
        match self.request(
            Message::StartAudioCapture(AudioCaptureMessage { device_name }),
            Duration::from_secs(10),
        )? {
            Message::StartAudioCaptureResult(result) => Ok(result),
            other => Err(unexpected("StartAudioCapture", other)),
        }
    }

    /// Starts the camera; the outer error is the IPC failing, the inner one core's answer.
    pub fn start_camera(&self, device_name: String) -> io::Result<Result<(), String>> {
        match self.request(
            Message::StartCamera(CameraStartMessage {
                device_name: Some(device_name),
            }),
            Duration::from_secs(10),
        )? {
            Message::StartCameraResult(result) => Ok(result),
            other => Err(unexpected("StartCamera", other)),
        }
    }

    /// Round trip through core's event loop. `ListAudioDevices` is answered on the main
    /// thread, so a hung event loop shows up as a timeout here. Returns the round-trip time.
    pub fn probe(&self, timeout: Duration) -> io::Result<Duration> {
        let started = Instant::now();
        match self.request(Message::ListAudioDevices, timeout)? {
            Message::AudioDeviceList(_) => Ok(started.elapsed()),
            other => Err(unexpected("ListAudioDevices", other)),
        }
    }
}

fn unexpected(request: &str, response: Message) -> io::Error {
    io::Error::other(format!("unexpected response to {request}: {response:?}"))
}

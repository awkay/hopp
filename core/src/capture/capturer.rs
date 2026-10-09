use livekit::webrtc::video_source::native::NativeVideoSource;

use socket_lib::Content;
use winit::{dpi::PhysicalPosition, event_loop::EventLoopProxy, monitor::MonitorHandle};

use crate::{
    utils::geometry::{Extent, Frame},
    UserEvent, STREAM_FAILURE_EXIT_CODE,
};

/// Platform-agnostic monitor identifier.
///
/// Different platforms use different types of identifiers for monitors:
/// - macOS uses numeric CGDirectDisplayID
/// - Windows uses device name strings like "\\.\DISPLAY1"
/// - Linux falls back to position-based identification
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitorId {
    /// Numeric identifier (macOS CGDirectDisplayID)
    Numeric(u32),
    /// Named identifier (Windows device name)
    Named(String),
    /// Position-based identifier (Linux fallback)
    Position(PhysicalPosition<i32>),
}
use std::sync::{mpsc, Arc, Mutex};

#[cfg_attr(target_os = "macos", path = "macos_stream.rs")]
#[cfg_attr(not(target_os = "macos"), path = "stream.rs")]
mod stream;
use stream::{Stream, StreamRuntimeMessage};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppVeilCaptureFilter {
    pub excluded_bundle_ids: Vec<String>,
    pub revealed_window_ids: Vec<u32>,
}

// Constants for magic numbers
const MAX_STREAM_FAILURES_BEFORE_EXIT: u64 = 10;
const POLL_STREAM_TIMEOUT_SECS: u64 = 100;
const POLL_STREAM_DATA_SLEEP_MS: u64 = 100;
#[cfg(target_os = "macos")]
const STREAM_RECONFIGURE_DEBOUNCE_MS: u64 = 10;

#[cfg_attr(target_os = "windows", path = "windows.rs")]
#[cfg_attr(target_os = "macos", path = "macos.rs")]
#[cfg_attr(target_os = "linux", path = "linux.rs")]
mod platform;
pub use platform::ScreenshareFunctions;

/// Errors that can occur during screen capturing operations.
///
/// This enum represents various failure modes that can occur when initializing
/// or operating the screen capture system.
#[derive(Debug, thiserror::Error)]
pub enum CapturerError {
    /// Failed to create the underlying desktop capturer instance.
    ///
    /// This error occurs when the system cannot initialize the platform-specific
    /// screen capture functionality. Common causes include:
    #[error("Failed to create DesktopCapturer")]
    DesktopCapturerCreationError,

    /// Capture source list is empty.
    ///
    /// This error could occur when the screen sharing engine fails from the os and
    /// then we try to restart the stream.
    #[error("Capture source list is empty")]
    CaptureSourceListEmpty,

    /// Couldn't find selected source.
    #[error("Couldn't find selected source")]
    SelectedSourceNotFound,

    /// Capture source type is unsupported on this platform.
    #[error("Capture source type is unsupported on this platform")]
    UnsupportedContentType,

    /// Invalid stream dimensions.
    #[error("Invalid stream dimensions")]
    InvalidStreamDimensions,
}

/// Platform-specific extensions for screen sharing and monitor management.
///
/// This trait provides platform-specific functionality for handling monitor
/// selection and sizing in the screen capture system. Implementations of this
/// trait are provided by platform-specific modules (windows.rs, macos.rs) and
/// handle the differences in how each operating system manages displays.
pub trait ScreenshareExt {
    /// Selects and returns a specific monitor handle by ID.
    ///
    /// # Parameters
    /// - `monitors`: A list of all available monitors from the window system
    /// - `input_id`: The identifier of the target monitor to select
    ///
    /// # Returns
    /// The `MonitorHandle` for the specified monitor. If the monitor ID is not found,
    /// returns the first available monitor as a fallback.
    fn get_selected_monitor(monitors: &[MonitorHandle], input_id: u32) -> MonitorHandle;

    /// Returns a platform-agnostic identifier for the given monitor.
    ///
    /// # Parameters
    /// - `monitor`: The monitor handle to get the ID for
    ///
    /// # Returns
    /// A `MonitorId` that uniquely identifies this monitor across position changes.
    fn get_monitor_id(monitor: &MonitorHandle) -> MonitorId;

    /// Reverse mapping: returns the capture content id (used by `Content.id`)
    /// for the given monitor, or `None` if it cannot be resolved.
    fn capture_content_id_for_monitor(monitor: &MonitorHandle) -> Option<u32>;
}

/// Main interface for managing screen capture operations and stream lifecycle.
///
/// The `Capturer` serves as the primary coordinator for screen capture functionality,
/// managing stream creation, lifecycle events, error handling, and communication
/// with the UI layer. It maintains a single active capture stream and provides
/// methods for starting/stopping captures and handling runtime errors through
/// automatic stream recovery.
pub struct Capturer {
    /// Receiver for runtime messages from capture streams.
    ///
    /// Wrapped in Arc<Mutex<>> to allow sharing with the polling thread
    /// that monitors for stream failures and other runtime events without
    /// keeping the main Capturer locked during message processing.
    rx: Arc<Mutex<mpsc::Receiver<StreamRuntimeMessage>>>,

    /// Sender for runtime messages to coordinate stream operations.
    ///
    /// Used internally to send control messages and by streams to report
    /// failures and status changes back to the main capturer.
    tx: mpsc::Sender<StreamRuntimeMessage>,

    /// The currently active capture stream, if any.
    ///
    /// Only one stream can be active at a time. When `None`, no capture
    /// is currently in progress.
    active_stream: Option<Stream>,

    /// Event loop proxy for triggering UI updates and application events.
    ///
    /// Used to communicate capture state changes back to the main application,
    /// particularly for updating the UI when users stop screen sharing through
    /// system controls. This ensures proper cleanup of tracks and room connections.
    event_loop_proxy: EventLoopProxy<UserEvent>,

    app_veil_filter: AppVeilCaptureFilter,

    /// What the capture frame rate is chosen from.
    framerate_inputs: FramerateInputs,
}

/// What the capture frame rate is chosen from. See `bandwidth_mode::capture_framerate`.
#[derive(Debug, Clone, Copy)]
struct FramerateInputs {
    /// Frame rate the screen share encoder takes, set by the room service.
    encoder_fps: f64,
    /// Refresh rate in Hz of the captured display, or of the monitor the shared window is on;
    /// `None` when unknown.
    refresh_hz: Option<u32>,
}

impl FramerateInputs {
    fn capture_framerate(&self) -> u32 {
        crate::bandwidth_mode::capture_framerate(self.encoder_fps, self.refresh_hz)
    }

    /// Returns whether the capture rate changed.
    fn set_encoder_fps(&mut self, fps: f64) -> bool {
        let before = self.capture_framerate();
        self.encoder_fps = fps;
        before != self.capture_framerate()
    }

    /// Returns whether the capture rate changed.
    fn set_refresh_hz(&mut self, refresh_hz: Option<u32>) -> bool {
        let before = self.capture_framerate();
        self.refresh_hz = refresh_hz;
        before != self.capture_framerate()
    }
}

impl Capturer {
    /// Creates a new capturer instance.
    ///
    /// # Parameters
    /// - `event_loop_proxy`: Proxy for sending events back to the main application event loop
    ///
    /// # Returns
    /// A new `Capturer` instance ready to capture screen sources.
    ///
    /// # Notes
    /// The capturer is created in an idle state with no active streams.
    /// Use `start_capture()` with a display `Content` id to begin capturing.
    pub fn new(event_loop_proxy: EventLoopProxy<UserEvent>) -> Self {
        let (tx, rx) = mpsc::channel();
        Capturer {
            rx: Arc::new(Mutex::new(rx)),
            tx,
            active_stream: None,
            event_loop_proxy,
            app_veil_filter: AppVeilCaptureFilter::default(),
            framerate_inputs: FramerateInputs {
                encoder_fps: crate::bandwidth_mode::MAX_FRAMERATE,
                refresh_hz: None,
            },
        }
    }

    /// Follows the encoder's frame rate (it changes with low-bandwidth mode), so the capture
    /// doesn't produce frames the encoder drops. See `bandwidth_mode::capture_framerate`.
    pub fn set_encoder_framerate(&mut self, fps: f64) {
        if self.framerate_inputs.set_encoder_fps(fps) {
            self.capture_framerate_changed();
        }
    }

    /// Sets the refresh rate the capture rate is chosen for: a shared window's monitor is only
    /// known once its capture started, and the window can move to another one.
    pub fn set_display_refresh_hz(&mut self, refresh_hz: Option<u32>) {
        if self.framerate_inputs.set_refresh_hz(refresh_hz) {
            self.capture_framerate_changed();
        }
    }

    /// Leaves switching the running stream to the new capture rate to the poll thread. The
    /// setters above run on the main thread, the encoder's at any participant's request
    /// (low-bandwidth mode), and changing a running stream's rate blocks until ScreenCaptureKit
    /// answers. A setter that leaves the rate unchanged sends nothing: the stream already
    /// captures at it, or an earlier change is still pending and the poll thread reads the
    /// latest rate when it applies it.
    fn capture_framerate_changed(&self) {
        #[cfg(target_os = "macos")]
        if self.active_stream.is_some() {
            if let Err(error) = self.tx.send(StreamRuntimeMessage::CaptureFramerateChanged) {
                log::error!("capture_framerate_changed: error notifying the poll thread: {error}");
            }
        }
    }

    /// Switches the running stream to the current capture rate. Blocks on ScreenCaptureKit when
    /// the rate changes, so only the poll thread calls it.
    #[cfg(target_os = "macos")]
    fn apply_capture_framerate(&self) {
        if let Some(stream) = self.active_stream.as_ref() {
            stream.set_framerate(self.framerate_inputs.capture_framerate());
        }
    }

    /// Starts capturing frames from the specified content source.
    ///
    /// # Parameters
    /// - `content`: The content source to capture (display or window with display_id)
    /// - `stream_resolution`: The resolution of the stream buffer
    /// - `display_refresh_hz`: Refresh rate of the captured display, `None` for a window (see
    ///   `set_display_refresh_hz`)
    ///
    /// # Returns
    /// - `Ok(())`: Successfully started the capture stream
    /// - `Err(CapturerError)`: Failed to create or start the capture stream
    ///
    /// # Behavior
    /// - Stops any existing active stream
    /// - Selects the appropriate monitor based on the content's display_id
    /// - Creates a new capture stream configured for the target resolution
    /// - Starts the capture loop and frame processing pipeline
    ///
    /// # Notes
    /// Only one stream can be active at a time. Starting a new capture automatically
    /// stops the previous one. The returned monitor handle represents the physical
    /// display being captured.
    pub fn start_capture(
        &mut self,
        content: Content,
        stream_resolution: Extent,
        buffer_source: NativeVideoSource,
        scale: f64,
        display_refresh_hz: Option<u32>,
    ) -> Result<(), CapturerError> {
        log::info!(
            "start_capture: content {content:?} resolution: {stream_resolution:?} scale: {scale} refresh: {display_refresh_hz:?} Hz"
        );
        self.framerate_inputs.refresh_hz = display_refresh_hz;
        if self.active_stream.is_some() {
            log::warn!("start_capture: active stream, stopping it");
            self.active_stream.as_mut().unwrap().stop_capture();
            self.active_stream = None;
        }

        let mut stream = Stream::new(
            content,
            stream_resolution,
            scale,
            self.tx.clone(),
            buffer_source,
            self.app_veil_filter.clone(),
        )?;
        #[cfg(target_os = "macos")]
        stream.set_framerate(self.framerate_inputs.capture_framerate());

        stream.start_capture()?;
        self.active_stream = Some(stream);
        Ok(())
    }

    /// Signals the capture thread to stop and releases the active stream.
    /// The thread is detached and will exit on its own.
    /// Safe to call when no stream is active (no-op).
    pub fn stop_capture(&mut self) {
        log::info!("stop_capture");
        if self.active_stream.is_none() {
            log::warn!("stop_capture: no active stream");
            return;
        }
        self.active_stream.as_mut().unwrap().stop_capture();
        self.active_stream = None;
    }

    /// Restarts the current stream to recover from permanent errors.
    ///
    /// # Behavior
    /// - Stops the current stream if running
    /// - Checks failure count and exits process if too many consecutive failures
    /// - Creates a new stream instance sharing the same buffers and configuration
    /// - Restarts capture on the same source ID
    /// - Preserves failure tracking across restart
    ///
    /// # Error Handling
    /// If the failure count exceeds MAX_STREAM_FAILURES_BEFORE_EXIT, the process
    /// will exit with STREAM_FAILURE_EXIT_CODE to trigger application restart.
    /// This prevents infinite restart loops when the capture system is fundamentally broken.
    ///
    /// # Notes
    /// This method is typically called automatically by the polling thread when
    /// permanent capture errors are detected. Manual calls should be rare.
    pub fn restart_stream(&mut self) {
        log::info!("restart_stream");
        std::thread::sleep(std::time::Duration::from_millis(200));

        self.active_stream = match self.active_stream.take() {
            Some(mut stream) => {
                stream.stop_capture();

                // If something fails here we are killing the process in
                // order to trigger the health check in the tauri app.
                // The health check will instruct the user to restart.
                // We should do this via a message in the future.
                let failures_count = stream.get_failures_count();
                if failures_count > MAX_STREAM_FAILURES_BEFORE_EXIT {
                    log::error!("restart_stream: Too many failures, killing the process");
                    sentry_utils::upload_logs_event("Stream failed".to_string());
                    sentry_utils::flush(std::time::Duration::from_secs(2));
                    std::process::exit(STREAM_FAILURE_EXIT_CODE);
                }

                let mut new_stream = match stream.copy() {
                    Ok(new_stream) => new_stream,
                    Err(_) => {
                        log::error!("restart_stream: Failed to copy stream");
                        sentry_utils::upload_logs_event("Stream copy failed".to_string());
                        sentry_utils::flush(std::time::Duration::from_secs(2));
                        std::process::exit(STREAM_FAILURE_EXIT_CODE);
                    }
                };

                // Sometimes the capturer fails with a permanent error from the os.
                // We can't really do much about it, as we are relying on the os
                // and DesktopCapturer from libwebrtc for capturing the screen.
                // So we just sleep and retry a few times in case it's a temporary error.
                // If we can't restart the stream after 10 retries, we exit the process
                // and inform the user to restart the application.
                let mut res = new_stream.start_capture();
                for i in 0..MAX_STREAM_FAILURES_BEFORE_EXIT {
                    if res.is_ok() {
                        break;
                    }

                    if matches!(res, Err(CapturerError::SelectedSourceNotFound)) {
                        log::info!("restart_stream: Source not found, stopping screen share");
                        let _ = self.event_loop_proxy.send_event(UserEvent::StopScreenShare);
                        return;
                    }

                    log::info!("restart_stream: Failed to start capture, retrying {i}/10 {res:?}");
                    std::thread::sleep(std::time::Duration::from_millis(100));

                    new_stream = match new_stream.copy() {
                        Ok(new_stream) => new_stream,
                        Err(_) => {
                            log::error!("restart_stream: Failed to copy stream");
                            sentry_utils::upload_logs_event("Stream copy failed".to_string());
                            sentry_utils::flush(std::time::Duration::from_secs(2));
                            std::process::exit(STREAM_FAILURE_EXIT_CODE);
                        }
                    };
                    res = new_stream.start_capture();
                }

                if let Err(ref e) = res {
                    if matches!(e, CapturerError::SelectedSourceNotFound) {
                        log::info!(
                            "restart_stream: Source not found after retries, stopping screen share"
                        );
                        let _ = self.event_loop_proxy.send_event(UserEvent::StopScreenShare);
                        return;
                    }
                    log::error!("restart_stream: Failed to start capture after 10 retries {res:?}");
                    sentry_utils::upload_logs_event("Stream start capture failed".to_string());
                    sentry_utils::flush(std::time::Duration::from_secs(2));
                    std::process::exit(STREAM_FAILURE_EXIT_CODE);
                }

                log::info!("restart_stream: new stream created");
                Some(new_stream)
            }
            None => None,
        };
    }

    /// Checks if there is currently an active capture stream.
    ///
    /// # Returns
    /// - `true`: A capture stream is currently active and capturing frames
    /// - `false`: No capture is in progress
    pub fn has_active_stream(&self) -> bool {
        self.active_stream.is_some()
    }

    pub fn set_app_veil_bundle_ids(&mut self, excluded_bundle_ids: Vec<String>) {
        self.app_veil_filter.excluded_bundle_ids = excluded_bundle_ids;
        self.refresh_app_veil_filter();
    }

    pub fn app_veil_enabled(&self) -> bool {
        !self.app_veil_filter.excluded_bundle_ids.is_empty()
    }

    pub fn app_veil_bundle_ids(&self) -> &[String] {
        &self.app_veil_filter.excluded_bundle_ids
    }

    pub fn refresh_app_veil_filter(&mut self) {
        let Some(stream) = self.active_stream.as_mut() else {
            return;
        };
        if let Err(error) = stream.update_app_veil_filter(self.app_veil_filter.clone()) {
            log::error!("refresh_app_veil_filter: failed to update capture filter: {error:?}");
            sentry_utils::upload_logs_event("App Veil capture filter update failed".to_string());
        }
    }

    pub fn frame(&self) -> Option<Arc<Mutex<Frame>>> {
        self.active_stream.as_ref()?.frame()
    }

    pub fn target_process_id(&self) -> Option<i32> {
        self.active_stream.as_ref()?.target_process_id()
    }

    pub fn target_window_id(&self) -> Option<u32> {
        self.active_stream.as_ref()?.target_window_id()
    }

    /// Signals the runtime stream monitoring thread to terminate.
    ///
    /// # Behavior
    /// Sends a `Stop` message to the polling thread that monitors for stream
    /// failures and runtime events. This is used during application shutdown
    /// to ensure all capture-related threads terminate cleanly.
    ///
    /// # Notes
    /// This method should be called before dropping the Capturer instance to
    /// prevent the polling thread from running indefinitely. The method is
    /// non-blocking and returns immediately after sending the stop signal.
    pub fn stop_runtime_stream_handler(&self) {
        let res = self.tx.send(StreamRuntimeMessage::Stop);
        if let Err(e) = res {
            log::error!("stop_runtime_stream_handler: error sending Stop message: {e}");
        }
    }

    pub fn get_selected_monitor(&self, monitors: &[MonitorHandle], input_id: u32) -> MonitorHandle {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            ScreenshareFunctions::get_selected_monitor(monitors, input_id)
        }
        #[cfg(target_os = "linux")]
        {
            if self.active_stream.is_none() {
                log::warn!("get_selected_monitor: no active stream");
                return monitors[0].clone();
            }
            let capturer = self.active_stream.as_ref().unwrap().capturer();
            let capturer = capturer.lock().unwrap();
            for _ in 0..150 {
                let rect = capturer.get_source_rect();
                if rect.top != 0 || rect.left != 0 || rect.width != 0 || rect.height != 0 {
                    for monitor in monitors {
                        let position = monitor.position();
                        let size = monitor.size();
                        if position.x == rect.left
                            && position.y == rect.top
                            && size.width == (rect.width as u32)
                            && size.height == (rect.height as u32)
                        {
                            return monitor.clone();
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(POLL_STREAM_DATA_SLEEP_MS));
            }
            log::error!("get_selected_monitor: capturer hasn't started");
            return monitors[0].clone();
        }
    }

    pub fn get_stream_extent(&self) -> Extent {
        if self.active_stream.is_none() {
            log::error!("get_stream_extent: no active stream");
            return Extent {
                width: 0.,
                height: 0.,
            };
        }
        let stream = self.active_stream.as_ref().unwrap();
        for i in 0..150 {
            let extent = stream.get_stream_extent();
            if extent.width > 1. && extent.height > 1. {
                log::info!("get_stream_extent: got extent in try {i}");
                return extent;
            }
            std::thread::sleep(std::time::Duration::from_millis(POLL_STREAM_DATA_SLEEP_MS));
        }
        Extent {
            width: 0.,
            height: 0.,
        }
    }
}

/// The latest of a burst of changes, due once no newer one has arrived for
/// `STREAM_RECONFIGURE_DEBOUNCE_MS`. The poll thread applies window resizes and capture rate
/// changes this way, so that a burst of them costs one blocking ScreenCaptureKit call.
#[cfg(target_os = "macos")]
#[derive(Debug)]
struct Debounced<T> {
    pending: Option<(T, std::time::Instant)>,
}

#[cfg(target_os = "macos")]
impl<T> Default for Debounced<T> {
    fn default() -> Self {
        Self { pending: None }
    }
}

#[cfg(target_os = "macos")]
impl<T> Debounced<T> {
    /// Replaces any pending change and restarts the wait.
    fn request(&mut self, change: T, now: std::time::Instant) {
        self.pending = Some((
            change,
            now + std::time::Duration::from_millis(STREAM_RECONFIGURE_DEBOUNCE_MS),
        ));
    }

    /// How long until the pending change is due, `None` when there is none.
    fn timeout(&self, now: std::time::Instant) -> Option<std::time::Duration> {
        self.pending
            .as_ref()
            .map(|(_, due)| due.saturating_duration_since(now))
    }

    /// Takes the pending change once it is due. A change requested after this, while the
    /// taken one is being applied, stays pending for a later call.
    fn take_due(&mut self, now: std::time::Instant) -> Option<T> {
        match self.pending {
            Some((_, due)) if due <= now => self.pending.take().map(|(change, _)| change),
            _ => None,
        }
    }
}

/*
 * This function is spawned in a separate thread and
 * is used for checking whether the stream failed, if it
 * failed it restarts it.
 *
 * This thread is owned by the Application struct.
 */
pub fn poll_stream(capturer: Arc<Mutex<Capturer>> /* mut socket: CursorSocket */) {
    let rx = { capturer.lock().unwrap().rx.clone() };
    #[cfg(target_os = "macos")]
    let mut pending_resize = Debounced::<(u32, u32)>::default();
    // Carries no rate: it is read from the capturer when applied, so it fits the share running
    // by then.
    #[cfg(target_os = "macos")]
    let mut pending_framerate = Debounced::<()>::default();
    loop {
        log::debug!("poll_stream: waiting for message");
        let rx_lock = rx.lock();
        if rx_lock.is_err() {
            log::error!("poll_stream: rx lock error");
            break;
        }
        let rx_lock = rx_lock.unwrap();
        #[cfg(target_os = "macos")]
        let timeout = {
            let now = std::time::Instant::now();
            [pending_resize.timeout(now), pending_framerate.timeout(now)]
                .into_iter()
                .flatten()
                .min()
                .unwrap_or(std::time::Duration::from_secs(POLL_STREAM_TIMEOUT_SECS))
        };
        #[cfg(not(target_os = "macos"))]
        let timeout = std::time::Duration::from_secs(POLL_STREAM_TIMEOUT_SECS);
        match rx_lock.recv_timeout(timeout) {
            Ok(StreamRuntimeMessage::Failed) => {
                log::info!("poll_stream: stream failed");
                let mut capturer = capturer.lock().unwrap();
                capturer.restart_stream();
            }
            Ok(StreamRuntimeMessage::UserStoppedCapture) => {
                log::info!("poll_stream: user stopped capture");
                let capturer = capturer.lock().unwrap();
                let _ = capturer
                    .event_loop_proxy
                    .send_event(UserEvent::StopScreenShare);
            }
            #[cfg(target_os = "macos")]
            Ok(StreamRuntimeMessage::FrameChanged { resize }) => {
                if let Some(size) = resize {
                    pending_resize.request(size, std::time::Instant::now());
                }
                let capturer = capturer.lock().unwrap();
                let _ = capturer
                    .event_loop_proxy
                    .send_event(UserEvent::CaptureFrameChanged);
            }
            #[cfg(target_os = "macos")]
            Ok(StreamRuntimeMessage::CaptureFramerateChanged) => {
                pending_framerate.request((), std::time::Instant::now());
            }
            Ok(StreamRuntimeMessage::Stop) => {
                log::info!("poll_stream: stop message");
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            _ => {}
        };
        #[cfg(target_os = "macos")]
        {
            let now = std::time::Instant::now();
            if let Some((width, height)) = pending_resize.take_due(now) {
                if let Some(stream) = capturer.lock().unwrap().active_stream.as_ref() {
                    stream.reconfigure(width, height);
                }
            }
            if pending_framerate.take_due(now).is_some() {
                capturer.lock().unwrap().apply_capture_framerate();
            }
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::bandwidth_mode::{LOW_BANDWIDTH_FRAMERATE, MAX_FRAMERATE};
    use std::time::{Duration, Instant};

    const DEBOUNCE: Duration = Duration::from_millis(STREAM_RECONFIGURE_DEBOUNCE_MS);

    #[test]
    fn only_a_changed_capture_rate_is_sent_to_the_poll_thread() {
        let mut inputs = FramerateInputs {
            encoder_fps: MAX_FRAMERATE,
            refresh_hz: Some(60),
        };
        assert_eq!(inputs.capture_framerate(), 60);
        // Low-bandwidth mode on a 60 Hz display: 60 -> 15 fps and back.
        assert!(inputs.set_encoder_fps(LOW_BANDWIDTH_FRAMERATE));
        assert!(!inputs.set_encoder_fps(LOW_BANDWIDTH_FRAMERATE));
        assert!(inputs.set_encoder_fps(MAX_FRAMERATE));
        // On a 144 Hz display both modes capture 60, so flipping them never reconfigures.
        assert!(!inputs.set_refresh_hz(Some(144)));
        assert!(!inputs.set_encoder_fps(LOW_BANDWIDTH_FRAMERATE));
        assert!(!inputs.set_encoder_fps(MAX_FRAMERATE));
        // A shared window moving to a 120 Hz monitor, then to one of unknown refresh rate.
        assert!(inputs.set_refresh_hz(Some(120)));
        assert_eq!(inputs.capture_framerate(), 40);
        assert!(inputs.set_refresh_hz(None));
        assert_eq!(inputs.capture_framerate(), 60);
    }

    #[test]
    fn a_change_is_due_after_the_debounce() {
        let mut pending = Debounced::default();
        let start = Instant::now();
        assert_eq!(pending.timeout(start), None);
        assert_eq!(pending.take_due(start), None);
        pending.request(15, start);
        assert_eq!(pending.timeout(start), Some(DEBOUNCE));
        assert_eq!(pending.take_due(start + DEBOUNCE / 2), None);
        assert_eq!(pending.timeout(start + DEBOUNCE / 2), Some(DEBOUNCE / 2));
        assert_eq!(pending.take_due(start + DEBOUNCE), Some(15));
        // Applied once.
        assert_eq!(pending.timeout(start + DEBOUNCE), None);
        assert_eq!(pending.take_due(start + DEBOUNCE * 2), None);
    }

    #[test]
    fn a_burst_waits_for_its_last_change_and_applies_only_that() {
        let mut pending = Debounced::default();
        let start = Instant::now();
        // A peer flipping low-bandwidth mode faster than the debounce.
        let step = DEBOUNCE / 2;
        for (i, fps) in [15, 60, 15, 60].into_iter().enumerate() {
            let now = start + step * i as u32;
            assert_eq!(pending.take_due(now), None);
            pending.request(fps, now);
        }
        let last = start + step * 3;
        assert_eq!(
            pending.take_due(last + DEBOUNCE - Duration::from_millis(1)),
            None
        );
        assert_eq!(pending.take_due(last + DEBOUNCE), Some(60));
    }

    #[test]
    fn a_change_requested_while_one_is_applied_stays_pending() {
        let mut pending = Debounced::default();
        let start = Instant::now();
        pending.request((1280, 720), start);
        let applied_at = start + DEBOUNCE;
        assert_eq!(pending.take_due(applied_at), Some((1280, 720)));
        // Requested while the poll thread waits on ScreenCaptureKit for the first one.
        let requested_at = applied_at + Duration::from_millis(1);
        pending.request((1920, 1080), requested_at);
        assert_eq!(pending.timeout(requested_at), Some(DEBOUNCE));
        assert_eq!(
            pending.take_due(requested_at + DEBOUNCE),
            Some((1920, 1080))
        );
    }
}

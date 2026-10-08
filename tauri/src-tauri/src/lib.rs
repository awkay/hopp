#[cfg(target_os = "macos")]
pub mod app_activation;
pub mod app_state;
pub mod application_catalog;
pub mod call_state;
pub mod core_client;
pub mod core_events;
pub mod permissions;
pub mod shortcuts;
#[cfg(target_os = "macos")]
pub mod sleep_prevention;
pub mod sounds;
pub mod tray;

use log::LevelFilter;
use rand::{distributions::Alphanumeric, Rng};
use sounds::SoundEntry;
use std::collections::VecDeque;
use std::env;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;
use tauri::async_runtime::Receiver;
#[cfg(target_os = "macos")]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, Wry};
#[cfg(target_os = "macos")]
use tauri::{Rect, TitleBarStyle, WebviewWindow};
use tauri_plugin_autostart::AutoLaunchManager;
use tauri_plugin_shell::{process::CommandChild, process::CommandEvent, ShellExt};

use socket_lib::call::CallTracker;
use socket_lib::{DrawingEnabled, EventSocket, Message, SentryMetadata, SocketSender};
#[cfg(target_os = "macos")]
use tauri::{LogicalPosition, PhysicalPosition, PhysicalSize};

#[cfg(all(target_os = "macos", not(debug_assertions)))]
use smappservice_rs::*;

const PING_SLEEP_SECS: u64 = 30;
const PING_CORE_PROCESS_INTERVAL_SECS: u64 = 15;
pub const CORNER_RADIUS: f64 = 12.0;

#[derive(Debug, thiserror::Error)]
pub enum CoreProcessCreationError {
    #[error("Failed to create socket")]
    SocketCreationFailed,
    #[error("Failed to send message to core process")]
    SendMessageFailed,
}

/// Wrapper for the core process child handle.
pub struct CoreProcess {
    pub process: CommandChild,
}

/// Persistent settings plus the runtime values that must be re-sent to a restarted core.
///
/// Lock rule: a setter saves and enqueues the matching message to core inside one critical
/// section (enqueueing never blocks), so the order of saves and sends always agrees, and a
/// core restart (which re-sends everything from here and swaps the connection under this
/// lock) can never lose or reorder an update. Never held across awaits.
pub struct Settings {
    pub app_state: app_state::AppState,
    /// Livekit server URL.
    pub livekit_server_url: String,
    /// Last metadata from the frontend, re-sent to a restarted core.
    pub sentry_metadata: Option<SentryMetadata>,
}

impl Settings {
    /// Everything a freshly started core needs to match the user's settings. Used at startup
    /// and after a restart, so the two can't drift apart (App Veil included).
    pub fn core_startup_config(&self) -> Vec<Message> {
        let settings = self.app_state.user_settings();
        // Privacy: never fail open. A malformed entry is skipped, the rest stay protected.
        let app_veil_bundle_ids = app_state::enabled_app_veil_bundle_ids_skipping_invalid(
            &settings.app_veil_applications,
        );
        let mut messages = vec![
            Message::SetAppVeilBundleIds(app_veil_bundle_ids),
            Message::SetNoiseCancellation(settings.noise_cancellation_enabled),
            Message::SetScreenShareResolution(settings.screen_share_resolution),
            Message::SetLowBandwidthDefault(settings.low_bandwidth_default),
            Message::SetScreenSharePickerMode(settings.screen_share_picker_mode),
            Message::SetTelemetryEnabled(settings.telemetry_enabled),
            Message::ControllerCursorEnabled(settings.remote_control_enabled),
            Message::ControllerDrawPersistChanged(self.app_state.controller_draw_persist()),
            Message::SetPreferredCamera(self.app_state.last_used_camera()),
        ];
        if let Some(mode) = self.app_state.last_mode() {
            messages.push(Message::LastModeChanged(mode));
        }
        if let Some(metadata) = &self.sentry_metadata {
            messages.push(Message::SentryMetadata(metadata.clone()));
        }
        if !self.livekit_server_url.is_empty() {
            messages.push(Message::LivekitServerUrl(self.livekit_server_url.clone()));
        }
        messages
    }
}

/// Runtime state shared by commands, the core event dispatcher and the main thread.
///
/// There is no global lock. Each part has its own synchronization, and the rules are:
/// - Nothing is held across I/O to core or across a wait for a response: requests go
///   through `core` (non-blocking enqueue) and are awaited with no lock held.
/// - The main thread (sync commands, activation observer, window events, shortcut
///   handlers) never blocks and only touches atomics, `tray_state`, and tiny locks that
///   are never held across I/O or waits.
/// - Global shortcuts are (un)registered only on the main thread, fire-and-forget.
/// - Lock order when nesting is unavoidable: `call` before `settings`.
pub struct AppData {
    /// The connection to core. All messages go through its single ordered queue.
    pub core: core_client::CoreClient,

    pub settings: Mutex<Settings>,

    /// Call lifecycle keyed by call id. Transitions and the queuing of their side effects
    /// (main-thread closures, CallStart/CallEnd enqueue) happen under this lock.
    pub call: Mutex<CallTracker>,

    /// Whether the local camera is currently on (pushed from frontend snapshots).
    pub is_camera_on: AtomicBool,

    /// Whether the local participant is currently screensharing (pushed from frontend snapshots).
    pub is_screensharing: AtomicBool,

    /// Whether drawing mode is currently enabled (runtime state).
    pub drawing_enabled: AtomicBool,

    /// Active sound entries currently being played by the application.
    /// Used to prevent duplicate sounds and manage sound lifecycle.
    pub sound_entries: Mutex<Vec<SoundEntry>>,

    /// Flag to control whether the main window should hide when it loses focus.
    /// This is set to true when the user is writing feedback.
    pub deactivate_hiding: Arc<Mutex<bool>>,

    /// Suppresses main window hide when activation policy is switched to Accessory after a call ends.
    pub suppress_hide_on_call_end: Arc<AtomicBool>,

    /// Tracks whether the activation policy is currently Regular (true) or Accessory (false).
    /// Written on the main thread together with the policy switch.
    pub activation_policy_regular: Arc<AtomicBool>,

    /// Tray icon state. Only touched on the main thread. `None` when there is no tray icon.
    pub tray_state: Mutex<Option<tray::TrayState>>,

    /// The window style this session runs with (fixed at launch).
    pub window_style: app_state::WindowStyleSettings,
    /// Whether the menu-bar item shows the draw / stop sharing buttons (written on the main
    /// thread together with the icon; read when placing the popup and handling clicks).
    pub tray_sharing_controls: AtomicBool,

    /// macOS app activation observer — keeps the NSNotificationCenter observer alive.
    #[cfg(target_os = "macos")]
    pub activation_observer: Mutex<Option<app_activation::AppActivationObserver>>,

    /// macOS sleep prevention state — holds an activity assertion while a call is active.
    #[cfg(target_os = "macos")]
    pub sleep_prevention: Mutex<sleep_prevention::SleepPrevention>,

    /// Recent core restarts, for backoff.
    pub core_restarts: Mutex<RestartBackoff>,

    /// Mirror of the current call id (0 = none) for lock-free readers such as main-thread
    /// shortcut handlers. Written by `call_state` under the call lock.
    pub current_call_id: std::sync::atomic::AtomicU64,

    /// Incremented for every core connection; lets a process monitor tell whether the
    /// connection it belongs to is still the current one.
    pub core_generation: AtomicUsize,
}

impl AppData {
    pub fn new(
        deactivate_hiding: Arc<Mutex<bool>>,
        app_state: app_state::AppState,
        suppress_hide_on_call_end: Arc<AtomicBool>,
    ) -> Self {
        let window_style = app_state::WindowStyleSettings::effective(&app_state.user_settings());
        AppData {
            core: core_client::CoreClient::new(),
            settings: Mutex::new(Settings {
                app_state,
                livekit_server_url: String::new(),
                sentry_metadata: None,
            }),
            call: Mutex::new(CallTracker::default()),
            is_camera_on: AtomicBool::new(false),
            is_screensharing: AtomicBool::new(false),
            drawing_enabled: AtomicBool::new(false),
            sound_entries: Mutex::new(Vec::new()),
            deactivate_hiding,
            suppress_hide_on_call_end,
            activation_policy_regular: Arc::new(AtomicBool::new(false)),
            tray_state: Mutex::new(None),
            window_style,
            tray_sharing_controls: AtomicBool::new(false),
            #[cfg(target_os = "macos")]
            activation_observer: Mutex::new(None),
            #[cfg(target_os = "macos")]
            sleep_prevention: Mutex::new(sleep_prevention::SleepPrevention::new()),
            core_restarts: Mutex::new(RestartBackoff::default()),
            current_call_id: std::sync::atomic::AtomicU64::new(0),
            core_generation: AtomicUsize::new(0),
        }
    }

    pub fn settings(&self) -> std::sync::MutexGuard<'_, Settings> {
        self.settings.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Turns the sharer's local drawing on or off. Used by the popup and the menu-bar draw
/// button; emits `drawing_enabled_changed` so the popup reflects either.
pub fn set_drawing_enabled(app: &AppHandle, enabled: bool, permanent: bool) {
    log::info!("set_drawing_enabled: enabled={enabled} permanent={permanent}");
    let data = app.state::<AppData>();

    if data.drawing_enabled.swap(enabled, Ordering::Relaxed) == enabled {
        return;
    }

    if data
        .core
        .send(Message::DrawingEnabled(DrawingEnabled { permanent }))
        .is_err()
    {
        data.drawing_enabled.store(!enabled, Ordering::Relaxed);
        return;
    }

    if let Err(e) = app.emit("drawing_enabled_changed", enabled) {
        log::error!("set_drawing_enabled: failed to emit drawing_enabled_changed: {e:?}");
    }
    tray::update_drawing_icon(app);

    if let Some(window) = app.get_webview_window("main") {
        #[cfg(not(target_os = "macos"))]
        let _ = window.set_always_on_top(enabled);
        if enabled {
            #[cfg(target_os = "macos")]
            let _ = window.hide();
            #[cfg(target_os = "windows")]
            let _ = window.minimize();
        }
    }
}

/// Limits restarts of a core that keeps exiting with code 2.
#[derive(Debug, Default)]
pub struct RestartBackoff {
    recent: VecDeque<Instant>,
}

impl RestartBackoff {
    pub const WINDOW: Duration = Duration::from_secs(10 * 60);
    pub const MAX_RESTARTS: usize = 3;

    /// Records a restart attempt at `now`. Returns the delay to wait before restarting, or
    /// `None` when too many restarts happened within `WINDOW` (give up).
    pub fn next_delay(&mut self, now: Instant) -> Option<Duration> {
        while let Some(oldest) = self.recent.front() {
            if now.duration_since(*oldest) > Self::WINDOW {
                self.recent.pop_front();
            } else {
                break;
            }
        }
        if self.recent.len() >= Self::MAX_RESTARTS {
            return None;
        }
        let delay = Duration::from_secs(1u64 << self.recent.len());
        self.recent.push_back(now);
        Some(delay)
    }
}

/// Monitors core process output and emits crash events.
async fn show_stdout(
    mut receiver: Receiver<CommandEvent>,
    app_handle: AppHandle,
    generation: usize,
) {
    let mut crash_msg = String::new();
    let mut restart = false;
    while let Some(event) = receiver.recv().await {
        match event {
            CommandEvent::Stdout(line) => {
                log::info!("{}", String::from_utf8(line).unwrap_or_default());
            }
            CommandEvent::Stderr(line) => {
                /* For some reason the sidecar process logs to stderr. */
                log::info!("{}", String::from_utf8(line).unwrap_or_default());
            }
            CommandEvent::Terminated(payload) => {
                log::error!("show_stdout: Terminated {payload:?}");
                match payload.code {
                    Some(code) => {
                        if code == 1 {
                            crash_msg = "Core process terminated because it failed to receive messages from tauri, please restart the app".to_string();
                        } else if code == 2 {
                            // When hopp_core is terminated because capturing failed from the OS
                            // and couldn't be recovered, we restart it.
                            restart = true;
                        } else if code == 3 {
                            crash_msg = "Core process terminated because the event loop stopped responding, please restart the app".to_string();
                            sentry_utils::upload_logs_event(
                                "Hang protection triggered".to_string(),
                            );
                        }
                    }
                    None => {
                        crash_msg = "Core process terminated because of an unknown error. Please restart the app, please submit a bug report".to_string();
                        sentry_utils::upload_logs_event("?Unknown crash".to_string());
                    }
                }
                break;
            }
            CommandEvent::Error(e) => {
                log::error!("show_stdout: Error: {e:?}");
                break;
            }
            _ => {}
        }
    }
    log::info!("show_stdout: Finished");

    let current_generation = app_handle
        .state::<AppData>()
        .core_generation
        .load(std::sync::atomic::Ordering::SeqCst);
    if generation != current_generation {
        log::info!("show_stdout: core generation {generation} already replaced, ignoring exit");
        return;
    }

    if restart {
        // Process spawn and socket connect block (sysinfo scan, connect retries), so keep
        // them off the async runtime.
        let app = app_handle.clone();
        std::thread::spawn(move || restart_core(app));
        return;
    }

    // Communicate to the frontend that the core process has crashed.
    let res = app_handle.emit("core_process_crashed", crash_msg);
    if let Err(e) = res {
        log::error!("Failed to emit core_process_crashed: {e:?}");
    }
}

/// Restarts core after it exited with code 2. Runs on its own thread.
fn restart_core(app: AppHandle) {
    let data = app.state::<AppData>();
    // Fail requests still waiting on the dead process right away.
    data.core.shutdown();
    // The call lived in the dead process: end it everywhere.
    call_state::reset_for_core_restart(&app);

    let delay = data
        .core_restarts
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .next_delay(Instant::now());
    let crash_msg = match delay {
        None => {
            log::error!("restart_core: core keeps failing, giving up");
            sentry_utils::upload_logs_event("Core restart limit reached".to_string());
            "Core process keeps failing because capturing failed from the OS, please restart the app".to_string()
        }
        Some(delay) => {
            log::info!("restart_core: restarting core in {delay:?}");
            std::thread::sleep(delay);
            match connect_core(&app) {
                Ok(()) => "Core process restarted because capturing failed from the OS and couldn't be recovered, please start the call again".to_string(),
                Err(e) => {
                    log::error!("restart_core: failed: {e}");
                    "Core process terminated because capturing failed from the OS and couldn't be recovered, please restart the app".to_string()
                }
            }
        }
    };
    if let Err(e) = app.emit("core_process_crashed", crash_msg) {
        log::error!("Failed to emit core_process_crashed: {e:?}");
    }
}

/// Spawns the core process sidecar with required arguments.
fn start_sidecar(
    app: &tauri::AppHandle,
    socket_path: &str,
) -> (Receiver<CommandEvent>, CommandChild) {
    log::info!("start_sidecar:");

    /* First we check if the process is already running and kill it. */
    if !cfg!(debug_assertions) {
        let system = sysinfo::System::new_all();
        for process in system.processes().values() {
            if let Some(name) = process.name().to_str() {
                if name.contains("hopp_core") {
                    log::info!("start_sidecar: Found running core process, killing it");
                    let _ = process.kill();
                }
            }
        }
    }

    let mut args = vec!["--socket-path", socket_path];

    let sentry_dsn = get_sentry_dsn();
    if !cfg!(debug_assertions) {
        args.push("--sentry-dsn");
        args.push(&sentry_dsn);
    }

    let mut hopp_core_name = "hopp_core".to_string();
    if cfg!(debug_assertions) {
        hopp_core_name = format!("hopp_core{}", env::var("HOPP_SUFFIX").unwrap_or_default());
    }
    let command = app.shell().sidecar(hopp_core_name).unwrap().args(args);
    let (rx, child) = command.spawn().expect("Failed to spawn sidecar");
    (rx, child)
}

/// Creates a socket connection to communicate with the core process.
/// Blocking (retries for up to 20 s): call from a plain thread, never from async code.
fn create_core_process_socket(
    socket_path: &str,
) -> Result<(SocketSender, EventSocket), CoreProcessCreationError> {
    let max_tries = 20;
    let mut tries = 0;
    loop {
        match socket_lib::connect(socket_path) {
            Ok(pair) => return Ok(pair),
            Err(_) => {
                log::debug!("create_core_process_socket: Failed to connect, retrying in 1 second");
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        tries += 1;
        if tries >= max_tries {
            log::error!(
                "create_core_process_socket: Failed to create socket after {max_tries} tries"
            );
            break;
        }
    }
    Err(CoreProcessCreationError::SocketCreationFailed)
}

/// Pings core through the shared queue so it doesn't time out (core exits when Tauri is
/// gone). Runs for the app's lifetime and survives core restarts.
pub async fn ping_core(app: AppHandle) {
    let mut interval = tokio::time::interval(Duration::from_secs(PING_CORE_PROCESS_INTERVAL_SECS));
    loop {
        interval.tick().await;
        // Errors (e.g. mid-restart) are logged by send; keep pinging.
        let _ = app.state::<AppData>().core.send(Message::Ping);
    }
}

/// Spawns core, connects to it, sends it the full startup configuration and makes it the
/// current connection. Blocking: call from a plain thread (or synchronous setup).
pub fn connect_core(app: &tauri::AppHandle) -> Result<(), CoreProcessCreationError> {
    log::info!("connect_core: Creating core process");
    let data = app.state::<AppData>();
    let generation = data
        .core_generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;
    let tmp_dir = std::env::temp_dir();
    let socket_name = format!("core-socket-{}", create_random_suffix());
    let socket_path = format!("{}/{socket_name}", tmp_dir.display());

    let (rx, _core_process) = start_sidecar(app, &socket_path);
    tauri::async_runtime::spawn(show_stdout(rx, app.clone(), generation));
    let (sender, event_socket) = create_core_process_socket(&socket_path)?;
    let client = socket_lib::client::Client::start(
        sender,
        event_socket,
        core_events::CoreEventHandler::new(app.clone()),
    );

    // Under the settings lock: nothing can change a setting between reading the config
    // and swapping the connection, so no update is lost or reaches core out of order.
    let settings = data.settings();
    for message in settings.core_startup_config() {
        client
            .send(message)
            .map_err(|_| CoreProcessCreationError::SendMessageFailed)?;
    }
    data.core.install(client);
    drop(settings);
    Ok(())
}

/// This is a workaround which we use in order to wake up the
/// webview window and process incoming ws messages, e.g. incoming
/// call request.
pub fn ping_frontend(app: AppHandle) {
    loop {
        let res = app.emit("ping", ());
        if let Err(e) = res {
            log::error!("Failed to emit ping: {e:?}");
            sentry_utils::upload_logs_event("Failed to emit ping".to_string());
        }
        std::thread::sleep(std::time::Duration::from_secs(PING_SLEEP_SECS));
    }
}

/// Returns the platform-specific log file path.
pub fn get_log_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir().map(|mut path| {
            path.push("Library/Logs/com.hopp.app/hopp.log");
            path
        })
    }
    #[cfg(target_os = "windows")]
    {
        dirs::data_local_dir().map(|mut path| {
            path.push("com.hopp.app/logs/hopp.log");
            path
        })
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        log::warn!("get_log_path: Unsupported target OS, returning None for log path.");
        None
    }
}

/// Determines the log level from environment variables.
pub fn get_log_level() -> LevelFilter {
    let level = match env::var("RUST_LOG")
        .unwrap_or_else(|_| "info".to_string())
        .as_str()
    {
        "debug" => LevelFilter::Debug,
        "info" => LevelFilter::Info,
        "warn" => LevelFilter::Warn,
        "error" => LevelFilter::Error,
        _ => LevelFilter::Info,
    };
    let level_value = env::var("LOG_LEVEL").unwrap_or_else(|_| level.to_string());
    env::set_var(
        "RUST_LOG",
        // livekit/livekit_api at warn surface signal connect retries, reconnects and join
        // failures, which are otherwise invisible when a call can't connect.
        format!(
            "hopp_core={level_value},sentry_utils={level_value},socket_lib={level_value},livekit=warn,livekit_api=warn"
        ),
    );
    level
}

/// Centers the window relative to the tray icon position with multi-monitor support.
#[cfg(target_os = "macos")]
pub fn center_window_on_tray(window: &WebviewWindow, tray_rect: Rect, show_window: bool) {
    log::info!("center_window_on_tray: tray_rect: {tray_rect:?}, show_window: {show_window:?}");

    /*
     * Because centering the window using the move_window function is
     * broken we have to calculate the position of the window manually.
     * See https://github.com/tauri-apps/tauri/issues/7139.
     * First we find in which monitor the tray icon is located and then we store
     * the scale. Then we calculate the size of the window by checking the
     * scale and comparing the width with the expected hardcoded value defined
     * in the tauri.conf.json file. This is needed because when we the tray icon
     * is clicked from a different monitor the window size keeps the scale from the
     * previous one and this can cause wrong calculations.
     * We are setting logical position because the physical position is not
     * working as expected, probably for the same reason as the window size.
     */
    let mut scale = 1.0;
    /* The tray rect position is in physical units. Its top edge can sit just
     * outside the monitor (for example y == -2 on an external display), so
     * use the icon center for monitor detection. */
    let tray_pos: PhysicalPosition<i32> = tray_rect.position.to_physical(1.0);
    let tray_size_at_1x: PhysicalSize<f64> = tray_rect.size.to_physical(1.0);
    let tray_center = PhysicalPosition::new(
        tray_pos.x + (tray_size_at_1x.width / 2.0) as i32,
        tray_pos.y + (tray_size_at_1x.height / 2.0) as i32,
    );
    let mut found_monitor = false;
    let monitors = window.available_monitors();
    if let Ok(monitors) = monitors {
        for monitor in monitors {
            let monitor_pos = monitor.position();
            let monitor_size = monitor.size();
            let x_offset = tray_center.x - monitor_pos.x;
            let y_offset = tray_center.y - monitor_pos.y;
            if (x_offset >= 0)
                && (x_offset <= (monitor_size.width as i32))
                && (y_offset >= 0)
                && (y_offset <= (monitor_size.height as i32))
            {
                log::info!("center_window_on_tray: Found monitor: {monitor:?}");
                scale = monitor.scale_factor();
                found_monitor = true;
                break;
            }
        }
    } else {
        log::warn!("center_window_on_tray: Available monitors errored scale to 1.0");
    }

    if !found_monitor {
        log::warn!(
            "center_window_on_tray: Tray center {tray_center:?} is outside all monitors, skipping"
        );
        return;
    }

    let tray_size: PhysicalSize<f64> = tray_rect.size.to_physical(scale);
    let mut window_size = match window.outer_size() {
        Ok(size) => size,
        Err(e) => {
            log::error!("center_window_on_tray: Failed to get window outer size: {e:?}");
            return;
        }
    };
    if scale > 1.0 && window_size.width < 800 {
        window_size = PhysicalSize::new(
            ((window_size.width as f64) * scale) as u32,
            ((window_size.height as f64) * scale) as u32,
        );
    } else if scale == 1.0 && window_size.width >= 800 {
        // TODO: Here we hardcode the size if the size changes
        // we should change this as well.
        window_size = PhysicalSize::new(400, 500);
    }
    // While sharing, the item also holds the draw and stop buttons left of the Hopp icon;
    // center on the Hopp icon.
    let extra_width = if window
        .state::<AppData>()
        .tray_sharing_controls
        .load(Ordering::Relaxed)
    {
        tray::SHARING_CONTROLS_EXTRA_WIDTH * scale
    } else {
        0.0
    };
    let x = ((tray_pos.x as f64) + extra_width + (tray_size.width - extra_width) / 2.0
        - (window_size.width as f64) / 2.0)
        / scale;
    let y = (tray_pos.y as f64) / scale;

    let new_position = LogicalPosition::new(x, y);
    let _ = window.set_position(new_position);
    if show_window {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Creates the main window from its `tauri.conf.json` entry (`create: false` there).
/// - Menu bar style: the hidden borderless popover the config describes.
/// - Floating style: the same borderless fixed-size window, but a normal-level window that
///   is shown at once, at its saved position when enough of it is on screen.
/// - Regular style: a normal titled, resizable window, shown at once.
///
/// The frontend gets the style as a class on `<html>` (`floating-window` / `regular-window`,
/// see App.css), set before the first paint.
pub fn create_main_window(app: &App<Wry>) -> Result<(), Box<dyn std::error::Error>> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")
        .ok_or("main window missing from tauri.conf.json")?;
    #[allow(unused_mut)]
    let mut builder = WebviewWindowBuilder::from_config(app.handle(), config)?;
    #[cfg(target_os = "macos")]
    {
        let window_style = app.state::<AppData>().window_style;
        if window_style.is_regular() {
            // The main window UI has no dark variant, so the title bar stays light too.
            builder = builder
                .title("Hopp")
                .decorations(true)
                .title_bar_style(TitleBarStyle::Visible)
                .hidden_title(false)
                .transparent(false)
                .resizable(true)
                .maximizable(true)
                .min_inner_size(config.width, config.height)
                .theme(Some(tauri::Theme::Light))
                .initialization_script("document.documentElement.classList.add('regular-window');")
                .center();
        } else if window_style.is_floating() {
            builder = builder.title("Hopp").initialization_script(
                "document.documentElement.classList.add('floating-window');",
            );
            let saved = app
                .state::<AppData>()
                .settings()
                .app_state
                .main_window_position();
            let work_areas = monitor_work_areas(app.handle());
            builder = match app_state::restorable_window_position(
                saved,
                config.width,
                config.height,
                &work_areas,
            ) {
                Some(position) => builder.position(position.x, position.y),
                None => builder.center(),
            };
        }
        if window_style.has_dock_icon() {
            builder = builder
                .always_on_top(false)
                .skip_taskbar(false)
                .visible(true)
                .focused(true);
        }
        let window = builder.build()?;
        if window_style.is_floating() {
            // Borderless windows are not closable by default, so Cmd-W (the Window menu's
            // Close, `performClose:`) would only beep. Closable gives no visible button.
            let _ = window.set_closable(true);
        }
    }
    #[cfg(not(target_os = "macos"))]
    builder.build()?;
    Ok(())
}

/// Work areas of all monitors in logical (point) desktop coordinates.
#[cfg(target_os = "macos")]
fn monitor_work_areas(app: &AppHandle) -> Vec<app_state::LogicalRect> {
    match app.available_monitors() {
        Ok(monitors) => monitors
            .iter()
            .map(|monitor| {
                let scale = monitor.scale_factor();
                let area = monitor.work_area();
                app_state::LogicalRect {
                    x: area.position.x as f64 / scale,
                    y: area.position.y as f64 / scale,
                    width: area.size.width as f64 / scale,
                    height: area.size.height as f64 / scale,
                }
            })
            .collect(),
        Err(e) => {
            log::warn!("monitor_work_areas: failed to list monitors: {e:?}");
            Vec::new()
        }
    }
}

/// Saves the floating main window's position `delay` after a move, unless it moved again in
/// the meantime. `generation` counts moves; `scale_factor` is the window's.
#[cfg(target_os = "macos")]
pub fn save_main_window_position_debounced(
    app: &AppHandle,
    position: PhysicalPosition<i32>,
    scale_factor: f64,
    generation: Arc<std::sync::atomic::AtomicU64>,
    delay: Duration,
) {
    let logical = position.to_logical::<f64>(scale_factor);
    let this_move = generation.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(delay).await;
        if generation.load(Ordering::SeqCst) != this_move {
            return;
        }
        app.state::<AppData>()
            .settings()
            .app_state
            .set_main_window_position(app_state::WindowPosition {
                x: logical.x,
                y: logical.y,
            });
    });
}

/// Shows, restores and focuses `window`.
#[cfg(target_os = "macos")]
pub fn show_main_window(window: &WebviewWindow) {
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
}

/// Hides the Dock icon again (Accessory policy) once a temporary reason for it (a call, the
/// permissions or tray notification window) is gone. No-op in the floating and regular
/// window styles, where the Dock icon stays for the whole session.
#[cfg(target_os = "macos")]
pub fn restore_accessory_policy(app: &AppHandle) {
    let data = app.state::<AppData>();
    if data.window_style.has_dock_icon() {
        return;
    }
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    data.activation_policy_regular
        .store(false, Ordering::Relaxed);
}

/// Add a tray icon to the app on macos, on windows we don't use it. No tray icon is
/// created when the window style turns it off; in the floating and regular styles the
/// window is never positioned relative to the tray.
#[allow(unused_variables)]
pub fn setup_tray_icon(
    app: &mut App<Wry>,
    menu: &tauri::menu::Menu<Wry>,
    location_set: Arc<Mutex<bool>>,
    tray_clicked: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    {
        let window_style = app.state::<AppData>().window_style;
        if !window_style.show_menu_bar_icon {
            log::info!("setup_tray_icon: menu bar icon turned off");
            return Ok(());
        }
        let dock_style = window_style.has_dock_icon();
        let location_set_clone = location_set.clone();
        let app_handle = app.handle().clone();

        // Use dark icon as template - macOS will automatically invert for dark menu bars
        let mut builder = TrayIconBuilder::new()
            .menu(menu)
            .show_menu_on_left_click(false)
            .icon_as_template(true);

        if let Some(icon) = tray::load_tray_icon(&app_handle, tray::HOPP_ICON) {
            log::info!("setup_tray_icon: Using template icon tray-dark-default.png");
            builder = builder.icon(icon);
        } else if let Some(icon) = app.default_window_icon() {
            log::warn!("setup_tray_icon: Failed to load tray icon, using default window icon");
            builder = builder.icon(icon.clone());
        }

        let tray = builder
            .on_tray_icon_event(move |tray, event| {
                tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    position,
                    rect,
                    ..
                } = event
                {
                    tray_clicked.store(true, Ordering::Relaxed);
                    let tray_clicked_reset = tray_clicked.clone();
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                        tray_clicked_reset.store(false, Ordering::Relaxed);
                    });

                    let app_handle = tray.app_handle();
                    if tray::handle_sharing_controls_click(app_handle, position, rect) {
                        return;
                    }
                    if let Some(window) = app_handle.get_webview_window("main") {
                        if dock_style {
                            show_main_window(&window);
                            return;
                        }
                        match window.is_visible() {
                            Ok(true) => {
                                let _ = window.hide();
                            }
                            Ok(false) => {
                                if let Ok(mut location_set) = location_set.lock() {
                                    if !*location_set {
                                        *location_set = true;
                                    }
                                }
                                if let Ok(Some(rect)) = tray.rect() {
                                    center_window_on_tray(&window, rect, true);
                                }
                            }
                            Err(e) => log::error!(
                                "setup_tray_icon: Failed to check window visibility: {e:?}"
                            ),
                        }
                    }
                }
            })
            .on_menu_event(|app, event| {
                if event.id.as_ref() == "quit" {
                    log::info!("Quit menu item clicked");
                    app.exit(0);
                }
            })
            .build(app)?;

        // Store the tray state in AppData for dynamic icon updates (main thread only).
        let tray_state = tray::TrayState::new(tray.clone());
        *app.state::<AppData>()
            .tray_state
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(tray_state);

        if dock_style {
            return Ok(());
        }

        let app_handle = app.handle().clone();

        /*
         * Spawns an async task to manage window positioning relative to the tray icon.
         * This runs once during app initialization and continues indefinitely.
         *
         * Initially it waits for the OS to assign a valid tray icon position (y == 0 indicates
         * the menu bar). Polls every 100ms for up to 100 attempts. Once valid,
         * centers the window on the tray if it's not visible.
         *
         * After initial centering, it polls every 200ms to detect tray icon position changes
         * (e.g., when the user rearranges menu bar items). If the position changed and the window
         * is visible, re-centers it to follow the tray icon.
         * See: https://github.com/gethopp/hopp/issues/211
         */
        tauri::async_runtime::spawn(async move {
            let mut tray_rect = match tray.rect() {
                Ok(Some(rect)) => rect,
                _ => {
                    log::warn!("setup_tray_icon: Initial tray rect not available");
                    return;
                }
            };
            for _ in 0..100 {
                if tray_rect.position.to_physical::<i32>(1.0).y == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                if let Ok(Some(rect)) = tray.rect() {
                    tray_rect = rect;
                }
            }

            // Initial centering
            let mut last_pos = tray_rect.position.to_physical::<i32>(1.0);
            if let Some(window) = app_handle.get_webview_window("main") {
                match window.is_visible() {
                    Ok(false) => {
                        if let Ok(mut location_set) = location_set_clone.lock() {
                            if !*location_set {
                                *location_set = true;
                            }
                        }
                        center_window_on_tray(&window, tray_rect, false);
                    }
                    Ok(true) => {}
                    Err(e) => log::error!(
                        "setup_tray_icon: Failed to check window visibility in loop: {e:?}"
                    ),
                }
            }

            loop {
                tokio::time::sleep(Duration::from_millis(200)).await;

                if let Ok(Some(rect)) = tray.rect() {
                    let pos = rect.position.to_physical::<i32>(1.0);

                    if pos.x != last_pos.x || pos.y != last_pos.y {
                        last_pos = pos;
                        if let Some(window) = app_handle.get_webview_window("main") {
                            let is_visible = window.is_visible();

                            if let Ok(true) = is_visible {
                                center_window_on_tray(&window, rect, false);
                            }
                        }
                    }
                } else {
                    log::warn!("cannot pull tray rect");
                }
            }
        });
    }
    Ok(())
}

/// Setup start on launch.
#[allow(unused)]
pub fn setup_start_on_launch(manager: &AutoLaunchManager, first_run: bool) {
    // Only on macos call set_login_item
    #[cfg(all(target_os = "macos", not(debug_assertions)))]
    {
        let service = AppService::new(ServiceType::MainApp);
        let status = service.status();
        if status != ServiceStatus::Enabled && first_run {
            let res = service.register();
            if let Err(e) = res {
                log::error!("Failed to register app service: {:?}", e);
            }
        }
    }

    #[cfg(all(target_os = "windows", not(debug_assertions)))]
    {
        if first_run {
            let _ = manager.enable();
        }
    }
}

pub fn get_sentry_dsn() -> String {
    env!("SENTRY_DSN_RUST").to_string()
}

#[cfg(target_os = "macos")]
pub fn set_window_corner_radius_and_decorations(
    window: &tauri::WebviewWindow,
    radius: f64,
    decorations: bool,
) {
    use objc2_app_kit::NSWindowButton;

    let ns_window: &objc2_app_kit::NSWindow = match window.ns_window() {
        Ok(ns_window) => unsafe { &*ns_window.cast() },
        Err(e) => {
            log::error!("set_window_corner_radius: Failed to get NSWindow: {e:?}");
            return;
        }
    };

    if !decorations {
        if let Some(button) = ns_window.standardWindowButton(NSWindowButton::CloseButton) {
            button.setHidden(true);
        }
        if let Some(button) = ns_window.standardWindowButton(NSWindowButton::MiniaturizeButton) {
            button.setHidden(true);
        }
        if let Some(button) = ns_window.standardWindowButton(NSWindowButton::ZoomButton) {
            button.setHidden(true);
        }
    }

    let ns_view = match ns_window.contentView() {
        Some(view) => view,
        None => {
            log::error!("set_window_corner_radius: Failed to get NSView");
            return;
        }
    };
    ns_view.setWantsLayer(true);

    if let Some(layer) = ns_view.layer() {
        layer.setCornerRadius(radius);
        layer.setMasksToBounds(true);
    }
}

#[cfg(target_os = "macos")]
pub fn disable_app_nap() {
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

    let process_info = NSProcessInfo::processInfo();
    let reason = NSString::from_str("Avoid WebKit throttling for uninterrupted operation");

    let activity = process_info.beginActivityWithOptions_reason(
        NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
        &reason,
    );

    // Leak the activity token so App Nap stays disabled for the lifetime of the process.
    std::mem::forget(activity);

    log::info!("App Nap disabled");
}

fn create_random_suffix() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(10)
        .map(char::from)
        .collect()
}

pub struct MediaWindowConfig<'a> {
    pub label: &'a str,
    pub title: &'a str,
    pub url: &'a str,
    pub width: f64,
    pub height: f64,
    pub resizable: bool,
    pub always_on_top: bool,
    pub content_protected: bool,
    pub maximizable: bool,
    pub minimizable: bool,
    pub decorations: bool,
    pub transparent: bool,
    pub background_color: Option<tauri::webview::Color>,
}

pub fn create_media_window(app: &AppHandle, config: MediaWindowConfig<'_>) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(config.label) {
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }

    #[allow(unused_mut)]
    let mut window_builder =
        WebviewWindowBuilder::new(app, config.label, WebviewUrl::App(config.url.into()))
            .title(config.title)
            .inner_size(config.width, config.height)
            .resizable(config.resizable)
            .visible(false)
            .transparent(config.transparent)
            .shadow(true)
            .always_on_top(config.always_on_top)
            .maximizable(config.maximizable)
            .minimizable(config.minimizable)
            .content_protected(config.content_protected);

    if let Some(bg_color) = config.background_color {
        window_builder = window_builder.background_color(bg_color);
    }

    #[cfg(target_os = "macos")]
    {
        window_builder = window_builder.hidden_title(true);
        window_builder = window_builder.title_bar_style(TitleBarStyle::Overlay);
    }

    #[cfg(target_os = "windows")]
    {
        window_builder = window_builder.decorations(config.decorations);
    }

    let window = window_builder
        .build()
        .map_err(|e| format!("Failed to create {} window: {}", config.label, e))?;

    let window_clone = window.clone();
    let label_clone = config.label.to_string();

    window
        .run_on_main_thread(move || {
            #[cfg(target_os = "macos")]
            {
                set_window_corner_radius_and_decorations(
                    &window_clone,
                    CORNER_RADIUS,
                    config.decorations,
                );
            }

            #[cfg(target_os = "windows")]
            {
                use window_vibrancy::apply_blur;

                if let Err(e) = apply_blur(&window_clone, Some((18, 18, 18, 125))) {
                    log::warn!("Failed to apply blur to {} window: {}", label_clone, e);
                }
            }

            if let Err(e) = window_clone.show() {
                log::error!("Failed to show {} window: {}", label_clone, e);
            }

            if let Err(e) = window_clone.set_focus() {
                log::error!("Failed to focus {} window: {}", label_clone, e);
            }
        })
        .map_err(|e| format!("Failed to run on main thread: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::RestartBackoff;
    use std::time::{Duration, Instant};

    #[test]
    fn restart_backoff_doubles_then_gives_up_within_the_window() {
        let mut backoff = RestartBackoff::default();
        let start = Instant::now();
        assert_eq!(backoff.next_delay(start), Some(Duration::from_secs(1)));
        assert_eq!(backoff.next_delay(start), Some(Duration::from_secs(2)));
        assert_eq!(backoff.next_delay(start), Some(Duration::from_secs(4)));
        assert_eq!(backoff.next_delay(start), None);
    }

    #[test]
    fn restart_backoff_forgets_restarts_outside_the_window() {
        let mut backoff = RestartBackoff::default();
        let start = Instant::now();
        for _ in 0..RestartBackoff::MAX_RESTARTS {
            backoff.next_delay(start);
        }
        let later = start + RestartBackoff::WINDOW + Duration::from_secs(1);
        assert_eq!(backoff.next_delay(later), Some(Duration::from_secs(1)));
    }
}

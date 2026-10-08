// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use hopp::sounds::{self, SoundConfig};
use log::LevelFilter;
use socket_lib::{
    AudioCaptureMessage, AudioDevice, CallId, CameraDevice, Message, ScreenSharePickerMode,
    ScreenShareResolution, SentryMetadata,
};
use tauri::Manager;
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    path::BaseDirectory,
    Emitter,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

use tauri_plugin_log::{Target, TargetKind};

use hopp::{
    app_state::{AppState, AppVeilApplication, UserSettings, WindowStyle, WindowStyleSettings},
    application_catalog::InstalledApplication,
    call_state, connect_core,
    core_client::{CoreError, REQUEST_TIMEOUT},
    create_main_window, get_log_level, get_log_path, get_sentry_dsn, permissions, ping_core,
    ping_frontend, setup_start_on_launch, setup_tray_icon, AppData,
};
#[cfg(target_os = "macos")]
use hopp::{
    disable_app_nap, restore_accessory_policy, save_main_window_position_debounced,
    set_window_corner_radius_and_decorations, show_main_window, CORNER_RADIUS,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use std::{env, sync::Arc};

#[cfg(any(target_os = "windows", target_os = "linux"))]
use tauri::PhysicalPosition;

/// How long the floating main window must stay put before its position is saved.
#[cfg(target_os = "macos")]
const MAIN_WINDOW_POSITION_SAVE_DELAY: Duration = Duration::from_millis(500);

/// How long `show_main_window_when_placed` waits for the menu-bar popup's launch placement:
/// the placement's own wait for the tray icon's position (~10 s), plus a margin (15 s).
/// The placement sets its flag on every path, so this is only a safety net.
#[cfg(target_os = "macos")]
const MAIN_WINDOW_PLACED_TIMEOUT: Duration =
    hopp::TRAY_POSITION_MAX_WAIT.saturating_add(Duration::from_secs(5));

/// CallStart only dispatches the room connect in core, so its answer is quick.
const CALL_START_TIMEOUT: Duration = Duration::from_secs(10);

/*
 * Command rules: commands never hold a lock while waiting for core. Requests are
 * `async fn`s that enqueue through `AppData::core` and await the response with no lock
 * held; fire-and-forget commands only enqueue. Setters that persist a value and forward it
 * to core do both under the settings lock (see `hopp::Settings`).
 */

fn core_send(app: &tauri::AppHandle, message: Message) {
    let _ = app.state::<AppData>().core.send(message);
}

fn core_error_message(error: CoreError) -> String {
    match error {
        CoreError::Timeout => "Failed to receive message from hopp_core".to_string(),
        _ => "Failed to send message to hopp_core".to_string(),
    }
}

#[tauri::command(async)]
fn open_stats_window(app: tauri::AppHandle) {
    log::info!("open_stats_window");
    core_send(&app, Message::OpenStatsWindow);
}

#[tauri::command(async)]
fn stop_sharing(app: tauri::AppHandle) {
    log::info!("stop_sharing");
    core_send(&app, Message::StopScreenshare);
}

#[tauri::command(async)]
fn get_available_content(app: tauri::AppHandle) -> Result<(), String> {
    log::info!("get_available_content: open core screen selection");

    let data = app.state::<AppData>();
    let settings = data.settings();
    let remote_control_enabled = settings.app_state.user_settings().remote_control_enabled;
    data.core
        .send(Message::ControllerCursorEnabled(remote_control_enabled))
        .map_err(|_| "Failed to apply remote control setting".to_string())?;
    data.core
        .send(Message::GetAvailableContent)
        .map_err(|_| "Failed to start screen selection".to_string())
}

#[tauri::command(async)]
fn play_sound(app: tauri::AppHandle, sound_name: String) {
    log::info!("play_sound");
    let tmp_sound_name = sound_name.split("/").last();
    if let Some(tmp_sound_name) = tmp_sound_name {
        log::info!("Playing sound: {}", tmp_sound_name);
    }

    let sounds = hopp::sounds::get_all_sounds();
    let mut sound_path = "".to_string();
    let mut sound_config = SoundConfig::default();
    for sound in sounds {
        if sound.0.contains(&sound_name) {
            let resource_path = app.path().resolve(sound.0, BaseDirectory::Resource);
            if let Err(e) = resource_path {
                log::error!("play_sound: Failed to resolve sound path: {e:?}");
                return;
            }
            sound_path = resource_path.unwrap().to_string_lossy().to_string();
            sound_config = sound.1;
            break;
        }
    }
    if sound_path.is_empty() {
        log::error!("play_sound: Failed to find sound");
        return;
    }

    /*
     * Check-and-register atomically: if the sound is already playing (its playback
     * thread still holds the receiver) do nothing; drop entries whose playback ended.
     */
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let data = app.state::<AppData>();
        let mut entries = data.sound_entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|entry| entry.tx.send(sounds::SoundCommand::Ping).is_ok());
        if entries.iter().any(|entry| entry.name == sound_name) {
            log::warn!("play_sound: Sound is already playing");
            return;
        }
        entries.push(sounds::SoundEntry {
            name: sound_name,
            tx,
        });
    }

    // Playback blocks until the sound ends (or forever when looped): own thread.
    std::thread::spawn(move || {
        if let Err(e) = hopp::sounds::play_sound(sound_path, sound_config, rx) {
            log::error!("play_sound: Failed to play sound: {e:?}");
        }
    });
}

#[tauri::command(async)]
fn stop_sound(app: tauri::AppHandle, sound_name: String) {
    log::info!("stop_sound");
    let tmp_sound_name = sound_name.split("/").last();
    if let Some(tmp_sound_name) = tmp_sound_name {
        log::info!("Stopping sound: {}", tmp_sound_name);
    }
    let data = app.state::<AppData>();
    let mut entries = data.sound_entries.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(index) = entries.iter().position(|entry| entry.name == sound_name) {
        let _ = entries[index].tx.send(sounds::SoundCommand::Stop);
        entries.remove(index);
    }
    log::debug!("stop_sound: entries left: {}", entries.len());
}

/// Ends call `call_id` in Tauri and core (`None`: whatever call is current). Core's
/// CallEnd also stops screen sharing, so a StopScreenshare is not needed first.
#[tauri::command(async)]
fn reset_core_process(app: tauri::AppHandle, call_id: Option<CallId>) {
    log::info!("reset_core_process: call_id={call_id:?}");
    call_state::end_call_from_ui(&app, call_id);
}

#[tauri::command(async)]
fn store_token_cmd(app: tauri::AppHandle, token: String) {
    log::info!("store_token_cmd");
    app.state::<AppData>()
        .settings()
        .app_state
        .set_user_jwt(Some(token.clone()));

    if let Err(e) = app.emit("token_changed", token) {
        log::error!("Failed to emit token_changed event: {e:?}");
    }
}

#[tauri::command(async)]
fn get_stored_token(app: tauri::AppHandle) -> Option<String> {
    log::info!("get_stored_token");
    let token = app.state::<AppData>().settings().app_state.user_jwt();
    log::debug!("get_stored_token: {token:?}");
    token
}

#[tauri::command(async)]
fn delete_stored_token(app: tauri::AppHandle) {
    log::info!("Deleting stored token");
    app.state::<AppData>()
        .settings()
        .app_state
        .set_user_jwt(None);

    if let Err(e) = app.emit("token_changed", "".to_string()) {
        log::error!("Failed to emit token_changed event: {e:?}");
    }
}

#[tauri::command(async)]
fn get_logs(_app: tauri::AppHandle) -> String {
    log::info!("get_logs:");
    let log_file = get_log_path();
    if let Some(path) = log_file {
        path.to_string_lossy().to_string()
    } else {
        log::error!("Failed to get log path");
        "".to_string()
    }
}

/// Reveals the app state file (settings, login token, ...) in the platform file
/// manager, or its folder when the file does not exist.
#[tauri::command(async)]
fn reveal_settings_file(app: tauri::AppHandle) -> Result<(), String> {
    let file = app.state::<AppData>().settings().app_state.file_path();
    let target = match file.parent() {
        Some(folder) if !file.exists() => folder,
        _ => file.as_path(),
    };
    log::info!("reveal_settings_file: {}", target.display());
    tauri_plugin_opener::reveal_item_in_dir(target).map_err(|e| {
        log::error!("Failed to reveal {}: {e}", target.display());
        format!("Failed to reveal {}: {e}", target.display())
    })
}

#[tauri::command(async)]
fn set_deactivate_hiding(app: tauri::AppHandle, deactivate: bool) {
    log::debug!("set_deactivate_hiding: {deactivate}");
    let data = app.state::<AppData>();
    *data
        .deactivate_hiding
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = deactivate;
}

#[tauri::command(async)]
fn set_controller_cursor(app: tauri::AppHandle, enabled: bool) {
    log::info!("set_controller_cursor: {enabled}");
    core_send(&app, Message::ControllerCursorEnabled(enabled));
}

#[tauri::command(async)]
fn open_accessibility_settings(_app: tauri::AppHandle) {
    log::info!("open_accessibility_settings");
    let mut process = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn()
        .expect("Failed to open System Preferences for Accessibility permissions");
    let _ = process.wait();
}

#[tauri::command(async)]
fn open_microphone_settings(_app: tauri::AppHandle) {
    log::info!("open_microphone_settings");
    permissions::request_microphone();
}

#[tauri::command(async)]
fn open_camera_settings(_app: tauri::AppHandle) {
    log::info!("open_camera_settings");
    permissions::request_camera();
}

#[tauri::command(async)]
fn open_screenshare_settings(_app: tauri::AppHandle) {
    log::info!("open_screenshare_settings");
    let mut process = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        .spawn()
        .expect("Failed to open System Preferences for Screen Capture permissions");
    let _ = process.wait();
}

#[tauri::command(async)]
async fn trigger_screenshare_permission(_app: tauri::AppHandle) -> bool {
    log::info!("trigger_screenshare_permission");
    permissions::request_screenshare()
}

#[tauri::command(async)]
fn get_control_permission(_app: tauri::AppHandle) -> bool {
    let res = permissions::accessibility();
    log::info!("get_control_permission: {res}");
    res
}

#[tauri::command(async)]
fn get_microphone_permission(_app: tauri::AppHandle) -> bool {
    let res = permissions::microphone();
    log::info!("get_microphone_permission: {res}");
    res
}

#[tauri::command(async)]
fn get_screenshare_permission(_app: tauri::AppHandle) -> bool {
    let res = permissions::screenshare();
    log::info!("get_screenshare_permission: {res}");
    res
}

#[tauri::command(async)]
fn get_camera_permission(_app: tauri::AppHandle) -> bool {
    let res = permissions::camera();
    log::info!("get_camera_permission: {res}");
    res
}

#[tauri::command(async)]
fn skip_tray_notification_selection_window(app: tauri::AppHandle) {
    log::info!("executing skip_tray_notification_selection_window");
    app.state::<AppData>()
        .settings()
        .app_state
        .set_tray_notification(false);
}

#[tauri::command(async)]
fn get_last_used_mic(app: tauri::AppHandle) -> Option<String> {
    log::info!("get_last_used_mic");
    let value = app.state::<AppData>().settings().app_state.last_used_mic();
    log::info!("get_last_used_mic: {value:?}");
    value
}

#[tauri::command(async)]
fn set_last_used_mic(app: tauri::AppHandle, mic: String) {
    log::info!("set_last_used_mic: {mic}");
    app.state::<AppData>()
        .settings()
        .app_state
        .set_last_used_mic(mic);
}

#[tauri::command(async)]
fn get_last_used_camera(app: tauri::AppHandle) -> Option<String> {
    log::info!("get_last_used_camera");
    let value = app
        .state::<AppData>()
        .settings()
        .app_state
        .last_used_camera();
    log::info!("get_last_used_camera: {value:?}");
    value
}

#[tauri::command(async)]
fn set_last_used_camera(app: tauri::AppHandle, camera: String) {
    log::info!("set_last_used_camera: {camera}");
    let data = app.state::<AppData>();
    let mut settings = data.settings();
    settings.app_state.set_last_used_camera(camera.clone());
    let _ = data.core.send(Message::SetPreferredCamera(Some(camera)));
}

#[tauri::command(async)]
fn get_favorite_teammates(app: tauri::AppHandle) -> Vec<String> {
    log::info!("get_favorite_teammates");
    app.state::<AppData>()
        .settings()
        .app_state
        .favorite_teammates()
}

#[tauri::command(async)]
fn set_favorite_teammate(
    app: tauri::AppHandle,
    user_id: String,
    favorite: bool,
) -> Result<(), String> {
    app.state::<AppData>()
        .settings()
        .app_state
        .set_favorite_teammate(user_id, favorite)
}

#[tauri::command(async)]
fn retain_favorite_teammates(app: tauri::AppHandle, known_ids: Vec<String>) -> Result<(), String> {
    app.state::<AppData>()
        .settings()
        .app_state
        .retain_favorite_teammates(&known_ids)
}

#[tauri::command(async)]
fn get_sharer_draw_persist(app: tauri::AppHandle) -> bool {
    log::info!("get_sharer_draw_persist");
    let value = app
        .state::<AppData>()
        .settings()
        .app_state
        .sharer_draw_persist();
    log::info!("get_sharer_draw_persist: {value}");
    value
}

#[tauri::command(async)]
fn set_sharer_draw_persist(app: tauri::AppHandle, persist: bool) {
    log::info!("set_sharer_draw_persist: {persist}");
    let data = app.state::<AppData>();
    let mut settings = data.settings();
    settings.app_state.set_sharer_draw_persist(persist);
    if data.drawing_enabled.load(Ordering::Relaxed) {
        let _ = data.core.send(Message::SharerDrawPersistChanged(persist));
    }
}

#[tauri::command(async)]
fn get_drawing_hint_shown(app: tauri::AppHandle) -> bool {
    log::info!("get_drawing_hint_shown");
    let value = app
        .state::<AppData>()
        .settings()
        .app_state
        .drawing_hint_shown();
    log::info!("get_drawing_hint_shown: {value}");
    value
}

#[tauri::command(async)]
fn set_drawing_hint_shown(app: tauri::AppHandle, shown: bool) {
    log::info!("set_drawing_hint_shown: {shown}");
    app.state::<AppData>()
        .settings()
        .app_state
        .set_drawing_hint_shown(shown);
}

#[tauri::command(async)]
fn get_drawing_enabled(app: tauri::AppHandle) -> bool {
    log::info!("get_drawing_enabled");
    app.state::<AppData>()
        .drawing_enabled
        .load(Ordering::Relaxed)
}

#[tauri::command(async)]
fn set_drawing_enabled(app: tauri::AppHandle, enabled: bool, permanent: bool) {
    hopp::set_drawing_enabled(&app, enabled, permanent);
}

#[tauri::command(async)]
fn quit_app(app: tauri::AppHandle) {
    log::info!("quit_app");
    app.state::<AppData>()
        .core
        .send_before_exit(Message::CallEnd(None));
    app.exit(0);
}

#[tauri::command(async)]
fn minimize_main_window(app: tauri::AppHandle) {
    log::info!("minimize_main_window");
    if let Some(window) = app.get_webview_window("main") {
        if let Err(e) = window.minimize() {
            log::error!("Failed to minimize main window: {e:?}");
        }
    } else {
        log::error!("Main window not found");
    }
}

/// Shows the main window once at startup, after an in-app update relaunched the app.
#[tauri::command(async)]
async fn show_main_window_when_placed(app: tauri::AppHandle) -> Result<(), String> {
    log::info!("show_main_window_when_placed");
    #[cfg(target_os = "macos")]
    let result = hopp::show_main_window_when_placed(&app, MAIN_WINDOW_PLACED_TIMEOUT).await;
    #[cfg(not(target_os = "macos"))]
    let result = match app.get_webview_window("main") {
        Some(window) => {
            let _ = window.show();
            let _ = window.set_focus();
            Ok(())
        }
        None => Err("main window not found".to_string()),
    };
    if let Err(e) = &result {
        log::warn!("show_main_window_when_placed: {e}");
    }
    result
}

#[tauri::command(async)]
fn set_livekit_url(app: tauri::AppHandle, url: String) {
    log::info!("set_livekit_url");
    let data = app.state::<AppData>();
    // Same lock as a core restart, which re-sends the URL and swaps the connection.
    let mut settings = data.settings();
    if settings.livekit_server_url != url {
        settings.livekit_server_url = url.clone();
        let _ = data.core.send(Message::LivekitServerUrl(url));
    }
}

#[tauri::command(async)]
fn get_livekit_url(app: tauri::AppHandle) -> String {
    log::info!("get_livekit_url");
    app.state::<AppData>().settings().livekit_server_url.clone()
}

#[tauri::command(async)]
fn set_sentry_metadata(app: tauri::AppHandle, user_id: String, app_version: String) {
    log::info!("set_sentry_metadata");
    sentry_utils::init_metadata(user_id.clone(), app_version.clone());
    let data = app.state::<AppData>();
    let metadata = SentryMetadata {
        user_id,
        app_version,
    };
    let mut settings = data.settings();
    settings.sentry_metadata = Some(metadata.clone());
    let _ = data.core.send(Message::SentryMetadata(metadata));
}

fn resolve_audio_device(last_used: Option<String>, devices: Option<&[AudioDevice]>) -> String {
    // Device list unavailable (core slow, e.g. still tearing down the last call): trust the
    // persisted last-used mic rather than falling back to the system default.
    let Some(devices) = devices else {
        return last_used.unwrap_or_default();
    };
    // Resolve the audio device name: last used → default → first → ""
    if let Some(last) = last_used {
        if devices.iter().any(|d| d.name == last) {
            return last;
        }
    }
    devices
        .iter()
        .find(|d| d.default)
        .or_else(|| devices.first())
        .map(|d| d.name.clone())
        .unwrap_or_default()
}

/// Starts call `call_id` in core. Call-state effects (shortcuts, dock icon) are applied by
/// the core dispatcher when CallStartResult arrives, not here, so they are ordered with
/// CallEnded events.
#[tauri::command]
async fn call_started(
    app: tauri::AppHandle,
    call_id: CallId,
    audio_token: String,
    video_token: String,
    source: Option<String>,
) -> Result<(), String> {
    log::info!(
        "call_started: call_id={call_id} source={}",
        source.as_deref().unwrap_or("unknown")
    );
    call_state::begin_call(&app, call_id);
    let data = app.state::<AppData>();
    let (user_settings, last_used_mic) = {
        let settings = data.settings();
        (
            settings.app_state.user_settings(),
            settings.app_state.last_used_mic(),
        )
    };
    let devices = match data
        .core
        .request(Message::ListAudioDevices, REQUEST_TIMEOUT)
        .await
    {
        Ok(Message::AudioDeviceList(devices)) => Some(devices),
        Ok(other) => {
            log::error!("call_started: unexpected response to ListAudioDevices: {other:?}");
            None
        }
        Err(e) => {
            log::error!("call_started: failed to list audio devices: {e}");
            None
        }
    };
    let audio_device_name = resolve_audio_device(last_used_mic, devices.as_deref());
    log::info!("call_started: resolved audio_device_name={audio_device_name:?}");

    let pending = call_state::start_call(
        &app,
        call_id,
        Message::CallStart(socket_lib::CallStartMessage {
            call_id,
            audio_token,
            video_token,
            audio_device_name,
            start_mic_on_call: Some(user_settings.start_mic_on_call),
            start_camera_on_call: Some(user_settings.start_camera_on_call),
        }),
    )?;
    match pending.wait(CALL_START_TIMEOUT).await {
        Ok(Message::CallStartResult(result)) => result.result,
        Ok(other) => {
            log::error!("call_started: unexpected response: {other:?}");
            Err(core_error_message(CoreError::UnexpectedResponse))
        }
        Err(e) => {
            log::error!("call_started: recv failed: {e}");
            Err(core_error_message(e))
        }
    }
}

/// When enabled=true, shows the notification variant of the icon.
/// When enabled=false, shows the default variant.
///
/// NOTE: must NOT be `async`. The macOS implementation manipulates AppKit/CALayer,
/// which has to run on the main thread; an async command would run off-thread and
/// the notification dot would silently never update. It only takes the main-thread-only
/// tray lock.
#[tauri::command]
fn set_tray_notification(app: tauri::AppHandle, enabled: bool) {
    log::info!("set_tray_notification: enabled={}", enabled);
    let data = app.state::<AppData>();
    let mut tray_state = data.tray_state.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(ref mut tray) = *tray_state {
        tray.set_notification_enabled(enabled);
    }
}

#[tauri::command(async)]
fn get_hopp_server_url(app: tauri::AppHandle) -> Option<String> {
    log::info!("get_hopp_server_url");
    let url = app
        .state::<AppData>()
        .settings()
        .app_state
        .user_settings()
        .hopp_server_url;
    log::debug!("get_hopp_server_url: {url:?}");
    url
}

#[tauri::command(async)]
fn set_hopp_server_url(app: tauri::AppHandle, url: Option<String>) {
    log::info!("set_hopp_server_url: {url:?}");
    app.state::<AppData>()
        .settings()
        .app_state
        .update_user_setting(|s| s.hopp_server_url = url.clone());
    let _ = app.emit("hopp_server_url_changed", &url);
}

#[tauri::command(async)]
async fn create_feedback_window(
    app: tauri::AppHandle,
    team_id: String,
    room_id: String,
    participant_id: String,
) -> Result<(), String> {
    log::info!("create_feedback_window");

    let url = format!(
        "feedback.html?teamId={}&roomId={}&participantId={}",
        team_id, room_id, participant_id
    );
    hopp::create_media_window(
        &app,
        hopp::MediaWindowConfig {
            label: "feedback",
            title: "Call Feedback",
            url: &url,
            width: 500.0,
            height: 420.0,
            resizable: false,
            always_on_top: true,
            content_protected: false,
            maximizable: false,
            minimizable: false,
            decorations: true,
            transparent: false,
            background_color: Some(tauri::webview::Color(0, 0, 0, 0)),
        },
    )
}

#[tauri::command(async)]
async fn create_settings_window(app: tauri::AppHandle) -> Result<(), String> {
    log::info!("create_settings_window");
    hopp::create_media_window(
        &app,
        hopp::MediaWindowConfig {
            label: "settings",
            title: "Settings",
            url: "settings.html",
            width: 800.0,
            height: 840.0,
            resizable: false,
            always_on_top: false,
            content_protected: false,
            maximizable: false,
            minimizable: true,
            decorations: true,
            transparent: false,
            background_color: None,
        },
    )
}

#[tauri::command(async)]
fn get_user_settings(app: tauri::AppHandle) -> UserSettings {
    log::info!("get_user_settings");
    let mut settings = app.state::<AppData>().settings().app_state.user_settings();
    settings.resolve_shortcuts();
    settings
}

#[tauri::command(async)]
fn list_installed_applications() -> Result<Vec<InstalledApplication>, String> {
    Ok(hopp::application_catalog::list_installed_applications())
}

#[tauri::command(async)]
fn set_app_veil_applications(
    app: tauri::AppHandle,
    applications: Vec<AppVeilApplication>,
) -> Result<(), String> {
    let data = app.state::<AppData>();
    let mut settings = data.settings();
    let enabled_bundle_ids = settings.app_state.set_app_veil_applications(applications)?;
    data.core
        .send(Message::SetAppVeilBundleIds(enabled_bundle_ids))
        .map_err(|error| {
            log::error!("set_app_veil_applications: failed to send settings: {error:?}");
            sentry_utils::simple_event(format!(
                "Failed to send App Veil settings to core: {error}"
            ));
            "App Veil was saved but could not be applied until Hopp restarts".to_string()
        })
}

fn set_shortcut(
    app: &tauri::AppHandle,
    accel: String,
    apply: impl FnOnce(&mut UserSettings, Option<String>),
) {
    let value = if accel.is_empty() { None } else { Some(accel) };
    app.state::<AppData>()
        .settings()
        .app_state
        .update_user_setting(|s| apply(s, value));
    call_state::refresh_call_shortcuts(app);
}

#[tauri::command(async)]
fn set_shortcut_toggle_mic(app: tauri::AppHandle, accel: String) {
    log::info!("set_shortcut_toggle_mic: {accel}");
    set_shortcut(&app, accel, |s, v| s.shortcut_toggle_mic = v);
}

#[tauri::command(async)]
fn set_shortcut_toggle_camera(app: tauri::AppHandle, accel: String) {
    log::info!("set_shortcut_toggle_camera: {accel}");
    set_shortcut(&app, accel, |s, v| s.shortcut_toggle_camera = v);
}

#[tauri::command(async)]
fn set_shortcut_toggle_screenshare(app: tauri::AppHandle, accel: String) {
    log::info!("set_shortcut_toggle_screenshare: {accel}");
    set_shortcut(&app, accel, |s, v| s.shortcut_toggle_screenshare = v);
}

#[tauri::command(async)]
fn set_shortcut_end_call(app: tauri::AppHandle, accel: String) {
    log::info!("set_shortcut_end_call: {accel}");
    set_shortcut(&app, accel, |s, v| s.shortcut_end_call = v);
}

#[tauri::command(async)]
fn set_is_camera_on(app: tauri::AppHandle, value: bool) {
    app.state::<AppData>()
        .is_camera_on
        .store(value, Ordering::Relaxed);
}

#[tauri::command(async)]
fn set_is_screensharing(app: tauri::AppHandle, value: bool) {
    app.state::<AppData>()
        .is_screensharing
        .store(value, Ordering::Relaxed);
}

fn update_user_setting(app: &tauri::AppHandle, f: impl FnOnce(&mut UserSettings)) {
    app.state::<AppData>()
        .settings()
        .app_state
        .update_user_setting(f);
}

/// Saves a user setting and forwards it to core in one critical section.
fn update_user_setting_and_send(
    app: &tauri::AppHandle,
    f: impl FnOnce(&mut UserSettings),
    message: Message,
) {
    let data = app.state::<AppData>();
    let mut settings = data.settings();
    settings.app_state.update_user_setting(f);
    let _ = data.core.send(message);
}

#[tauri::command(async)]
fn set_call_feedback_popup(app: tauri::AppHandle, enabled: bool) {
    log::info!("set_call_feedback_popup: {enabled}");
    update_user_setting(&app, |s| s.call_feedback_popup = enabled);
}

#[tauri::command(async)]
fn set_telemetry_enabled(app: tauri::AppHandle, enabled: bool) {
    log::info!("set_telemetry_enabled: {enabled}");
    sentry_utils::set_telemetry_enabled(enabled);
    update_user_setting_and_send(
        &app,
        |s| s.telemetry_enabled = enabled,
        Message::SetTelemetryEnabled(enabled),
    );
    let _ = app.emit("telemetry_enabled_changed", enabled);
}

#[tauri::command(async)]
fn set_show_dock_icon_in_call(app: tauri::AppHandle, enabled: bool) {
    log::info!("set_show_dock_icon_in_call: {enabled}");
    update_user_setting(&app, |s| s.show_dock_icon_in_call = enabled);
}

/// Takes effect on the next launch.
#[tauri::command(async)]
fn set_window_style(app: tauri::AppHandle, style: WindowStyle) {
    log::info!("set_window_style: {style:?}");
    update_user_setting(&app, |s| s.window_style = style);
}

/// Takes effect on the next launch.
#[tauri::command(async)]
fn set_show_menu_bar_icon(app: tauri::AppHandle, enabled: bool) {
    log::info!("set_show_menu_bar_icon: {enabled}");
    update_user_setting(&app, |s| s.show_menu_bar_icon = enabled);
}

/// The window style this session runs with (the saved settings may differ until a restart).
#[tauri::command(async)]
fn get_launch_window_style(app: tauri::AppHandle) -> WindowStyleSettings {
    app.state::<AppData>().window_style
}

#[tauri::command(async)]
fn set_show_menu_bar_sharing_buttons(app: tauri::AppHandle, enabled: bool) {
    log::info!("set_show_menu_bar_sharing_buttons: {enabled}");
    let data = app.state::<AppData>();
    // Save and post under the settings lock, in order with the snapshot handler.
    let mut settings = data.settings();
    settings
        .app_state
        .update_user_setting(|s| s.show_menu_bar_sharing_buttons = enabled);
    hopp::tray::update_sharing_controls(
        &app,
        enabled && data.is_screensharing.load(Ordering::Relaxed),
    );
    drop(settings);
}

#[tauri::command(async)]
fn set_auto_update_enabled(app: tauri::AppHandle, enabled: bool) {
    log::info!("set_auto_update_enabled: {enabled}");
    update_user_setting(&app, |s| s.auto_update_enabled = enabled);
}

#[tauri::command(async)]
fn set_start_camera_on_call(app: tauri::AppHandle, enabled: bool) {
    log::info!("set_start_camera_on_call: {enabled}");
    update_user_setting(&app, |s| s.start_camera_on_call = enabled);
}

#[tauri::command(async)]
fn set_start_mic_on_call(app: tauri::AppHandle, enabled: bool) {
    log::info!("set_start_mic_on_call: {enabled}");
    update_user_setting(&app, |s| s.start_mic_on_call = enabled);
}

#[tauri::command(async)]
fn set_remote_control_enabled(app: tauri::AppHandle, enabled: bool) {
    log::info!("set_remote_control_enabled: {enabled}");
    update_user_setting_and_send(
        &app,
        |s| s.remote_control_enabled = enabled,
        Message::ControllerCursorEnabled(enabled),
    );
}

#[tauri::command(async)]
fn mute_mic(app: tauri::AppHandle) {
    core_send(&app, Message::MuteAudio);
}

#[tauri::command(async)]
fn unmute_mic(app: tauri::AppHandle) {
    core_send(&app, Message::UnmuteAudio);
}

#[tauri::command(async)]
fn toggle_mic(app: tauri::AppHandle) {
    core_send(&app, Message::ToggleMic);
}

#[tauri::command(async)]
fn set_noise_cancellation(app: tauri::AppHandle, enabled: bool) {
    update_user_setting_and_send(
        &app,
        |settings| settings.noise_cancellation_enabled = enabled,
        Message::SetNoiseCancellation(enabled),
    );
}

#[tauri::command(async)]
fn set_screen_share_resolution(app: tauri::AppHandle, resolution: ScreenShareResolution) {
    log::info!("set_screen_share_resolution: {resolution:?}");
    update_user_setting_and_send(
        &app,
        |settings| settings.screen_share_resolution = resolution,
        Message::SetScreenShareResolution(resolution),
    );
}

#[tauri::command(async)]
fn set_low_bandwidth_default(app: tauri::AppHandle, enabled: bool) {
    log::info!("set_low_bandwidth_default: {enabled}");
    update_user_setting_and_send(
        &app,
        |settings| settings.low_bandwidth_default = enabled,
        Message::SetLowBandwidthDefault(enabled),
    );
}

#[tauri::command(async)]
fn set_call_low_bandwidth(app: tauri::AppHandle, enabled: bool) {
    log::info!("set_call_low_bandwidth: {enabled}");
    core_send(&app, Message::SetCallLowBandwidth(enabled));
}

#[tauri::command(async)]
fn set_screen_share_picker_mode(app: tauri::AppHandle, mode: ScreenSharePickerMode) {
    log::info!("set_screen_share_picker_mode: {mode:?}");
    update_user_setting_and_send(
        &app,
        |settings| settings.screen_share_picker_mode = mode,
        Message::SetScreenSharePickerMode(mode),
    );
}

#[tauri::command]
async fn start_camera(app: tauri::AppHandle, device_name: Option<String>) -> Result<(), String> {
    let data = app.state::<AppData>();
    // Resolve the preferred camera here so core never has to ask back.
    let device_name = device_name.or_else(|| data.settings().app_state.last_used_camera());
    let response = data
        .core
        .request(
            Message::StartCamera(socket_lib::CameraStartMessage { device_name }),
            REQUEST_TIMEOUT,
        )
        .await;
    match response {
        Ok(Message::StartCameraResult(result)) => result,
        Ok(other) => {
            log::error!("start_camera: unexpected response: {other:?}");
            Err(core_error_message(CoreError::UnexpectedResponse))
        }
        Err(e) => {
            log::error!("start_camera: {e}");
            Err(core_error_message(e))
        }
    }
}

#[tauri::command(async)]
fn stop_camera(app: tauri::AppHandle) {
    core_send(&app, Message::StopCamera);
}

#[tauri::command(async)]
fn open_camera_preview(app: tauri::AppHandle) {
    core_send(&app, Message::OpenCamera);
}

#[tauri::command(async)]
fn open_screenshare_viewer(app: tauri::AppHandle) {
    core_send(&app, Message::OpenScreenShareWindow);
}

#[tauri::command(async)]
fn close_screenshare_viewer(app: tauri::AppHandle) {
    core_send(&app, Message::CloseScreenShareWindow);
}

#[tauri::command]
async fn list_microphones(app: tauri::AppHandle) -> Vec<AudioDevice> {
    let response = app
        .state::<AppData>()
        .core
        .request(Message::ListAudioDevices, REQUEST_TIMEOUT)
        .await;
    match response {
        Ok(Message::AudioDeviceList(devices)) => devices,
        Ok(other) => {
            log::error!("list_microphones: unexpected response: {other:?}");
            vec![]
        }
        Err(e) => {
            log::error!("list_microphones: {e}");
            vec![]
        }
    }
}

#[tauri::command]
async fn select_microphone(app: tauri::AppHandle, device_name: String) {
    let response = app
        .state::<AppData>()
        .core
        .request(
            Message::StartAudioCapture(AudioCaptureMessage { device_name }),
            REQUEST_TIMEOUT,
        )
        .await;
    match response {
        Ok(Message::StartAudioCaptureResult(Ok(()))) => {}
        Ok(Message::StartAudioCaptureResult(Err(e))) => {
            log::error!("select_microphone: core failed: {e}")
        }
        Ok(other) => log::error!("select_microphone: unexpected response: {other:?}"),
        Err(e) => log::error!("select_microphone: no result: {e}"),
    }
}

#[tauri::command]
async fn list_webcams(app: tauri::AppHandle) -> Vec<CameraDevice> {
    let response = app
        .state::<AppData>()
        .core
        .request(Message::ListCameras, REQUEST_TIMEOUT)
        .await;
    match response {
        Ok(Message::CameraList(devices)) => devices,
        Ok(other) => {
            log::error!("list_webcams: unexpected response: {other:?}");
            vec![]
        }
        Err(e) => {
            log::error!("list_webcams: {e}");
            vec![]
        }
    }
}

#[tauri::command]
async fn bring_windows_to_front(app: tauri::AppHandle) -> bool {
    log::info!("bring_windows_to_front");
    let response = app
        .state::<AppData>()
        .core
        .request(Message::BringWindowsToFront, REQUEST_TIMEOUT)
        .await;
    match response {
        Ok(Message::BringWindowsToFrontResult(focused)) => focused,
        Ok(other) => {
            log::error!("bring_windows_to_front: unexpected response: {other:?}");
            false
        }
        Err(e) => {
            log::error!("bring_windows_to_front: {e}");
            false
        }
    }
}

#[tauri::command(async)]
fn toggle_call_sleep_prevention(app: tauri::AppHandle, enabled: bool) {
    #[cfg(target_os = "macos")]
    {
        let data = app.state::<AppData>();
        let mut sleep_prevention = data
            .sleep_prevention
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if enabled {
            sleep_prevention.enable();
        } else {
            sleep_prevention.disable();
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, enabled);
    }
}

fn main() {
    let _guard = sentry_utils::init_sentry("Tauri backend".to_string(), Some(get_sentry_dsn()));

    /*
     * Flag for disabling hiding the window on focus lost.
     * This is used to prevent the window from hiding when the user is writing feedback.
     */
    let deactivate_hiding = Arc::new(Mutex::new(false));
    let deactivate_hiding_clone = deactivate_hiding.clone();

    /*
     * Flag for disabling hiding the window on focus lost.
     * This is used to prevent the window from hiding when the user uses Raycast/Spotlight
     * to open the app again.
     */
    let reopen_requested = Arc::new(Mutex::new(false));
    #[allow(unused_variables)]
    let reopen_requested_clone = reopen_requested.clone();

    /* This is used to guard against showing the main window if the location is not set. */
    #[allow(unused_variables)]
    let location_set = Arc::new(Mutex::new(false));
    #[allow(unused_variables)]
    let location_set_clone = location_set.clone();
    #[allow(unused_variables)]
    let location_set_setup = location_set.clone();

    /* Flag set during tray icon clicks to suppress spurious activation events. */
    let tray_clicked = Arc::new(AtomicBool::new(false));

    /* Flag to suppress main window hide when activation policy switches to Accessory after a call ends. */
    let suppress_hide_on_call_end = Arc::new(AtomicBool::new(false));
    let suppress_hide_on_call_end_clone = suppress_hide_on_call_end.clone();

    /* Counts main window moves, to save the floating window position once it settles. */
    #[cfg(target_os = "macos")]
    let main_window_moves = Arc::new(std::sync::atomic::AtomicU64::new(0));

    let log_level = get_log_level();
    let mut app = tauri::Builder::default().plugin(tauri_plugin_opener::init());
    if !cfg!(debug_assertions) {
        app = app.plugin(tauri_plugin_single_instance::init(
            move |app, _args, _cwd| {
                log::info!("Reopening the app, single instance handler");
                log::debug!("app {app:?}");
                #[cfg(target_os = "macos")]
                {
                    let location_set = location_set_clone.lock().unwrap();
                    if !*location_set {
                        log::info!("Location not set, don't show the main window");
                        return;
                    }

                    let main_window = app.get_webview_window("main");
                    if let Some(window) = main_window {
                        log::info!("Single instance handler: showing main window");
                        show_main_window(&window);
                    } else {
                        log::error!("Main window not found");
                    }
                }
            },
        ));
    }
    let log_file_name = if cfg!(debug_assertions) {
        Some("debug".to_string())
    } else {
        None
    };
    let app = app
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_positioner::init())
        .plugin(
            tauri_plugin_log::Builder::default()
                .targets([
                    Target::new(TargetKind::LogDir {
                        file_name: log_file_name,
                    }),
                    Target::new(TargetKind::Stdout),
                    Target::new(TargetKind::Webview),
                ])
                .level(LevelFilter::Warn)
                .level_for("hopp", log_level)
                .level_for("sentry_utils", log_level)
                .max_file_size(50 * 1024 * 1024) // We are emptying them on startup
                .build(),
        )
        .setup(move |app| {
            /* Create the app_data_dir if it doesn't exist. */
            let app_data_dir = app
                .path()
                .app_data_dir()
                .expect("Failed to get app data dir.");
            if !app_data_dir.exists() {
                if let Err(e) = std::fs::create_dir_all(&app_data_dir) {
                    log::error!("Failed to create app data dir: {e:?}");
                }
            }

            let app_state = AppState::new(&app_data_dir);
            sentry_utils::set_telemetry_enabled(app_state.user_settings().telemetry_enabled);
            app.manage(AppData::new(
                deactivate_hiding_clone,
                app_state,
                suppress_hide_on_call_end.clone(),
            ));
            create_main_window(app)?;

            // Spawns core, connects, sends the full startup configuration (same code path as
            // a restart) and installs the connection. Core events are handled on the
            // connection's dispatcher thread.
            connect_core(app.handle()).expect("Failed to create core process");
            tauri::async_runtime::spawn(ping_core(app.handle().clone()));

            std::thread::spawn(|| {
                sentry_utils::upload_latest_crash();
            });

            let quit = MenuItemBuilder::new("Quit")
                .id("quit")
                .accelerator("Cmd+Q")
                .build(app)?;
            let menu = MenuBuilder::new(app).items(&[&quit]).build()?;

            setup_tray_icon(app, &menu, location_set_setup.clone(), tray_clicked.clone())?;

            /* Clear app logs in the beginning of a session. */
            let dir = app.path().app_log_dir();
            if let Err(e) = dir {
                log::warn!("Failed to get app log dir: {e:?}");
            } else {
                let dir = dir.unwrap();
                let log_file = dir.join("hopp.log");
                if log_file.exists() {
                    if let Err(e) = std::fs::write(&log_file, "") {
                        log::warn!("Failed to clear log file: {e:?}");
                    }
                }
            }

            /*
             * We are sending a ping event to the frontend
             * to keep it alive.
             * TODO: do graceful shutdown on exit
             */
            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                ping_frontend(app_handle);
            });

            let first_run = app.state::<AppData>().settings().app_state.first_run();

            setup_start_on_launch(&app.autolaunch(), first_run);

            /* Set first run to false after checking the start on launch. */
            if first_run {
                app.state::<AppData>()
                    .settings()
                    .app_state
                    .set_first_run(false);
            }

            /* Main window configuration on windows */
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            {
                let handle = app.handle();
                if let Some(window) = handle.get_webview_window("main") {
                    let _ = window.set_shadow(false);
                    let _ = window.set_skip_taskbar(false);
                    /* Place window on the bottom right corner of the active display. */
                    let current_monitor = window.current_monitor();
                    if let Ok(Some(monitor)) = current_monitor {
                        let monitor_size = monitor.size();
                        let monitor_pos = monitor.position();
                        let window_size = window.inner_size().unwrap();
                        let base_offset = 20 * monitor.scale_factor() as u32;
                        let offset_y = (25. * monitor.scale_factor()) as u32 + base_offset;
                        let x = monitor_pos.x
                            + (monitor_size.width - window_size.width - base_offset) as i32;
                        let y = monitor_pos.y
                            + (monitor_size.height - window_size.height - offset_y) as i32;
                        let new_position = PhysicalPosition::new(x as f64, y as f64);
                        let _ = window.set_position(new_position);
                    }
                    let _ = window.set_always_on_top(false);
                    let _ = window.show();
                }
            }

            /* macOS specific setup */
            #[cfg(target_os = "macos")]
            {
                disable_app_nap();
                /*
                 * Menu bar style: start as Accessory, switch to Regular during calls or when
                 * permission windows are visible. Floating and regular styles: Regular for the
                 * whole session, and the main window (already shown) can be reopened right away.
                 */
                let dock_style = app.state::<AppData>().window_style.has_dock_icon();
                if dock_style {
                    app.set_activation_policy(tauri::ActivationPolicy::Regular);
                    *location_set_setup.lock().unwrap_or_else(|e| e.into_inner()) = true;
                } else {
                    app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                }

                /*
                 * Make the menubar popup a plain borderless window. Tauri gives it the
                 * miniaturizable and full-size-content-view style bits, and any style bit makes
                 * it report itself as a standard window. While we are Regular (in a call) tiling
                 * window managers like AeroSpace then adopt it into a workspace and switch to
                 * that workspace every time the tray icon shows it.
                 * Menu bar style only: in the floating and regular styles the main window is a
                 * standalone window and needs its style bits (closable for Cmd-W, the regular
                 * style's title bar and resizing).
                 */
                if !dock_style {
                    if let Some(window) = app.get_webview_window("main") {
                        match window.ns_window() {
                            Ok(ns_window) => {
                                let ns_window: &objc2_app_kit::NSWindow =
                                    unsafe { &*ns_window.cast() };
                                ns_window
                                    .setStyleMask(objc2_app_kit::NSWindowStyleMask::Borderless);
                            }
                            Err(e) => log::error!("Failed to get the main NSWindow: {e:?}"),
                        }
                    }
                }

                /*
                 * First show the notification window which explains that hopp lives in the
                 * menubar (menu bar style only). Then show the permissions window if needed.
                 */
                let mut show_dock = dock_style;
                let show_tray_notification_selection = !dock_style
                    && app
                        .state::<AppData>()
                        .settings()
                        .app_state
                        .tray_notification();
                if show_tray_notification_selection {
                    let height = 250.;
                    let width = 450.;

                    let notification_window = tauri::WebviewWindowBuilder::new(
                        app,
                        "trayNotification",
                        tauri::WebviewUrl::App("trayNotification.html".into()),
                    )
                    .visible(true)
                    .focused(true)
                    .resizable(false)
                    .hidden_title(true)
                    .always_on_top(true)
                    .title_bar_style(tauri::TitleBarStyle::Overlay)
                    .title("Tray Notification")
                    .inner_size(width, height)
                    .build();
                    if let Err(e) = notification_window {
                        log::error!("Failed to create notification window: {e:?}");
                    } else {
                        let notification_window = notification_window.unwrap();
                        let _ = notification_window.show();
                        let _ = notification_window.set_focus();
                        show_dock = true;
                    }
                }

                if permissions::has_ungranted_permissions() {
                    log::info!("Opening permissions window");
                    let permissions_window = tauri::WebviewWindowBuilder::new(
                        app,
                        "permissions",
                        tauri::WebviewUrl::App("permissions.html".into()),
                    )
                    .visible(false)
                    .focused(true)
                    .resizable(false)
                    .hidden_title(true)
                    .always_on_top(false)
                    .title_bar_style(tauri::TitleBarStyle::Overlay)
                    .title("Permissions Configuration")
                    .inner_size(900., 730.)
                    .transparent(true)
                    .shadow(true)
                    .build();
                    if let Err(e) = permissions_window {
                        log::error!("Failed to create permissions window: {e:?}");
                    } else {
                        let permissions_window = permissions_window.unwrap();
                        show_dock = true;

                        // Apply native styling on macOS
                        #[cfg(target_os = "macos")]
                        {
                            set_window_corner_radius_and_decorations(
                                &permissions_window,
                                CORNER_RADIUS,
                                true,
                            );
                        }

                        /*
                         * Focus the window only if the notification window is not shown.
                         * When the notification window is shown we open the permissions window
                         * when it's closed.
                         */
                        if !show_tray_notification_selection {
                            let _ = permissions_window.show();
                            let _ = permissions_window.set_focus();
                        }
                    }
                }

                // Tackles Alt+Tab activation
                if show_dock {
                    app.set_activation_policy(tauri::ActivationPolicy::Regular);
                }
                {
                    let data = app.state::<AppData>();
                    if show_dock {
                        data.activation_policy_regular
                            .store(true, Ordering::Relaxed);
                    }
                    if !cfg!(debug_assertions) {
                        *data
                            .activation_observer
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) =
                            Some(hopp::app_activation::AppActivationObserver::new(
                                app.handle().clone(),
                                location_set_setup.clone(),
                                reopen_requested_clone.clone(),
                                tray_clicked.clone(),
                            ));
                    }
                }
            }

            Ok(())
        })
        .on_window_event(move |window, event| {
            #[cfg(target_os = "macos")]
            if let tauri::WindowEvent::Moved(position) = event {
                let floating = window
                    .try_state::<AppData>()
                    .is_some_and(|data| data.window_style.is_floating());
                if floating && window.label() == "main" {
                    save_main_window_position_debounced(
                        window.app_handle(),
                        *position,
                        window.scale_factor().unwrap_or(1.0),
                        main_window_moves.clone(),
                        MAIN_WINDOW_POSITION_SAVE_DELAY,
                    );
                }
            }
            if let tauri::WindowEvent::Focused(is_focused) = event {
                #[cfg(any(target_os = "windows", target_os = "linux"))]
                if *is_focused && window.label() == "main" {
                    /* Place window on the bottom right corner of the active display. */
                    let current_monitor = window.current_monitor();
                    if let Ok(Some(monitor)) = current_monitor {
                        let monitor_size = monitor.size();
                        let monitor_pos = monitor.position();
                        let window_size = window.inner_size().unwrap();
                        let base_offset = 20 * monitor.scale_factor() as u32;
                        let offset_y = (25. * monitor.scale_factor()) as u32 + base_offset;
                        let x = monitor_pos.x
                            + (monitor_size.width - window_size.width - base_offset) as i32;
                        let y = monitor_pos.y
                            + (monitor_size.height - window_size.height - offset_y) as i32;
                        let new_position = PhysicalPosition::new(x as f64, y as f64);
                        let _ = window.set_position(new_position);
                    }
                }

                // detect click outside of the focused window and hide the app
                let dock_style = window
                    .try_state::<AppData>()
                    .is_some_and(|data| data.window_style.has_dock_icon());
                let deactivate_hiding = deactivate_hiding.lock().unwrap();
                let reopen_requested = reopen_requested.lock().unwrap();
                if !is_focused
                    && window.label() == "main"
                    && !dock_style
                    && !cfg!(debug_assertions)
                    && !*deactivate_hiding
                    && !*reopen_requested
                    && !suppress_hide_on_call_end_clone.load(Ordering::Relaxed)
                {
                    log::info!("Hiding main window on focus lost: {}", *reopen_requested);

                    #[cfg(target_os = "macos")]
                    window.hide().unwrap();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            stop_sharing,
            get_available_content,
            store_token_cmd,
            get_stored_token,
            get_favorite_teammates,
            set_favorite_teammate,
            retain_favorite_teammates,
            delete_stored_token,
            play_sound,
            stop_sound,
            reset_core_process,
            get_logs,
            reveal_settings_file,
            set_deactivate_hiding,
            set_controller_cursor,
            open_accessibility_settings,
            open_microphone_settings,
            open_screenshare_settings,
            trigger_screenshare_permission,
            get_control_permission,
            get_microphone_permission,
            get_screenshare_permission,
            skip_tray_notification_selection_window,
            set_last_used_mic,
            get_last_used_mic,
            set_last_used_camera,
            get_last_used_camera,
            get_sharer_draw_persist,
            set_sharer_draw_persist,
            get_drawing_hint_shown,
            set_drawing_hint_shown,
            get_drawing_enabled,
            set_drawing_enabled,
            minimize_main_window,
            show_main_window_when_placed,
            set_livekit_url,
            get_livekit_url,
            get_camera_permission,
            open_camera_settings,
            set_sentry_metadata,
            call_started,
            set_tray_notification,
            get_hopp_server_url,
            set_hopp_server_url,
            create_feedback_window,
            create_settings_window,
            get_user_settings,
            list_installed_applications,
            set_app_veil_applications,
            set_call_feedback_popup,
            set_telemetry_enabled,
            set_show_dock_icon_in_call,
            set_window_style,
            set_show_menu_bar_icon,
            get_launch_window_style,
            set_show_menu_bar_sharing_buttons,
            set_auto_update_enabled,
            set_start_camera_on_call,
            set_start_mic_on_call,
            set_remote_control_enabled,
            set_shortcut_toggle_mic,
            set_shortcut_toggle_camera,
            set_shortcut_toggle_screenshare,
            set_shortcut_end_call,
            set_is_camera_on,
            set_is_screensharing,
            mute_mic,
            unmute_mic,
            toggle_mic,
            set_noise_cancellation,
            set_screen_share_resolution,
            set_low_bandwidth_default,
            set_call_low_bandwidth,
            set_screen_share_picker_mode,
            list_microphones,
            select_microphone,
            list_webcams,
            start_camera,
            stop_camera,
            open_camera_preview,
            open_screenshare_viewer,
            close_screenshare_viewer,
            toggle_call_sleep_prevention,
            bring_windows_to_front,
            open_stats_window,
            quit_app,
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application");

    /*
     * Core must get CallEnd before we go. Bounded (1 s) and normally instant, since the
     * writer thread just has to drain the queue. Sent once: app.exit() raises both
     * ExitRequested and Exit.
     */
    let call_end_sent_on_exit = AtomicBool::new(false);
    let end_call_before_exit = move |app_handle: &tauri::AppHandle| {
        if !call_end_sent_on_exit.swap(true, Ordering::SeqCst) {
            app_handle
                .state::<AppData>()
                .core
                .send_before_exit(Message::CallEnd(None));
        }
    };

    app.run(move |app_handle, event| match event {
        tauri::RunEvent::ExitRequested { .. } => {
            log::info!("Exit requested");
            // Tray menu, quit_app, core's ExitRequested and relaunch end here.
            end_call_before_exit(app_handle);
            sentry_utils::upload_logs_event("Tauri app quit".to_string());
            sentry_utils::flush(std::time::Duration::from_secs(2));
        }
        tauri::RunEvent::Exit => {
            // The app menu's Quit (Cmd+Q) and Dock > Quit terminate the app without
            // ExitRequested.
            log::info!("Exit");
            end_call_before_exit(app_handle);
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => {
            // Dock icon click.
            if app_handle.state::<AppData>().window_style.has_dock_icon() {
                if let Some(window) = app_handle.get_webview_window("main") {
                    let hidden = !window.is_visible().unwrap_or(false)
                        || window.is_minimized().unwrap_or(false);
                    if hidden {
                        log::info!("Reopen: showing main window");
                        show_main_window(&window);
                    }
                }
            }
        }
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::CloseRequested { api, .. },
            ..
        } => {
            log::info!("Close requested for window: {label}");
            if label == "main" {
                #[cfg(target_os = "macos")]
                if app_handle.state::<AppData>().window_style.has_dock_icon() {
                    // Closing hides the main window; Cmd+Q / Dock > Quit quit the app.
                    api.prevent_close();
                    if let Some(window) = app_handle.get_webview_window("main") {
                        let _ = window.hide();
                    }
                }
                #[cfg(not(target_os = "macos"))]
                let _ = api;
            } else if label == "trayNotification" {
                /* Make the permissions window visible in this case. */
                let permissions_window = app_handle.get_webview_window("permissions");
                if let Some(window) = permissions_window {
                    log::info!("Show permissions window");
                    let _ = window.show();
                    let _ = window.set_focus();
                } else {
                    #[cfg(target_os = "macos")]
                    restore_accessory_policy(app_handle);
                }
            } else if label == "permissions" {
                #[cfg(target_os = "macos")]
                restore_accessory_policy(app_handle);
            }
        }
        _ => {}
    });
}

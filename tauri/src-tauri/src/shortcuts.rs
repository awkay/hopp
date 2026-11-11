use crate::app_state::is_shortcut_set;
use crate::AppData;
use socket_lib::{CameraStartMessage, Message};
use std::sync::atomic::Ordering;
use tauri::Manager;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

// Rules (see AppData): (un)register only on the main thread, where the plugin's
// run-on-main-thread round trip runs inline; handlers run on the main thread while the
// plugin holds its shortcut map lock, so they only read atomics and enqueue to core.
pub struct CallShortcuts {
    pub mic: String,
    pub camera: String,
    pub screenshare: String,
    pub end_call: String,
}

pub fn register_call_shortcuts(app: &tauri::AppHandle, shortcuts: CallShortcuts) {
    register(app, "mic", &shortcuts.mic, handle_mic);
    register(app, "camera", &shortcuts.camera, handle_camera);
    register(
        app,
        "screenshare",
        &shortcuts.screenshare,
        handle_screenshare,
    );
    register(app, "end_call", &shortcuts.end_call, handle_end_call);
}

/// Registers one call shortcut; a cleared one (Settings "Not set") is skipped, leaving its
/// keys to other apps.
fn register(app: &tauri::AppHandle, name: &str, accel: &str, handler: fn(&tauri::AppHandle)) {
    if !is_shortcut_set(accel) {
        log::info!("register_call_shortcuts: {name} shortcut not set, skipped");
        return;
    }
    if let Err(e) = app
        .global_shortcut()
        .on_shortcut(accel, move |app, _sc, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            handler(app);
        })
    {
        log::error!("register_call_shortcuts: failed to register {name} shortcut '{accel}': {e}");
    }
}

pub fn unregister_call_shortcuts(app: &tauri::AppHandle) {
    if let Err(e) = app.global_shortcut().unregister_all() {
        log::error!("unregister_call_shortcuts: {e}");
    }
}

pub fn resolved_call_shortcuts(app_state: &crate::app_state::AppState) -> CallShortcuts {
    let mut settings = app_state.user_settings();
    settings.resolve_shortcuts();
    CallShortcuts {
        mic: settings.shortcut_toggle_mic.unwrap_or_default(),
        camera: settings.shortcut_toggle_camera.unwrap_or_default(),
        screenshare: settings.shortcut_toggle_screenshare.unwrap_or_default(),
        end_call: settings.shortcut_end_call.unwrap_or_default(),
    }
}

fn handle_mic(app: &tauri::AppHandle) {
    let _ = app.state::<AppData>().core.send(Message::ToggleMic);
}

fn handle_camera(app: &tauri::AppHandle) {
    let data = app.state::<AppData>();
    let message = if data.is_camera_on.load(Ordering::Relaxed) {
        Message::StopCamera
    } else {
        // Core falls back to the preferred camera Tauri pushed to it.
        Message::StartCamera(CameraStartMessage { device_name: None })
    };
    let _ = data.core.send(message);
}

fn handle_end_call(app: &tauri::AppHandle) {
    // Name the call the key press belongs to, so a repeated press (before the shortcut is
    // unregistered) can't end a call started afterwards. Core reports CallEnded(id); the
    // dispatcher then clears Tauri's state and the UI ends the call.
    let data = app.state::<AppData>();
    if let Some(call_id) = crate::call_state::current_call_id(&data) {
        let _ = data.core.send(Message::CallEnd(Some(call_id)));
    }
}

fn handle_screenshare(app: &tauri::AppHandle) {
    let data = app.state::<AppData>();
    let message = if data.is_screensharing.load(Ordering::Relaxed) {
        Message::StopScreenshare
    } else {
        Message::GetAvailableContent
    };
    let _ = data.core.send(message);
}

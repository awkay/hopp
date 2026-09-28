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
    if let Err(e) = app
        .global_shortcut()
        .on_shortcut(shortcuts.mic.as_str(), |app, _sc, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            handle_mic(app);
        })
    {
        log::error!(
            "register_call_shortcuts: failed to register mic shortcut '{}': {e}",
            shortcuts.mic
        );
    }

    if let Err(e) =
        app.global_shortcut()
            .on_shortcut(shortcuts.camera.as_str(), |app, _sc, event| {
                if event.state() != ShortcutState::Pressed {
                    return;
                }
                handle_camera(app);
            })
    {
        log::error!(
            "register_call_shortcuts: failed to register camera shortcut '{}': {e}",
            shortcuts.camera
        );
    }

    if let Err(e) =
        app.global_shortcut()
            .on_shortcut(shortcuts.screenshare.as_str(), |app, _sc, event| {
                if event.state() != ShortcutState::Pressed {
                    return;
                }
                handle_screenshare(app);
            })
    {
        log::error!(
            "register_call_shortcuts: failed to register screenshare shortcut '{}': {e}",
            shortcuts.screenshare
        );
    }

    if !shortcuts.end_call.is_empty() {
        if let Err(e) =
            app.global_shortcut()
                .on_shortcut(shortcuts.end_call.as_str(), |app, _sc, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    handle_end_call(app);
                })
        {
            log::error!(
                "register_call_shortcuts: failed to register end_call shortcut '{}': {e}",
                shortcuts.end_call
            );
        }
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
    // Core ends whatever call it has and reports CallEnded(id); the dispatcher then
    // clears Tauri's state and the UI ends the call.
    let _ = app.state::<AppData>().core.send(Message::CallEnd(None));
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

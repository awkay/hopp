//! Call lifecycle side effects on the Tauri side.
//!
//! State lives in `AppData::call` (a [`socket_lib::call::CallTracker`]). Transitions happen
//! where the corresponding message is processed, keyed by call id:
//! - the UI starts a call (`begin_call`) and ends it (`end_call_from_ui`),
//! - core's `CallStartResult` / `CallEnded` are handled on the core dispatcher thread, in
//!   wire order with all other core events,
//! - a core restart resets it.
//!
//! Effects that must run on the main thread (dock icon / activation policy, global
//! shortcuts, the hide-suppression flag paired with the policy switch) are posted as one
//! fire-and-forget closure while the call lock is held, so they run in transition order.

use crate::core_client::CoreError;
use crate::{shortcuts, AppData};
use serde::Serialize;
use socket_lib::{CallId, CallStartResultMessage, Message};
use std::sync::atomic::Ordering;
use std::sync::MutexGuard;
use tauri::{AppHandle, Emitter, Manager};

/// How long after switching back to the Accessory policy a focus loss must not hide the
/// main window. Started on the main thread right after the switch, so main-thread latency
/// can't eat into it.
#[cfg(target_os = "macos")]
const SUPPRESS_HIDE_AFTER_CALL_END_MS: u64 = 300;

#[derive(Debug, Clone, Serialize)]
pub struct CallEndedPayload {
    pub call_id: CallId,
}

fn call_lock(data: &AppData) -> MutexGuard<'_, socket_lib::call::CallTracker> {
    data.call.lock().unwrap_or_else(|e| e.into_inner())
}

/// Publishes the current call id for lock-free readers (main-thread shortcut handlers).
/// Call after every transition, with the call lock still held.
fn publish_current_call(data: &AppData, call: &socket_lib::call::CallTracker) {
    data.current_call_id
        .store(call.current().unwrap_or(0), Ordering::SeqCst);
}

/// The current call id without taking any lock (0 is never a call id).
pub fn current_call_id(data: &AppData) -> Option<CallId> {
    match data.current_call_id.load(Ordering::SeqCst) {
        0 => None,
        call_id => Some(call_id),
    }
}

/// Starts call `call_id`: records it and enqueues CallStart while holding the call lock, so
/// a concurrent `end_call_from_ui` either comes first (and this returns an error without
/// sending anything) or enqueues its CallEnd after the CallStart.
pub fn start_call(
    app: &AppHandle,
    call_id: CallId,
    message: Message,
) -> Result<crate::core_client::PendingResponse, String> {
    let data = app.state::<AppData>();
    let call = call_lock(&data);
    if call.current() != Some(call_id) {
        return Err("Call was ended before it started".to_string());
    }
    let pending = data.core.start_request(message).map_err(|e: CoreError| {
        log::error!("start_call: {e}");
        "Failed to send message to hopp_core".to_string()
    });
    drop(call);
    pending
}

/// Records that the UI is starting `call_id` (before the device lookup that precedes
/// CallStart).
pub fn begin_call(app: &AppHandle, call_id: CallId) {
    let data = app.state::<AppData>();
    let mut call = call_lock(&data);
    call.begin(call_id);
    publish_current_call(&data, &call);
}

/// Handles core's answer to CallStart (dispatcher thread, before the command sees it).
pub fn on_call_start_result(app: &AppHandle, result: &CallStartResultMessage) {
    let data = app.state::<AppData>();
    // Read settings before taking the call lock (lock order: never settings inside call
    // when avoidable).
    let (show_dock_icon_in_call, call_shortcuts) = {
        let settings = data.settings();
        (
            settings.app_state.user_settings().show_dock_icon_in_call,
            shortcuts::resolved_call_shortcuts(&settings.app_state),
        )
    };
    let mut call = call_lock(&data);
    if !call.on_start_result(result.call_id, result.result.is_ok()) {
        log::info!(
            "on_call_start_result: call {} not activated (current {:?}, ok {})",
            result.call_id,
            call.current(),
            result.result.is_ok()
        );
        return;
    }
    log::info!("on_call_start_result: call {} active", result.call_id);
    data.is_camera_on.store(false, Ordering::Relaxed);
    data.is_screensharing.store(false, Ordering::Relaxed);

    let app_main = app.clone();
    let posted = app.run_on_main_thread(move || {
        #[cfg(target_os = "macos")]
        if show_dock_icon_in_call {
            let data = app_main.state::<AppData>();
            let _ = app_main.set_activation_policy(tauri::ActivationPolicy::Regular);
            data.activation_policy_regular
                .store(true, Ordering::Relaxed);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = show_dock_icon_in_call;
        shortcuts::unregister_call_shortcuts(&app_main);
        shortcuts::register_call_shortcuts(&app_main, call_shortcuts);
    });
    if let Err(e) = posted {
        log::error!("on_call_start_result: failed to post to main thread: {e:?}");
    }
    drop(call);
}

/// Re-registers the call shortcuts after a settings change, if a call is active.
pub fn refresh_call_shortcuts(app: &AppHandle) {
    let data = app.state::<AppData>();
    let call_shortcuts = shortcuts::resolved_call_shortcuts(&data.settings().app_state);
    let call = call_lock(&data);
    if !call.is_active() {
        return;
    }
    let app_main = app.clone();
    if let Err(e) = app.run_on_main_thread(move || {
        shortcuts::unregister_call_shortcuts(&app_main);
        shortcuts::register_call_shortcuts(&app_main, call_shortcuts);
    }) {
        log::error!("refresh_call_shortcuts: failed to post to main thread: {e:?}");
    }
    drop(call);
}

/// Undoes everything a call turned on. Idempotent. Call with the call lock held.
fn apply_call_ended_effects(app: &AppHandle, data: &AppData) {
    data.is_camera_on.store(false, Ordering::Relaxed);
    data.is_screensharing.store(false, Ordering::Relaxed);
    #[cfg(target_os = "macos")]
    data.sleep_prevention
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .disable();

    let app_main = app.clone();
    let posted = app.run_on_main_thread(move || {
        shortcuts::unregister_call_shortcuts(&app_main);
        #[cfg(target_os = "macos")]
        {
            let data = app_main.state::<AppData>();
            // Suppress the hide-on-blur caused by the policy switch; set in the same
            // main-thread closure as the switch and reset by a timer started only after it.
            data.suppress_hide_on_call_end
                .store(true, Ordering::Relaxed);
            let _ = app_main.set_activation_policy(tauri::ActivationPolicy::Accessory);
            data.activation_policy_regular
                .store(false, Ordering::Relaxed);
            let suppress = data.suppress_hide_on_call_end.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(
                    SUPPRESS_HIDE_AFTER_CALL_END_MS,
                ))
                .await;
                suppress.store(false, Ordering::Relaxed);
            });
        }
    });
    if let Err(e) = posted {
        log::error!("apply_call_ended_effects: failed to post to main thread: {e:?}");
    }
}

/// The UI ends call `call_id` (`None`: whatever is current). Clears Tauri's call state now
/// (not only when core's CallEnded arrives) and enqueues CallEnd under the call lock.
/// A `call_id` for an older call leaves the current call alone; core ignores it too.
pub fn end_call_from_ui(app: &AppHandle, call_id: Option<CallId>) {
    let data = app.state::<AppData>();
    let mut call = call_lock(&data);
    let current = call.current();
    let ended = call.end(call_id);
    publish_current_call(&data, &call);
    if ended {
        log::info!("end_call_from_ui: ending call {current:?}");
        apply_call_ended_effects(app, &data);
    } else {
        log::info!("end_call_from_ui: call {call_id:?} is not current ({current:?})");
    }
    let _ = data.core.send(Message::CallEnd(call_id));
    drop(call);
}

/// Handles core's CallEnded (dispatcher thread). Always forwards it to the UI with its id;
/// the UI ignores ids that aren't its current call.
pub fn on_call_ended(app: &AppHandle, call_id: CallId) {
    let data = app.state::<AppData>();
    {
        let mut call = call_lock(&data);
        let ended = call.on_call_ended(call_id);
        publish_current_call(&data, &call);
        if ended {
            log::info!("on_call_ended: call {call_id:?} ended by core");
            apply_call_ended_effects(app, &data);
        } else {
            log::info!(
                "on_call_ended: call {call_id:?} is not current ({:?}), no state change",
                call.current()
            );
        }
    }
    if let Err(e) = app.emit("core_call_ended", CallEndedPayload { call_id }) {
        log::error!("on_call_ended: failed to emit core_call_ended: {e:?}");
    }
}

/// Core died and is being restarted: the call it carried is gone. Clear the call state and
/// tell the UI the call ended.
pub fn reset_for_core_restart(app: &AppHandle) {
    let data = app.state::<AppData>();
    let ended = {
        let mut call = call_lock(&data);
        let ended = call.reset();
        publish_current_call(&data, &call);
        apply_call_ended_effects(app, &data);
        ended
    };
    data.drawing_enabled.store(false, Ordering::Relaxed);
    if let Some(call_id) = ended {
        if let Err(e) = app.emit("core_call_ended", CallEndedPayload { call_id }) {
            log::error!("reset_for_core_restart: failed to emit core_call_ended: {e:?}");
        }
    }
}

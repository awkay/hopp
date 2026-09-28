//! Handling of messages from core, on the core dispatcher thread, in wire order.
//!
//! Core → UI events are emitted as `core_<event>`. Handlers only take short locks that are
//! never held across I/O to core or waits, so they can't deadlock with a command waiting
//! for a response.

use crate::{call_state, AppData};
use socket_lib::client::IncomingHandler;
use socket_lib::Message;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, Manager};

pub struct CoreEventHandler {
    app: AppHandle,
}

impl CoreEventHandler {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }

    fn emit<S: serde::Serialize + Clone>(&self, event: &str, payload: S) {
        if let Err(e) = self.app.emit(event, payload) {
            log::error!("core_events: failed to emit {event}: {e:?}");
        }
    }
}

impl IncomingHandler for CoreEventHandler {
    fn on_response(&mut self, message: &Message) {
        // Call state changes are applied here, in order with CallEnded events, and before
        // the waiting `call_started` command resumes.
        if let Message::CallStartResult(result) = message {
            call_state::on_call_start_result(&self.app, result);
        }
    }

    fn on_disconnect(&mut self) {
        log::info!("core_events: core connection closed");
    }

    fn on_event(&mut self, message: Message) {
        let app = &self.app;
        match message {
            Message::ParticipantsSnapshot(snapshot) => {
                log::info!(
                    "core_events: participants snapshot ({} participants)",
                    snapshot.len()
                );
                self.emit("core_participants_snapshot", &snapshot);
            }
            Message::RoleChange(event) => {
                log::info!("core_events: role change: {event:?}");
                self.emit("core_role_change", &event);
            }
            Message::CameraFailed(error) => {
                log::error!("core_events: camera failed: {error}");
                self.emit("core_camera_failed", &error);
            }
            Message::CallStartResult(result) => {
                // Only reaches here if core answered without a request id.
                call_state::on_call_start_result(app, &result);
            }
            Message::CallEnded(call_id) => {
                log::info!("core_events: call ended: {call_id}");
                call_state::on_call_ended(app, call_id);
            }
            Message::ControllerDrawPersistChanged(persist) => {
                log::info!("core_events: controller draw persist changed: {persist}");
                app.state::<AppData>()
                    .settings()
                    .app_state
                    .set_controller_draw_persist(persist);
            }
            Message::LastModeChanged(mode) => {
                log::info!("core_events: last mode changed: {mode:?}");
                app.state::<AppData>()
                    .settings()
                    .app_state
                    .set_last_mode(mode);
            }
            Message::RoomConnectionFailed(failure) => {
                log::error!(
                    "core_events: room connection failed for call {}: {}",
                    failure.call_id,
                    failure.reason
                );
                self.emit("core_room_connection_failed", &failure);
            }
            Message::AppVeilFailed(reason) => {
                log::error!("core_events: app veil failed: {reason}");
                self.emit("core_app_veil_failed", &reason);
            }
            Message::ActiveMicChanged(device_name) => {
                log::info!("core_events: active mic changed to: {device_name}");
                app.state::<AppData>()
                    .settings()
                    .app_state
                    .set_last_used_mic(device_name.clone());
                self.emit("core_active_mic_changed", &device_name);
            }
            Message::ActiveCameraChanged(device_name) => {
                // Core already uses this camera as its preferred one; just persist it.
                log::info!("core_events: active camera changed to: {device_name}");
                app.state::<AppData>()
                    .settings()
                    .app_state
                    .set_last_used_camera(device_name.clone());
                self.emit("core_active_camera_changed", &device_name);
            }
            Message::MicrophoneAudioLevel(level) => {
                self.emit("core_mic_audio_level", &level);
            }
            Message::BandwidthModeState(state) => {
                log::info!("core_events: bandwidth mode state: {state:?}");
                self.emit("core_bandwidth_mode_state", &state);
            }
            Message::StartScreenShareResult(Err(error)) => {
                log::error!("core_events: screen share failed: {error}");
                self.emit("core_screenshare_failed", &error);
            }
            Message::StartScreenShareResult(Ok(())) => {
                log::info!("core_events: screen share started");
            }
            Message::DrawingDisabled => {
                log::info!("core_events: drawing disabled");
                app.state::<AppData>()
                    .drawing_enabled
                    .store(false, Ordering::Relaxed);
                #[cfg(not(target_os = "macos"))]
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.set_always_on_top(false);
                }
                self.emit("core_drawing_disabled", ());
            }
            Message::ExitRequested => {
                log::info!("core_events: exit requested from core");
                app.state::<AppData>()
                    .core
                    .send_before_exit(Message::CallEnd(None));
                app.exit(0);
            }
            other => {
                log::error!("core_events: unhandled event: {other:?}");
            }
        }
    }
}

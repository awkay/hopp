use crate::core_client::REQUEST_TIMEOUT;
use crate::AppData;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::NSApplicationDidBecomeActiveNotification;
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};
use socket_lib::Message;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Manager;

// SAFETY: The observer is an ObjC object registered on the main thread notification center.
// We only store it so it stays alive; we never access it from another thread.
struct SendSyncObserver(#[allow(dead_code)] Retained<ProtocolObject<dyn NSObjectProtocol>>);
unsafe impl Send for SendSyncObserver {}
unsafe impl Sync for SendSyncObserver {}

pub struct AppActivationObserver {
    _observer: SendSyncObserver,
}

impl AppActivationObserver {
    pub fn new(
        app_handle: tauri::AppHandle,
        location_set: Arc<Mutex<bool>>,
        reopen_requested: Arc<Mutex<bool>>,
        tray_clicked: Arc<AtomicBool>,
    ) -> Self {
        let observer = unsafe {
            let center = NSNotificationCenter::defaultCenter();

            let bringing_to_front = Arc::new(AtomicBool::new(false));

            let block = block2::RcBlock::new(move |_notification: NonNull<NSNotification>| {
                log::info!("app_activation: received NSApplicationDidBecomeActiveNotification");

                // Guard: skip if activation was triggered by a tray click
                if tray_clicked.load(Ordering::Relaxed) {
                    log::info!("app_activation: tray_clicked flag set, skipping");
                    return;
                }

                // If the user directly clicked the main
                if app_handle
                    .get_webview_window("main")
                    .and_then(|w| w.is_focused().ok())
                    .unwrap_or(false)
                {
                    log::info!("app_activation: main window already focused, skipping");
                    return;
                }

                // Floating/regular window style: activating the app by clicking one of its windows
                // must not pull the main window over it.
                if app_handle.state::<AppData>().window_style.has_dock_icon()
                    && app_handle
                        .webview_windows()
                        .values()
                        .any(|w| w.is_focused().unwrap_or(false))
                {
                    log::info!("app_activation: a window is already focused, skipping");
                    return;
                }

                // Regular mode is either when permissions/notification windows are open, when we are in a call,
                // or the whole session in the floating and regular window styles.
                // This runs on the main thread: only atomics here, never a lock or a wait.
                if app_handle
                    .state::<AppData>()
                    .activation_policy_regular
                    .load(Ordering::Relaxed)
                {
                    log::info!("app_activation: activation_policy_regular is true, showing permissions window if exists");
                    if let Some(window) = app_handle.get_webview_window("permissions") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    } else if bringing_to_front.load(Ordering::Relaxed) {
                        log::info!(
                            "app_activation: BringWindowsToFront already in flight, skipping"
                        );
                    } else if app_handle
                        .get_webview_window("main")
                        .and_then(|w| w.is_focused().ok())
                        .unwrap_or(false)
                    {
                        log::info!(
                            "app_activation: main window focused, skipping BringWindowsToFront"
                        );
                    } else {
                        bringing_to_front.store(true, Ordering::Relaxed);
                        Self::bring_windows_to_front(app_handle.clone(), bringing_to_front.clone());
                    }
                    return;
                } else {
                    log::info!("app_activation: reset policy to accessory");
                    let _ = app_handle.set_activation_policy(tauri::ActivationPolicy::Accessory);
                }

                // Guard: location must be set
                {
                    let is_location_set = location_set.lock().unwrap();
                    if !*is_location_set {
                        log::info!("app_activation: location not set, ignoring");
                        return;
                    }
                }

                // Guard: skip if reopen already in progress
                {
                    let is_reopen_in_progress = reopen_requested.lock().unwrap();
                    if *is_reopen_in_progress {
                        return;
                    }
                }

                // Set reopen flag
                {
                    let mut is_reopen_in_progress = reopen_requested.lock().unwrap();
                    *is_reopen_in_progress = true;
                }

                // Show screenshare window if exists, otherwise show main
                if let Some(window) = app_handle.get_webview_window("screenshare") {
                    let _ = window.show();
                    let _ = window.set_focus();
                } else if let Some(window) = app_handle.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                } else {
                    log::error!("app_activation: no window found to show");
                }

                Self::reset_reopen_requested_after_delay(reopen_requested.clone());
            });

            center.addObserverForName_object_queue_usingBlock(
                Some(NSApplicationDidBecomeActiveNotification),
                None,
                None,
                &block,
            )
        };

        Self {
            _observer: SendSyncObserver(observer),
        }
    }

    /// Asks core to focus its windows and shows the main window if none was focused.
    /// Asynchronous: the observer (main thread) never waits for core. `in_flight` is cleared
    /// on every outcome (answer, error, timeout).
    fn bring_windows_to_front(app_handle: tauri::AppHandle, in_flight: Arc<AtomicBool>) {
        tauri::async_runtime::spawn(async move {
            struct ClearOnDrop(Arc<AtomicBool>);
            impl Drop for ClearOnDrop {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Relaxed);
                }
            }
            let _clear = ClearOnDrop(in_flight);

            let response = app_handle
                .state::<AppData>()
                .core
                .request(Message::BringWindowsToFront, REQUEST_TIMEOUT)
                .await;
            let focused = match response {
                Ok(Message::BringWindowsToFrontResult(focused)) => focused,
                Ok(other) => {
                    log::error!("app_activation: unexpected response: {other:?}");
                    false
                }
                Err(e) => {
                    log::error!("app_activation: BringWindowsToFront failed: {e}");
                    false
                }
            };
            if focused {
                return;
            }
            log::info!("app_activation: BringWindowsToFront returned false, showing main window");
            let app_main = app_handle.clone();
            let _ = app_handle.run_on_main_thread(move || {
                if let Some(window) = app_main.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            });
        });
    }

    fn reset_reopen_requested_after_delay(reopen_in_progress: Arc<Mutex<bool>>) {
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            *reopen_in_progress.lock().unwrap() = false;
        });
    }
}

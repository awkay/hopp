// Tray icon management with platform-specific implementations.
//
// macOS: Uses template images for automatic light/dark adaptation per display,
// with a CALayer overlay for the colored notification dot during calls. While the local
// user shares their screen, the icon widens to [draw] [stop sharing] [Hopp], each third
// clickable on its own.
//
// Other platforms: No-op implementation (tray features not yet implemented).

use std::sync::atomic::Ordering;
use tauri::image::Image;
use tauri::path::BaseDirectory;
use tauri::tray::TrayIcon;
use tauri::{AppHandle, Manager, Wry};

#[cfg(target_os = "macos")]
pub use macos::handle_sharing_controls_click;

/// The Hopp icon.
pub const HOPP_ICON: &str = "tray-dark-default.png";

/// Width (points) the sharing icon adds left of the Hopp glyph: `tray-sharing*.png` is
/// 86pt wide (three 18pt glyphs, 16pt gaps), the Hopp icon 18pt.
pub const SHARING_CONTROLS_EXTRA_WIDTH: f64 = 68.0;

// Platform-specific type alias
#[cfg(target_os = "macos")]
type PlatformTrayState = macos::MacOSTrayState;
#[cfg(not(target_os = "macos"))]
type PlatformTrayState = default::DefaultTrayState;

/// Platform-agnostic tray state manager.
pub struct TrayState {
    inner: PlatformTrayState,
}

impl TrayState {
    pub fn new(tray_icon: TrayIcon<Wry>) -> Self {
        Self {
            inner: PlatformTrayState::new(tray_icon),
        }
    }

    pub fn set_notification_enabled(&mut self, enabled: bool) {
        self.inner.set_notification_enabled(enabled);
    }

    pub fn is_notification_enabled(&self) -> bool {
        self.inner.is_notification_enabled()
    }

    pub fn rect(&self) -> Option<tauri::Rect> {
        self.inner.rect()
    }

    /// Shows or hides the draw and stop sharing buttons in the menu-bar item; `drawing`
    /// picks the draw button's icon.
    pub fn set_sharing_controls(&mut self, app: &AppHandle, visible: bool, drawing: bool) {
        self.inner.set_sharing_controls(app, visible, drawing);
    }

    pub fn sharing_controls_visible(&self) -> bool {
        self.inner.sharing_controls_visible()
    }
}

// =============================================================================
// Default (no-op) implementation for non-macOS platforms
// =============================================================================

#[cfg(not(target_os = "macos"))]
mod default {
    use super::*;

    pub struct DefaultTrayState {
        #[allow(dead_code)]
        tray_icon: TrayIcon<Wry>,
        notification_enabled: bool,
    }

    impl DefaultTrayState {
        pub fn new(tray_icon: TrayIcon<Wry>) -> Self {
            Self {
                tray_icon,
                notification_enabled: false,
            }
        }

        pub fn set_notification_enabled(&mut self, enabled: bool) {
            self.notification_enabled = enabled;
            // No-op: platform-specific tray features not implemented
        }

        pub fn is_notification_enabled(&self) -> bool {
            self.notification_enabled
        }

        pub fn rect(&self) -> Option<tauri::Rect> {
            self.tray_icon.rect().ok().flatten()
        }

        pub fn set_sharing_controls(&mut self, _app: &AppHandle, _visible: bool, _drawing: bool) {
            // No-op: platform-specific tray features not implemented
        }

        pub fn sharing_controls_visible(&self) -> bool {
            false
        }
    }
}

// =============================================================================
// macOS implementation using CALayer for notification dot overlay
// =============================================================================

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{msg_send, MainThreadMarker};
    use objc2_app_kit::NSStatusBar;
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use objc2_quartz_core::CALayer;
    use socket_lib::Message;

    #[derive(Clone, Copy, PartialEq)]
    enum ItemIcon {
        Hopp,
        Sharing { drawing: bool },
    }

    pub struct MacOSTrayState {
        tray_icon: TrayIcon<Wry>,
        notification_enabled: bool,
        icon: ItemIcon,
    }

    impl MacOSTrayState {
        pub fn new(tray_icon: TrayIcon<Wry>) -> Self {
            make_click_target_follow_button(&tray_icon);
            Self {
                tray_icon,
                notification_enabled: false,
                icon: ItemIcon::Hopp,
            }
        }

        pub fn set_notification_enabled(&mut self, enabled: bool) {
            self.notification_enabled = enabled;
            update_notification_dot(enabled);
        }

        pub fn is_notification_enabled(&self) -> bool {
            self.notification_enabled
        }

        pub fn rect(&self) -> Option<tauri::Rect> {
            self.tray_icon.rect().ok().flatten()
        }

        pub fn set_sharing_controls(&mut self, app: &AppHandle, visible: bool, drawing: bool) {
            let icon = if visible {
                ItemIcon::Sharing { drawing }
            } else {
                ItemIcon::Hopp
            };
            if icon == self.icon {
                return;
            }
            let file = match icon {
                ItemIcon::Hopp => HOPP_ICON,
                ItemIcon::Sharing { drawing: false } => "tray-sharing.png",
                ItemIcon::Sharing { drawing: true } => "tray-sharing-drawing.png",
            };
            let Some(image) = load_tray_icon(app, file) else {
                log::error!("[TRAY] failed to load {file}");
                return;
            };
            let result = self
                .tray_icon
                .set_icon(Some(image))
                .and_then(|()| self.tray_icon.set_icon_as_template(true));
            if let Err(e) = result {
                log::error!("[TRAY] failed to set {file}: {e:?}");
                return;
            }
            self.icon = icon;
            app.state::<crate::AppData>()
                .tray_sharing_controls
                .store(visible, Ordering::Relaxed);
            // The dot sits on the Hopp glyph, which moves when the icon width changes.
            if self.notification_enabled {
                update_notification_dot(true);
            }
        }

        pub fn sharing_controls_visible(&self) -> bool {
            self.icon != ItemIcon::Hopp
        }
    }

    /// Handles a left click on the menu-bar item while it shows the sharing controls: the
    /// left third toggles drawing, the middle third stops sharing (the thirds split inside
    /// the gaps between glyphs). Returns false for the Hopp third, and when the controls are
    /// hidden, so the caller toggles the popup as before. Main thread.
    pub fn handle_sharing_controls_click(
        app: &AppHandle,
        position: tauri::PhysicalPosition<f64>,
        rect: tauri::Rect,
    ) -> bool {
        if !app
            .state::<crate::AppData>()
            .tray_sharing_controls
            .load(Ordering::Relaxed)
        {
            return false;
        }
        let left = rect.position.to_physical::<f64>(1.0).x;
        let width = rect.size.to_physical::<f64>(1.0).width;
        let fraction = (position.x - left) / width;
        if fraction < 1.0 / 3.0 {
            toggle_drawing(app);
        } else if fraction < 2.0 / 3.0 {
            stop_sharing(app);
        } else {
            return false;
        }
        true
    }

    /// tray-icon sizes its click-catching view to the button once, but the button widens
    /// and narrows with the sharing icon; let the view follow it so the whole item clicks.
    fn make_click_target_follow_button(tray_icon: &TrayIcon<Wry>) {
        let result = tray_icon.with_inner_tray_icon(|inner| {
            let Some(item) = inner.ns_status_item() else {
                return;
            };
            unsafe {
                let button: *const AnyObject = msg_send![&*item, button];
                if button.is_null() {
                    return;
                }
                let subviews: *const AnyObject = msg_send![button, subviews];
                let count: usize = msg_send![subviews, count];
                for i in 0..count {
                    let view: *const AnyObject = msg_send![subviews, objectAtIndex: i];
                    // NSViewWidthSizable | NSViewHeightSizable
                    let _: () = msg_send![view, setAutoresizingMask: 18usize];
                }
            }
        });
        if let Err(e) = result {
            log::error!("[TRAY] make_click_target_follow_button: {e:?}");
        }
    }

    fn stop_sharing(app: &AppHandle) {
        log::info!("[TRAY] stop sharing clicked");
        let _ = app
            .state::<crate::AppData>()
            .core
            .send(Message::StopScreenshare);
    }

    fn toggle_drawing(app: &AppHandle) {
        log::info!("[TRAY] draw clicked");
        let app = app.clone();
        // Reads settings, which the main thread must not lock (see AppData).
        tauri::async_runtime::spawn(async move {
            let data = app.state::<crate::AppData>();
            let enabled = !data.drawing_enabled.load(Ordering::Relaxed);
            let permanent = data.settings().app_state.sharer_draw_persist();
            crate::set_drawing_enabled(&app, enabled, permanent);
        });
    }

    /// Add or remove a colored dot overlay on the tray icon button using CALayer.
    /// This preserves the template behavior of the base icon while adding color.
    fn update_notification_dot(show: bool) {
        unsafe {
            let Some(_mtm) = MainThreadMarker::new() else {
                log::warn!("[TRAY] update_notification_dot: not on main thread");
                return;
            };

            let status_bar = NSStatusBar::systemStatusBar();

            // Access status items via private API (NSPointerArray)
            let items: *const AnyObject =
                msg_send![&*status_bar, valueForKey: objc2_foundation::ns_string!("_statusItems")];
            if items.is_null() {
                return;
            }

            let count: usize = msg_send![items, count];

            // We might get >1 item for our app, but its filtered from the image selector.
            for i in 0..count {
                let item: *const AnyObject = msg_send![items, pointerAtIndex: i];
                if item.is_null() {
                    continue;
                }

                let button: *const AnyObject = msg_send![item, button];
                if button.is_null() {
                    continue;
                }

                // Only process items with a template image (our tray icon)
                let image: *const AnyObject = msg_send![button, image];
                if image.is_null() {
                    continue;
                }

                // Log if it's a template image
                let is_template: bool = msg_send![image, isTemplate];
                if !is_template {
                    continue;
                }

                let _: () = msg_send![button, setWantsLayer: true];
                let layer: *const AnyObject = msg_send![button, layer];
                if layer.is_null() {
                    continue;
                }

                let bounds: NSRect = msg_send![button, bounds];

                // Look for existing dot layer by name
                let dot_layer_name = objc2_foundation::ns_string!("notificationDot");
                let sublayers: *const AnyObject = msg_send![layer, sublayers];

                let mut existing_dot: *const AnyObject = std::ptr::null();
                if !sublayers.is_null() {
                    let sublayer_count: usize = msg_send![sublayers, count];
                    for j in 0..sublayer_count {
                        let sublayer: *const AnyObject = msg_send![sublayers, objectAtIndex: j];
                        let name: *const AnyObject = msg_send![sublayer, name];
                        if !name.is_null() {
                            let is_equal: bool = msg_send![name, isEqualToString: dot_layer_name];
                            if is_equal {
                                existing_dot = sublayer;
                                break;
                            }
                        }
                    }
                }

                if show {
                    // Dot size and position (coordinates from top-left of button). It sits
                    // on the Hopp glyph, the rightmost 18pt of the centered image, so this
                    // holds for the Hopp icon and the wider sharing icon alike.
                    let image_size: NSSize = msg_send![image, size];
                    let dot_size: f64 = 4.0;
                    let dot_x: f64 = image_size.width - 18.0 + 10.5;
                    let dot_y: f64 = bounds.size.height - dot_size - 13.5;

                    let dot_frame =
                        NSRect::new(NSPoint::new(dot_x, dot_y), NSSize::new(dot_size, dot_size));

                    if existing_dot.is_null() {
                        let dot = CALayer::new();
                        let _: () = msg_send![&*dot, setName: dot_layer_name];
                        dot.setFrame(dot_frame);

                        // Green color: #05df72
                        let ns_color_class = objc2::runtime::AnyClass::get(c"NSColor").unwrap();
                        let green_color: Retained<AnyObject> = msg_send![
                            ns_color_class,
                            colorWithSRGBRed: 0.02_f64,
                            green: 0.875_f64,
                            blue: 0.447_f64,
                            alpha: 1.0_f64
                        ];
                        let cg_color: *const AnyObject = msg_send![&*green_color, CGColor];

                        let _: () = msg_send![&*dot, setBackgroundColor: cg_color];
                        dot.setCornerRadius(dot_size / 2.0);

                        let _: () = msg_send![layer, addSublayer: &*dot];
                    } else {
                        let _: () = msg_send![existing_dot, setFrame: dot_frame];
                        let _: () = msg_send![existing_dot, setHidden: false];
                    }
                } else if !existing_dot.is_null() {
                    let _: () = msg_send![existing_dot, setHidden: true];
                }
            }
        }
    }
}

// =============================================================================
// Shared utilities
// =============================================================================

/// Shows the sharing controls if `show` (the local user is sharing and the setting is on)
/// and a call is active, hides them otherwise. Callable from any thread; the update runs
/// on the main thread.
pub fn update_sharing_controls(app: &AppHandle, show: bool) {
    on_main_thread(app, move |app, data, tray| {
        // Checked when this runs, not when it is posted, so a sharing snapshot processed
        // after the call ended can't bring the controls back.
        let visible = show && crate::call_state::current_call_id(data).is_some();
        tray.set_sharing_controls(app, visible, data.drawing_enabled.load(Ordering::Relaxed));
    });
}

/// Updates the draw button's icon after drawing was turned on or off. Callable from any
/// thread.
pub fn update_drawing_icon(app: &AppHandle) {
    on_main_thread(app, |app, data, tray| {
        let visible = tray.sharing_controls_visible();
        tray.set_sharing_controls(app, visible, data.drawing_enabled.load(Ordering::Relaxed));
    });
}

fn on_main_thread(
    app: &AppHandle,
    f: impl FnOnce(&AppHandle, &crate::AppData, &mut TrayState) + Send + 'static,
) {
    let app_main = app.clone();
    let posted = app.run_on_main_thread(move || {
        let data = app_main.state::<crate::AppData>();
        let mut tray_state = data.tray_state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tray) = tray_state.as_mut() {
            f(&app_main, &data, tray);
        }
    });
    if let Err(e) = posted {
        log::error!("[TRAY] failed to post to main thread: {e:?}");
    }
}

/// Load a tray icon from bundled resources.
/// Used by `setup_tray_icon()` and when switching to and from the sharing icon.
pub fn load_tray_icon(app_handle: &AppHandle, filename: &str) -> Option<Image<'static>> {
    let icon_path = app_handle
        .path()
        .resolve(
            format!("resources/tray-icons/{}", filename),
            BaseDirectory::Resource,
        )
        .ok()?;

    let icon_bytes = std::fs::read(&icon_path).ok()?;
    Image::from_bytes(&icon_bytes).ok()
}

use iced::widget::canvas;
use iced::{Rectangle, Theme};
use iced_wgpu::core::mouse;

use fontdb::Database;
use resvg::{tiny_skia, usvg};

use crate::graphics::graphics_context::click_animation::ClickAnimationRenderer;
use crate::graphics::graphics_context::participant::{ParticipantsManager, MAX_TEXT_CHARS};
use crate::room_service::{ClientPoint, DrawTextData};
use crate::utils::geometry::Position;

pub(crate) const LOCAL_PARTICIPANT_IDENTITY: &str = "local";
pub(crate) const CURSOR_LOGICAL_SIZE: f64 = 30.0;

pub(crate) const CURSOR_ICON_PENCIL: &[u8] =
    include_bytes!("../../resources/icons/local-participant-pencil.svg");
pub(crate) const CURSOR_ICON_POINTER: &[u8] =
    include_bytes!("../../resources/icons/local-participant-cursor.svg");
pub(crate) const CURSOR_ICON_POINT: &[u8] =
    include_bytes!("../../resources/icons/local-participant-pointer.svg");

pub(crate) fn rasterize_svg_to_rgba(svg_bytes: &[u8], px_size: u32) -> (Vec<u8>, u32, u32) {
    let fontdb = std::sync::Arc::new(Database::new());
    let usvg_options = usvg::Options {
        fontdb,
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(svg_bytes, &usvg_options)
        .expect("rasterize_svg_to_rgba: failed to parse cursor SVG");
    let svg_size = tree.size();
    let max_dim = svg_size.width().max(svg_size.height());
    let scale = if max_dim > 0.0 {
        px_size as f32 / max_dim
    } else {
        1.0
    };
    let w = (svg_size.width() * scale).ceil().max(1.0) as u32;
    let h = (svg_size.height() * scale).ceil().max(1.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(w, h).expect("rasterize_svg_to_rgba: pixmap");
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let mut rgba = pixmap.data().to_vec();
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = px[3] as f32;
        if a > 0.0 && a < 255.0 {
            let inv = 255.0 / a;
            px[0] = (px[0] as f32 * inv).round().min(255.0) as u8;
            px[1] = (px[1] as f32 * inv).round().min(255.0) as u8;
            px[2] = (px[2] as f32 * inv).round().min(255.0) as u8;
        }
    }
    (rgba, w, h)
}

#[cfg(target_os = "macos")]
pub(crate) fn create_macos_cursor(
    rgba: &[u8],
    pixel_w: u32,
    pixel_h: u32,
    logical_w: f64,
    logical_h: f64,
    hotspot_x: f64,
    hotspot_y: f64,
) -> objc2::rc::Retained<objc2_app_kit::NSCursor> {
    use objc2::rc::Retained;
    use objc2::AnyThread;
    use objc2_app_kit::{NSBitmapImageRep, NSCursor, NSImage, NSImageRep};
    use objc2_foundation::{NSPoint, NSSize};

    unsafe {
        let planes_ptr: *mut *mut u8 = std::ptr::null_mut();
        let rep: Retained<NSBitmapImageRep> = objc2::msg_send![
            NSBitmapImageRep::alloc(),
            initWithBitmapDataPlanes: planes_ptr,
            pixelsWide: pixel_w as isize,
            pixelsHigh: pixel_h as isize,
            bitsPerSample: 8_isize,
            samplesPerPixel: 4_isize,
            hasAlpha: true,
            isPlanar: false,
            colorSpaceName: objc2_app_kit::NSDeviceRGBColorSpace,
            bytesPerRow: (pixel_w * 4) as isize,
            bitsPerPixel: 32_isize
        ];

        let bitmap_data: *mut u8 = objc2::msg_send![&rep, bitmapData];
        std::ptr::copy_nonoverlapping(rgba.as_ptr(), bitmap_data, rgba.len());

        let image = NSImage::new();
        let rep_as_imagerep: &NSImageRep =
            &*((&rep as &NSBitmapImageRep) as *const NSBitmapImageRep as *const NSImageRep);
        image.addRepresentation(rep_as_imagerep);
        image.setSize(NSSize::new(logical_w, logical_h));

        NSCursor::initWithImage_hotSpot(
            NSCursor::alloc(),
            &image,
            NSPoint::new(hotspot_x, hotspot_y),
        )
    }
}

pub(crate) struct ParticipantOverlay<'a> {
    pub(crate) participants: &'a ParticipantsManager,
    pub(crate) click_animation_renderer: Option<&'a ClickAnimationRenderer>,
}

impl<'a, Message> canvas::Program<Message> for ParticipantOverlay<'a> {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let translate = |pos: Position| -> Position {
            Position {
                x: pos.x * bounds.width as f64,
                y: pos.y * bounds.height as f64,
            }
        };
        let mut geometries = self.participants.draw(renderer, bounds, &translate);
        if let Some(click_renderer) = self.click_animation_renderer {
            geometries.push(click_renderer.draw(renderer, bounds, &translate));
        }
        geometries
    }
}

/// A key press that edits text typed in drawing mode.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TextKey {
    Insert(String),
    Backspace,
    Enter,
    Escape,
}

/// Classifies a key event for drawing-mode text input.
///
/// Printable characters come from winit's composed `text`, so dead keys and
/// Option/AltGr characters work. Shortcuts (Cmd, or Ctrl without Alt, since
/// Windows reports AltGr as Ctrl+Alt) and control characters are not text.
pub(crate) fn text_key_from_event(
    event: &winit::event::KeyEvent,
    modifiers: winit::keyboard::ModifiersState,
) -> Option<TextKey> {
    use winit::keyboard::{Key, NamedKey};

    if !event.state.is_pressed() {
        return None;
    }
    match &event.logical_key {
        Key::Named(NamedKey::Enter) => return Some(TextKey::Enter),
        Key::Named(NamedKey::Backspace) => return Some(TextKey::Backspace),
        Key::Named(NamedKey::Escape) => return Some(TextKey::Escape),
        _ => {}
    }
    if modifiers.super_key() || (modifiers.control_key() && !modifiers.alt_key()) {
        return None;
    }
    let text: String = event
        .text
        .as_ref()?
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    (!text.is_empty()).then_some(TextKey::Insert(text))
}

/// Change to the local participant's text annotation, to apply locally and publish.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DrawTextUpdate {
    /// Full state of the text; `committed` places it at `point`.
    Text(DrawTextData),
    /// The pending text was abandoned.
    Cancel { path_id: u64 },
}

#[derive(Debug)]
struct PendingText {
    path_id: u64,
    anchor: Position,
    text: String,
}

/// Text being typed in drawing mode. It follows the cursor until committed.
#[derive(Debug, Default)]
pub(crate) struct DrawTextInput {
    pending: Option<PendingText>,
}

impl DrawTextInput {
    pub(crate) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Applies a key to the pending text. The first printable key starts a new
    /// text at `cursor` using the next id from `path_id_counter`.
    ///
    /// Returns `None` when the key is not consumed (e.g. Escape or Enter with no
    /// pending text), so callers can apply their own meaning to it.
    pub(crate) fn handle_key(
        &mut self,
        key: TextKey,
        cursor: Option<Position>,
        path_id_counter: &mut u64,
    ) -> Option<DrawTextUpdate> {
        match key {
            TextKey::Insert(chars) => {
                if self.pending.is_none() {
                    let anchor = cursor?;
                    *path_id_counter += 1;
                    self.pending = Some(PendingText {
                        path_id: *path_id_counter,
                        anchor,
                        text: String::new(),
                    });
                }
                let pending = self.pending.as_mut()?;
                let room = MAX_TEXT_CHARS.saturating_sub(pending.text.chars().count());
                pending.text.extend(chars.chars().take(room));
                if pending.text.is_empty() {
                    return self.cancel();
                }
                self.update(false)
            }
            TextKey::Backspace => {
                let pending = self.pending.as_mut()?;
                pending.text.pop();
                if pending.text.is_empty() {
                    self.cancel()
                } else {
                    self.update(false)
                }
            }
            TextKey::Enter => self.commit(),
            TextKey::Escape => self.cancel(),
        }
    }

    /// Moves the pending text to follow the cursor.
    pub(crate) fn move_to(&mut self, anchor: Position) -> Option<DrawTextUpdate> {
        let pending = self.pending.as_mut()?;
        if pending.anchor == anchor {
            return None;
        }
        pending.anchor = anchor;
        self.update(false)
    }

    /// Places the pending text at its current anchor.
    pub(crate) fn commit(&mut self) -> Option<DrawTextUpdate> {
        let update = self.update(true);
        self.pending = None;
        update
    }

    /// Abandons the pending text.
    pub(crate) fn cancel(&mut self) -> Option<DrawTextUpdate> {
        self.pending.take().map(|pending| DrawTextUpdate::Cancel {
            path_id: pending.path_id,
        })
    }

    fn update(&self, committed: bool) -> Option<DrawTextUpdate> {
        self.pending.as_ref().map(|pending| {
            DrawTextUpdate::Text(DrawTextData {
                path_id: pending.path_id,
                point: ClientPoint {
                    x: pending.anchor.x,
                    y: pending.anchor.y,
                },
                text: pending.text.clone(),
                committed,
            })
        })
    }
}

/// Applies a local text update to the local participant's drawing.
pub(crate) fn apply_local_text_update(
    participants: &mut ParticipantsManager,
    update: &DrawTextUpdate,
) {
    match update {
        DrawTextUpdate::Text(data) => participants.draw_text(
            LOCAL_PARTICIPANT_IDENTITY,
            data.path_id,
            Position {
                x: data.point.x,
                y: data.point.y,
            },
            &data.text,
            data.committed,
        ),
        DrawTextUpdate::Cancel { path_id } => {
            participants.draw_clear_path(LOCAL_PARTICIPANT_IDENTITY, *path_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: f64, y: f64) -> Position {
        Position { x, y }
    }

    fn insert(s: &str) -> TextKey {
        TextKey::Insert(s.to_string())
    }

    fn text(path_id: u64, anchor: Position, text: &str, committed: bool) -> DrawTextUpdate {
        DrawTextUpdate::Text(DrawTextData {
            path_id,
            point: ClientPoint {
                x: anchor.x,
                y: anchor.y,
            },
            text: text.to_string(),
            committed,
        })
    }

    #[test]
    fn first_key_starts_text_at_cursor_with_new_id() {
        let mut input = DrawTextInput::default();
        let mut id = 4;

        let update = input.handle_key(insert("h"), Some(pos(0.1, 0.2)), &mut id);

        assert_eq!(update, Some(text(5, pos(0.1, 0.2), "h", false)));
        assert_eq!(id, 5);
        assert!(input.is_pending());
    }

    #[test]
    fn typing_appends_and_cursor_moves_the_anchor() {
        let mut input = DrawTextInput::default();
        let mut id = 0;
        input.handle_key(insert("h"), Some(pos(0.1, 0.1)), &mut id);

        assert_eq!(
            input.move_to(pos(0.3, 0.4)),
            Some(text(1, pos(0.3, 0.4), "h", false))
        );
        assert_eq!(input.move_to(pos(0.3, 0.4)), None);
        assert_eq!(
            input.handle_key(insert("i"), Some(pos(0.9, 0.9)), &mut id),
            Some(text(1, pos(0.3, 0.4), "hi", false))
        );
        assert_eq!(id, 1);
    }

    #[test]
    fn enter_commits_and_ends_pending_text() {
        let mut input = DrawTextInput::default();
        let mut id = 0;
        input.handle_key(insert("ok"), Some(pos(0.1, 0.1)), &mut id);

        assert_eq!(
            input.handle_key(TextKey::Enter, None, &mut id),
            Some(text(1, pos(0.1, 0.1), "ok", true))
        );
        assert!(!input.is_pending());
        assert_eq!(input.move_to(pos(0.5, 0.5)), None);
    }

    #[test]
    fn backspace_edits_and_cancels_when_empty() {
        let mut input = DrawTextInput::default();
        let mut id = 0;
        input.handle_key(insert("ab"), Some(pos(0.1, 0.1)), &mut id);

        assert_eq!(
            input.handle_key(TextKey::Backspace, None, &mut id),
            Some(text(1, pos(0.1, 0.1), "a", false))
        );
        assert_eq!(
            input.handle_key(TextKey::Backspace, None, &mut id),
            Some(DrawTextUpdate::Cancel { path_id: 1 })
        );
        assert!(!input.is_pending());
    }

    #[test]
    fn escape_cancels_only_pending_text() {
        let mut input = DrawTextInput::default();
        let mut id = 0;

        assert_eq!(input.handle_key(TextKey::Escape, None, &mut id), None);

        input.handle_key(insert("x"), Some(pos(0.1, 0.1)), &mut id);
        assert_eq!(
            input.handle_key(TextKey::Escape, None, &mut id),
            Some(DrawTextUpdate::Cancel { path_id: 1 })
        );
        assert!(!input.is_pending());
    }

    #[test]
    fn keys_without_pending_text_or_cursor_are_not_consumed() {
        let mut input = DrawTextInput::default();
        let mut id = 0;

        assert_eq!(input.handle_key(TextKey::Enter, None, &mut id), None);
        assert_eq!(input.handle_key(TextKey::Backspace, None, &mut id), None);
        assert_eq!(input.handle_key(insert("a"), None, &mut id), None);
        assert_eq!(id, 0);
    }

    #[test]
    fn text_is_capped() {
        let mut input = DrawTextInput::default();
        let mut id = 0;
        input.handle_key(
            insert(&"a".repeat(MAX_TEXT_CHARS)),
            Some(pos(0.1, 0.1)),
            &mut id,
        );

        match input.handle_key(insert("b"), None, &mut id) {
            Some(DrawTextUpdate::Text(data)) => {
                assert_eq!(data.text.chars().count(), MAX_TEXT_CHARS);
                assert!(!data.text.contains('b'));
            }
            other => panic!("unexpected update {other:?}"),
        }
    }
}

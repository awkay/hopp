use std::time::{Duration, Instant};

use iced::widget::canvas::{self, path, stroke, Cache, Frame, Geometry, Stroke};
use iced::{alignment, Color, Pixels, Point, Rectangle, Renderer, Size};
use iced_core::text::Paragraph as _;

use crate::components::fonts::GEIST_MEDIUM;
use crate::{room_service::DrawingMode, utils::geometry::Position};

const PATH_EXPIRY_DURATION: Duration = Duration::from_secs(3);

/// Maximum number of characters kept in a single text annotation.
pub const MAX_TEXT_CHARS: usize = 256;

/// Text height as a fraction of the shared screen height, so the controller's
/// preview matches the placement on the sharer's screen.
const TEXT_HEIGHT_FRACTION: f64 = 0.022;
const TEXT_MIN_SIZE: f32 = 8.0;
const TEXT_MAX_SIZE: f32 = 96.0;
const TEXT_CARET: char = '|';

fn color_from_hex(hex: &str) -> Color {
    let hex = hex.trim_start_matches('#');

    // Check if the hex string has at least 6 characters to avoid panic
    if hex.len() < 6 {
        log::warn!(
            "color_from_hex: invalid hex color '{}', using default black color",
            hex
        );
        return Color::from_rgb8(0, 0, 0);
    }

    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
    Color::from_rgb8(r, g, b)
}

#[derive(Debug, Clone, PartialEq)]
enum DrawShape {
    Stroke(Vec<Position>),
    /// Text whose top-left corner is placed at `anchor`.
    Text {
        anchor: Position,
        content: String,
    },
}

#[derive(Debug, Clone)]
struct DrawPath {
    path_id: u64,
    shape: DrawShape,
    finished_at: Option<Instant>,
}

impl DrawPath {
    pub fn new(path_id: u64, point: Position) -> Self {
        Self {
            path_id,
            shape: DrawShape::Stroke(vec![point]),
            finished_at: None,
        }
    }
}

/// Truncates `text` to at most [`MAX_TEXT_CHARS`] characters.
pub fn truncate_text(text: &str) -> &str {
    match text.char_indices().nth(MAX_TEXT_CHARS) {
        Some((idx, _)) => &text[..idx],
        None => text,
    }
}

pub struct Draw {
    in_progress_path: Option<DrawPath>,
    /// Text being typed; never expires until committed or cleared.
    in_progress_text: Option<DrawPath>,
    completed_paths: Vec<DrawPath>,
    completed_cache: Cache,
    mode: DrawingMode,
    color: Color,
    auto_clear: bool,
}

impl std::fmt::Debug for Draw {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Draw")
            .field("in_progress_path", &self.in_progress_path)
            .field("in_progress_text", &self.in_progress_text)
            .field("completed_paths", &self.completed_paths)
            .field("mode", &self.mode)
            .field("color", &self.color)
            .finish()
    }
}

impl Draw {
    pub fn new(color: &str, auto_clear: bool, initial_drawing_mode: DrawingMode) -> Self {
        Self {
            in_progress_path: None,
            in_progress_text: None,
            completed_paths: Vec::new(),
            completed_cache: Cache::new(),
            mode: initial_drawing_mode,
            color: color_from_hex(color),
            auto_clear,
        }
    }

    pub fn mode(&self) -> DrawingMode {
        self.mode.clone()
    }

    pub fn set_mode(&mut self, mode: DrawingMode) {
        self.mode = mode.clone();
        if mode == DrawingMode::Disabled {
            self.clear();
        }
    }

    pub fn start_path(&mut self, path_id: u64, point: Position) {
        if self.mode == DrawingMode::Disabled {
            log::warn!("start_path: drawing mode is disabled, skipping path");
            return;
        }

        log::info!("start_path: starting new path with id {}", path_id);
        self.in_progress_path = Some(DrawPath::new(path_id, point));
    }

    pub fn add_point(&mut self, point: Position) {
        if self.mode == DrawingMode::Disabled {
            log::warn!("add_point: drawing mode is disabled, skipping point");
            return;
        }

        if let Some(DrawPath {
            shape: DrawShape::Stroke(points),
            ..
        }) = self.in_progress_path.as_mut()
        {
            points.push(point);
        } else {
            log::warn!("add_point: no current path in progress, skipping point");
        }
    }

    pub fn finish_path(&mut self) {
        if self.mode == DrawingMode::Disabled {
            log::warn!("finish_path: drawing mode is disabled, skipping path");
            return;
        }

        if let Some(mut in_progress_path) = self.in_progress_path.take() {
            log::info!("finish_path: finishing path {}", in_progress_path.path_id);
            in_progress_path.finished_at = Some(Instant::now());
            self.completed_paths.push(in_progress_path);
            self.completed_cache.clear();
        } else {
            log::warn!("finish_path: no path in progress");
        }
    }

    /// Sets the full state of a text annotation.
    ///
    /// Updates are idempotent: each one carries the complete text and anchor.
    /// A committed update moves the text to the completed elements, where it
    /// fades or persists exactly like a finished stroke. Updates for a text that
    /// was already committed are ignored, and empty text drops the pending text.
    pub fn set_text(&mut self, path_id: u64, anchor: Position, content: &str, committed: bool) {
        if self.mode == DrawingMode::Disabled {
            log::warn!("set_text: drawing mode is disabled, skipping text");
            return;
        }

        if self
            .completed_paths
            .iter()
            .any(|path| path.path_id == path_id)
        {
            log::debug!("set_text: text {} already committed, skipping", path_id);
            return;
        }

        let is_pending = self
            .in_progress_text
            .as_ref()
            .is_some_and(|text| text.path_id == path_id);

        let content = truncate_text(content);
        if content.is_empty() {
            if is_pending {
                self.in_progress_text = None;
            }
            return;
        }

        let text = DrawPath {
            path_id,
            shape: DrawShape::Text {
                anchor,
                content: content.to_string(),
            },
            finished_at: None,
        };

        if committed {
            log::info!("set_text: committing text {}", path_id);
            if is_pending {
                self.in_progress_text = None;
            }
            self.completed_paths.push(DrawPath {
                finished_at: Some(Instant::now()),
                ..text
            });
            self.completed_cache.clear();
        } else {
            self.in_progress_text = Some(text);
        }
    }

    pub fn clear_path(&mut self, path_id: u64) {
        log::info!("clear_path: clearing path {}", path_id);

        // Clear current path if it matches
        if let Some(in_progress) = &self.in_progress_path {
            if in_progress.path_id == path_id {
                self.in_progress_path = None;
            }
        }
        if let Some(in_progress) = &self.in_progress_text {
            if in_progress.path_id == path_id {
                self.in_progress_text = None;
            }
        }

        // Remove from completed paths
        self.completed_paths.retain(|path| path.path_id != path_id);
        self.completed_cache.clear();
    }

    pub fn clear(&mut self) {
        self.in_progress_path = None;
        self.in_progress_text = None;
        self.completed_paths.clear();
        self.completed_cache.clear();
    }

    pub fn clear_cache(&mut self) {
        self.completed_cache.clear();
    }

    pub fn clear_expired_paths(&mut self) -> Vec<u64> {
        if !self.auto_clear {
            return Vec::new();
        }

        // Only clear in non-permanent mode
        if let DrawingMode::Draw(settings) = &self.mode {
            if settings.permanent {
                return Vec::new();
            }
        } else {
            return Vec::new();
        }

        let now = Instant::now();
        let mut removed_ids = Vec::new();

        self.completed_paths.retain(|path| {
            if let Some(finished_at) = path.finished_at {
                let should_keep = now.duration_since(finished_at) < PATH_EXPIRY_DURATION;
                if !should_keep {
                    removed_ids.push(path.path_id);
                }
                should_keep
            } else {
                true
            }
        });

        if !removed_ids.is_empty() {
            self.completed_cache.clear();
        }

        removed_ids
    }

    /// Returns cached geometry for completed paths.
    pub fn draw_completed(
        &self,
        renderer: &Renderer,
        bounds: Rectangle,
        translate: &dyn Fn(Position) -> Position,
    ) -> Geometry {
        self.completed_cache.draw(renderer, bounds.size(), |frame| {
            for draw_path in &self.completed_paths {
                self.draw_shape(frame, &draw_path.shape, false, translate);
            }
        })
    }

    /// Draws in-progress path and text onto the provided frame.
    pub fn draw_in_progress_to_frame(
        &self,
        frame: &mut Frame,
        translate: &dyn Fn(Position) -> Position,
    ) {
        if let Some(in_progress) = &self.in_progress_path {
            self.draw_shape(frame, &in_progress.shape, true, translate);
        }
        if let Some(in_progress) = &self.in_progress_text {
            self.draw_shape(frame, &in_progress.shape, true, translate);
        }
    }

    fn draw_shape(
        &self,
        frame: &mut Frame,
        shape: &DrawShape,
        in_progress: bool,
        translate: &dyn Fn(Position) -> Position,
    ) {
        match shape {
            DrawShape::Stroke(points) => {
                if let Some(path) = Self::build_path(points, translate) {
                    frame.stroke(&path, self.make_outline_stroke());
                    frame.stroke(&path, self.make_stroke());
                }
            }
            DrawShape::Text { anchor, content } => {
                self.draw_text(frame, *anchor, content, in_progress, translate);
            }
        }
    }

    /// Draws text on a translucent dark pill so it stays legible on any background.
    fn draw_text(
        &self,
        frame: &mut Frame,
        anchor: Position,
        content: &str,
        in_progress: bool,
        translate: &dyn Fn(Position) -> Position,
    ) {
        let size = Self::text_size(translate);
        let content = if in_progress {
            format!("{content}{TEXT_CARET}")
        } else {
            content.to_string()
        };
        let line_height = iced_core::text::LineHeight::Relative(1.2);
        let paragraph = iced_wgpu::graphics::text::Paragraph::with_text(iced_core::text::Text {
            content: content.as_str(),
            bounds: Size::new(f32::INFINITY, f32::INFINITY),
            size: Pixels(size),
            line_height,
            font: GEIST_MEDIUM,
            align_x: iced_core::text::Alignment::Left,
            align_y: alignment::Vertical::Top,
            shaping: iced_core::text::Shaping::Auto,
            wrapping: iced_core::text::Wrapping::None,
        });
        let text_bounds = paragraph.min_bounds();

        let p = translate(anchor);
        let top_left = Point::new(p.x as f32, p.y as f32);
        let pad_x = size * 0.4;
        let pad_y = size * 0.15;
        let pill = path::Path::rounded_rectangle(
            top_left,
            Size::new(
                text_bounds.width + pad_x * 2.0,
                text_bounds.height + pad_y * 2.0,
            ),
            (size * 0.35).into(),
        );
        frame.fill(&pill, Color::from_rgba(0.06, 0.07, 0.09, 0.75));
        frame.stroke(
            &pill,
            Stroke {
                style: stroke::Style::Solid(self.color),
                width: 1.0,
                ..Stroke::default()
            },
        );
        frame.fill_text(canvas::Text {
            content,
            position: Point::new(top_left.x + pad_x, top_left.y + pad_y),
            color: self.color,
            size: Pixels(size),
            line_height,
            font: GEIST_MEDIUM,
            align_x: iced_core::text::Alignment::Left,
            align_y: alignment::Vertical::Top,
            ..Default::default()
        });
    }

    /// Font size in pixels, derived from the translated screen height.
    ///
    /// Measured around the screen centre, where every translator yields valid points.
    fn text_size(translate: &dyn Fn(Position) -> Position) -> f32 {
        let top = translate(Position { x: 0.5, y: 0.5 });
        let bottom = translate(Position {
            x: 0.5,
            y: 0.5 + TEXT_HEIGHT_FRACTION,
        });
        let size = (bottom.y - top.y).abs() as f32;
        if size.is_finite() {
            size.clamp(TEXT_MIN_SIZE, TEXT_MAX_SIZE)
        } else {
            TEXT_MIN_SIZE
        }
    }

    fn make_stroke(&self) -> Stroke<'static> {
        Stroke {
            style: stroke::Style::Solid(self.color),
            width: 3.0,
            line_cap: stroke::LineCap::Round,
            line_join: stroke::LineJoin::Round,
            line_dash: stroke::LineDash::default(),
        }
    }

    fn make_outline_stroke(&self) -> Stroke<'static> {
        let mut outline_color = self.color;
        outline_color.a *= 0.25;
        Stroke {
            style: stroke::Style::Solid(outline_color),
            width: 3.0 + 0.75,
            line_cap: stroke::LineCap::Round,
            line_join: stroke::LineJoin::Round,
            line_dash: stroke::LineDash::default(),
        }
    }

    fn build_path(
        points: &[Position],
        translate: &dyn Fn(Position) -> Position,
    ) -> Option<path::Path> {
        if points.is_empty() {
            return None;
        }

        let mut builder = path::Builder::new();
        let p = translate(points[0]);
        builder.move_to(Point::new(p.x as f32, p.y as f32));
        for point in &points[1..] {
            let p = translate(*point);
            builder.line_to(Point::new(p.x as f32, p.y as f32));
        }
        Some(builder.build())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room_service::DrawSettings;

    fn draw(permanent: bool) -> Draw {
        Draw::new(
            "#FF0000",
            true,
            DrawingMode::Draw(DrawSettings { permanent }),
        )
    }

    fn pos(x: f64, y: f64) -> Position {
        Position { x, y }
    }

    fn pending_content(draw: &Draw) -> Option<(u64, Position, String)> {
        draw.in_progress_text
            .as_ref()
            .map(|text| match &text.shape {
                DrawShape::Text { anchor, content } => (text.path_id, *anchor, content.clone()),
                DrawShape::Stroke(_) => panic!("pending text holds a stroke"),
            })
    }

    #[test]
    fn text_updates_replace_pending_state() {
        let mut d = draw(false);
        d.set_text(1, pos(0.1, 0.1), "he", false);
        d.set_text(1, pos(0.2, 0.3), "hel", false);

        assert_eq!(
            pending_content(&d),
            Some((1, pos(0.2, 0.3), "hel".to_string()))
        );
        assert!(d.completed_paths.is_empty());
    }

    #[test]
    fn committed_text_moves_to_completed() {
        let mut d = draw(false);
        d.set_text(1, pos(0.1, 0.1), "hello", false);
        d.set_text(1, pos(0.4, 0.5), "hello", true);

        assert!(d.in_progress_text.is_none());
        assert_eq!(d.completed_paths.len(), 1);
        let text = &d.completed_paths[0];
        assert_eq!(text.path_id, 1);
        assert!(text.finished_at.is_some());
        assert_eq!(
            text.shape,
            DrawShape::Text {
                anchor: pos(0.4, 0.5),
                content: "hello".to_string()
            }
        );
    }

    #[test]
    fn updates_after_commit_are_ignored() {
        let mut d = draw(false);
        d.set_text(1, pos(0.1, 0.1), "hello", true);
        d.set_text(1, pos(0.9, 0.9), "hello", false);
        d.set_text(1, pos(0.9, 0.9), "hello", true);

        assert!(d.in_progress_text.is_none());
        assert_eq!(d.completed_paths.len(), 1);
    }

    #[test]
    fn empty_text_cancels_pending_text() {
        let mut d = draw(false);
        d.set_text(1, pos(0.1, 0.1), "h", false);
        d.set_text(1, pos(0.1, 0.1), "", false);

        assert!(d.in_progress_text.is_none());
        assert!(d.completed_paths.is_empty());
    }

    #[test]
    fn clear_path_cancels_pending_and_removes_committed_text() {
        let mut d = draw(true);
        d.set_text(1, pos(0.1, 0.1), "done", true);
        d.set_text(2, pos(0.2, 0.2), "typing", false);

        d.clear_path(2);
        assert!(d.in_progress_text.is_none());
        assert_eq!(d.completed_paths.len(), 1);

        d.clear_path(1);
        assert!(d.completed_paths.is_empty());
    }

    #[test]
    fn clear_and_disable_drop_all_text() {
        let mut d = draw(true);
        d.set_text(1, pos(0.1, 0.1), "done", true);
        d.set_text(2, pos(0.2, 0.2), "typing", false);
        d.clear();
        assert!(d.in_progress_text.is_none());
        assert!(d.completed_paths.is_empty());

        d.set_text(3, pos(0.1, 0.1), "done", true);
        d.set_text(4, pos(0.2, 0.2), "typing", false);
        d.set_mode(DrawingMode::Disabled);
        assert!(d.in_progress_text.is_none());
        assert!(d.completed_paths.is_empty());

        d.set_text(5, pos(0.1, 0.1), "ignored", false);
        assert!(d.in_progress_text.is_none());
    }

    #[test]
    fn committed_text_expires_but_pending_text_does_not() {
        let mut d = draw(false);
        d.set_text(1, pos(0.1, 0.1), "old", true);
        d.set_text(2, pos(0.2, 0.2), "typing", false);
        d.completed_paths[0].finished_at =
            Some(Instant::now() - PATH_EXPIRY_DURATION - Duration::from_millis(1));

        assert_eq!(d.clear_expired_paths(), vec![1]);
        assert!(d.completed_paths.is_empty());
        assert_eq!(pending_content(&d).map(|(id, _, _)| id), Some(2));
    }

    #[test]
    fn committed_text_persists_in_permanent_mode() {
        let mut d = draw(true);
        d.set_text(1, pos(0.1, 0.1), "keep", true);
        d.completed_paths[0].finished_at =
            Some(Instant::now() - PATH_EXPIRY_DURATION - Duration::from_millis(1));

        assert!(d.clear_expired_paths().is_empty());
        assert_eq!(d.completed_paths.len(), 1);
    }

    #[test]
    fn text_is_capped_on_char_boundaries() {
        let long = "é".repeat(MAX_TEXT_CHARS + 10);
        assert_eq!(truncate_text(&long).chars().count(), MAX_TEXT_CHARS);
        assert_eq!(truncate_text("short"), "short");

        let mut d = draw(false);
        d.set_text(1, pos(0.1, 0.1), &long, false);
        let (_, _, content) = pending_content(&d).unwrap();
        assert_eq!(content.chars().count(), MAX_TEXT_CHARS);
    }
}

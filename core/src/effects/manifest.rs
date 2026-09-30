//! Screen-effect manifest: types, caps and validation.
//!
//! This file is compiled twice: by `build.rs` (via `#[path]`), which validates
//! `resources/effects/effects.toml` and every asset at build time, and by the crate
//! as `effects::manifest`, which uses the caps and the frame helpers at run time.
//! It must therefore only depend on `std`, `serde` and `image` (never `crate::`).
//!
//! See `resources/effects/effects.md` for the author-facing documentation.
#![allow(dead_code)]

use std::io::Cursor;

use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, Frames, ImageDecoder, Rgba};
use serde::Deserialize;

/// Only manifest schema this build understands.
pub const SCHEMA_VERSION: u32 = 1;

/// Largest canvas an asset may have (width x height).
pub const MAX_CANVAS_WIDTH: u32 = 1280;
pub const MAX_CANVAS_HEIGHT: u32 = 720;
/// Smallest canvas edge.
pub const MIN_CANVAS_EDGE: u32 = 32;
/// Most frames stored in one file.
pub const MAX_FRAMES: usize = 72;
/// Per-frame delay bounds (inclusive).
pub const MIN_FRAME_DELAY_MS: u32 = 20;
pub const MAX_FRAME_DELAY_MS: u32 = 1000;
/// Longest single pass through the frames.
pub const MAX_LOOP_MS: u32 = 3000;
/// `loops` bounds (inclusive).
pub const MIN_LOOPS: u32 = 1;
pub const MAX_LOOPS: u32 = 16;
/// Longest total play time (`loops` x loop duration).
pub const MAX_TOTAL_MS: u32 = 12000;
/// Largest single asset file.
pub const MAX_FILE_BYTES: usize = 3 * 1024 * 1024;
/// Largest sum of all asset files.
pub const MAX_TOTAL_BYTES: usize = 24 * 1024 * 1024;
/// Most effects in the manifest.
pub const MAX_EFFECTS: usize = 24;
/// Wire id: `^[a-z0-9_]{1,32}$`.
pub const MAX_ID_LEN: usize = 32;
/// Picker tooltip length in characters.
pub const MAX_LABEL_LEN: usize = 24;
/// On-screen height as a fraction of the shared screen's height.
pub const MIN_HEIGHT_FRACTION: f64 = 0.05;
pub const MAX_HEIGHT_FRACTION: f64 = 0.66;
pub const DEFAULT_HEIGHT_FRACTION: f64 = 0.33;
/// Edge of the square picker thumbnail generated at build time (RGBA).
pub const THUMBNAIL_EDGE: u32 = 64;
/// A generated thumbnail is cropped to the pixels with alpha above this...
pub const CROP_ALPHA: u8 = 16;
/// ...plus this fraction of the crop's longer edge as padding on each side.
pub const CROP_PADDING_FRACTION: f64 = 0.06;
/// The picker thumbnail must have at least this fraction of its pixels with alpha
/// above `COVERAGE_ALPHA`, or the build fails (the icon would look empty).
pub const MIN_THUMBNAIL_COVERAGE: f64 = 0.05;
pub const COVERAGE_ALPHA: u8 = 128;
/// Optional hand-made picker icon (`icon`): an RGBA PNG, roughly square.
pub const MIN_ICON_EDGE: u32 = 32;
pub const MAX_ICON_EDGE: u32 = 256;
pub const MAX_ICON_BYTES: usize = 64 * 1024;
/// Longer edge / shorter edge.
pub const MAX_ICON_ASPECT: f64 = 1.25;

/// The parsed `effects.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    #[serde(default, rename = "effect")]
    pub effects: Vec<EffectEntry>,
}

/// One `[[effect]]` table.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectEntry {
    pub id: String,
    pub label: String,
    pub file: String,
    #[serde(default = "default_loops")]
    pub loops: u32,
    #[serde(default = "default_height_fraction")]
    pub height_fraction: f64,
    #[serde(default)]
    pub order: i32,
    #[serde(default)]
    pub thumbnail_frame: Option<u32>,
    /// Picker icon PNG used instead of a thumbnail generated from a frame.
    #[serde(default)]
    pub icon: Option<String>,
}

fn default_loops() -> u32 {
    4
}

fn default_height_fraction() -> f64 {
    DEFAULT_HEIGHT_FRACTION
}

/// A single validation failure. `effect` is the entry's id (or `#<index>` when the
/// id itself is unusable); `field` names the manifest field or the asset property.
#[derive(Debug, Clone, PartialEq)]
pub struct ManifestError {
    pub effect: Option<String>,
    pub field: &'static str,
    pub message: String,
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.effect {
            Some(effect) => write!(f, "effect {effect}: {}: {}", self.field, self.message),
            None => write!(f, "{}: {}", self.field, self.message),
        }
    }
}

/// What decoding an asset revealed.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetInfo {
    pub width: u32,
    pub height: u32,
    /// Per-frame delays in ms, in file order.
    pub delays_ms: Vec<u32>,
    /// Frame with the most opaque coverage (thumbnail default).
    pub most_opaque_frame: usize,
}

impl AssetInfo {
    pub fn loop_ms(&self) -> u32 {
        self.delays_ms.iter().sum()
    }
}

/// An entry that passed every rule, with its decoded metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedEffect {
    pub id: String,
    pub label: String,
    pub file: String,
    pub loops: u32,
    pub height_fraction: f64,
    pub order: i32,
    pub thumbnail_frame: usize,
    pub icon: Option<String>,
    /// `THUMBNAIL_EDGE`² straight RGBA picker thumbnail (from `icon` or a frame).
    pub thumbnail: Vec<u8>,
    pub file_bytes: usize,
    pub asset: AssetInfo,
}

/// Wraps a TOML parse error (unknown field, wrong type, bad syntax). The caller
/// parses with `toml::from_str::<Manifest>` (the `toml` crate is a build- and
/// dev-dependency only).
pub fn parse_error(error: impl std::fmt::Display) -> ManifestError {
    ManifestError {
        effect: None,
        field: "effects.toml",
        message: error.to_string(),
    }
}

/// True for `^[a-z0-9_]{1,32}$`.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// True for a bare `.webp` file name (no directories, no `..`).
pub fn is_valid_file_name(file: &str) -> bool {
    !file.is_empty()
        && !file.contains('/')
        && !file.contains('\\')
        && !file.contains("..")
        && !file.starts_with('.')
        && file.ends_with(".webp")
}

/// True for a bare `.png` file name (no directories, no `..`).
pub fn is_valid_icon_name(file: &str) -> bool {
    !file.is_empty()
        && !file.contains('/')
        && !file.contains('\\')
        && !file.contains("..")
        && !file.starts_with('.')
        && file.ends_with(".png")
}

/// Opens an animated WebP for frame-by-frame decoding.
///
/// Checks the RIFF/WEBP magic, that the file is animated (the `image-webp` frame
/// reader asserts on still images) and has alpha, and sets a transparent
/// background so disposed areas are cleared to transparent, not to the file's
/// background hint. Returns `(width, height, frames)`; frames are straight RGBA.
pub fn open_animation(bytes: &[u8]) -> Result<(u32, u32, Frames<'_>), String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return Err("not a WebP file (missing RIFF/WEBP header)".to_string());
    }
    let mut decoder =
        WebPDecoder::new(Cursor::new(bytes)).map_err(|e| format!("cannot read WebP: {e}"))?;
    if !decoder.has_animation() {
        return Err("not an animated WebP (still image)".to_string());
    }
    if decoder.color_type() != image::ColorType::Rgba8 {
        return Err("has no alpha channel; effects must be transparent".to_string());
    }
    let (width, height) = decoder.dimensions();
    decoder
        .set_background_color(Rgba([0, 0, 0, 0]))
        .map_err(|e| format!("cannot set background: {e}"))?;
    Ok((width, height, decoder.into_frames()))
}

/// Converts an `image` frame delay to whole ms (rounded).
pub fn delay_ms(delay: image::Delay) -> u32 {
    let (numer, denom) = delay.numer_denom_ms();
    if denom == 0 {
        return 0;
    }
    ((numer as u64 + denom as u64 / 2) / denom as u64).min(u32::MAX as u64) as u32
}

/// Decodes every frame and reports dimensions and delays. Stops early (with an
/// error) once the canvas or frame-count caps are exceeded, so an oversized file
/// is not fully decoded.
pub fn inspect_asset(bytes: &[u8]) -> Result<AssetInfo, ManifestError> {
    let asset_err = |field: &'static str, message: String| ManifestError {
        effect: None,
        field,
        message,
    };
    let (width, height, frames) = open_animation(bytes).map_err(|e| asset_err("file", e))?;
    check_canvas(width, height).map_err(|m| asset_err("canvas", m))?;

    let mut delays_ms = Vec::new();
    let mut best = (0usize, 0u64);
    for (index, frame) in frames.enumerate() {
        if index >= MAX_FRAMES {
            return Err(asset_err(
                "frames",
                format!("more than {MAX_FRAMES} frames"),
            ));
        }
        let frame = frame.map_err(|e| asset_err("file", format!("frame {index}: {e}")))?;
        delays_ms.push(delay_ms(frame.delay()));
        let coverage: u64 = frame.buffer().pixels().map(|pixel| pixel.0[3] as u64).sum();
        if coverage > best.1 {
            best = (index, coverage);
        }
    }
    if delays_ms.is_empty() {
        return Err(asset_err("frames", "no frames".to_string()));
    }
    Ok(AssetInfo {
        width,
        height,
        delays_ms,
        most_opaque_frame: best.0,
    })
}

fn check_canvas(width: u32, height: u32) -> Result<(), String> {
    if width > MAX_CANVAS_WIDTH || height > MAX_CANVAS_HEIGHT {
        return Err(format!(
            "{width}x{height} exceeds the {MAX_CANVAS_WIDTH}x{MAX_CANVAS_HEIGHT} maximum"
        ));
    }
    if width < MIN_CANVAS_EDGE || height < MIN_CANVAS_EDGE {
        return Err(format!(
            "{width}x{height} is below the {MIN_CANVAS_EDGE}px minimum edge"
        ));
    }
    Ok(())
}

/// Checks the timing rules for a decoded asset played `loops` times.
pub fn check_timing(delays_ms: &[u32], loops: u32) -> Vec<(&'static str, String)> {
    let mut errors = Vec::new();
    if delays_ms.len() > MAX_FRAMES {
        errors.push(("frames", format!("more than {MAX_FRAMES} frames")));
    }
    for (index, &delay) in delays_ms.iter().enumerate() {
        if !(MIN_FRAME_DELAY_MS..=MAX_FRAME_DELAY_MS).contains(&delay) {
            errors.push((
                "frame delay",
                format!(
                    "frame {index} lasts {delay} ms; must be {MIN_FRAME_DELAY_MS}..={MAX_FRAME_DELAY_MS} ms"
                ),
            ));
        }
    }
    let loop_ms: u64 = delays_ms.iter().map(|&d| d as u64).sum();
    if loop_ms > MAX_LOOP_MS as u64 {
        errors.push((
            "loop duration",
            format!("one loop lasts {loop_ms} ms; must be at most {MAX_LOOP_MS} ms"),
        ));
    }
    let total_ms = loop_ms * loops as u64;
    if (MIN_LOOPS..=MAX_LOOPS).contains(&loops) && total_ms > MAX_TOTAL_MS as u64 {
        errors.push((
            "loops",
            format!(
                "{loops} loops of {loop_ms} ms play {total_ms} ms; must be at most {MAX_TOTAL_MS} ms"
            ),
        ));
    }
    errors
}

/// Validates a parsed manifest. `load(file)` returns the asset bytes (or an error
/// message when the file cannot be read). Returns every error found, not just the
/// first, so an author can fix them in one pass.
pub fn validate(
    manifest: &Manifest,
    mut load: impl FnMut(&str) -> Result<Vec<u8>, String>,
) -> Result<Vec<ValidatedEffect>, Vec<ManifestError>> {
    let mut errors = Vec::new();
    let mut validated = Vec::new();

    if manifest.schema != SCHEMA_VERSION {
        errors.push(ManifestError {
            effect: None,
            field: "schema",
            message: format!(
                "schema {} is not supported; expected {SCHEMA_VERSION}",
                manifest.schema
            ),
        });
    }
    if manifest.effects.len() > MAX_EFFECTS {
        errors.push(ManifestError {
            effect: None,
            field: "effect",
            message: format!(
                "{} effects; at most {MAX_EFFECTS} are allowed",
                manifest.effects.len()
            ),
        });
    }

    let mut seen_ids: Vec<&str> = Vec::new();
    let mut total_bytes = 0usize;

    for (index, entry) in manifest.effects.iter().enumerate() {
        let name = if is_valid_id(&entry.id) {
            entry.id.clone()
        } else {
            format!("#{index}")
        };
        let mut push = |field: &'static str, message: String| {
            errors.push(ManifestError {
                effect: Some(name.clone()),
                field,
                message,
            })
        };
        let mut entry_ok = true;

        if !is_valid_id(&entry.id) {
            push(
                "id",
                format!("{:?} must match ^[a-z0-9_]{{1,{MAX_ID_LEN}}}$", entry.id),
            );
            entry_ok = false;
        } else if seen_ids.contains(&entry.id.as_str()) {
            push("id", format!("{:?} is used more than once", entry.id));
            entry_ok = false;
        } else {
            seen_ids.push(entry.id.as_str());
        }

        let label_chars = entry.label.chars().count();
        if entry.label.trim().is_empty() || label_chars > MAX_LABEL_LEN {
            push(
                "label",
                format!("must be 1..={MAX_LABEL_LEN} characters and not blank"),
            );
            entry_ok = false;
        }

        if !(MIN_LOOPS..=MAX_LOOPS).contains(&entry.loops) {
            push(
                "loops",
                format!(
                    "{} is out of range; must be {MIN_LOOPS}..={MAX_LOOPS}",
                    entry.loops
                ),
            );
            entry_ok = false;
        }

        if !entry.height_fraction.is_finite()
            || entry.height_fraction < MIN_HEIGHT_FRACTION
            || entry.height_fraction > MAX_HEIGHT_FRACTION
        {
            push(
                "height_fraction",
                format!(
                    "{} is out of range; must be {MIN_HEIGHT_FRACTION}..={MAX_HEIGHT_FRACTION}",
                    entry.height_fraction
                ),
            );
            entry_ok = false;
        }

        if !is_valid_file_name(&entry.file) {
            push(
                "file",
                format!(
                    "{:?} must be a bare .webp file name (no '/', '\\\\' or '..')",
                    entry.file
                ),
            );
            continue;
        }

        let bytes = match load(&entry.file) {
            Ok(bytes) => bytes,
            Err(message) => {
                push("file", format!("{:?}: {message}", entry.file));
                continue;
            }
        };
        total_bytes += bytes.len();
        if bytes.len() > MAX_FILE_BYTES {
            push(
                "file size",
                format!(
                    "{:?} is {} bytes; at most {MAX_FILE_BYTES} bytes are allowed",
                    entry.file,
                    bytes.len()
                ),
            );
            continue;
        }

        let asset = match inspect_asset(&bytes) {
            Ok(asset) => asset,
            Err(error) => {
                push(error.field, format!("{:?}: {}", entry.file, error.message));
                continue;
            }
        };

        let timing = check_timing(&asset.delays_ms, entry.loops);
        if !timing.is_empty() {
            entry_ok = false;
            for (field, message) in timing {
                push(field, message);
            }
        }

        let thumbnail_frame = match entry.thumbnail_frame {
            Some(frame) if frame as usize >= asset.delays_ms.len() => {
                push(
                    "thumbnail_frame",
                    format!(
                        "{frame} is past the last frame ({})",
                        asset.delays_ms.len() - 1
                    ),
                );
                entry_ok = false;
                0
            }
            Some(frame) => frame as usize,
            None => asset.most_opaque_frame,
        };

        let thumbnail = match &entry.icon {
            Some(icon) if !is_valid_icon_name(icon) => Err((
                "icon",
                format!("{icon:?} must be a bare .png file name (no '/', '\\\\' or '..')"),
            )),
            Some(icon) => load(icon)
                .map_err(|message| ("icon", format!("{icon:?}: {message}")))
                .and_then(|bytes| {
                    icon_thumbnail_rgba(&bytes)
                        .map_err(|message| ("icon", format!("{icon:?}: {message}")))
                }),
            None if entry_ok => {
                thumbnail_rgba(&bytes, thumbnail_frame).map_err(|message| ("thumbnail", message))
            }
            None => Ok(Vec::new()),
        };
        let thumbnail = match thumbnail {
            Ok(thumbnail) => thumbnail,
            Err((field, message)) => {
                push(field, message);
                continue;
            }
        };
        if entry_ok {
            if let Err(message) = check_thumbnail_coverage(&thumbnail, entry.icon.is_some()) {
                push("thumbnail", message);
                continue;
            }
        }

        if entry_ok {
            validated.push(ValidatedEffect {
                id: entry.id.clone(),
                label: entry.label.clone(),
                file: entry.file.clone(),
                loops: entry.loops,
                height_fraction: entry.height_fraction,
                order: entry.order,
                thumbnail_frame,
                icon: entry.icon.clone(),
                thumbnail,
                file_bytes: bytes.len(),
                asset,
            });
        }
    }

    if total_bytes > MAX_TOTAL_BYTES {
        errors.push(ManifestError {
            effect: None,
            field: "total size",
            message: format!(
                "all effect files add up to {total_bytes} bytes; at most {MAX_TOTAL_BYTES} are allowed"
            ),
        });
    }

    if errors.is_empty() {
        // Stable sort: `order`, then manifest position.
        validated.sort_by_key(|effect| effect.order);
        Ok(validated)
    } else {
        Err(errors)
    }
}

/// Premultiplies straight RGBA in place (gamma space, rounded).
pub fn premultiply_rgba(pixels: &mut [u8]) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let alpha = pixel[3] as u32;
        if alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((*channel as u32 * alpha + 127) / 255) as u8;
        }
    }
}

/// Bounding box `(x, y, width, height)` of the pixels with alpha above `threshold`.
pub fn alpha_bounds(image: &image::RgbaImage, threshold: u8) -> Option<(u32, u32, u32, u32)> {
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel.0[3] <= threshold {
            continue;
        }
        bounds = Some(match bounds {
            None => (x, y, x, y),
            Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        });
    }
    bounds.map(|(x0, y0, x1, y1)| (x0, y0, x1 - x0 + 1, y1 - y0 + 1))
}

/// Crops to the content (alpha above `CROP_ALPHA`) plus `CROP_PADDING_FRACTION`
/// padding, clamped to the image. An image with no such pixel is returned whole.
pub fn crop_to_content(image: &image::RgbaImage) -> image::RgbaImage {
    let Some((x, y, width, height)) = alpha_bounds(image, CROP_ALPHA) else {
        return image.clone();
    };
    let pad = ((width.max(height) as f64 * CROP_PADDING_FRACTION).round() as u32).max(1);
    let left = x.saturating_sub(pad);
    let top = y.saturating_sub(pad);
    let right = (x + width + pad).min(image.width());
    let bottom = (y + height + pad).min(image.height());
    image::imageops::crop_imm(image, left, top, right - left, bottom - top).to_image()
}

/// Scales `image` to fit a `THUMBNAIL_EDGE` square (aspect preserved, centred,
/// transparent padding), straight RGBA.
pub fn fit_thumbnail(image: &image::RgbaImage) -> Vec<u8> {
    let (width, height) = image.dimensions();
    let scale = THUMBNAIL_EDGE as f64 / width.max(height).max(1) as f64;
    let scaled_w = ((width as f64 * scale).round() as u32).clamp(1, THUMBNAIL_EDGE);
    let scaled_h = ((height as f64 * scale).round() as u32).clamp(1, THUMBNAIL_EDGE);
    let scaled = image::imageops::resize(
        image,
        scaled_w,
        scaled_h,
        image::imageops::FilterType::Triangle,
    );
    let mut canvas = image::RgbaImage::new(THUMBNAIL_EDGE, THUMBNAIL_EDGE);
    image::imageops::overlay(
        &mut canvas,
        &scaled,
        ((THUMBNAIL_EDGE - scaled_w) / 2) as i64,
        ((THUMBNAIL_EDGE - scaled_h) / 2) as i64,
    );
    canvas.into_raw()
}

/// Picker thumbnail from frame `index`: cropped to its content, then fitted.
pub fn thumbnail_rgba(bytes: &[u8], index: usize) -> Result<Vec<u8>, String> {
    let (_, _, mut frames) = open_animation(bytes)?;
    let frame = frames
        .nth(index)
        .ok_or_else(|| format!("frame {index} missing"))?
        .map_err(|e| e.to_string())?;
    Ok(fit_thumbnail(&crop_to_content(frame.buffer())))
}

/// Decodes and checks an `icon` PNG: RGBA8, each edge
/// `MIN_ICON_EDGE..=MAX_ICON_EDGE`, aspect at most `MAX_ICON_ASPECT`, at most
/// `MAX_ICON_BYTES`.
pub fn decode_icon(bytes: &[u8]) -> Result<image::RgbaImage, String> {
    if bytes.len() > MAX_ICON_BYTES {
        return Err(format!(
            "is {} bytes; at most {MAX_ICON_BYTES} bytes are allowed",
            bytes.len()
        ));
    }
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("not a PNG file".to_string());
    }
    let decoded = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|e| format!("cannot read PNG: {e}"))?;
    if decoded.color() != image::ColorType::Rgba8 {
        return Err(format!(
            "is {:?}; icons must be 8-bit RGBA (with alpha)",
            decoded.color()
        ));
    }
    let (width, height) = (decoded.width(), decoded.height());
    let edges = MIN_ICON_EDGE..=MAX_ICON_EDGE;
    if !edges.contains(&width) || !edges.contains(&height) {
        return Err(format!(
            "is {width}x{height}; each edge must be {MIN_ICON_EDGE}..={MAX_ICON_EDGE} px"
        ));
    }
    let aspect = width.max(height) as f64 / width.min(height) as f64;
    if aspect > MAX_ICON_ASPECT {
        return Err(format!(
            "is {width}x{height}; it must be roughly square (longer edge at most {MAX_ICON_ASPECT}x the shorter)"
        ));
    }
    Ok(decoded.into_rgba8())
}

/// Picker thumbnail from an `icon` PNG: fitted whole (the author's framing is kept).
pub fn icon_thumbnail_rgba(bytes: &[u8]) -> Result<Vec<u8>, String> {
    Ok(fit_thumbnail(&decode_icon(bytes)?))
}

/// Fraction of the thumbnail's pixels with alpha above `COVERAGE_ALPHA`.
pub fn thumbnail_coverage(rgba: &[u8]) -> f64 {
    let pixels = rgba.as_chunks::<4>().0;
    if pixels.is_empty() {
        return 0.0;
    }
    let opaque = pixels.iter().filter(|p| p[3] > COVERAGE_ALPHA).count();
    opaque as f64 / pixels.len() as f64
}

/// Fails when the picker thumbnail would look (nearly) empty.
pub fn check_thumbnail_coverage(rgba: &[u8], from_icon: bool) -> Result<(), String> {
    let coverage = thumbnail_coverage(rgba);
    if coverage >= MIN_THUMBNAIL_COVERAGE {
        return Ok(());
    }
    let source = if from_icon {
        "the icon PNG"
    } else {
        "the generated picker thumbnail"
    };
    Err(format!(
        "{source} is nearly invisible: {:.1}% of its pixels have alpha > {COVERAGE_ALPHA}, \
         at least {:.0}% are needed. Set `icon = \"<name>.png\"` (a hand-made picker icon) \
         or `thumbnail_frame` (a fuller frame) for this effect",
        coverage * 100.0,
        MIN_THUMBNAIL_COVERAGE * 100.0
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!("{}/src/effects/testdata/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn parse(text: &str) -> Result<Manifest, ManifestError> {
        toml::from_str::<Manifest>(text).map_err(parse_error)
    }

    fn entry(id: &str, file: &str) -> EffectEntry {
        EffectEntry {
            id: id.to_string(),
            label: "Label".to_string(),
            file: file.to_string(),
            loops: 1,
            height_fraction: DEFAULT_HEIGHT_FRACTION,
            order: 0,
            thumbnail_frame: None,
            icon: None,
        }
    }

    fn manifest(entries: Vec<EffectEntry>) -> Manifest {
        Manifest {
            schema: SCHEMA_VERSION,
            effects: entries,
        }
    }

    /// Loads fixtures by file name; unknown names are "missing".
    fn fixtures(name: &str) -> Result<Vec<u8>, String> {
        let path = format!("{}/src/effects/testdata/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(path).map_err(|_| "No such file or directory".to_string())
    }

    fn errors_for(entries: Vec<EffectEntry>) -> Vec<ManifestError> {
        validate(&manifest(entries), fixtures).expect_err("expected validation errors")
    }

    fn assert_single_error(entries: Vec<EffectEntry>, field: &str, contains: &str) {
        let errors = errors_for(entries);
        assert!(
            errors
                .iter()
                .any(|e| e.field == field && e.message.contains(contains)),
            "expected a {field} error containing {contains:?}, got {errors:#?}"
        );
    }

    fn png(image: image::DynamicImage) -> Vec<u8> {
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    /// Transparent RGBA PNG with an opaque `dot`x`dot` square in the middle.
    fn icon_png(width: u32, height: u32, dot: u32) -> Vec<u8> {
        let mut image = image::RgbaImage::new(width, height);
        let (x0, y0) = ((width - dot) / 2, (height - dot) / 2);
        for y in y0..y0 + dot {
            for x in x0..x0 + dot {
                image.put_pixel(x, y, Rgba([200, 40, 40, 255]));
            }
        }
        png(image::DynamicImage::ImageRgba8(image))
    }

    fn with_icon(file: &str, icon: &str) -> EffectEntry {
        let mut entry = entry("pop", file);
        entry.icon = Some(icon.to_string());
        entry
    }

    /// Fixtures plus in-memory icons: `good.png` (full), `dot.png` (a 4 px dot).
    fn fixtures_and_icons(name: &str) -> Result<Vec<u8>, String> {
        match name {
            "good.png" => Ok(icon_png(96, 96, 80)),
            "dot.png" => Ok(icon_png(128, 128, 4)),
            _ => fixtures(name),
        }
    }

    #[test]
    fn crops_to_the_alpha_bounds_with_padding() {
        let mut image = image::RgbaImage::new(200, 100);
        for y in 40..50 {
            for x in 150..160 {
                image.put_pixel(x, y, Rgba([255, 255, 255, 255]));
            }
        }
        // Faint pixels (alpha <= CROP_ALPHA) do not extend the crop.
        image.put_pixel(2, 2, Rgba([255, 255, 255, CROP_ALPHA]));
        assert_eq!(alpha_bounds(&image, CROP_ALPHA), Some((150, 40, 10, 10)));
        let cropped = crop_to_content(&image);
        assert_eq!(cropped.dimensions(), (12, 12)); // 1 px padding each side
        assert_eq!(cropped.get_pixel(0, 0).0[3], 0);
        assert_eq!(cropped.get_pixel(1, 1).0[3], 255);

        // Padding is clamped at the image edge.
        let mut corner = image::RgbaImage::new(100, 100);
        for y in 0..50 {
            for x in 0..20 {
                corner.put_pixel(x, y, Rgba([255, 255, 255, 255]));
            }
        }
        assert_eq!(crop_to_content(&corner).dimensions(), (23, 53));

        // Nothing visible: returned whole.
        let empty = image::RgbaImage::new(40, 30);
        assert_eq!(crop_to_content(&empty).dimensions(), (40, 30));
    }

    #[test]
    fn cropping_makes_a_small_subject_fill_the_thumbnail() {
        let mut image = image::RgbaImage::new(640, 360);
        for y in 170..190 {
            for x in 310..330 {
                image.put_pixel(x, y, Rgba([255, 0, 0, 255]));
            }
        }
        let whole = fit_thumbnail(&image);
        let cropped = fit_thumbnail(&crop_to_content(&image));
        assert_eq!(
            cropped.len(),
            (THUMBNAIL_EDGE * THUMBNAIL_EDGE * 4) as usize
        );
        assert!(thumbnail_coverage(&whole) < MIN_THUMBNAIL_COVERAGE);
        assert!(
            thumbnail_coverage(&cropped) > 0.6,
            "{}",
            thumbnail_coverage(&cropped)
        );
    }

    #[test]
    fn icon_validation_rules() {
        assert!(decode_icon(&icon_png(128, 128, 100)).is_ok());
        assert!(decode_icon(&icon_png(32, 40, 20)).is_ok());
        let err = |bytes: Vec<u8>| decode_icon(&bytes).unwrap_err();
        assert!(err(icon_png(31, 31, 20)).contains("each edge must be 32..=256"));
        assert!(err(icon_png(257, 257, 20)).contains("each edge must be 32..=256"));
        assert!(err(icon_png(128, 96, 20)).contains("roughly square"));
        let rgb = image::DynamicImage::ImageRgb8(image::RgbImage::new(64, 64));
        assert!(err(png(rgb)).contains("8-bit RGBA"));
        assert!(err(b"GIF89a....".to_vec()).contains("not a PNG"));
        let noise = image::RgbaImage::from_fn(256, 256, |x, y| {
            let v = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)).wrapping_mul(97);
            Rgba(v.to_le_bytes())
        });
        let big = png(image::DynamicImage::ImageRgba8(noise));
        assert!(big.len() > MAX_ICON_BYTES);
        assert!(err(big).contains("at most 65536 bytes"));

        assert!(is_valid_icon_name("pop_icon.png"));
        for bad in [
            "",
            "icon.webp",
            "dir/icon.png",
            "..png",
            ".icon.png",
            "a\\b.png",
        ] {
            assert!(!is_valid_icon_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn an_icon_replaces_the_generated_thumbnail() {
        let effects = validate(
            &manifest(vec![with_icon("valid.webp", "good.png")]),
            fixtures_and_icons,
        )
        .unwrap();
        assert_eq!(effects[0].icon.as_deref(), Some("good.png"));
        assert_eq!(
            effects[0].thumbnail,
            icon_thumbnail_rgba(&icon_png(96, 96, 80)).unwrap()
        );
        assert_ne!(
            effects[0].thumbnail,
            thumbnail_rgba(&fixture("valid.webp"), 0).unwrap()
        );
    }

    #[test]
    fn rejects_bad_or_missing_icons() {
        let errors = |entry: EffectEntry| {
            validate(&manifest(vec![entry]), fixtures_and_icons).expect_err("expected errors")
        };
        let has = |errors: &[ManifestError], field: &str, text: &str| {
            errors
                .iter()
                .any(|e| e.field == field && e.message.contains(text))
        };
        assert!(has(
            &errors(with_icon("valid.webp", "../x.png")),
            "icon",
            "bare .png"
        ));
        assert!(has(
            &errors(with_icon("valid.webp", "gone.png")),
            "icon",
            "gone.png"
        ));
    }

    #[test]
    fn rejects_a_nearly_invisible_thumbnail() {
        let errors = validate(
            &manifest(vec![with_icon("valid.webp", "dot.png")]),
            fixtures_and_icons,
        )
        .expect_err("expected a coverage error");
        assert!(
            errors.iter().any(|e| e.field == "thumbnail"
                && e.message.contains("nearly invisible")
                && e.message.contains("icon")
                && e.message.contains("thumbnail_frame")),
            "{errors:#?}"
        );
        assert!(check_thumbnail_coverage(&vec![0; 64 * 64 * 4], false).is_err());
        let mut rgba = vec![0u8; 64 * 64 * 4];
        // Exactly 5% of 4096 pixels (205) with alpha > 128 passes; 204 does not.
        for pixel in rgba.chunks_mut(4).take(204) {
            pixel[3] = 129;
        }
        assert!(check_thumbnail_coverage(&rgba, false).is_err());
        rgba[204 * 4 + 3] = 255;
        assert!(check_thumbnail_coverage(&rgba, false).is_ok());
    }

    #[test]
    fn accepts_a_valid_manifest_and_applies_defaults() {
        let parsed = parse(
            r#"
            schema = 1
            [[effect]]
            id = "wave"
            label = "Wave"
            file = "valid.webp"
            order = 5
            [[effect]]
            id = "clap_2"
            label = "Clap"
            file = "loop_1500.webp"
            loops = 2
            height_fraction = 0.66
            thumbnail_frame = 2
            order = 1
            "#,
        )
        .unwrap();
        assert_eq!(parsed.effects[0].loops, 4);
        assert_eq!(parsed.effects[0].height_fraction, DEFAULT_HEIGHT_FRACTION);
        assert_eq!(parsed.effects[0].thumbnail_frame, None);

        let effects = validate(&parsed, fixtures).unwrap();
        // Sorted by `order`.
        assert_eq!(effects[0].id, "clap_2");
        assert_eq!(effects[1].id, "wave");
        let wave = &effects[1];
        assert_eq!((wave.asset.width, wave.asset.height), (64, 48));
        assert_eq!(wave.asset.delays_ms, vec![40, 200, 40]);
        assert_eq!(wave.asset.loop_ms(), 280);
        assert_eq!(effects[0].thumbnail_frame, 2);
    }

    #[test]
    fn rejects_unknown_fields_and_wrong_types_at_parse_time() {
        let unknown = parse(
            "schema = 1\n[[effect]]\nid = \"a\"\nlabel = \"A\"\nfile = \"valid.webp\"\nsize = 128\n",
        )
        .unwrap_err();
        assert_eq!(unknown.field, "effects.toml");
        assert!(unknown.message.contains("size"), "{}", unknown.message);

        let top_level = parse("schema = 1\nextra = true\n").unwrap_err();
        assert!(top_level.message.contains("extra"), "{}", top_level.message);

        let infinite = parse(
            "schema = 1\n[[effect]]\nid = \"a\"\nlabel = \"A\"\nfile = \"valid.webp\"\nloops = \"infinite\"\n",
        )
        .unwrap_err();
        assert!(infinite.message.contains("loops"), "{}", infinite.message);

        let missing = parse("schema = 1\n[[effect]]\nid = \"a\"\nlabel = \"A\"\n").unwrap_err();
        assert!(missing.message.contains("file"), "{}", missing.message);
    }

    #[test]
    fn rejects_an_unsupported_schema() {
        let mut bad = manifest(vec![entry("a", "valid.webp")]);
        bad.schema = 2;
        let errors = validate(&bad, fixtures).unwrap_err();
        assert_eq!(errors[0].field, "schema");
    }

    #[test]
    fn rejects_bad_and_duplicate_ids() {
        for id in [
            "",
            "Upper",
            "dash-id",
            "space id",
            "dot.id",
            "emoji_\u{1F600}",
            "a_very_long_identifier_over_32_chars",
        ] {
            let errors = errors_for(vec![entry(id, "valid.webp")]);
            assert!(
                errors
                    .iter()
                    .any(|e| e.field == "id" && e.effect.as_deref() == Some("#0")),
                "{id}: {errors:?}"
            );
        }
        assert!(is_valid_id(&"a".repeat(MAX_ID_LEN)));
        assert_single_error(
            vec![entry("same", "valid.webp"), entry("same", "valid.webp")],
            "id",
            "more than once",
        );
    }

    #[test]
    fn rejects_empty_or_long_labels() {
        for label in ["", "   ", "This label is far too long for a tooltip"] {
            let mut bad = entry("a", "valid.webp");
            bad.label = label.to_string();
            assert_single_error(vec![bad], "label", "characters");
        }
        let mut ok = entry("a", "valid.webp");
        ok.label = "é".repeat(MAX_LABEL_LEN);
        assert!(validate(&manifest(vec![ok]), fixtures).is_ok());
    }

    #[test]
    fn rejects_paths_and_non_webp_file_names() {
        for file in [
            "",
            "dir/valid.webp",
            "dir\\valid.webp",
            "../valid.webp",
            "..valid.webp",
            ".hidden.webp",
            "valid.gif",
            "valid.WEBP",
        ] {
            assert_single_error(vec![entry("a", file)], "file", "bare .webp");
        }
    }

    #[test]
    fn rejects_a_missing_file() {
        assert_single_error(vec![entry("a", "nope.webp")], "file", "No such file");
    }

    #[test]
    fn rejects_non_webp_still_and_opaque_assets() {
        let mut files: HashMap<&str, Vec<u8>> = HashMap::new();
        files.insert("gif.webp", b"GIF89a\x01\x00\x01\x00\x00\x00\x00;".to_vec());
        files.insert("short.webp", b"RIFF".to_vec());
        files.insert("truncated.webp", fixture("valid.webp")[..60].to_vec());
        let load = |name: &str| files.get(name).cloned().ok_or("missing".to_string());
        for (file, contains) in [
            ("gif.webp", "RIFF/WEBP"),
            ("short.webp", "RIFF/WEBP"),
            ("truncated.webp", "WebP"),
        ] {
            let errors = validate(&manifest(vec![entry("a", file)]), load).unwrap_err();
            assert!(
                errors
                    .iter()
                    .any(|e| e.field == "file" && e.message.contains(contains)),
                "{file}: {errors:?}"
            );
        }
        assert_single_error(vec![entry("a", "still.webp")], "file", "still image");
        assert_single_error(vec![entry("a", "no_alpha.webp")], "file", "no alpha");
    }

    #[test]
    fn rejects_canvas_outside_the_caps() {
        assert_single_error(
            vec![entry("a", "canvas_too_wide.webp")],
            "canvas",
            "1290x40",
        );
        assert_single_error(vec![entry("a", "canvas_too_tall.webp")], "canvas", "40x730");
        assert_single_error(vec![entry("a", "canvas_too_small.webp")], "canvas", "31x64");
    }

    #[test]
    fn rejects_too_many_frames() {
        assert_single_error(vec![entry("a", "too_many_frames.webp")], "frames", "72");
    }

    #[test]
    fn rejects_frame_delays_outside_the_caps() {
        assert_single_error(vec![entry("a", "delay_10ms.webp")], "frame delay", "10 ms");
        assert_single_error(
            vec![entry("a", "delay_1100ms.webp")],
            "frame delay",
            "1100 ms",
        );
        let errors = check_timing(&[0, 100], 1);
        assert!(errors
            .iter()
            .any(|(field, m)| *field == "frame delay" && m.contains("0 ms")));
        assert!(check_timing(&[20, 1000], 1).is_empty());
    }

    #[test]
    fn rejects_a_loop_longer_than_the_cap() {
        assert_single_error(
            vec![entry("a", "loop_3100ms.webp")],
            "loop duration",
            "3100 ms",
        );
        assert!(check_timing(&[1000, 1000, 1000], 1).is_empty());
    }

    #[test]
    fn rejects_loops_out_of_range_and_total_over_the_cap() {
        for loops in [0, 17, 100] {
            let mut bad = entry("a", "valid.webp");
            bad.loops = loops;
            assert_single_error(vec![bad], "loops", "out of range");
        }
        // 9 x 1500 ms = 13500 ms > 12000 ms.
        let mut long = entry("a", "loop_1500.webp");
        long.loops = 9;
        assert_single_error(vec![long], "loops", "13500 ms");
        let mut ok = entry("a", "loop_1500.webp");
        ok.loops = 8;
        assert!(validate(&manifest(vec![ok]), fixtures).is_ok());
    }

    #[test]
    fn rejects_height_fraction_out_of_range() {
        for fraction in [0.0, 0.04, 0.67, 1.0, -0.5, f64::NAN, f64::INFINITY] {
            let mut bad = entry("a", "valid.webp");
            bad.height_fraction = fraction;
            assert_single_error(vec![bad], "height_fraction", "out of range");
        }
        for fraction in [MIN_HEIGHT_FRACTION, MAX_HEIGHT_FRACTION] {
            let mut ok = entry("a", "valid.webp");
            ok.height_fraction = fraction;
            assert!(validate(&manifest(vec![ok]), fixtures).is_ok());
        }
    }

    #[test]
    fn rejects_a_thumbnail_frame_past_the_end() {
        let mut bad = entry("a", "valid.webp");
        bad.thumbnail_frame = Some(3);
        assert_single_error(vec![bad], "thumbnail_frame", "past the last frame");
    }

    #[test]
    fn rejects_oversized_files_and_totals() {
        let big = vec![0u8; MAX_FILE_BYTES + 1];
        let errors =
            validate(&manifest(vec![entry("a", "big.webp")]), |_| Ok(big.clone())).unwrap_err();
        assert!(errors.iter().any(|e| e.field == "file size"), "{errors:?}");

        let near_cap = vec![0u8; MAX_FILE_BYTES];
        let entries = (0..9).map(|i| entry(&format!("e{i}"), "x.webp")).collect();
        let errors = validate(&manifest(entries), |_| Ok(near_cap.clone())).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.field == "total size" && e.effect.is_none()),
            "{errors:?}"
        );
    }

    #[test]
    fn rejects_too_many_effects() {
        let entries = (0..=MAX_EFFECTS)
            .map(|i| entry(&format!("e{i}"), "valid.webp"))
            .collect();
        let errors = errors_for(entries);
        assert!(errors
            .iter()
            .any(|e| e.field == "effect" && e.message.contains("24")));
        let entries = (0..MAX_EFFECTS)
            .map(|i| entry(&format!("e{i}"), "valid.webp"))
            .collect();
        assert!(validate(&manifest(entries), fixtures).is_ok());
    }

    #[test]
    fn reports_every_error_with_the_effect_and_field() {
        let mut first = entry("first", "valid.webp");
        first.loops = 0;
        first.label = String::new();
        let second = entry("second", "delay_10ms.webp");
        let errors = errors_for(vec![first, second]);
        let fields: Vec<_> = errors
            .iter()
            .map(|e| (e.effect.clone().unwrap(), e.field))
            .collect();
        assert!(fields.contains(&("first".to_string(), "label")));
        assert!(fields.contains(&("first".to_string(), "loops")));
        assert!(fields.contains(&("second".to_string(), "frame delay")));
        assert_eq!(
            errors[0].to_string(),
            format!("effect first: {}: {}", errors[0].field, errors[0].message)
        );
    }

    #[test]
    fn inspect_does_not_panic_on_garbage() {
        let valid = fixture("valid.webp");
        for len in 0..valid.len() {
            let _ = inspect_asset(&valid[..len]);
        }
        let mut flipped = valid.clone();
        for index in 12..flipped.len() {
            flipped[index] ^= 0x5A;
            let _ = inspect_asset(&flipped);
            flipped[index] ^= 0x5A;
        }
    }

    #[test]
    fn thumbnail_is_a_square_rgba_image() {
        let thumbnail = thumbnail_rgba(&fixture("valid.webp"), 1).unwrap();
        assert_eq!(
            thumbnail.len(),
            (THUMBNAIL_EDGE * THUMBNAIL_EDGE * 4) as usize
        );
        // 64x48 scaled to 64x48 and centred: top rows are transparent padding.
        assert_eq!(thumbnail[3], 0);
        assert!(thumbnail_rgba(&fixture("valid.webp"), 3).is_err());
    }
}

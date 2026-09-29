//! Draws the playing screen effect as an alpha-blended textured quad.
//!
//! Effect frames are large (up to 1280x720), so they do not go through iced
//! image handles: iced_wgpu uploads images of 2 MB or more asynchronously and
//! draws nothing for them that frame, and every new handle re-uploads. Instead
//! each window owns one wgpu texture sized to the effect, updated with
//! `queue.write_texture` only when the shown frame changes, and drawn in its own
//! render pass (LoadOp::Load) after iced has presented into the same view.
//!
//! The texture is created when an effect starts and destroyed when it ends, is
//! cancelled, or the window is hidden, so an idle window holds no effect memory.

use std::sync::Arc;

use crate::effects::player::{DecodedFrame, EffectPlayer, PlayOutcome, Tick};
use crate::effects::EffectDef;
use crate::utils::clock::Clock;

/// A rectangle in physical pixels of the render target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl PixelRect {
    fn is_usable(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            && self.width >= 1.0
            && self.height >= 1.0
    }
}

/// Where an effect goes inside `content`: centred, `height_fraction` of the
/// content height, aspect ratio preserved, shrunk to fit the content width.
pub fn fit_effect(
    content: PixelRect,
    effect_width: u32,
    effect_height: u32,
    height_fraction: f64,
) -> Option<PixelRect> {
    if !content.is_usable() || effect_width == 0 || effect_height == 0 {
        return None;
    }
    let fraction = height_fraction.clamp(0.0, 1.0) as f32;
    let mut height = content.height * fraction;
    let mut width = height * effect_width as f32 / effect_height as f32;
    if width > content.width {
        let shrink = content.width / width;
        width *= shrink;
        height *= shrink;
    }
    let rect = PixelRect {
        x: content.x + (content.width - width) / 2.0,
        y: content.y + (content.height - height) / 2.0,
        width,
        height,
    };
    rect.is_usable().then_some(rect)
}

/// `rect` in normalized device coordinates: left, top, right, bottom.
fn to_ndc(rect: PixelRect, target_width: u32, target_height: u32) -> [f32; 4] {
    let tw = target_width.max(1) as f32;
    let th = target_height.max(1) as f32;
    [
        rect.x / tw * 2.0 - 1.0,
        1.0 - rect.y / th * 2.0,
        (rect.x + rect.width) / tw * 2.0 - 1.0,
        1.0 - (rect.y + rect.height) / th * 2.0,
    ]
}

/// Integer scissor for `clip` inside the target, or `None` when empty.
fn scissor(clip: PixelRect, target_width: u32, target_height: u32) -> Option<[u32; 4]> {
    if !clip.is_usable() {
        return None;
    }
    let left = clip.x.max(0.0).floor().min(target_width as f32) as u32;
    let top = clip.y.max(0.0).floor().min(target_height as f32) as u32;
    let right = (clip.x + clip.width).ceil().clamp(0.0, target_width as f32) as u32;
    let bottom = (clip.y + clip.height)
        .ceil()
        .clamp(0.0, target_height as f32) as u32;
    (right > left && bottom > top).then_some([left, top, right - left, bottom - top])
}

#[repr(C)]
#[derive(Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    rect: [f32; 4],
    opacity: f32,
    linearize: u32,
    _pad: [f32; 2],
}

struct EffectTexture {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
}

/// GPU half: pipeline (kept) and the per-effect texture (only while playing).
struct EffectQuad {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
    linearize: bool,
    texture: Option<EffectTexture>,
}

impl EffectQuad {
    fn new(device: wgpu::Device, queue: wgpu::Queue, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Effect quad shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "../shaders/effect_quad.wgsl"
            ))),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Effect quad bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Effect quad pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Effect quad pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    // The shader outputs premultiplied colour.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Effect quad sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Effect quad params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            device,
            queue,
            pipeline,
            bind_group_layout,
            sampler,
            params,
            linearize: target_format.is_srgb(),
            texture: None,
        }
    }

    fn upload(&mut self, frame: &DecodedFrame) {
        let expected = frame.width as usize * frame.height as usize * 4;
        let max_edge = self.device.limits().max_texture_dimension_2d;
        if frame.width == 0
            || frame.height == 0
            || frame.width > max_edge
            || frame.height > max_edge
            || frame.rgba.len() != expected
        {
            log::warn!("effects: skipping malformed frame {:?}", frame);
            return;
        }
        let reuse = self
            .texture
            .as_ref()
            .is_some_and(|t| (t.width, t.height) == (frame.width, frame.height));
        if !reuse {
            self.release();
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Effect frame"),
                size: wgpu::Extent3d {
                    width: frame.width,
                    height: frame.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // Raw bytes: sRGB decoding (if needed) happens in the shader, after
                // un-premultiplying.
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Effect quad bind group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.params.as_entire_binding(),
                    },
                ],
            });
            self.texture = Some(EffectTexture {
                texture,
                bind_group,
                width: frame.width,
                height: frame.height,
            });
        }
        let Some(target) = self.texture.as_ref() else {
            return;
        };
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &target.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &frame.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(frame.width * 4),
                rows_per_image: Some(frame.height),
            },
            wgpu::Extent3d {
                width: frame.width,
                height: frame.height,
                depth_or_array_layers: 1,
            },
        );
    }

    fn draw(
        &self,
        view: &wgpu::TextureView,
        target_width: u32,
        target_height: u32,
        dest: PixelRect,
        clip: PixelRect,
    ) {
        let Some(texture) = self.texture.as_ref() else {
            return;
        };
        let Some([sx, sy, sw, sh]) = scissor(clip, target_width, target_height) else {
            return;
        };
        let params = Params {
            rect: to_ndc(dest, target_width, target_height),
            opacity: 1.0,
            linearize: u32::from(self.linearize),
            _pad: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.params, 0, bytemuck::bytes_of(&params));

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Effect quad encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Effect quad pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_scissor_rect(sx, sy, sw, sh);
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &texture.bind_group, &[]);
            pass.draw(0..6, 0..1);
        }
        self.queue.submit(Some(encoder.finish()));
    }

    fn release(&mut self) {
        if let Some(texture) = self.texture.take() {
            texture.texture.destroy();
        }
    }
}

/// A window's effect: the one-at-a-time player plus its GPU quad.
pub(crate) struct EffectLayer {
    player: EffectPlayer,
    quad: EffectQuad,
}

impl std::fmt::Debug for EffectLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectLayer")
            .field("player", &self.player)
            .finish()
    }
}

impl EffectLayer {
    pub(crate) fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        target_format: wgpu::TextureFormat,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            player: EffectPlayer::new(clock),
            quad: EffectQuad::new(device, queue, target_format),
        }
    }

    /// Starts `effect` unless one is already playing (then it is dropped).
    pub(crate) fn try_play(&mut self, effect: &'static EffectDef) -> PlayOutcome {
        let outcome = self.player.try_play(effect);
        if outcome != PlayOutcome::Busy {
            log::debug!("effects: play {} -> {outcome:?}", effect.id);
        }
        outcome
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.player.is_playing()
    }

    pub(crate) fn deadline(&self) -> Option<std::time::Instant> {
        self.player.deadline()
    }

    /// Stops the effect and frees the worker, channel and texture.
    pub(crate) fn clear(&mut self) {
        self.player.stop();
        self.quad.release();
    }

    /// Advances the effect and draws it into `view` (already presented by iced),
    /// centred in `content` and clipped to `clip`, both in target pixels.
    pub(crate) fn render(
        &mut self,
        view: &wgpu::TextureView,
        target_width: u32,
        target_height: u32,
        content: PixelRect,
        clip: PixelRect,
    ) {
        match self.player.tick() {
            Tick::Idle => {}
            Tick::Ended => self.quad.release(),
            Tick::Playing {
                effect,
                new_frame,
                has_frame,
            } => {
                if let Some(frame) = new_frame {
                    self.quad.upload(&frame);
                    // `frame` (the CPU copy) is dropped here; only the texture remains.
                }
                if !has_frame {
                    return;
                }
                if let Some(dest) =
                    fit_effect(content, effect.width, effect.height, effect.height_fraction)
                {
                    self.quad
                        .draw(view, target_width, target_height, dest, clip);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> PixelRect {
        PixelRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn fits_by_height_fraction_centred_with_aspect() {
        // 1280x720 asset at 0.66 of a 1920x1080 screen: 712.8 px tall.
        let dest = fit_effect(rect(0.0, 0.0, 1920.0, 1080.0), 1280, 720, 0.66).unwrap();
        assert!((dest.height - 712.8).abs() < 0.01);
        assert!((dest.width - 1267.2).abs() < 0.01);
        assert!((dest.x - (1920.0 - dest.width) / 2.0).abs() < 0.01);
        assert!((dest.y - (1080.0 - dest.height) / 2.0).abs() < 0.01);
    }

    #[test]
    fn centres_inside_an_offset_content_rect() {
        let dest = fit_effect(rect(100.0, 50.0, 400.0, 400.0), 256, 256, 0.5).unwrap();
        assert_eq!(dest, rect(200.0, 150.0, 200.0, 200.0));
    }

    #[test]
    fn shrinks_to_fit_a_narrow_content_rect() {
        // Wide asset on a tall, narrow window: width-limited.
        let dest = fit_effect(rect(0.0, 0.0, 300.0, 1000.0), 1280, 720, 0.66).unwrap();
        assert!((dest.width - 300.0).abs() < 0.01);
        assert!((dest.height - 300.0 * 720.0 / 1280.0).abs() < 0.01);
        assert!(dest.x.abs() < 0.01);
    }

    #[test]
    fn rejects_degenerate_inputs() {
        assert!(fit_effect(rect(0.0, 0.0, 0.0, 100.0), 64, 64, 0.5).is_none());
        assert!(fit_effect(rect(0.0, 0.0, 100.0, f32::NAN), 64, 64, 0.5).is_none());
        assert!(fit_effect(rect(-1.0, -1.0, 100.0, 100.0), 0, 64, 0.5).is_none());
        assert!(fit_effect(rect(0.0, 0.0, 100.0, 1.0), 64, 64, 0.5).is_none());
    }

    #[test]
    fn ndc_maps_corners() {
        assert_eq!(
            to_ndc(rect(0.0, 0.0, 200.0, 100.0), 200, 100),
            [-1.0, 1.0, 1.0, -1.0]
        );
        assert_eq!(
            to_ndc(rect(50.0, 25.0, 100.0, 50.0), 200, 100),
            [-0.5, 0.5, 0.5, -0.5]
        );
    }

    #[test]
    fn scissor_is_clamped_to_the_target() {
        assert_eq!(
            scissor(rect(-10.0, -10.0, 5000.0, 5000.0), 800, 600),
            Some([0, 0, 800, 600])
        );
        assert_eq!(
            scissor(rect(10.4, 20.6, 100.2, 50.0), 800, 600),
            Some([10, 20, 101, 51])
        );
        assert_eq!(scissor(rect(900.0, 0.0, 100.0, 100.0), 800, 600), None);
        assert_eq!(scissor(rect(0.0, 0.0, 0.5, 100.0), 800, 600), None);
        assert_eq!(
            scissor(rect(0.0, 0.0, 100.0, 100.0), 1, 1),
            Some([0, 0, 1, 1])
        );
    }
}

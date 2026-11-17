//! The GPU compositor: one quad per layer, ascending `z_index`, plus the
//! two-source quads the Alignment Studio compares with.
//!
//! Resources live in `egui_wgpu`'s `callback_resources` type map; the UI submits
//! a [`QuadDesc`] per visible layer and this renderer owns the textures, the
//! pipeline and the uniform ring. Textures are created and resized lazily and
//! uploaded only when a newer frame is presented, so a paused layer costs
//! nothing per frame beyond one draw call.
//!
//! Layers and compare views share one pipeline: the difference between them is
//! the bind group (a layer binds a 1x1 dummy as its second source) and the mode
//! flag in the uniform.

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use eframe::egui_wgpu::{self, wgpu};

use crate::engine::video_frame::{FrameFormat, VideoFrame};

/// Upper bound on quads drawn in one frame: four layers plus the two captures
/// of a compare view, with room to spare.
pub const MAX_DRAWS: usize = 24;

/// Logical size of one quad's uniform (5 x vec4).
pub const UNIFORM_SIZE: u64 = 80;

/// Per-quad uniform. Field order matches `Quad` in `shader.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct QuadUniform {
    pub uv_crop: [f32; 4],
    pub rect: [f32; 4],
    pub tex: [f32; 4],
    pub params: [f32; 4],
    pub target: [f32; 4],
}

impl QuadUniform {
    /// Unit-testable struct builder so the layout stays in one place.
    pub fn build(
        crop: [f32; 4],
        rect: [f32; 4],
        frame: &VideoFrame,
        opacity: f32,
        mode: CompareMode,
        target: (u32, u32),
        srgb: bool,
    ) -> Self {
        let (kind, wipe, mix) = mode.flags();
        Self {
            uv_crop: crop,
            rect,
            tex: [
                frame.width as f32,
                frame.height as f32,
                frame.format.shader_flag(),
                frame.range.shader_flag(),
            ],
            params: [opacity, kind, wipe, mix],
            target: [
                target.0 as f32,
                target.1 as f32,
                if srgb { 1.0 } else { 0.0 },
                0.0,
            ],
        }
    }
}

/// How a quad's two sources are combined.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CompareMode {
    Single,
    OnionSkin { mix: f32 },
    WipeLine { percent: f32 },
}

impl CompareMode {
    fn flags(self) -> (f32, f32, f32) {
        match self {
            Self::Single => (0.0, 0.0, 0.0),
            Self::OnionSkin { mix } => (1.0, 0.0, mix.clamp(0.0, 1.0)),
            Self::WipeLine { percent } => (2.0, percent.clamp(0.0, 1.0), 0.0),
        }
    }

    fn is_compare(self) -> bool {
        !matches!(self, Self::Single)
    }
}

/// A rectangle in physical pixels of the render target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PixelRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl PixelRect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// From points plus the window's pixels-per-point scale.
    pub fn from_points(rect: eframe::egui::Rect, pixels_per_point: f32) -> Self {
        Self::new(
            rect.min.x * pixels_per_point,
            rect.min.y * pixels_per_point,
            rect.width() * pixels_per_point,
            rect.height() * pixels_per_point,
        )
    }

    pub fn is_empty(self) -> bool {
        self.w < 1.0 || self.h < 1.0
    }

    pub fn intersect(self, other: Self) -> Self {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.w).min(other.x + other.w);
        let y1 = (self.y + self.h).min(other.y + other.h);
        Self::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
    }
}

/// What the UI asks the compositor to draw.
pub struct QuadDesc {
    /// Unique per quad for the life of a session; keys the texture cache.
    pub key: u64,
    /// Where the quad lands, in pixels of the render target.
    pub rect: PixelRect,
    /// Where it may be painted (the canvas), in pixels of the render target.
    pub clip: PixelRect,
    /// Normalized source crop: left, top, right, bottom.
    pub crop: [f32; 4],
    pub opacity: f32,
    pub frame: Arc<VideoFrame>,
    /// The second capture for a compare quad.
    pub other: Option<Arc<VideoFrame>>,
    pub compare: CompareMode,
}

struct Plane {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

struct SourceTextures {
    y: Plane,
    u: Plane,
    v: Plane,
    size: (u32, u32),
    format: FrameFormat,
    pts_us: i64,
    generation: u64,
}

struct Prepared {
    key: u64,
    uniform_offset: u32,
    rect: PixelRect,
}

struct BindState {
    bind_group: wgpu::BindGroup,
    generation: [u64; 2],
}

/// The compositor. One instance, stored in the wgpu callback resources.
pub struct Compositor {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    uniform_stride: u32,
    dummy: Plane,
    sources: HashMap<u64, SourceTextures>,
    binds: HashMap<u64, BindState>,
    prepared: Vec<Prepared>,
    target_format: wgpu::TextureFormat,
    srgb_target: bool,
    generation: u64,
}

impl Compositor {
    /// Build the pipeline and insert the renderer into egui's callback
    /// resources. Called once, from `eframe`'s creation callback.
    pub fn install(render_state: &egui_wgpu::RenderState) {
        let compositor = Self::new(&render_state.device, render_state.target_format);
        render_state
            .renderer
            .write()
            .callback_resources
            .insert(compositor);
    }

    fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("reaction-player.shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let mut entries = vec![
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(UNIFORM_SIZE),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ];
        for binding in 2..8 {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            });
        }
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("reaction-player.quads"),
            entries: &entries,
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("reaction-player.quads"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("reaction-player.quad"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("reaction-player.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            lod_min_clamp: 0.0,
            lod_max_clamp: 32.0,
            compare: None,
            anisotropy_clamp: 1,
            border_color: None,
        });

        // Dynamic uniform offsets must respect the adapter's alignment.
        let uniform_stride = device
            .limits()
            .min_uniform_buffer_offset_alignment
            .max(crate::gpu::UNIFORM_STRIDE as u32) as usize;
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("reaction-player.uniforms"),
            size: (uniform_stride * MAX_DRAWS) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            uniforms,
            uniform_stride: uniform_stride as u32,
            dummy: create_plane(device, 1, 1, wgpu::TextureFormat::R8Unorm),
            sources: HashMap::new(),
            binds: HashMap::new(),
            prepared: Vec::with_capacity(MAX_DRAWS),
            target_format,
            srgb_target: target_format.is_srgb(),
            generation: 1,
        }
    }

    pub fn target_format(&self) -> wgpu::TextureFormat {
        self.target_format
    }

    /// Drop textures for quads that are no longer drawn (layer removed, compare
    /// view closed) so a long session does not leak VRAM.
    fn retain_keys(&mut self, live: &[u64]) {
        self.sources.retain(|k, _| live.contains(&(k >> 1)));
        self.binds.retain(|k, _| live.contains(k));
    }

    /// Make sure `id` has textures of the right size and format, and upload the
    /// frame if it is newer than what is already there. Returns the texture
    /// generation so bind groups can tell when they are stale.
    fn ensure_source(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: u64,
        frame: &VideoFrame,
    ) -> u64 {
        let needs_new = match self.sources.get(&id) {
            Some(s) => s.size != (frame.width, frame.height) || s.format != frame.format,
            None => true,
        };
        if needs_new {
            self.generation += 1;
            let generation = self.generation;
            self.sources
                .insert(id, create_source(device, frame, generation));
        }
        let stale = match self.sources.get(&id) {
            Some(s) => s.pts_us != frame.pts_us,
            None => true,
        };
        if stale {
            if let Some(source) = self.sources.get_mut(&id) {
                upload_source(queue, source, frame);
                source.pts_us = frame.pts_us;
            }
        }
        self.sources.get(&id).map(|s| s.generation).unwrap_or(0)
    }

    fn ensure_bind(
        &mut self,
        device: &wgpu::Device,
        key: u64,
        generation: [u64; 2],
        has_second: bool,
    ) {
        let fresh = match self.binds.get(&key) {
            Some(b) => b.generation != generation,
            None => true,
        };
        if !fresh {
            return;
        }
        let Some(first) = self.sources.get(&(key * 2)) else {
            return;
        };
        let second = self.sources.get(&(key * 2 + 1)).filter(|_| has_second);
        let dummy = &self.dummy.view;
        let y2 = second.map_or(dummy, |s| &s.y.view);
        let u2 = second.map_or(dummy, |s| &s.u.view);
        let v2 = second.map_or(dummy, |s| &s.v.view);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("reaction-player.quad"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.uniforms,
                        offset: 0,
                        size: NonZeroU64::new(UNIFORM_SIZE),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&first.y.view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&first.u.view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&first.v.view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(y2),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(u2),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(v2),
                },
            ],
        });
        self.binds.insert(
            key,
            BindState {
                bind_group,
                generation,
            },
        );
    }

    fn prepare_quads(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        quads: &[QuadDesc],
        target: (u32, u32),
    ) {
        self.prepared.clear();
        let mut live = Vec::with_capacity(quads.len());
        for (index, quad) in quads.iter().enumerate().take(MAX_DRAWS) {
            live.push(quad.key);
            let visible = quad.rect.intersect(quad.clip);
            if visible.is_empty() {
                continue;
            }
            // Map the visible part of the quad back into source UV space so the
            // scissor can cut the quad without squeezing the picture.
            let rx = quad.rect.w.max(1.0);
            let ry = quad.rect.h.max(1.0);
            let fx0 = (visible.x - quad.rect.x) / rx;
            let fy0 = (visible.y - quad.rect.y) / ry;
            let fx1 = (visible.x + visible.w - quad.rect.x) / rx;
            let fy1 = (visible.y + visible.h - quad.rect.y) / ry;
            let crop = [
                quad.crop[0] + (quad.crop[2] - quad.crop[0]) * fx0,
                quad.crop[1] + (quad.crop[3] - quad.crop[1]) * fy0,
                quad.crop[0] + (quad.crop[2] - quad.crop[0]) * fx1,
                quad.crop[1] + (quad.crop[3] - quad.crop[1]) * fy1,
            ];
            // A compare needs a second source; without one the quad is a plain
            // picture even if the caller asked for a wipe or an onion skin.
            let mode = if quad.compare.is_compare() && quad.other.is_none() {
                CompareMode::Single
            } else {
                quad.compare
            };
            let uniform = QuadUniform::build(
                crop,
                [visible.x, visible.y, visible.w, visible.h],
                &quad.frame,
                quad.opacity,
                mode,
                target,
                self.srgb_target,
            );
            let uniform_offset = index as u32 * self.uniform_stride;
            queue.write_buffer(
                &self.uniforms,
                u64::from(uniform_offset),
                bytemuck::bytes_of(&uniform),
            );

            let gen_a = self.ensure_source(device, queue, quad.key * 2, &quad.frame);
            let mut gen_b = 0;
            if let Some(other) = &quad.other {
                gen_b = self.ensure_source(device, queue, quad.key * 2 + 1, other);
            }
            self.ensure_bind(device, quad.key, [gen_a, gen_b], quad.other.is_some());
            self.prepared.push(Prepared {
                key: quad.key,
                uniform_offset,
                rect: visible,
            });
        }
        self.retain_keys(&live);
    }

    fn paint(&self, info: eframe::egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>) {
        let clip = info.clip_rect_in_pixels();
        let clip_rect = PixelRect::new(
            clip.left_px as f32,
            clip.top_px as f32,
            clip.width_px as f32,
            clip.height_px as f32,
        );
        pass.set_pipeline(&self.pipeline);
        for prepared in &self.prepared {
            let Some(bind) = self.binds.get(&prepared.key) else {
                continue;
            };
            let rect = prepared.rect.intersect(clip_rect);
            if rect.is_empty() {
                continue;
            }
            pass.set_bind_group(0, &bind.bind_group, &[prepared.uniform_offset]);
            pass.set_scissor_rect(
                rect.x.max(0.0) as u32,
                rect.y.max(0.0) as u32,
                rect.w as u32,
                rect.h as u32,
            );
            pass.draw(0..3, 0..1);
        }
    }
}

fn create_plane(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> Plane {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("reaction-player.plane"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Plane { texture, view }
}

fn create_source(device: &wgpu::Device, frame: &VideoFrame, generation: u64) -> SourceTextures {
    let (cw, ch) = frame.format.chroma_dims(frame.width, frame.height);
    let (u, v) = match frame.format {
        FrameFormat::Nv12 => (
            create_plane(device, cw, ch, wgpu::TextureFormat::Rg8Unorm),
            create_plane(device, 1, 1, wgpu::TextureFormat::R8Unorm),
        ),
        FrameFormat::Yuv420p => (
            create_plane(device, cw, ch, wgpu::TextureFormat::R8Unorm),
            create_plane(device, cw, ch, wgpu::TextureFormat::R8Unorm),
        ),
    };
    SourceTextures {
        y: create_plane(device, frame.width, frame.height, wgpu::TextureFormat::R8Unorm),
        u,
        v,
        size: (frame.width, frame.height),
        format: frame.format,
        pts_us: i64::MIN,
        generation,
    }
}

fn upload_source(queue: &wgpu::Queue, source: &SourceTextures, frame: &VideoFrame) {
    // One write per plane. The FFmpeg stride is the row pitch: `write_texture`
    // has no 256-byte alignment rule, and the extent is the true plane size, so
    // padding columns never reach the sampler.
    let (cw, ch) = frame.format.chroma_dims(frame.width, frame.height);
    let sizes = [(frame.width, frame.height), (cw, ch), (cw, ch)];
    let planes = [&source.y, &source.u, &source.v];
    for i in 0..frame.format.plane_count() {
        let (w, h) = sizes[i];
        if w == 0 || h == 0 {
            continue;
        }
        let stride = frame.strides[i].max(1);
        let needed = stride * h as usize;
        let data = frame.plane(i);
        if data.len() < needed {
            continue;
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &planes[i].texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data[..needed],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride as u32),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }
}

/// The wgpu callback: a snapshot of the quads for one frame.
pub struct CompositorCallback {
    quads: Arc<Vec<QuadDesc>>,
}

impl CompositorCallback {
    pub fn new(quads: Vec<QuadDesc>) -> Self {
        Self {
            quads: Arc::new(quads),
        }
    }
}

impl egui_wgpu::CallbackTrait for CompositorCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(compositor) = resources.get_mut::<Compositor>() {
            let size = screen_descriptor.size_in_pixels;
            compositor.prepare_quads(device, queue, &self.quads, (size[0], size[1]));
        }
        Vec::new()
    }

    fn paint(
        &self,
        info: eframe::egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(compositor) = resources.get::<Compositor>() {
            compositor.paint(info, render_pass);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::video_frame::{ColorRange, PlaneBuf};

    fn frame() -> VideoFrame {
        VideoFrame::from_planes(
            FrameFormat::Nv12,
            2,
            2,
            [
                PlaneBuf::new(vec![0; 4]),
                PlaneBuf::new(vec![128; 2]),
                PlaneBuf::new(Vec::new()),
            ],
            [2, 2, 0],
            1000,
            ColorRange::Limited,
        )
    }

    #[test]
    fn uniform_matches_shader_layout() {
        assert_eq!(std::mem::size_of::<QuadUniform>(), UNIFORM_SIZE as usize);
        assert_eq!(UNIFORM_SIZE % 16, 0);
        const { assert!(crate::gpu::UNIFORM_STRIDE >= UNIFORM_SIZE) };
    }

    #[test]
    fn uniform_encodes_mode_and_format() {
        let f = frame();
        let u = QuadUniform::build(
            [0.0, 0.0, 1.0, 1.0],
            [10.0, 20.0, 30.0, 40.0],
            &f,
            0.5,
            CompareMode::WipeLine { percent: 0.25 },
            (1920, 1080),
            true,
        );
        assert_eq!(u.rect, [10.0, 20.0, 30.0, 40.0]);
        assert_eq!(u.tex[2], 1.0); // NV12
        assert_eq!(u.tex[3], 0.0); // limited range
        assert_eq!(u.params, [0.5, 2.0, 0.25, 0.0]);
        assert_eq!(u.target, [1920.0, 1080.0, 1.0, 0.0]);
    }

    #[test]
    fn shader_parses_and_validates() {
        let source = include_str!("shader.wgsl");
        let module = naga::front::wgsl::parse_str(source).expect("shader.wgsl must parse");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        );
        validator
            .validate(&module)
            .expect("shader.wgsl must validate");
    }

    #[test]
    fn rect_math_intersects() {
        let a = PixelRect::new(0.0, 0.0, 100.0, 100.0);
        let b = PixelRect::new(50.0, 90.0, 100.0, 100.0);
        assert_eq!(a.intersect(b), PixelRect::new(50.0, 90.0, 50.0, 10.0));
        assert!(a.intersect(PixelRect::new(200.0, 0.0, 10.0, 10.0)).is_empty());
        assert!(PixelRect::new(0.0, 0.0, 0.5, 10.0).is_empty());
    }

    #[test]
    fn compare_mode_flags() {
        assert_eq!(CompareMode::Single.flags(), (0.0, 0.0, 0.0));
        assert_eq!(CompareMode::OnionSkin { mix: 0.5 }.flags(), (1.0, 0.0, 0.5));
        assert_eq!(CompareMode::WipeLine { percent: 1.5 }.flags(), (2.0, 1.0, 0.0));
        assert!(!CompareMode::Single.is_compare());
        assert!(CompareMode::WipeLine { percent: 0.2 }.is_compare());
    }
}

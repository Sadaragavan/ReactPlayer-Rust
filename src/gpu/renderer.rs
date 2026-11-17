//! wgpu resources for the compositor: one pipeline, one texture set per
//! layer, and per-layer uniforms.
//!
//! Textures are re-created only when a layer's frame geometry changes and are
//! re-uploaded only when the frame's `pts_us` changes. Rows are never
//! repacked: FFmpeg's `linesize` is handed to `Queue::write_texture` as
//! `bytes_per_row` and the copy extent is the true plane size. `write_texture`
//! has no 256-byte row-alignment requirement (that rule applies to
//! buffer-to-texture copies), so a stride of, say, 1920 or 1918 bytes is fine.

use std::collections::HashMap;

use eframe::egui::Rect;
use eframe::egui_wgpu::{self, wgpu};
use std::sync::Arc;

use crate::engine::hwaccel::ColorRange;
use crate::engine::video_frame::{PixelFormat, VideoFrame};
use crate::model::CropRect;

use super::COMPOSITE_SHADER;

/// Per-layer uniform block. Must match `struct Uniforms` in `shader.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LayerUniforms {
    /// 0 = YUV420P, 1 = NV12.
    pub format: u32,
    /// 1.0 = limited range, 0.0 = full range.
    pub limited_range: f32,
    /// 1 when the compare branch is enabled.
    pub compare_mode: u32,
    /// 0 = onion skin, 1 = wipe.
    pub compare_kind: u32,
    /// Crop window `(u0, v0, u1, v1)`.
    pub crop: [f32; 4],
    /// Layer opacity.
    pub opacity: f32,
    /// Wipe position `0..1`.
    pub wipe: f32,
    /// Onion-skin weight of layer B.
    pub onion: f32,
    /// Padding, `f32`.
    pub pad0: f32,
}

impl LayerUniforms {
    /// Size in bytes, as declared in the WGSL struct.
    pub const SIZE: usize = 48;

    /// Build uniforms for one draw.
    pub fn new(
        format: PixelFormat,
        range: ColorRange,
        crop: &CropRect,
        opacity: f32,
        compare: Option<CompareKind>,
        wipe: f32,
        onion: f32,
    ) -> Self {
        let (u0, v0) = crop.uv_min();
        let (u1, v1) = crop.uv_max();
        Self {
            format: match format {
                PixelFormat::Yuv420p => 0,
                PixelFormat::Nv12 => 1,
            },
            limited_range: range.shader_flag(),
            compare_mode: u32::from(compare.is_some()),
            compare_kind: match compare {
                Some(CompareKind::Wipe) => 1,
                _ => 0,
            },
            crop: [u0, v0, u1, v1],
            opacity: opacity.clamp(0.0, 1.0),
            wipe: wipe.clamp(0.0, 1.0),
            onion: onion.clamp(0.0, 1.0),
            pad0: 0.0,
        }
    }
}

/// Per-layer-B uniform block. Must match `struct CmpUniforms` in `shader.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CompareUniforms {
    /// 0 = YUV420P, 1 = NV12.
    pub format: u32,
    /// 1.0 = limited range, 0.0 = full range.
    pub limited_range: f32,
    /// Opacity of layer B.
    pub opacity: f32,
    /// Padding, `f32`.
    pub pad0: f32,
    /// Crop window of layer B.
    pub crop: [f32; 4],
}

impl CompareUniforms {
    /// Size in bytes, as declared in the WGSL struct.
    pub const SIZE: usize = 32;

    /// Build uniforms for the compare texture set.
    pub fn new(format: PixelFormat, range: ColorRange, crop: &CropRect) -> Self {
        let (u0, v0) = crop.uv_min();
        let (u1, v1) = crop.uv_max();
        Self {
            format: match format {
                PixelFormat::Yuv420p => 0,
                PixelFormat::Nv12 => 1,
            },
            limited_range: range.shader_flag(),
            opacity: 1.0,
            pad0: 0.0,
            crop: [u0, v0, u1, v1],
        }
    }
}

/// How the Alignment Studio compares the two captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareKind {
    /// Cross-fade between A and B.
    Onion,
    /// Split screen with an accent divider.
    Wipe,
}

impl CompareKind {
    /// Label for the UI toggle.
    pub fn label(self) -> &'static str {
        match self {
            CompareKind::Onion => "Onion",
            CompareKind::Wipe => "Wipe",
        }
    }
}

/// A decoded frame ready to be uploaded.
#[derive(Clone)]
pub struct FrameUpload {
    /// Decoded planes, shared with the engine so no CPU copy is made here.
    pub frame: Arc<VideoFrame>,
    /// Colour range the frame was decoded in.
    pub range: ColorRange,
}

impl std::fmt::Debug for FrameUpload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameUpload")
            .field("pts_us", &self.frame.pts_us)
            .field("size", &(self.frame.width, self.frame.height))
            .field("range", &self.range)
            .finish()
    }
}

/// The second capture of an alignment comparison.
#[derive(Clone, Debug)]
pub struct CompareUpload {
    /// Cache key for layer B's textures.
    pub key: u64,
    /// The frame.
    pub frame: FrameUpload,
    /// Layer B's crop window.
    pub crop: CropRect,
    /// Onion weight / wipe position, `0..1`.
    pub t: f32,
    /// Onion or wipe.
    pub kind: CompareKind,
}

/// One layer draw. Each becomes an egui paint callback, so egui sets the
/// render-pass viewport to exactly this layer's rectangle.
#[derive(Clone, Debug)]
pub struct LayerDraw {
    /// The layer this draw belongs to; the texture cache key.
    pub layer_id: u64,
    /// Rectangle on screen, in points.
    pub rect: Rect,
    /// Layer crop.
    pub crop: CropRect,
    /// Layer opacity.
    pub opacity: f32,
    /// The frame to draw.
    pub upload: Option<FrameUpload>,
    /// Optional alignment comparison.
    pub compare: Option<CompareUpload>,
}

impl LayerDraw {
    /// A draw with no frame yet.
    pub fn empty(layer_id: u64, rect: Rect) -> Self {
        Self {
            layer_id,
            rect,
            crop: CropRect::none(),
            opacity: 1.0,
            upload: None,
            compare: None,
        }
    }

    /// True when there is nothing to put on the GPU.
    pub fn is_empty(&self) -> bool {
        self.upload.is_none() && self.compare.is_none()
    }

    /// Crop to send to the shader. A wipe compares two differently-cropped
    /// sources, so the crop is folded in through the compare uniforms.
    fn primary_crop(&self) -> CropRect {
        self.crop
    }
}

/// Which cache a texture set belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheKind {
    /// The primary layer.
    Layer,
    /// Layer B in an alignment comparison.
    Compare,
}

/// One plane texture plus the size it was created for.
struct PlaneTexture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
}

impl PlaneTexture {
    fn new(device: &wgpu::Device, width: u32, height: u32, format: wgpu::TextureFormat) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("reaction-player plane"),
            size: wgpu::Extent3d {
                width,
                height,
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
        Self {
            texture,
            view,
            width,
            height,
            format,
        }
    }
}

/// Cached GPU state for one texture set (a layer, or layer B in a compare).
struct TextureSet {
    /// Textures for the three planes; `None` entries are unused planes.
    planes: [Option<PlaneTexture>; 3],
    /// The last `pts_us` written into the textures.
    uploaded_pts: Option<i64>,
    /// The pixel format the textures were created for.
    format: Option<PixelFormat>,
}

impl TextureSet {
    fn new() -> Self {
        Self {
            planes: [None, None, None],
            uploaded_pts: None,
            format: None,
        }
    }
}

/// Cached GPU state for one layer: uniforms, textures, and bind group.
struct LayerGpu {
    uniforms: wgpu::Buffer,
    set: TextureSet,
    bind_group: Option<wgpu::BindGroup>,
}

/// Everything the compositor owns on the GPU. Installed once into
/// `egui_wgpu::Renderer::callback_resources`, which is why the pipeline (and
/// therefore the render pass) can be borrowed for `'static`.
pub struct CompositorResources {
    pipeline: wgpu::RenderPipeline,
    layer_layout: wgpu::BindGroupLayout,
    compare_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    layers: HashMap<u64, LayerGpu>,
    compares: HashMap<u64, LayerGpu>,
    /// 1x1 stand-ins bound at group 1 when no comparison is active.
    dummy_views: [wgpu::TextureView; 3],
    dummy_compare_group: wgpu::BindGroup,
    /// Texture uploads performed (diagnostics).
    pub uploads: u64,
}

impl std::fmt::Debug for CompositorResources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompositorResources")
            .field("cached_layers", &self.layers.len())
            .field("cached_compares", &self.compares.len())
            .field("uploads", &self.uploads)
            .finish()
    }
}

impl CompositorResources {
    /// Build the pipeline and register it in `render_state`.
    ///
    /// Called once, from the eframe creation callback, where
    /// `render_state.renderer` is available.
    pub fn install(render_state: &egui_wgpu::RenderState) -> Self {
        let device = &render_state.device;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("reaction-player compositor"),
            source: wgpu::ShaderSource::Wgsl(COMPOSITE_SHADER.into()),
        });

        // The layout is format-agnostic: `TextureSampleType::Float` already
        // accepts R8/Rg8 NV12 planes and RGBA, so no per-plane format is
        // needed here.
        let texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let uniform_entry = |binding: u32, size: usize| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(size as u64),
            },
            count: None,
        };
        let sampler_entry = wgpu::BindGroupLayoutEntry {
            binding: 4,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };

        let layer_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("reaction-player layer layout"),
            entries: &[
                uniform_entry(0, LayerUniforms::SIZE),
                texture_entry(1),
                texture_entry(2),
                texture_entry(3),
                sampler_entry,
            ],
        });
        let compare_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("reaction-player compare layout"),
            entries: &[
                uniform_entry(0, CompareUniforms::SIZE),
                texture_entry(1),
                texture_entry(2),
                texture_entry(3),
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("reaction-player pipeline layout"),
            bind_group_layouts: &[Some(&layer_layout), Some(&compare_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("reaction-player compositor"),
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
                    format: render_state.target_format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("reaction-player linear"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let dummy_views = [
            PlaneTexture::new(device, 1, 1, wgpu::TextureFormat::R8Unorm),
            PlaneTexture::new(device, 1, 1, wgpu::TextureFormat::R8Unorm),
            PlaneTexture::new(device, 1, 1, wgpu::TextureFormat::R8Unorm),
        ];
        let dummy_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("reaction-player dummy compare uniforms"),
            size: CompareUniforms::SIZE as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let dummy_compare_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("reaction-player dummy compare group"),
            layout: &compare_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: dummy_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&dummy_views[0].view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&dummy_views[1].view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&dummy_views[2].view),
                },
            ],
        });

        let views = [
            dummy_views[0].view.clone(),
            dummy_views[1].view.clone(),
            dummy_views[2].view.clone(),
        ];

        Self {
            pipeline,
            layer_layout,
            compare_layout,
            sampler,
            layers: HashMap::new(),
            compares: HashMap::new(),
            dummy_views: views,
            dummy_compare_group,
            uploads: 0,
        }
    }

    /// Number of cached layer texture sets (diagnostic hook).
    pub fn cached_layers(&self) -> usize {
        self.layers.len()
    }

    /// Drop cached state for layers that no longer exist.
    pub fn retain_layers(&mut self, live: &[u64]) {
        self.layers.retain(|id, _| live.contains(id));
        self.compares.retain(|id, _| live.contains(id));
    }

    /// Create the GPU state for a new layer: its uniform buffer and an empty
    /// texture set. Must be called with the layer absent from `layers`.
    fn layer_entry(&mut self, device: &wgpu::Device, key: u64) -> &mut LayerGpu {
        self.layers.entry(key).or_insert_with(|| LayerGpu {
            uniforms: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reaction-player layer uniforms"),
                size: LayerUniforms::SIZE as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            set: TextureSet::new(),
            bind_group: None,
        })
    }

    /// Create the GPU state for a comparison capture, keyed by the capture key.
    fn compare_entry(&mut self, device: &wgpu::Device, key: u64) -> &mut LayerGpu {
        self.compares.entry(key).or_insert_with(|| LayerGpu {
            uniforms: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reaction-player compare uniforms"),
                size: CompareUniforms::SIZE as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            set: TextureSet::new(),
            bind_group: None,
        })
    }

    /// Make sure the cached texture set for `kind`/`key` matches `frame`, and
    /// upload the planes if its pts changed.
    ///
    /// Returns true when the textures were re-created, which means any bind
    /// group built from the old views is stale.
    fn stage_textures(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        kind: CacheKind,
        key: u64,
        frame: &FrameUpload,
    ) -> bool {
        // The entry borrow ends before `upload_frame` runs, so `self` is free
        // again for the next call; `upload_frame` is an associated function
        // precisely so it does not borrow `self`.
        let entry = match kind {
            CacheKind::Layer => self.layer_entry(device, key),
            CacheKind::Compare => self.compare_entry(device, key),
        };
        Self::upload_frame(device, queue, &mut entry.set, &frame.frame)
    }

    /// Ensure `set` has textures matching `frame`'s format and size, creating
    /// or re-creating them when either changed, and upload the planes if the
    /// frame's `pts_us` differs from the last upload.
    ///
    /// Returns true when the textures were re-created, which invalidates any
    /// bind group holding the old views.
    fn upload_frame(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        set: &mut TextureSet,
        frame: &VideoFrame,
    ) -> bool {
        let extents = frame.plane_extents();
        let mut recreated = false;
        if set.format != Some(frame.format) {
            set.planes = [None, None, None];
            set.format = Some(frame.format);
            set.uploaded_pts = None;
            recreated = true;
        }
        for plane in 0..frame.format.plane_count() {
            let (w, h) = extents[plane];
            if w == 0 || h == 0 {
                continue;
            }
            // NV12 chroma is an Rg8 texture: one texel covers a 2x2 luma
            // block, so its width is already the chroma width.
            let texture_format = if frame.format == PixelFormat::Nv12 && plane == 1 {
                wgpu::TextureFormat::Rg8Unorm
            } else {
                wgpu::TextureFormat::R8Unorm
            };
            let needs_new = match set.planes[plane].as_ref() {
                Some(t) => t.width != w || t.height != h || t.format != texture_format,
                None => true,
            };
            if needs_new {
                set.planes[plane] = Some(PlaneTexture::new(device, w, h, texture_format));
                recreated = true;
                set.uploaded_pts = None;
            }
        }
        if set.uploaded_pts == Some(frame.pts_us) {
            return recreated;
        }
        for plane in 0..frame.format.plane_count() {
            let Some(texture) = set.planes[plane].as_ref() else {
                continue;
            };
            let (stride, len) = frame.plane_layout(plane);
            let data = frame.planes[plane].as_slice();
            if stride == 0 || len == 0 || data.len() < len {
                continue;
            }
            // `write_texture` takes the row stride as `bytes_per_row` and has
            // no 256-byte alignment requirement, which is exactly why rows are
            // never repacked on the way in.
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &data[..len],
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride as u32),
                    rows_per_image: Some(texture.height),
                },
                wgpu::Extent3d {
                    width: texture.width,
                    height: texture.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        set.uploaded_pts = Some(frame.pts_us);
        recreated
    }

    /// Rebuild the bind group for a layer if its textures changed.
    fn refresh_layer_bind_group(&mut self, device: &wgpu::Device, id: u64) {
        let Some(entry) = self.layers.get(&id) else {
            return;
        };
        let Some(views) = views_of(&entry.set) else {
            return;
        };
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("reaction-player layer bind group"),
            layout: &self.layer_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: entry.uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&views[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&views[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&views[2]),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        if let Some(entry) = self.layers.get_mut(&id) {
            entry.bind_group = Some(group);
        }
    }

    /// Create the compare bind group and uniforms for layer B.
    fn refresh_compare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        compare: &CompareUpload,
    ) -> Option<()> {
        let frame = &compare.frame;
        self.stage_textures(device, queue, CacheKind::Compare, compare.key, frame);
        let entry = self.compares.get(&compare.key)?;
        let views = views_of(&entry.set)?;
        let uniforms = CompareUniforms::new(
            frame.frame.format,
            frame.range,
            &compare.crop,
        );
        queue.write_buffer(&entry.uniforms, 0, bytemuck::bytes_of(&uniforms));
        let uniforms = entry.uniforms.clone();
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("reaction-player compare bind group"),
            layout: &self.compare_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&views[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&views[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&views[2]),
                },
            ],
        });
        if let Some(entry) = self.compares.get_mut(&compare.key) {
            entry.bind_group = Some(group);
        }
        Some(())
    }

    /// Upload everything a draw needs and build its bind groups.
    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, draw: &LayerDraw) {
        let Some(upload) = draw.upload.as_ref() else {
            return;
        };
        let changed = self.stage_textures(device, queue, CacheKind::Layer, draw.layer_id, upload);
        let needs_group = {
            let entry = self.layers.get(&draw.layer_id);
            match entry {
                Some(e) => e.bind_group.is_none() || changed,
                None => true,
            }
        };
        if needs_group {
            self.refresh_layer_bind_group(device, draw.layer_id);
        }

        let uniforms = LayerUniforms::new(
            upload.frame.format,
            upload.range,
            &draw.primary_crop(),
            draw.opacity,
            draw.compare.as_ref().map(|c| c.kind),
            draw.compare.as_ref().map(|c| c.t).unwrap_or(0.0),
            draw.compare.as_ref().map(|c| c.t).unwrap_or(0.0),
        );
        if let Some(entry) = self.layers.get(&draw.layer_id) {
            queue.write_buffer(&entry.uniforms, 0, bytemuck::bytes_of(&uniforms));
        }

        if let Some(compare) = draw.compare.as_ref() {
            self.refresh_compare(device, queue, compare);
        }
    }

    /// Issue the draw for one layer into egui's render pass.
    pub fn paint(&self, draw: &LayerDraw, render_pass: &mut wgpu::RenderPass<'static>) {
        let Some(entry) = self.layers.get(&draw.layer_id) else {
            return;
        };
        let Some(group) = entry.bind_group.as_ref() else {
            return;
        };
        render_pass.set_pipeline(&self.pipeline);
        render_pass.set_bind_group(0, group, &[]);
        match draw.compare.as_ref().and_then(|c| self.compares.get(&c.key)) {
            Some(cmp) => {
                if let Some(g) = cmp.bind_group.as_ref() {
                    render_pass.set_bind_group(1, g, &[]);
                } else {
                    render_pass.set_bind_group(1, &self.dummy_compare_group, &[]);
                }
            }
            None => render_pass.set_bind_group(1, &self.dummy_compare_group, &[]),
        }
        render_pass.draw(0..6, 0..1);
    }
}

/// The three plane views of a texture set, or `None` if it is not populated.
fn views_of(set: &TextureSet) -> Option<[wgpu::TextureView; 3]> {
    Some([
        set.planes[0].as_ref()?.view.clone(),
        set.planes[1].as_ref()?.view.clone(),
        // NV12 has no third plane; fall back to the chroma plane so the bind
        // group is always complete (the shader never samples it). `or` works
        // on the `Option` itself, so no `?` inside a closure is needed.
        set.planes[2]
            .as_ref()
            .or(set.planes[1].as_ref())?
            .view
            .clone(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui;

    #[test]
    fn uniform_sizes_match_shader_layout() {
        // WGSL: u32, f32, u32, u32 (16) + vec4<f32> (16) + 4 * f32 (16) = 48.
        assert_eq!(LayerUniforms::SIZE, 48);
        assert_eq!(std::mem::size_of::<LayerUniforms>(), 48);
        assert_eq!(
            std::mem::align_of::<LayerUniforms>().max(4),
            std::mem::align_of::<f32>()
        );
        // WGSL: 4 * 4 bytes + vec4<f32> (16) = 32.
        assert_eq!(CompareUniforms::SIZE, 32);
        assert_eq!(std::mem::size_of::<CompareUniforms>(), 32);
    }

    #[test]
    fn uniforms_encode_format_and_range() {
        let u = LayerUniforms::new(
            PixelFormat::Nv12,
            ColorRange::Full,
            &CropRect::none(),
            1.0,
            None,
            0.0,
            0.0,
        );
        assert_eq!(u.format, 1);
        assert_eq!(u.limited_range, 0.0);
        assert_eq!(u.crop, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(u.compare_mode, 0);

        let u = LayerUniforms::new(
            PixelFormat::Yuv420p,
            ColorRange::Limited,
            &CropRect::none(),
            0.5,
            Some(CompareKind::Wipe),
            0.25,
            0.0,
        );
        assert_eq!(u.format, 0);
        assert_eq!(u.limited_range, 1.0);
        assert_eq!(u.compare_mode, 1);
        assert_eq!(u.compare_kind, 1);
        assert_eq!(u.opacity, 0.5);
        assert_eq!(u.wipe, 0.25);
    }

    #[test]
    fn crop_window_reaches_uniforms() {
        let crop = CropRect {
            left: 0.25,
            top: 0.1,
            right: 0.25,
            bottom: 0.1,
        };
        let u = CompareUniforms::new(PixelFormat::Yuv420p, ColorRange::Limited, &crop);
        assert_eq!(u.crop, [0.25, 0.1, 0.75, 0.9]);
    }

    #[test]
    fn uniforms_are_pod() {
        let u = LayerUniforms::new(
            PixelFormat::Yuv420p,
            ColorRange::Limited,
            &CropRect::none(),
            1.0,
            None,
            0.0,
            0.0,
        );
        let bytes: &[u8] = bytemuck::bytes_of(&u);
        assert_eq!(bytes.len(), LayerUniforms::SIZE);
    }

    #[test]
    fn opacity_and_wipe_are_clamped() {
        let u = LayerUniforms::new(
            PixelFormat::Yuv420p,
            ColorRange::Limited,
            &CropRect::none(),
            7.0,
            Some(CompareKind::Onion),
            1.5,
            -1.0,
        );
        assert_eq!(u.opacity, 1.0);
        assert_eq!(u.wipe, 1.0);
        assert_eq!(u.onion, 0.0);
    }

    #[test]
    fn compare_kind_labels() {
        assert_eq!(CompareKind::Onion.label(), "Onion");
        assert_eq!(CompareKind::Wipe.label(), "Wipe");
    }

    #[test]
    fn empty_draw_reports_empty() {
        let d = LayerDraw::empty(3, Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0)));
        assert!(d.is_empty());
        assert_eq!(d.primary_crop(), CropRect::none());
    }
}

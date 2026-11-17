//! Decoded frames in system memory, exactly as the decoder produced them.
//!
//! The playback path never converts pixels: a frame keeps FFmpeg's `linesize`
//! as its stride (padding included) and the three planes are copied once into
//! pooled buffers. The GPU samples the planes directly and knows their true
//! extent, so the padding rows are never sampled.
//!
//! Memory: a 1920x1080 NV12 frame is `1920*1080 + 1920*1080/2` = 3.11 MB, and
//! one YUV420P frame is the same. Fifteen of those per layer is ~47 MB, so four
//! layers of backward history is at worst ~187 MB plus the 5-frame render
//! queues (~60 MB) - see `docs/ARCHITECTURE.md`.

use std::ops::Deref;
use std::sync::{Arc, Mutex};

/// Planes in a frame: Y, U, V (the third slot is unused for NV12).
pub const MAX_PLANES: usize = 3;

/// Decoded frames kept per track for instant backward stepping.
pub const HISTORY_FRAMES: usize = 15;

/// Frames in flight between the decode worker and the render thread.
pub const QUEUE_FRAMES: usize = 6;

/// The two layouts the playback path understands natively.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameFormat {
    /// Planar 8-bit 4:2:0: three `R8Unorm` textures.
    Yuv420p,
    /// Semi-planar 8-bit 4:2:0: `R8Unorm` + `Rg8Unorm`.
    Nv12,
}

impl FrameFormat {
    pub fn plane_count(self) -> usize {
        match self {
            Self::Yuv420p => 3,
            Self::Nv12 => 2,
        }
    }

    /// The badge shown on a layer in the UI.
    pub fn badge(self) -> &'static str {
        match self {
            Self::Yuv420p => "YUV420P",
            Self::Nv12 => "NV12",
        }
    }

    /// The format flag the shader switches on: 0 = YUV420P, 1 = NV12.
    pub fn shader_flag(self) -> f32 {
        match self {
            Self::Yuv420p => 0.0,
            Self::Nv12 => 1.0,
        }
    }

    /// Chroma plane dimensions for a `width` x `height` frame.
    pub fn chroma_dims(self, width: u32, height: u32) -> (u32, u32) {
        (width.div_ceil(2), height.div_ceil(2))
    }
}

/// BT.709 studio (limited) or full range. Comes from the decoder's
/// `color_range`, so the CPU and GPU paths agree on the maths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorRange {
    Limited,
    Full,
}

impl ColorRange {
    /// 0 = limited (16..235 / 16..240), 1 = full (0..255).
    pub fn shader_flag(self) -> f32 {
        match self {
            Self::Limited => 0.0,
            Self::Full => 1.0,
        }
    }
}

/// One plane's pixels in a pooled allocation.
pub struct PlaneBuf {
    data: Vec<u8>,
}

impl PlaneBuf {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    pub fn capacity(&self) -> usize {
        self.data.capacity()
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.data
    }
}

impl Deref for PlaneBuf {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

/// A decoded frame. Cheap to share: wrap it in an [`Arc`]; wrapping is how the
/// decode worker hands frames to the render thread without copying.
pub struct VideoFrame {
    pub format: FrameFormat,
    pub width: u32,
    pub height: u32,
    pub planes: [PlaneBuf; MAX_PLANES],
    /// True pixel extent of each plane, padding excluded.
    pub plane_sizes: [(u32, u32); MAX_PLANES],
    /// FFmpeg's `linesize`, kept as-is.
    pub strides: [usize; MAX_PLANES],
    /// Presentation timestamp in microseconds on the source's own timeline.
    pub pts_us: i64,
    pub range: ColorRange,
    /// Set when this frame had to go through swscale (10-bit, 4:2:2, ...).
    pub converted: bool,
}

impl VideoFrame {
    /// Build a frame from planes that already have their final layout. The
    /// caller is responsible for `planes[i].len() >= strides[i] * rows[i]`.
    pub fn from_planes(
        format: FrameFormat,
        width: u32,
        height: u32,
        planes: [PlaneBuf; MAX_PLANES],
        strides: [usize; MAX_PLANES],
        pts_us: i64,
        range: ColorRange,
    ) -> Self {
        let chroma = format.chroma_dims(width, height);
        let plane_sizes = match format {
            FrameFormat::Yuv420p => [(width, height), chroma, chroma],
            FrameFormat::Nv12 => [(width, height), chroma, (0, 0)],
        };
        Self {
            format,
            width,
            height,
            planes,
            plane_sizes,
            strides,
            pts_us,
            range,
            converted: false,
        }
    }

    /// Rows of real pixels in plane `i` (0 for planes that do not exist).
    pub fn rows(&self, i: usize) -> usize {
        self.plane_sizes[i].1 as usize
    }

    /// Bytes the decoder wrote for plane `i`, stride included.
    pub fn plane_bytes(&self, i: usize) -> usize {
        self.rows(i) * self.strides[i]
    }

    /// The plane's bytes, trimmed to `stride * rows`.
    pub fn plane(&self, i: usize) -> &[u8] {
        let n = self.plane_bytes(i).min(self.planes[i].len());
        &self.planes[i][..n]
    }

    /// Total bytes of pixel payload, used for the layer's memory badge and by
    /// the buffer pool accounting.
    pub fn byte_len(&self) -> usize {
        (0..self.format.plane_count())
            .map(|i| self.plane_bytes(i))
            .sum()
    }

    pub fn duration_us(&self) -> i64 {
        0
    }

    fn luma(&self, x: u32, y: u32) -> f32 {
        let stride = self.strides[0];
        let idx = y as usize * stride + x as usize;
        f32::from(self.planes[0].get(idx).copied().unwrap_or(16))
    }

    /// Chroma at (x, y) in luma coordinates. Nearest neighbour: one sample per
    /// 2x2 block, which is what the source actually carries.
    fn chroma(&self, x: u32, y: u32) -> (f32, f32) {
        let cx = (x / 2) as usize;
        let cy = (y / 2) as usize;
        match self.format {
            FrameFormat::Yuv420p => {
                let u = cy * self.strides[1] + cx;
                let v = cy * self.strides[2] + cx;
                (
                    f32::from(self.planes[1].get(u).copied().unwrap_or(128)),
                    f32::from(self.planes[2].get(v).copied().unwrap_or(128)),
                )
            }
            FrameFormat::Nv12 => {
                let i = cy * self.strides[1] + cx * 2;
                (
                    f32::from(self.planes[1].get(i).copied().unwrap_or(128)),
                    f32::from(self.planes[1].get(i + 1).copied().unwrap_or(128)),
                )
            }
        }
    }

    /// One pixel in RGBA8, BT.709, honoring the frame's range flag.
    pub fn pixel_rgba(&self, x: u32, y: u32) -> [u8; 4] {
        let (u, v) = self.chroma(x, y);
        let [r, g, b] = bt709_to_rgb8(self.luma(x, y), u, v, self.range);
        [r, g, b, 255]
    }

    /// Crop-and-convert for the clipboard and thumbnails. `crop` is in pixels
    /// of the decoded frame (already resolved from the layer's normalized
    /// crop). The CPU path may repack rows; the playback path never does.
    pub fn to_rgba_cropped(&self, crop: Option<CropPx>) -> RgbaBuffer {
        let crop = crop.unwrap_or(CropPx {
            x: 0,
            y: 0,
            w: self.width,
            h: self.height,
        });
        let crop = crop.clamped(self.width, self.height);
        let mut data = Vec::with_capacity(crop.w as usize * crop.h as usize * 4);
        for y in 0..crop.h {
            for x in 0..crop.w {
                data.extend_from_slice(&self.pixel_rgba(crop.x + x, crop.y + y));
            }
        }
        RgbaBuffer {
            width: crop.w,
            height: crop.h,
            data,
        }
    }
}

impl std::fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoFrame")
            .field("format", &self.format)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("strides", &self.strides)
            .field("pts_us", &self.pts_us)
            .field("converted", (&self.converted))
            .finish()
    }
}

/// A rectangle in pixels of the decoded frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropPx {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl CropPx {
    pub fn clamped(self, frame_w: u32, frame_h: u32) -> Self {
        let x = self.x.min(frame_w.saturating_sub(1));
        let y = self.y.min(frame_h.saturating_sub(1));
        let w = self.w.max(1).min(frame_w - x);
        let h = self.h.max(1).min(frame_h - y);
        Self { x, y, w, h }
    }
}

/// An RGBA8 buffer in row-major order with no padding.
pub struct RgbaBuffer {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl RgbaBuffer {
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ]
    }
}

/// BT.709 YUV -> RGB on 0..255 samples. `y` is 16..235 and `u`/`v` are 16..240
/// when the frame is studio range, matching the shader's arithmetic exactly.
pub fn bt709_to_rgb8(y: f32, u: f32, v: f32, range: ColorRange) -> [u8; 3] {
    let (y, u, v) = match range {
        ColorRange::Limited => (
            (y - 16.0) / 219.0,
            (u - 128.0) / 224.0,
            (v - 128.0) / 224.0,
        ),
        ColorRange::Full => (y / 255.0, (u - 128.0) / 255.0, (v - 128.0) / 255.0),
    };
    let r = y + 1.574_8 * v;
    let g = y - 0.187_3 * u - 0.468_1 * v;
    let b = y + 1.855_6 * u;
    [to_u8(r), to_u8(g), to_u8(b)]
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Recycles plane allocations so steady-state playback does no allocation per
/// frame. One pool per track; the decode worker allocates from it and the
/// render thread returns frames to it once they are no longer shown.
pub struct BufferPool {
    free: Mutex<Vec<Vec<u8>>>,
    limit: usize,
}

impl BufferPool {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            free: Mutex::new(Vec::with_capacity(limit)),
            limit,
        })
    }

    /// A buffer of at least `len` bytes, reusing a pooled allocation when one
    /// is available.
    pub fn alloc(&self, len: usize) -> PlaneBuf {
        let mut free = match self.free.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(mut buf) = free.pop() {
            buf.resize(len, 0);
            return PlaneBuf::new(buf);
        }
        PlaneBuf::new(vec![0u8; len])
    }

    /// Give a frame's buffers back. Callers hold frames behind `Arc`, so this
    /// takes the [`VideoFrame`] once the `Arc` is uniquely owned.
    pub fn recycle(&self, frame: VideoFrame) {
        let mut free = match self.free.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if free.len() >= self.limit {
            return;
        }
        for plane in frame.planes {
            let buf = plane.into_inner();
            if free.len() < self.limit {
                free.push(buf);
            }
        }
    }

    pub fn pooled(&self) -> usize {
        match self.free.lock() {
            Ok(g) => g.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4x4 luma ramp with flat chroma, written with `stride` > width.
    fn frame(format: FrameFormat, stride: usize) -> VideoFrame {
        let (w, h) = (8u32, 8u32);
        let mut y = vec![0u8; stride * h as usize];
        for row in 0..h as usize {
            for col in 0..w as usize {
                y[row * stride + col] = (col * 16 + row * 4) as u8;
            }
        }
        let (cw, ch) = format.chroma_dims(w, h);
        let cstride = stride / 2 + 4;
        let mut u = vec![0u8; cstride * ch as usize];
        let mut v = vec![0u8; cstride * ch as usize];
        for row in 0..ch as usize {
            for col in 0..cw as usize {
                if format == FrameFormat::Nv12 {
                    u[row * cstride + col * 2] = 90;
                    u[row * cstride + col * 2 + 1] = 200;
                } else {
                    u[row * cstride + col] = 90;
                    v[row * cstride + col] = 200;
                }
            }
        }
        let planes = [
            PlaneBuf::new(y),
            PlaneBuf::new(u),
            PlaneBuf::new(v),
        ];
        VideoFrame::from_planes(
            format,
            w,
            h,
            planes,
            [stride, cstride, cstride],
            0,
            ColorRange::Limited,
        )
    }

    #[test]
    fn strides_are_not_repacked() {
        let f = frame(FrameFormat::Nv12, 24);
        assert_eq!(f.strides[0], 24);
        assert_eq!(f.plane_sizes[0], (8, 8));
        assert_eq!(f.plane_bytes(0), 24 * 8);
        assert_eq!(f.plane(0).len(), 24 * 8);
        // Padding columns are inside the slice but never sampled.
        assert_eq!(f.plane(0)[8], 0);
    }

    #[test]
    fn nv12_and_yuv420p_agree() {
        let a = frame(FrameFormat::Yuv420p, 16);
        let b = frame(FrameFormat::Nv12, 16);
        for (x, y) in [(0, 0), (3, 2), (7, 7), (5, 1)] {
            assert_eq!(a.pixel_rgba(x, y), b.pixel_rgba(x, y), "at {x},{y}");
        }
    }

    #[test]
    fn chroma_planes_have_ceil_half_size() {
        assert_eq!(FrameFormat::Nv12.chroma_dims(1921, 1081), (961, 541));
        assert_eq!(FrameFormat::Yuv420p.chroma_dims(1920, 1080), (960, 540));
        assert_eq!(FrameFormat::Nv12.plane_count(), 2);
        assert_eq!(FrameFormat::Yuv420p.plane_count(), 3);
        assert_eq!(FrameFormat::Yuv420p.shader_flag(), 0.0);
        assert_eq!(FrameFormat::Nv12.shader_flag(), 1.0);
    }

    #[test]
    fn bt709_matches_known_values() {
        // Studio-range white and black.
        assert_eq!(bt709_to_rgb8(235.0, 128.0, 128.0, ColorRange::Limited), [255, 255, 255]);
        assert_eq!(bt709_to_rgb8(16.0, 128.0, 128.0, ColorRange::Limited), [0, 0, 0]);
        // Full-range white and black.
        assert_eq!(bt709_to_rgb8(255.0, 128.0, 128.0, ColorRange::Full), [255, 255, 255]);
        assert_eq!(bt709_to_rgb8(0.0, 128.0, 128.0, ColorRange::Full), [0, 0, 0]);
        // Pure red in studio range is Y=63, U=102, V=240 for BT.709.
        let [r, g, b] = bt709_to_rgb8(63.0, 102.0, 240.0, ColorRange::Limited);
        assert!(r > 240, "red channel {r}");
        assert!(g < 40, "green channel {g}");
        assert!(b < 40, "blue channel {b}");
    }

    #[test]
    fn crop_uses_frame_coordinates_and_respects_stride() {
        let f = frame(FrameFormat::Yuv420p, 20);
        let full = f.to_rgba_cropped(None);
        assert_eq!((full.width, full.height), (8, 8));
        assert_eq!(full.pixel(0, 0), f.pixel_rgba(0, 0));

        let cropped = f.to_rgba_cropped(Some(CropPx { x: 2, y: 3, w: 3, h: 2 }));
        assert_eq!((cropped.width, cropped.height), (3, 2));
        assert_eq!(cropped.pixel(0, 0), f.pixel_rgba(2, 3));
        assert_eq!(cropped.pixel(2, 1), f.pixel_rgba(4, 4));
    }

    #[test]
    fn crop_is_clamped_inside_the_frame() {
        let f = frame(FrameFormat::Nv12, 16);
        let c = f.to_rgba_cropped(Some(CropPx { x: 6, y: 6, w: 40, h: 40 }));
        assert_eq!((c.width, c.height), (2, 2));
        let z = f.to_rgba_cropped(Some(CropPx { x: 0, y: 0, w: 0, h: 0 }));
        assert_eq!(z.width, 1);
    }

    #[test]
    fn pool_reuses_allocations() {
        let pool = BufferPool::new(8);
        let buf = pool.alloc(4096);
        assert_eq!(buf.len(), 4096);
        let f = VideoFrame::from_planes(
            FrameFormat::Nv12,
            32,
            32,
            [buf, pool.alloc(512), pool.alloc(0)],
            [32, 32, 0],
            0,
            ColorRange::Full,
        );
        assert!(pool.pooled() == 0);
        let len = f.byte_len();
        assert_eq!(len, 32 * 32 + 32 * 16);
        pool.recycle(f);
        assert_eq!(pool.pooled(), 3);
        let reused = pool.alloc(1024);
        assert_eq!(reused.len(), 1024);
        assert!(reused.capacity() >= 1024);
    }

    #[test]
    fn history_budget_is_documented() {
        // 1920x1080 4:2:0 is 1.5 bytes per pixel; 15 frames per layer.
        let per_frame = 1920 * 1080 * 3 / 2;
        assert_eq!(per_frame, 3_110_400);
        assert_eq!(per_frame * HISTORY_FRAMES / 1_000_000, 46);
        assert_eq!(per_frame * HISTORY_FRAMES * 4 / 1_000_000, 186);
    }
}

//! Decoded-frame representation.
//!
//! Frames keep FFmpeg's `linesize` as their stride and are copied **once** into
//! pooled plane buffers; rows are never repacked. The GPU upload passes the
//! stride directly as `bytes_per_row` and copies only the true plane extent,
//! so padding is never sampled.
//!
//! The CPU conversion here exists only for the "copy cropped frame" feature and
//! for thumbnails -- the playback path never converts to RGBA on the CPU.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use super::hwaccel::ColorRange;
use crate::error::{AppError, AppResult};
use crate::model::CropRect;

/// Pixel layout of a decoded frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    /// Planar 4:2:0 (Y, U, V).
    Yuv420p,
    /// Semi-planar 4:2:0 (Y, interleaved UV).
    Nv12,
}

impl PixelFormat {
    /// Number of planes used.
    pub fn plane_count(self) -> usize {
        match self {
            PixelFormat::Yuv420p => 3,
            PixelFormat::Nv12 => 2,
        }
    }

    /// Human readable label.
    pub fn label(self) -> &'static str {
        match self {
            PixelFormat::Yuv420p => "YUV420P",
            PixelFormat::Nv12 => "NV12",
        }
    }
}

/// A pool of reusable `Vec<u8>` plane buffers.
///
/// Frames hold an `Arc<PlanePool>`; dropping a [`PlaneBuf`] returns its
/// allocation to the pool, which is what makes steady-state playback
/// allocation free.
#[derive(Debug, Default)]
pub struct PlanePool {
    free: Mutex<Vec<Vec<u8>>>,
    /// Total allocations made (for tests / diagnostics).
    allocations: Mutex<usize>,
}

impl PlanePool {
    /// Create an empty pool.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Number of buffers currently parked in the pool.
    pub fn free_count(&self) -> usize {
        self.free.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// Total allocations ever performed by this pool.
    pub fn allocation_count(&self) -> usize {
        self.allocations.lock().map(|g| *g).unwrap_or(0)
    }

    fn take(&self, len: usize) -> Vec<u8> {
        if let Ok(mut free) = self.free.lock()
            && let Some(pos) = free.iter().position(|b| b.len() >= len)
        {
            let mut buf = free.swap_remove(pos);
            buf.truncate(len);
            return buf;
        }
        if let Ok(mut count) = self.allocations.lock() {
            *count += 1;
        }
        vec![0u8; len]
    }

    fn give(&self, buf: Vec<u8>) {
        if let Ok(mut free) = self.free.lock() {
            if free.len() < 48 {
                free.push(buf);
            }
        }
    }
}

/// One plane of a decoded frame.
#[derive(Debug, Clone)]
pub struct PlaneBuf {
    data: Vec<u8>,
    pool: Option<Arc<PlanePool>>,
}

impl PlaneBuf {
    /// An empty plane (NV12 has no third plane).
    pub fn empty() -> Self {
        Self {
            data: Vec::new(),
            pool: None,
        }
    }

    /// Acquire a zeroed plane of `len` bytes from `pool`.
    pub fn acquire(pool: &Arc<PlanePool>, len: usize) -> Self {
        let mut data = pool.take(len);
        data.fill(0);
        Self {
            data,
            pool: Some(Arc::clone(pool)),
        }
    }

    /// A plane that owns a fresh allocation (used by tests and captures).
    pub fn owned(len: usize) -> Self {
        Self {
            data: vec![0u8; len],
            pool: None,
        }
    }

    /// Copy exactly `len` bytes from `src` (which may be a larger FFmpeg
    /// plane slice) into this buffer.
    pub fn copy_from(&mut self, src: &[u8], len: usize) {
        if self.data.len() != len {
            self.data.resize(len, 0);
        }
        let n = len.min(src.len()).min(self.data.len());
        self.data[..n].copy_from_slice(&src[..n]);
    }

    /// View the plane.
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    /// Mutable view of the plane.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// True when the plane holds no data.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Reinterpret as `f32` is not meaningful; exposed for tests only.
    #[cfg(test)]
    fn bytes(&self) -> &[u8] {
        &self.data
    }
}

impl Drop for PlaneBuf {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.take() {
            pool.give(std::mem::take(&mut self.data));
        }
    }
}

/// A single decoded video frame in system memory.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    /// Pixel layout.
    pub format: PixelFormat,
    /// Luma width in pixels.
    pub width: u32,
    /// Luma height in pixels.
    pub height: u32,
    /// Planes; unused entries are empty.
    pub planes: [PlaneBuf; 3],
    /// Row stride (FFmpeg `linesize`) per plane, in bytes.
    pub strides: [usize; 3],
    /// Presentation timestamp in microseconds (stream time base, 0 at start).
    pub pts_us: i64,
}

impl VideoFrame {
    /// Total heap bytes held by this frame.
    pub fn byte_size(&self) -> usize {
        self.planes.iter().map(|p| p.len()).sum()
    }

    /// Chroma width (rounded up).
    pub fn chroma_width(&self) -> u32 {
        self.width.div_ceil(2)
    }

    /// Chroma height (rounded up).
    pub fn chroma_height(&self) -> u32 {
        self.height.div_ceil(2)
    }

    /// Size of a plane in bytes: stride * plane height.
    pub fn plane_byte_len(&self, plane: usize) -> usize {
        match (self.format, plane) {
            (_, 0) => self.strides[0].max(self.width as usize) * self.height as usize,
            (PixelFormat::Yuv420p, 1) | (PixelFormat::Yuv420p, 2) => {
                self.strides[plane].max(self.chroma_width() as usize) * self.chroma_height() as usize
            }
            (PixelFormat::Nv12, 1) => {
                self.strides[1].max(self.chroma_width() as usize * 2) * self.chroma_height() as usize
            }
            _ => 0,
        }
    }

    /// Pixel extent `(width, height)` of each plane, in the layouts the GPU
    /// path uploads. Unused planes report `(0, 0)`.
    pub fn plane_extents(&self) -> [(u32, u32); 3] {
        let cw = self.chroma_width();
        let ch = self.chroma_height();
        match self.format {
            PixelFormat::Yuv420p => [
                (self.width, self.height),
                (cw, ch),
                (cw, ch),
            ],
            PixelFormat::Nv12 => [
                (self.width, self.height),
                // NV12 chroma is one Rg8 texel per 2x2 luma block.
                (cw, ch),
                (0, 0),
            ],
        }
    }

    /// `(stride, byte length)` of one plane as uploaded to the GPU.
    ///
    /// The stride is FFmpeg's `linesize`, which is what `Queue::write_texture`
    /// takes as `bytes_per_row`; padding bytes past the visible width are
    /// uploaded but never sampled.
    pub fn plane_layout(&self, plane: usize) -> (usize, usize) {
        let extents = self.plane_extents();
        let (w, h) = extents.get(plane).copied().unwrap_or((0, 0));
        if w == 0 || h == 0 {
            return (0, 0);
        }
        let tight = w as usize;
        let stride = self.strides[plane].max(tight);
        (stride, stride * h as usize)
    }

    /// True when the frame has enough data to be displayed.
    pub fn is_valid(&self) -> bool {
        self.width > 0 && self.height > 0 && self.planes[0].len() >= self.plane_byte_len(0)
    }

    /// Read a luma sample with stride handling.
    fn luma(&self, x: u32, y: u32) -> u8 {
        let stride = self.strides[0].max(self.width as usize);
        let idx = y as usize * stride + x as usize;
        self.planes[0].as_slice().get(idx).copied().unwrap_or(0)
    }

    /// Read chroma `(u, v)` for a luma pixel, handling both layouts.
    fn chroma(&self, x: u32, y: u32) -> (u8, u8) {
        let cx = x / 2;
        let cy = y / 2;
        match self.format {
            PixelFormat::Yuv420p => {
                let stride_u = self.strides[1].max(self.chroma_width() as usize);
                let stride_v = self.strides[2].max(self.chroma_width() as usize);
                let iu = cy as usize * stride_u + cx as usize;
                let iv = cy as usize * stride_v + cx as usize;
                (
                    self.planes[1].as_slice().get(iu).copied().unwrap_or(128),
                    self.planes[2].as_slice().get(iv).copied().unwrap_or(128),
                )
            }
            PixelFormat::Nv12 => {
                let stride = self.strides[1].max(self.chroma_width() as usize * 2);
                let idx = cy as usize * stride + cx as usize * 2;
                let plane = self.planes[1].as_slice();
                (
                    plane.get(idx).copied().unwrap_or(128),
                    plane.get(idx + 1).copied().unwrap_or(128),
                )
            }
        }
    }

    /// Convert the cropped window to tightly packed RGBA8 using BT.709.
    ///
    /// This is the CPU path used by "copy cropped frame to clipboard" and by
    /// thumbnail generation. It matches the shader's range handling exactly.
    pub fn crop_to_rgba(&self, crop: &CropRect, range: ColorRange) -> AppResult<RgbaImage> {
        if !self.is_valid() {
            return Err(AppError::new("frame has no decodable planes"));
        }
        let (x0, y0, w, h) = crop.to_pixel_window(self.width, self.height);
        let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
        for row in 0..h {
            for col in 0..w {
                let sx = (x0 + col).min(self.width - 1);
                let sy = (y0 + row).min(self.height - 1);
                let yy = self.luma(sx, sy);
                let (uu, vv) = self.chroma(sx, sy);
                let [r, g, b] = yuv_to_rgb_709(yy, uu, vv, range);
                let i = ((row as usize * w as usize) + col as usize) * 4;
                out[i] = r;
                out[i + 1] = g;
                out[i + 2] = b;
                out[i + 3] = 255;
            }
        }
        Ok(RgbaImage {
            width: w,
            height: h,
            data: out,
        })
    }
}

/// A tightly packed RGBA8 image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes, RGBA order.
    pub data: Vec<u8>,
}

impl RgbaImage {
    /// Width and height as a tuple.
    pub fn dims(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Pixel at `(x, y)` as RGBA.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y as usize * self.width as usize) + x as usize) * 4;
        [
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ]
    }

    /// Write to a PNG file using the `image` crate.
    pub fn save_png(&self, path: &std::path::Path) -> AppResult<()> {
        image::save_buffer(
            path,
            &self.data,
            self.width,
            self.height,
            image::ColorType::Rgba8,
        )?;
        Ok(())
    }
}

/// BT.709 YUV to RGB, handling limited (MPEG) and full (JPEG) range.
///
/// Limited range: `Y' = (Y-16)/219`, `C = (C-128)/224`.
/// Full range:    `Y' = Y/255`,    `C = (C-128)/255`.
pub fn yuv_to_rgb_709(y: u8, u: u8, v: u8, range: ColorRange) -> [u8; 3] {
    let (yf, uf, vf) = match range {
        ColorRange::Limited => (
            (y as f32 - 16.0) / 219.0,
            (u as f32 - 128.0) / 224.0,
            (v as f32 - 128.0) / 224.0,
        ),
        ColorRange::Full => (
            y as f32 / 255.0,
            (u as f32 - 128.0) / 255.0,
            (v as f32 - 128.0) / 255.0,
        ),
    };
    let r = yf + 1.5748 * vf;
    let g = yf - 0.187_324 * uf - 0.468_124 * vf;
    let b = yf + 1.8556 * uf;
    [
        (r * 255.0).round().clamp(0.0, 255.0) as u8,
        (g * 255.0).round().clamp(0.0, 255.0) as u8,
        (b * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a YUV420P frame with a non-trivial stride (padding on every row).
    fn yuv_frame(width: u32, height: u32, stride_y: usize, stride_c: usize) -> VideoFrame {
        let cw = width.div_ceil(2);
        let ch = height.div_ceil(2);
        let mut y = PlaneBuf::owned(stride_y * height as usize);
        let mut u = PlaneBuf::owned(stride_c * ch as usize);
        let mut v = PlaneBuf::owned(stride_c * ch as usize);
        // Luma gradient: value depends on x and y, padding filled with 0xAA so a
        // stride bug would be visible.
        for row in 0..height as usize {
            for col in 0..width as usize {
                let val = ((col * 4 + row * 8) % 220) as u8;
                y.as_mut_slice()[row * stride_y + col] = val;
            }
            for col in width as usize..stride_y {
                y.as_mut_slice()[row * stride_y + col] = 0xAA;
            }
        }
        // Chroma: left half U=90, right half U=180, top V=54, bottom V=200.
        for row in 0..ch as usize {
            for col in 0..cw as usize {
                let uv = if col < cw as usize / 2 { 90 } else { 180 };
                u.as_mut_slice()[row * stride_c + col] = uv;
                v.as_mut_slice()[row * stride_c + col] = if row < ch as usize / 2 { 54 } else { 200 };
            }
            for col in cw as usize..stride_c {
                u.as_mut_slice()[row * stride_c + col] = 0x55;
                v.as_mut_slice()[row * stride_c + col] = 0x55;
            }
        }
        VideoFrame {
            format: PixelFormat::Yuv420p,
            width,
            height,
            planes: [y, u, v],
            strides: [stride_y, stride_c, stride_c],
            pts_us: 0,
        }
    }

    /// The same image in NV12, with interleaved chroma and padding.
    fn nv12_from(yuv: &VideoFrame) -> VideoFrame {
        let cw = yuv.width.div_ceil(2);
        let ch = yuv.height.div_ceil(2);
        let stride_uv = (cw as usize * 2) + 8;
        let mut y = PlaneBuf::owned(yuv.strides[0] * yuv.height as usize);
        y.copy_from(yuv.planes[0].as_slice(), yuv.strides[0] * yuv.height as usize);
        let mut uv = PlaneBuf::owned(stride_uv * ch as usize);
        for row in 0..ch as usize {
            for col in 0..cw as usize {
                let u = yuv.planes[1].as_slice()[row * yuv.strides[1] + col];
                let v = yuv.planes[2].as_slice()[row * yuv.strides[2] + col];
                uv.as_mut_slice()[row * stride_uv + col * 2] = u;
                uv.as_mut_slice()[row * stride_uv + col * 2 + 1] = v;
            }
            for col in cw as usize * 2..stride_uv {
                uv.as_mut_slice()[row * stride_uv + col] = 0x77;
            }
        }
        VideoFrame {
            format: PixelFormat::Nv12,
            width: yuv.width,
            height: yuv.height,
            planes: [y, uv, PlaneBuf::empty()],
            strides: [yuv.strides[0], stride_uv, 0],
            pts_us: yuv.pts_us,
        }
    }

    #[test]
    fn limited_range_black_and_white() {
        assert_eq!(yuv_to_rgb_709(16, 128, 128, ColorRange::Limited), [0, 0, 0]);
        assert_eq!(
            yuv_to_rgb_709(235, 128, 128, ColorRange::Limited),
            [255, 255, 255]
        );
    }

    #[test]
    fn full_range_black_and_white() {
        assert_eq!(yuv_to_rgb_709(0, 128, 128, ColorRange::Full), [0, 0, 0]);
        assert_eq!(
            yuv_to_rgb_709(255, 128, 128, ColorRange::Full),
            [255, 255, 255]
        );
    }

    #[test]
    fn neutral_chroma_is_gray() {
        let [r, g, b] = yuv_to_rgb_709(126, 128, 128, ColorRange::Limited);
        assert_eq!(r, g);
        assert_eq!(g, b);
    }

    #[test]
    fn high_v_is_red_and_high_u_is_blue() {
        let red = yuv_to_rgb_709(81, 90, 240, ColorRange::Limited);
        assert!(red[0] > 200 && red[1] < 120 && red[2] < 120, "{red:?}");
        let blue = yuv_to_rgb_709(41, 240, 110, ColorRange::Limited);
        assert!(blue[2] > 200 && blue[0] < 120, "{blue:?}");
    }

    #[test]
    fn yuv420p_crop_respects_stride_padding() {
        let frame = yuv_frame(8, 6, 12, 6);
        let img = frame.crop_to_rgba(&CropRect::none(), ColorRange::Limited).unwrap();
        assert_eq!(img.dims(), (8, 6));
        // Pixel (3, 2) corresponds to luma (3*4 + 2*8) % 220 = 28.
        let expected = yuv_to_rgb_709(28, 90, 54, ColorRange::Limited);
        assert_eq!(img.pixel(3, 2), [expected[0], expected[1], expected[2], 255]);
    }

    #[test]
    fn nv12_matches_yuv420p_for_the_same_image() {
        let yuv = yuv_frame(8, 6, 12, 6);
        let nv12 = nv12_from(&yuv);
        let a = yuv.crop_to_rgba(&CropRect::none(), ColorRange::Limited).unwrap();
        let b = nv12.crop_to_rgba(&CropRect::none(), ColorRange::Limited).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn crop_window_maps_to_the_expected_region() {
        let frame = yuv_frame(8, 6, 12, 6);
        let crop = CropRect {
            left: 0.25,
            top: 0.0,
            right: 0.25,
            bottom: 0.0,
        };
        let img = frame.crop_to_rgba(&crop, ColorRange::Limited).unwrap();
        assert_eq!(img.dims(), (4, 6));
        // First column of the crop is x = 2 of the source.
        let expected = yuv_to_rgb_709(8, 90, 54, ColorRange::Limited);
        assert_eq!(img.pixel(0, 0), [expected[0], expected[1], expected[2], 255]);
    }

    #[test]
    fn odd_dimensions_round_chroma_up() {
        let frame = yuv_frame(7, 5, 8, 4);
        assert_eq!(frame.chroma_width(), 4);
        assert_eq!(frame.chroma_height(), 3);
        let img = frame.crop_to_rgba(&CropRect::none(), ColorRange::Limited).unwrap();
        assert_eq!(img.dims(), (7, 5));
        assert_eq!(img.data.len(), 7 * 5 * 4);
    }

    #[test]
    fn full_range_is_brighter_than_limited_for_mid_gray() {
        let limited = yuv_to_rgb_709(126, 128, 128, ColorRange::Limited)[0];
        let full = yuv_to_rgb_709(126, 128, 128, ColorRange::Full)[0];
        assert!(full > limited);
    }

    #[test]
    fn plane_pool_reuses_allocations() {
        let pool = PlanePool::new();
        {
            let mut a = PlaneBuf::acquire(&pool, 1024);
            a.as_mut_slice()[0] = 42;
        }
        assert_eq!(pool.free_count(), 1);
        let b = PlaneBuf::acquire(&pool, 512);
        assert_eq!(pool.allocation_count(), 1, "second acquire must reuse");
        assert_eq!(b.len(), 512);
        assert!(b.as_slice().iter().all(|v| *v == 0), "buffers are zeroed");
    }

    #[test]
    fn plane_byte_len_matches_layout() {
        let yuv = yuv_frame(8, 6, 12, 6);
        assert_eq!(yuv.plane_byte_len(0), 12 * 6);
        assert_eq!(yuv.plane_byte_len(1), 6 * 3);
        assert_eq!(yuv.plane_byte_len(2), 6 * 3);
        let nv12 = nv12_from(&yuv);
        assert_eq!(nv12.plane_byte_len(1), nv12.strides[1] * 3);
        assert_eq!(nv12.plane_byte_len(2), 0);
    }

    #[test]
    fn invalid_frame_is_rejected() {
        let frame = VideoFrame {
            format: PixelFormat::Yuv420p,
            width: 0,
            height: 0,
            planes: [PlaneBuf::empty(), PlaneBuf::empty(), PlaneBuf::empty()],
            strides: [0, 0, 0],
            pts_us: 0,
        };
        assert!(frame.crop_to_rgba(&CropRect::none(), ColorRange::Limited).is_err());
    }

    #[test]
    fn rgba_png_round_trips_through_image_crate() {
        let frame = yuv_frame(8, 6, 12, 6);
        let img = frame.crop_to_rgba(&CropRect::none(), ColorRange::Limited).unwrap();
        let path = std::env::temp_dir().join(format!("rp-frame-{}.png", std::process::id()));
        img.save_png(&path).unwrap();
        let decoded = image::open(&path).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (8, 6));
        assert_eq!(decoded.get_pixel(0, 0).0[3], 255);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn plane_bytes_are_exposed_for_tests() {
        let p = PlaneBuf::owned(4);
        assert_eq!(p.bytes().len(), 4);
    }
}

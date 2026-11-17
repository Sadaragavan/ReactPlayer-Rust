//! The CPU side of the picture: crop a frame and hand it to the clipboard, or
//! scale one down for a session thumbnail.
//!
//! This is the only place that repacks rows. Playback never does.

use crate::engine::video_frame::{CropPx, RgbaBuffer, VideoFrame};

/// Put a frame's current picture (with the layer's crop applied) on the
/// clipboard as RGBA8. Returns the size that was copied.
pub fn copy_frame_to_clipboard(frame: &VideoFrame, crop: Option<CropPx>) -> Result<(u32, u32), String> {
    let rgba = frame.to_rgba_cropped(crop);
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("no clipboard available: {e}"))?;
    let image = arboard::ImageData {
        width: rgba.width as usize,
        height: rgba.height as usize,
        bytes: std::borrow::Cow::Owned(rgba.data),
    };
    clipboard
        .set_image(image)
        .map_err(|e| format!("the clipboard refused the frame: {e}"))?;
    Ok((rgba.width, rgba.height))
}

/// Box-filtered downscale to a maximum edge, for thumbnails.
pub fn scaled_rgba(source: &RgbaBuffer, max_edge: u32) -> RgbaBuffer {
    let longest = source.width.max(source.height);
    if longest <= max_edge || max_edge == 0 || source.width == 0 || source.height == 0 {
        return RgbaBuffer {
            width: source.width,
            height: source.height,
            data: source.data.clone(),
        };
    }
    let scale = max_edge as f32 / longest as f32;
    let out_w = ((source.width as f32 * scale) as u32).max(1);
    let out_h = ((source.height as f32 * scale) as u32).max(1);
    let mut data = Vec::with_capacity((out_w * out_h * 4) as usize);
    let x_ratio = source.width as f32 / out_w as f32;
    let y_ratio = source.height as f32 / out_h as f32;
    for y in 0..out_h {
        for x in 0..out_w {
            let sx = ((x as f32 * x_ratio) as u32).min(source.width - 1);
            let sy = ((y as f32 * y_ratio) as u32).min(source.height - 1);
            data.extend_from_slice(&source.pixel(sx, sy));
        }
    }
    RgbaBuffer {
        width: out_w,
        height: out_h,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::video_frame::{ColorRange, CropPx, FrameFormat, PlaneBuf};

    fn flat_frame(y: u8) -> VideoFrame {
        VideoFrame::from_planes(
            FrameFormat::Nv12,
            4,
            4,
            [
                PlaneBuf::new(vec![y; 16]),
                PlaneBuf::new(vec![128; 8]),
                PlaneBuf::new(Vec::new()),
            ],
            [4, 4, 0],
            0,
            ColorRange::Limited,
        )
    }

    #[test]
    fn copy_path_reports_its_size_without_a_clipboard() {
        // The clipboard itself is not available in a headless test, so this
        // exercises the conversion that runs before it is touched.
        let frame = flat_frame(235);
        let rgba = frame.to_rgba_cropped(Some(CropPx {
            x: 1,
            y: 1,
            w: 2,
            h: 2,
        }));
        assert_eq!((rgba.width, rgba.height), (2, 2));
        assert_eq!(rgba.pixel(0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn scaling_keeps_the_aspect_and_the_longest_edge() {
        let source = RgbaBuffer {
            width: 400,
            height: 200,
            data: vec![7u8; 400 * 200 * 4],
        };
        let scaled = scaled_rgba(&source, 100);
        assert_eq!((scaled.width, scaled.height), (100, 50));
        assert_eq!(scaled.data.len(), 100 * 50 * 4);
        assert_eq!(&scaled.data[..4], &[7, 7, 7, 7]);
    }

    #[test]
    fn scaling_never_enlarges() {
        let source = RgbaBuffer {
            width: 10,
            height: 10,
            data: vec![1u8; 400],
        };
        let scaled = scaled_rgba(&source, 480);
        assert_eq!((scaled.width, scaled.height), (10, 10));
    }
}

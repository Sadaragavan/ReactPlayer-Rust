//! GPU composition: YUV textures, the WGSL shader, and the egui paint
//! callbacks that draw the layer stack.
//!
//! Nothing here shares types with `egui` directly; `wgpu` and `egui_wgpu` come
//! from `eframe`'s re-exports so there is exactly one `wgpu` in the graph.

pub mod capture;
pub mod compositor;
pub mod renderer;

/// The compositor shader, compiled at startup from the file next to this
/// module so `include_str!` failures are compile-time errors.
pub const COMPOSITE_SHADER: &str = include_str!("shader.wgsl");

/// Sampler filtering used for both luma and chroma. Linear keeps 4:2:0 chroma
/// from looking blocky when the canvas is scaled up.
pub const SAMPLER_LINEAR: bool = true;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_declares_both_entry_points() {
        assert!(COMPOSITE_SHADER.contains("fn vs_main"));
        assert!(COMPOSITE_SHADER.contains("fn fs_main"));
        assert!(COMPOSITE_SHADER.contains("@vertex"));
        assert!(COMPOSITE_SHADER.contains("@fragment"));
    }

    #[test]
    fn shader_has_no_placeholder_markers() {
        for needle in ["todo!", "TODO", "unimplemented!", "FIXME", "placeholder"] {
            assert!(
                !COMPOSITE_SHADER.contains(needle),
                "shader contains placeholder marker {needle}"
            );
        }
    }

    #[test]
    fn shader_handles_both_pixel_formats_and_ranges() {
        assert!(COMPOSITE_SHADER.contains("format_flag == 1u"), "NV12 branch");
        assert!(COMPOSITE_SHADER.contains("limited > 0.5"), "range branch");
        assert!(COMPOSITE_SHADER.contains("1.5748"), "BT.709 V coefficient");
        assert!(COMPOSITE_SHADER.contains("1.8556"), "BT.709 U coefficient");
    }

    #[test]
    fn sampler_choice_is_linear() {
        assert!(SAMPLER_LINEAR);
    }
}

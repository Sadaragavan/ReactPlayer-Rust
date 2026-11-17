//! Pure, testable domain model: sessions, layers, presets, preferences.
//!
//! Nothing in this module touches FFmpeg, wgpu, cpal or egui, so it can be
//! unit-tested on any machine. All persisted structures carry a `version`
//! field for forward compatibility.

pub mod geom;
pub mod layout;
pub mod presets;
pub mod session;
pub mod settings;
pub mod timeline;
pub mod store;

#[allow(unused_imports)]
pub use geom::{snap_value, CropRect, Handle, NormalizedRect, PixelRect, Size};
#[allow(unused_imports)]
pub use layout::{aspect_ratios_close, fit_rect, layer_pixel_rect, CanvasAspect, FitMode};
#[allow(unused_imports)]
pub use presets::{Preset, PresetLibrary, PresetRole};
#[allow(unused_imports)]
pub use session::{
    HardwareAccel, LayerConfig, LayerRole, Session, SessionSummary, SCHEMA_VERSION,
};
#[allow(unused_imports)]
pub use settings::{Preferences, Settings};
#[allow(unused_imports)]
pub use store::{AppPaths, Store};
#[allow(unused_imports)]
pub use timeline::{clamp_track_time, layer_source_time_us, union_bounds, UnionBounds};

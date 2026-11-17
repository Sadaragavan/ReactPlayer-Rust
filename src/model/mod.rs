//! Sessions, layers, layouts, preferences and their on-disk form.
//!
//! Pure data and pure functions: no decoder, no GPU, no window. Everything here
//! is unit-tested without one.

pub mod geom;
pub mod presets;
pub mod session;
pub mod settings;
pub mod store;
pub mod timeline;

pub use geom::{Crop, Handle, MIN_LAYER_SIZE, NormRect, Point, Size, Snap};
pub use presets::{LayoutType, Preset, PresetSlot, apply_preset, builtin_presets, layout_rects};
pub use session::{CanvasAspect, FitMode, Layer, MediaSource, Role, Session};
pub use settings::{HwAccelPref, Settings};
pub use store::{SessionSummary, Store};
pub use timeline::{DriftClass, Target, Union, classify_drift, target_of, union_of};

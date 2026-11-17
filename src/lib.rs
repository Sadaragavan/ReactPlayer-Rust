//! Reaction Player: a frame-accurate multi-stream video reaction player and
//! alignment tool.
//!
//! Layout:
//! * [`model`] - sessions, layers, geometry, presets, preferences, persistence.
//! * [`engine`] - FFmpeg probing and decoding, the audio mixer, the master clock.
//! * [`gpu`] - the wgpu compositor and the CPU capture path.
//! * [`ui`] - screens, widgets, shortcuts, the theme.
//! * [`app`] - the `eframe` application that ties the above together.

pub mod app;
pub mod engine;
pub mod gpu;
pub mod model;
pub mod ui;

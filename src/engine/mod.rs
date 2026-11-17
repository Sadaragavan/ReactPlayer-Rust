//! Playback engine: decoding, audio output, the master clock and the
//! multi-layer runtime that ties them together.

pub mod audio;
pub mod audio_resample;
pub mod demo;
pub mod hwaccel;
pub mod runtime;
pub mod sync_clock;
pub mod track;
pub mod video_frame;

// The module tree mirrors the concepts, not the call sites: these
// re-exports are the facade the rest of the crate may use, so naming a
// path through the submodule instead is not dead code.
#[allow(unused_imports)]
pub use audio::{AudioEngine, AudioRing, PcmChunk};
#[allow(unused_imports)]
pub use hwaccel::{ColorRange, DecodeBackend};
#[allow(unused_imports)]
pub use runtime::{LayerState, Runtime};
#[allow(unused_imports)]
pub use sync_clock::{DriftAction, DriftController, MasterClock};
#[allow(unused_imports)]
pub use track::{MediaInfo, PresentedFrame, TrackError, TrackHandle, TrackSpawn};
#[allow(unused_imports)]
pub use video_frame::{PixelFormat, PlanePool, VideoFrame};

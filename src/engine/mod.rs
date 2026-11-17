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

pub use audio::{AudioEngine, AudioRing, PcmChunk};
pub use hwaccel::{ColorRange, DecodeBackend};
pub use runtime::{LayerState, Runtime};
pub use sync_clock::{DriftAction, DriftController, MasterClock};
pub use track::{MediaInfo, PresentedFrame, TrackError, TrackHandle, TrackSpawn};
pub use video_frame::{PixelFormat, PlanePool, VideoFrame};

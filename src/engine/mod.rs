//! The playback engine: probing, decoding, audio, and the clock that ties the
//! layers together.
//!
//! * [`track`] - one worker thread per layer: demux, decode, seek, queue.
//! * [`hwaccel`] - hardware decode with a documented, transparent fallback.
//! * [`audio`] - the cpal output stream and the per-track mixer.
//! * [`sync_clock`] - the master clock the UI interpolates.
//! * [`runtime`] - the glue that turns a session into pictures and sound.
//! * [`demo`] - synthetic clips for trying the player out.

pub mod audio;
pub mod audio_resample;
pub mod demo;
pub mod hwaccel;
pub mod runtime;
pub mod sync_clock;
pub mod track;
pub mod video_frame;

pub use audio::{AudioEngine, AudioSlot, AudioStatus, MAX_TRACKS};
pub use hwaccel::HwBackend;
pub use runtime::{Runtime, Tick};
pub use sync_clock::MasterClock;
pub use track::{probe, Command, MediaInfo, Track, TrackConfig, TrackShared};
pub use video_frame::{ColorRange, FrameFormat, VideoFrame};

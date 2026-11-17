//! FFmpeg demux/decode worker.
//!
//! One background thread per layer owns:
//!
//! * the `AVFormatContext` (demuxer),
//! * the video decoder (hardware or software),
//! * an optional audio decoder + `swresample` context,
//! * a bounded queue of decoded [`VideoFrame`]s (4-6 frames) for the UI thread,
//! * a 15-frame decoded history so stepping backwards while paused is instant,
//! * the SPSC ring buffer producer for its audio.
//!
//! The UI thread never blocks: it polls [`TrackHandle`], which only reads
//! atomics and drains channels.
//!
//! ## Frame-accurate seek
//!
//! `av_seek_frame` (through `format::Context::seek`, which uses
//! `avformat_seek_file`) with a backward range, `avcodec_flush_buffers`, then
//! decode and discard until `pts >= target`. Every seek bumps the shared seek
//! epoch, so frames and audio queued before the seek are discarded by the
//! consumers.

use std::collections::VecDeque;
use std::ffi::c_int;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

use ffmpeg_next as ffmpeg;
use ffmpeg_next::ffi;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::software::resampling;
use ffmpeg_next::util::frame;
use ffmpeg_next::util::frame::video::Video as VideoFrameFfmpeg;
use ffmpeg_next::{Rational, Rescale};

use crate::error::{AppError, AppResult};
use crate::model::HardwareAccel;

use super::audio::{AudioRing, PcmChunk};
use super::hwaccel::{
    self, ColorRange, DecodeBackend, HwCandidate, HwDevice, HwError,
};
use super::video_frame::{PixelFormat, PlaneBuf, PlanePool, VideoFrame};

/// Look up a stream by its stream index.
///
/// `StreamIter` is a plain `Iterator`, not an indexed collection, so it has no
/// `get`. `find` walks the streams once and matches on the `AVStream` index,
/// which is the number FFmpeg puts on packets and in `best()`.
fn stream_at(
    input: &ffmpeg::format::context::Input,
    index: usize,
) -> Option<ffmpeg_next::Stream<'_>> {
    input.streams().find(|s| s.index() == index)
}

/// How many decoded frames are kept for the renderer.
pub const QUEUE_CAPACITY: usize = 6;
/// How many past frames are kept so stepping back is instant.
pub const HISTORY_FRAMES: usize = 15;
/// Software decoder thread count.
pub const SOFTWARE_THREADS: usize = 4;

/// Probe result shared with the UI as soon as it is known.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaInfo {
    /// Exact frame rate as a rational (numerator, denominator).
    pub fps: (i32, i32),
    /// Width in pixels (after rotation).
    pub width: u32,
    /// Height in pixels (after rotation).
    pub height: u32,
    /// Duration in microseconds.
    pub duration_us: i64,
    /// Pixel format name of the decoder output.
    pub pixel_format: String,
    /// Rotation metadata in degrees (0/90/180/270).
    pub rotation_deg: i32,
    /// Audio channel count, `None` when the file has no audio.
    pub audio_channels: Option<u16>,
    /// Audio sample rate in Hz.
    pub audio_sample_rate: Option<u32>,
    /// True when the layer needed an `swscale` conversion to YUV420P/NV12.
    pub converted: bool,
    /// Which backend ended up decoding.
    pub backend: DecodeBackend,
}

impl MediaInfo {
    /// Frames per second as `f64`.
    pub fn fps_f64(&self) -> f64 {
        let (n, d) = self.fps;
        if d == 0 { 0.0 } else { n as f64 / d as f64 }
    }

    /// Frame duration in microseconds (rounded), at least 1.
    pub fn frame_duration_us(&self) -> i64 {
        let (n, d) = self.fps;
        if n <= 0 || d <= 0 {
            return 33_333;
        }
        ((d as i64) * 1_000_000 / (n as i64)).max(1)
    }
}

/// Errors surfaced by a track: a probe failure or a fatal decode failure.
#[derive(Debug, Clone, PartialEq)]
pub enum TrackError {
    /// The file or URL could not be opened/probed.
    Open(String),
    /// The file exists but has no decodable video stream.
    NoVideo,
    /// Decoding failed unrecoverably.
    Decode(String),
    /// A requested seek failed but the track can continue.
    Seek(String),
}

impl std::fmt::Display for TrackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrackError::Open(msg) => write!(f, "could not open the media: {msg}"),
            TrackError::NoVideo => write!(f, "the file has no decodable video stream"),
            TrackError::Decode(msg) => write!(f, "decoding failed: {msg}"),
            TrackError::Seek(msg) => write!(f, "seek failed: {msg}"),
        }
    }
}

impl std::error::Error for TrackError {}

/// Commands sent from the UI thread to the worker.
#[derive(Debug, Clone, PartialEq)]
enum Command {
    /// Decode towards `target_us` (in the track's own timeline).
    Seek(i64),
    /// Pause/resume decoding beyond the target.
    SetPlaying(bool),
    /// Change the playback rate used for the drift nudge.
    SetRate(f32),
    /// Set the per-track volume in `0..=2`.
    SetVolume(f32),
    /// Mute or unmute.
    SetMuted(bool),
    /// Stop the worker and free resources.
    Shutdown,
}

/// A trimmed history entry.
#[derive(Debug, Clone)]
struct HistoryEntry {
    frame: VideoFrame,
    color: ColorRange,
}

/// State shared between the worker and [`TrackHandle`].
struct Shared {
    playing: AtomicBool,
    volume_bits: AtomicU32,
    muted: AtomicBool,
    rate_bits: AtomicU32,
    position_us: AtomicI64,
    backend: AtomicU8,
    converted: AtomicBool,
    finished: AtomicBool,
    audio_available: AtomicBool,
    seek_epoch: Arc<AtomicU64>,
    history: Mutex<VecDeque<HistoryEntry>>,
}

/// A decoded frame with its colour range, ready to be uploaded.
#[derive(Debug, Clone)]
pub struct PresentedFrame {
    /// The frame itself.
    pub frame: VideoFrame,
    /// Limited or full range (from the decoder frame).
    pub color: ColorRange,
    /// Seek epoch this frame belongs to; consumers drop stale epochs.
    pub epoch: u64,
}

impl Shared {
    fn new(seek_epoch: Arc<AtomicU64>) -> Self {
        Self {
            playing: AtomicBool::new(false),
            volume_bits: AtomicU32::new(1.0f32.to_bits()),
            muted: AtomicBool::new(false),
            rate_bits: AtomicU32::new(1.0f32.to_bits()),
            position_us: AtomicI64::new(0),
            backend: AtomicU8::new(DecodeBackend::Software as u8),
            converted: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            audio_available: AtomicBool::new(false),
            seek_epoch,
            history: Mutex::new(VecDeque::with_capacity(HISTORY_FRAMES + 1)),
        }
    }

    fn backend(&self) -> DecodeBackend {
        match self.backend.load(Ordering::Relaxed) {
            0 => DecodeBackend::Vaapi,
            1 => DecodeBackend::Vulkan,
            2 => DecodeBackend::Vdpau,
            3 => DecodeBackend::D3d11va,
            4 => DecodeBackend::Dxva2,
            5 => DecodeBackend::VideoToolbox,
            _ => DecodeBackend::Software,
        }
    }

    fn set_backend(&self, b: DecodeBackend) {
        self.backend.store(b as u8, Ordering::Relaxed);
    }
}

/// The UI-side handle of a decode worker.
pub struct TrackHandle {
    id: u64,
    path: PathBuf,
    shared: Arc<Shared>,
    commands: Sender<Command>,
    frames: Receiver<PresentedFrame>,
    errors: Receiver<TrackError>,
    info: Option<MediaInfo>,
    audio: Option<Arc<AudioRing>>,
    thread: Option<thread::JoinHandle<()>>,
    /// Frames drained from the channel but not yet presented.
    pending: VecDeque<PresentedFrame>,
    pool: Arc<PlanePool>,
}

impl TrackHandle {
    /// Layer id this track belongs to.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Source path or URL.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Probe results once available.
    pub fn info(&self) -> Option<&MediaInfo> {
        self.info.as_ref()
    }

    /// Replace the cached info (used when the worker's probe lands later).
    pub fn set_info(&mut self, info: MediaInfo) {
        self.shared.set_backend(info.backend);
        self.shared.converted.store(info.converted, Ordering::Relaxed);
        self.shared
            .audio_available
            .store(info.audio_channels.is_some(), Ordering::Relaxed);
        self.info = Some(info);
    }

    /// Active decode backend.
    pub fn backend(&self) -> DecodeBackend {
        self.shared.backend()
    }

    /// True when the stream needed an swscale conversion.
    pub fn converted(&self) -> bool {
        self.shared.converted.load(Ordering::Relaxed)
    }

    /// Audio ring buffer, if the track has audio.
    pub fn audio(&self) -> Option<&Arc<AudioRing>> {
        self.audio.as_ref()
    }

    /// Current playback position of the decoded stream, in media microseconds.
    pub fn position_us(&self) -> i64 {
        self.shared.position_us.load(Ordering::Relaxed)
    }

    /// True when the worker reached end of stream.
    pub fn finished(&self) -> bool {
        self.shared.finished.load(Ordering::Relaxed)
    }

    /// Shared seek epoch.
    pub fn epoch(&self) -> u64 {
        self.shared.seek_epoch.load(Ordering::Acquire)
    }

    /// The plane pool in use.
    pub fn pool(&self) -> &Arc<PlanePool> {
        &self.pool
    }

    /// Send a command; a closed channel means the worker is gone.
    fn send(&self, cmd: Command) {
        let _ = self.commands.send(cmd);
    }

    /// Set the play state.
    pub fn set_playing(&self, playing: bool) {
        self.shared.playing.store(playing, Ordering::Relaxed);
        self.send(Command::SetPlaying(playing));
    }

    /// Set the fractional playback rate for drift absorption.
    pub fn set_rate(&self, rate: f32) {
        let clamped = rate.clamp(super::sync_clock::MIN_RATE, super::sync_clock::MAX_RATE);
        self.shared
            .rate_bits
            .store(clamped.to_bits(), Ordering::Relaxed);
        self.send(Command::SetRate(clamped));
    }

    /// Set volume.
    pub fn set_volume(&self, volume: f32) {
        let v = volume.clamp(0.0, 2.0);
        self.shared.volume_bits.store(v.to_bits(), Ordering::Relaxed);
        self.send(Command::SetVolume(v));
    }

    /// Set mute.
    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
        self.send(Command::SetMuted(muted));
    }

    /// Request a seek to `target_us` in the track's own media time.
    pub fn seek(&self, target_us: i64) {
        self.send(Command::Seek(target_us));
    }

    /// Drain the newest decoded frame, dropping older ones.
    ///
    /// Returns `None` when no new frame has arrived.
    pub fn latest_frame(&mut self) -> Option<PresentedFrame> {
        loop {
            match self.frames.try_recv() {
                Ok(f) => self.pending.push_back(f),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        while self.pending.len() > QUEUE_CAPACITY {
            self.pending.pop_front();
        }
        self.pending.pop_back()
    }

    /// Peek at the newest queued frame without removing it.
    pub fn peek_frame(&self) -> Option<&PresentedFrame> {
        self.pending.back()
    }

    /// The last `n` frames kept by the decoder history (oldest first). Stepping
    /// back while paused reads from here instead of seeking.
    pub fn history_tail(&self, n: usize) -> Vec<PresentedFrame> {
        let guard = match self.shared.history.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let epoch = self.epoch();
        guard
            .iter()
            .rev()
            .take(n)
            .map(|e| PresentedFrame {
                frame: e.frame.clone(),
                color: e.color,
                epoch,
            })
            .collect()
    }

    /// Take a specific frame from the history by pts (used by step-back).
    pub fn frame_at_pts(&self, pts_us: i64) -> Option<PresentedFrame> {
        let guard = match self.shared.history.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard
            .iter()
            .rev()
            .find(|e| e.frame.pts_us <= pts_us)
            .map(|e| PresentedFrame {
                frame: e.frame.clone(),
                color: e.color,
                epoch: self.epoch(),
            })
    }

    /// Non-blocking poll for worker errors.
    pub fn poll_error(&self) -> Option<TrackError> {
        match self.errors.try_recv() {
            Ok(e) => Some(e),
            Err(_) => None,
        }
    }
}

impl Drop for TrackHandle {
    fn drop(&mut self) {
        // The worker exits as soon as its current FFmpeg call returns, because
        // `Shutdown` is queued here and the command channel is then dropped.
        // We deliberately detach the join handle instead of joining: a network
        // demuxer can block for seconds and the UI thread must never wait.
        let _ = self.commands.send(Command::Shutdown);
        let _ = self.thread.take();
    }
}

/// Everything a worker needs to start.
pub struct TrackSpawn {
    /// Layer id.
    pub id: u64,
    /// Source path or URL.
    pub path: PathBuf,
    /// Hardware decode preference.
    pub hardware: HardwareAccel,
    /// Output sample rate (the audio device's rate).
    pub output_sample_rate: u32,
    /// Shared seek epoch counter.
    pub seek_epoch: Arc<AtomicU64>,
    /// Shared plane pool.
    pub pool: Arc<PlanePool>,
}

/// Spawn a decode worker for one layer.
///
/// Returns immediately with a handle; probing happens on the worker thread so
/// the UI never blocks on slow media or network URLs. Probe results are
/// delivered through `probed` so the UI can show the badge and dimensions.
pub fn spawn_track(
    spec: TrackSpawn,
    probed: Sender<(u64, Result<MediaInfo, TrackError>)>,
) -> TrackHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (frame_tx, frame_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();
    let shared = Arc::new(Shared::new(Arc::clone(&spec.seek_epoch)));
    let pool = Arc::clone(&spec.pool);
    let audio = AudioRing::new(spec.output_sample_rate.max(8_000), 2);
    let worker_audio = Arc::clone(&audio);

    let worker_shared = Arc::clone(&shared);
    let worker_pool = Arc::clone(&pool);
    let worker_probed = probed.clone();
    let id = spec.id;
    let path = spec.path.clone();
    let thread = thread::Builder::new()
        .name(format!("decode-{id}"))
        .spawn(move || {
            let res = Worker::run(
                spec,
                worker_shared.clone(),
                worker_pool,
                cmd_rx,
                frame_tx,
                worker_audio,
                worker_probed,
            );
            if let Err(e) = res {
                let _ = err_tx.send(e.clone());
                let _ = probed.send((id, Err(e)));
            }
        })
        .ok();

    TrackHandle {
        id,
        path,
        shared,
        commands: cmd_tx,
        frames: frame_rx,
        errors: err_rx,
        info: None,
        audio: Some(audio),
        thread,
        pending: VecDeque::new(),
        pool,
    }
}

/// Create a track handle that never opens a file. Used by unit tests and by
/// the UI previews before a real worker is bound.
pub fn synthetic_track(id: u64, duration_us: i64) -> TrackHandle {
    let (cmd_tx, _cmd_rx) = mpsc::channel();
    let (_frame_tx, frame_rx) = mpsc::channel();
    let (_err_tx, err_rx) = mpsc::channel();
    let pool = PlanePool::new();
    let audio = AudioRing::new(48_000, 2);
    let shared = Arc::new(Shared::new(Arc::new(AtomicU64::new(0))));
    TrackHandle {
        id,
        path: PathBuf::from(format!("/synthetic/{id}.mp4")),
        shared: Arc::clone(&shared),
        commands: cmd_tx,
        frames: frame_rx,
        errors: err_rx,
        info: Some(MediaInfo {
            fps: (30, 1),
            width: 1920,
            height: 1080,
            duration_us,
            pixel_format: "yuv420p".into(),
            rotation_deg: 0,
            audio_channels: None,
            audio_sample_rate: None,
            converted: false,
            backend: DecodeBackend::Software,
        }),
        audio: Some(audio),
        thread: None,
        pending: VecDeque::new(),
        pool,
    }
}

/// The worker thread body.
struct Worker {
    spec: TrackSpawn,
    shared: Arc<Shared>,
    pool: Arc<PlanePool>,
    commands: Receiver<Command>,
    frames: Sender<PresentedFrame>,
    audio: Arc<AudioRing>,
    probed: Sender<(u64, Result<MediaInfo, TrackError>)>,
    hardware_pref: HardwareAccel,
    // Decoder state, filled in by `open`.
    input: Option<ffmpeg::format::context::Input>,
    video_index: usize,
    video_tb: Rational,
    audio_index: Option<usize>,
    audio_tb: Rational,
    decoder: Option<ffmpeg::decoder::Video>,
    audio_decoder: Option<ffmpeg::decoder::Audio>,
    resampler: Option<resampling::Context>,
    swscaler: Option<ffmpeg_next::software::scaling::Context>,
    hw_device: Option<HwDevice>,
    hw_candidate: Option<HwCandidate>,
    hw_negotiated: bool,
    hw_failed: bool,
    target_us: i64,
    playing: bool,
    rate: f32,
    info: MediaInfo,
    frame_count: usize,
}

impl Worker {
    fn run(
        spec: TrackSpawn,
        shared: Arc<Shared>,
        pool: Arc<PlanePool>,
        commands: Receiver<Command>,
        frames: Sender<PresentedFrame>,
        audio: Arc<AudioRing>,
        probed: Sender<(u64, Result<MediaInfo, TrackError>)>,
    ) -> Result<(), TrackError> {
        let hardware_pref = spec.hardware;
        let mut w = Worker {
            spec,
            shared,
            pool,
            commands,
            frames,
            audio,
            probed,
            hardware_pref,
            input: None,
            video_index: 0,
            video_tb: Rational::new(1, 1),
            audio_index: None,
            audio_tb: Rational::new(1, 1),
            decoder: None,
            audio_decoder: None,
            resampler: None,
            swscaler: None,
            hw_device: None,
            hw_candidate: None,
            hw_negotiated: false,
            hw_failed: false,
            target_us: 0,
            playing: false,
            rate: 1.0,
            info: MediaInfo {
                fps: (30, 1),
                width: 0,
                height: 0,
                duration_us: 0,
                pixel_format: "none".into(),
                rotation_deg: 0,
                audio_channels: None,
                audio_sample_rate: None,
                converted: false,
                backend: DecodeBackend::Software,
            },
            frame_count: 0,
        };
        w.open()?;
        // Report the probe result as soon as the media is understood, so the
        // UI can drop in the correct aspect ratio, duration, and HW badge
        // before the first frame is decoded.
        let _ = w.probed.send((w.spec.id, Ok(w.info.clone())));
        w.decode_loop()
    }

    /// Open the container, pick streams, probe, and open decoders.
    fn open(&mut self) -> Result<(), TrackError> {
        ffmpeg::init().map_err(|e| TrackError::Open(e.to_string()))?;
        let path = self.spec.path.to_string_lossy().to_string();
        let input = ffmpeg::format::input(&path)
            .map_err(|e| TrackError::Open(format!("{path}: {e}")))?;
        let duration_us = if input.duration() > 0 {
            input.duration()
        } else {
            0
        };

        // ---- video stream
        let vstream = input
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or(TrackError::NoVideo)?;
        let video_index = vstream.index();
        let video_tb = vstream.time_base();
        let rotation = rotation_from_metadata(&vstream);
        let (fps_n, fps_d) = exact_fps(&vstream);
        let avg = (fps_n, fps_d);

        // ---- audio stream
        let astream = input.streams().best(ffmpeg::media::Type::Audio);
        let (audio_index, audio_tb, audio_channels, audio_sample_rate) = match &astream {
            Some(s) => {
                let ctx = ffmpeg::codec::context::Context::from_parameters(s.parameters())
                    .map_err(|e| TrackError::Open(e.to_string()))?;
                let dec = ctx.decoder().audio().map_err(|e| TrackError::Open(e.to_string()))?;
                (
                    Some(s.index()),
                    s.time_base(),
                    Some(dec.channels()),
                    Some(dec.rate()),
                )
            }
            None => (None, Rational::new(1, 1), None, None),
        };

        // ---- video decoder (software first to learn the real pixel format)
        let vctx = ffmpeg::codec::context::Context::from_parameters(
            stream_at(&input, video_index)
                .unwrap()
                .parameters(),
        )
        .map_err(|e| TrackError::Open(e.to_string()))?;
        let mut decoder = vctx.decoder().video().map_err(|e| TrackError::Open(e.to_string()))?;

        let coded_w = decoder.width();
        let coded_h = decoder.height();
        let coded_pix = decoder.format();
        let swap_axes = rotation == 90 || rotation == 270;
        let (width, height) = if swap_axes {
            (coded_h, coded_w)
        } else {
            (coded_w, coded_h)
        };

        // ---- hardware attempt
        let needs_swscale = !is_direct_yuv_format(coded_pix);
        let (decoder, backend, needs_swscale) = if matches!(
            self.hardware_pref,
            HardwareAccel::SoftwareOnly
        ) {
            configure_software(&mut decoder);
            (decoder, DecodeBackend::Software, needs_swscale)
        } else {
            match self.try_hardware(decoder, coded_pix) {
                Ok((dec, backend)) => (dec, backend, false),
                Err(_) => {
                    let mut d = ffmpeg::codec::context::Context::from_parameters(
                        input.streams().get(video_index).unwrap().parameters(),
                    )
                    .map_err(|e| TrackError::Open(e.to_string()))?
                    .decoder()
                    .video()
                    .map_err(|e| TrackError::Open(e.to_string()))?;
                    configure_software(&mut d);
                    (d, DecodeBackend::Software, needs_swscale)
                }
            }
        };

        if backend == DecodeBackend::Software && needs_swscale {
            // Unsupported source format (10-bit, 4:2:2, ...): convert to
            // YUV420P with swscale and mark the layer as "converted".
            self.swscaler = Some(
                ffmpeg_next::software::scaling::Context::get(
                    coded_pix,
                    coded_w,
                    coded_h,
                    Pixel::YUV420P,
                    coded_w,
                    coded_h,
                    ffmpeg_next::software::scaling::Flags::BILINEAR,
                )
                .map_err(|e| TrackError::Open(format!("swscale: {e}")))?,
            );
        }

        self.input = Some(input);
        self.video_index = video_index;
        self.video_tb = video_tb;
        self.audio_index = audio_index;
        self.audio_tb = audio_tb;
        self.decoder = Some(decoder);
        self.info = MediaInfo {
            fps: avg,
            width,
            height,
            duration_us,
            pixel_format: coded_pix
                .descriptor()
                .map(|d| d.name().to_string())
                .unwrap_or_else(|| "unknown".into()),
            rotation_deg: rotation,
            audio_channels,
            audio_sample_rate,
            converted: needs_swscale,
            backend,
        };
        self.shared.set_backend(backend);
        self.shared.converted.store(needs_swscale, Ordering::Relaxed);
        self.shared
            .audio_available
            .store(audio_index.is_some(), Ordering::Relaxed);
        self.audio
            .configure(audio_sample_rate.unwrap_or(48_000), audio_channels.unwrap_or(2));

        // ---- audio decoder
        if let Some(ai) = self.audio_index {
            let actx = ffmpeg::codec::context::Context::from_parameters(
                stream_at(self.input.as_ref().unwrap(), ai)
                    .ok_or_else(|| TrackError::Open("audio stream vanished".into()))?
                    .parameters(),
            )
            .map_err(|e| TrackError::Open(e.to_string()))?;
            let adec = actx.decoder().audio().map_err(|e| TrackError::Open(e.to_string()))?;
            let layout = adec.channel_layout();
            self.resampler = Some(
                resampling::Context::get(
                    adec.format(),
                    layout,
                    adec.rate(),
                    ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed),
                    ffmpeg_next::channel_layout::ChannelLayout::STEREO,
                    self.spec.output_sample_rate,
                )
                .map_err(|e| TrackError::Open(format!("swresample: {e}")))?,
            );
            self.audio_decoder = Some(adec);
        }
        Ok(())
    }

    /// Try each hardware candidate in order until one opens and negotiates.
    fn try_hardware(
        &mut self,
        _open_decoder: ffmpeg::decoder::Video,
        coded_pix: Pixel,
    ) -> Result<(ffmpeg::decoder::Video, DecodeBackend), HwError> {
        let candidates = hwaccel::candidates_for(self.hardware_pref);
        if candidates.is_empty() {
            return Err(HwError::NoCandidates);
        }
        let input = self.input.as_ref().ok_or(HwError::NoCandidates)?;
        // Each candidate builds its own context below; nothing here needs one.

        for cand in candidates {
            // Ask the decoder's own `AVCodec` whether it can do this. The
            // context is dropped immediately; its `codec` pointer is owned by
            // FFmpeg and stays valid, but we only use it inside this block.
            let supported = {
                let mut probe_ctx = ffmpeg::codec::context::Context::from_parameters(
                    stream_at(input, self.video_index)
                    .unwrap()
                    .parameters(),
                )
                .map_err(|e| HwError::Setup(e.to_string()))?;
                // SAFETY: `probe_ctx` is a live codec context whose `codec`
                // field points at an FFmpeg-owned `AVCodec`. The pointer is
                // only dereferenced inside `probe_codec_hw_support`, before
                // `probe_ctx` is dropped.
                unsafe { hwaccel::probe_codec_hw_support((*probe_ctx.as_mut_ptr()).codec, &cand) }
            };
            if !supported {
                continue;
            }

            // SAFETY: FFmpeg is initialised, and the device handle is released
            // either by `HwDevice::drop` or by the codec context when it dies.
            let device = match unsafe { HwDevice::create(cand) } {
                Ok(d) => d,
                Err(_) => continue,
            };

            let mut ctx = ffmpeg::codec::context::Context::from_parameters(
                input.streams().get(self.video_index).unwrap().parameters(),
            )
            .map_err(|e| HwError::Setup(e.to_string()))?;
            // SAFETY: `ctx` has not been opened yet (avcodec_open2 happens in
            // `decoder().video()` below), which is exactly the precondition of
            // `attach_device`.
            unsafe {
                if hwaccel::attach_device(ctx.as_mut_ptr(), &device, cand).is_err() {
                    continue;
                }
            }
            let dec = match ctx.decoder().video() {
                Ok(d) => d,
                Err(_) => continue,
            };
            self.hw_device = Some(device);
            self.hw_candidate = Some(cand);
            self.hw_negotiated = false;
            return Ok((dec, cand.backend));
        }
        let _ = coded_pix;
        Err(HwError::UnsupportedCodec)
    }

    /// Main loop: wait for a target, decode towards it, emit frames.
    fn decode_loop(&mut self) -> Result<(), TrackError> {
        loop {
            // Apply any pending commands without blocking.
            loop {
                match self.commands.try_recv() {
                    Ok(Command::Shutdown) => return Ok(()),
                    Ok(Command::Seek(t)) => {
                        self.target_us = t;
                        let _ = self.do_seek(t);
                    }
                    Ok(Command::SetPlaying(p)) => self.playing = p,
                    Ok(Command::SetRate(r)) => {
                        self.rate = r.clamp(super::sync_clock::MIN_RATE, super::sync_clock::MAX_RATE);
                    }
                    Ok(Command::SetVolume(_v)) => {}
                    Ok(Command::SetMuted(_m)) => {}
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return Ok(()),
                }
            }

            if self.shared.finished.load(Ordering::Relaxed) && !self.playing {
                thread::sleep(std::time::Duration::from_millis(8));
                continue;
            }

            if self.playing {
                self.target_us = self.target_us.saturating_add(self.frame_step_us());
            } else {
                // Paused: decode only what is needed for the current target.
                self.target_us = self.shared.position_us.load(Ordering::Relaxed);
            }

            if let Err(_e) = self.decode_until_target() {
                // Decoder errors at end of stream are normal.
            }

            self.shared.position_us.store(self.target_us, Ordering::Relaxed);

            let wait = if self.playing { 0 } else { 8 };
            if wait > 0 {
                thread::sleep(std::time::Duration::from_millis(wait));
            }
        }
    }

    fn frame_step_us(&self) -> i64 {
        self.info.frame_duration_us()
    }

    /// Seek to `target_us` and flush the decoder, then decode up to the target.
    fn do_seek(&mut self, target_us: i64) -> Result<(), TrackError> {
        self.shared
            .seek_epoch
            .fetch_add(1, Ordering::AcqRel);
        let Some(input) = self.input.as_mut() else {
            return Err(TrackError::Seek("not open".into()));
        };
        let Some(decoder) = self.decoder.as_mut() else {
            return Err(TrackError::Seek("no decoder".into()));
        };
        let tb = self.video_tb;
        let ts = target_us.rescale((1, 1_000_000), tb);
        // Backward seek to the closest keyframe before the target.
        input
            .seek(ts, ts.saturating_sub(2)..ts)
            .map_err(|e| TrackError::Seek(e.to_string()))?;
        decoder.flush();
        if let Some(ad) = self.audio_decoder.as_mut() {
            ad.flush();
        }
        if let Ok(mut hist) = self.shared.history.lock() {
            hist.clear();
        }
        self.audio
            .mark_seek(self.shared.seek_epoch.load(Ordering::Acquire));
        self.decode_until_target()?;
        self.shared.position_us.store(target_us, Ordering::Relaxed);
        Ok(())
    }

    /// Decode frames until one with `pts >= target` is produced, keeping the
    /// decoded history up to date.
    fn decode_until_target(&mut self) -> Result<(), TrackError> {
        let mut packets_sent = 0usize;
        loop {
            let Some(input) = self.input.as_mut() else {
                return Ok(());
            };
            let mut packet = ffmpeg_next::Packet::empty();
            if packet.read(input).is_err() {
                // EOF: flush the decoder and stop.
                if let Some(dec) = self.decoder.as_mut() {
                    let _ = dec.send_eof();
                    self.receive_all_frames()?;
                }
                self.shared.finished.store(true, Ordering::Relaxed);
                return Ok(());
            }
            let stream_index = packet.stream();
            if stream_index == self.video_index {
                if let Some(dec) = self.decoder.as_mut()
                    && dec.send_packet(&packet).is_err()
                {
                    // Try to recover a sw-decoder failure once.
                    if self.recover_to_software()? {
                        continue;
                    }
                }
            } else if Some(stream_index) == self.audio_index
                && let Some(adec) = self.audio_decoder.as_mut()
            {
                let _ = adec.send_packet(&packet);
                self.receive_audio()?;
            }
            packets_sent += 1;
            if self.receive_all_frames()? {
                // A frame at or beyond the target has been emitted.
                return Ok(());
            }
            if packets_sent > 600 {
                return Ok(());
            }
        }
    }

    /// Receive every available decoded frame, pushing hardware frames through
    /// `av_hwframe_transfer_data`, and stop once we passed the target.
    fn receive_all_frames(&mut self) -> Result<bool, TrackError> {
        loop {
            let Some(dec) = self.decoder.as_mut() else {
                return Ok(false);
            };
            let mut decoded = VideoFrameFfmpeg::empty();
            if dec.receive_frame(&mut decoded).is_err() {
                return Ok(false);
            }
            let pts = frame_pts_us(&decoded, self.video_tb);
            let hw_frame = unsafe { is_hw_frame(decoded.as_ptr()) };
            let converted = if hw_frame {
                match self.transfer_hw_frame(&decoded) {
                    Ok(f) => f,
                    Err(e) => {
                        if self.recover_to_software()? {
                            return Ok(false);
                        }
                        return Err(TrackError::Decode(e.to_string()));
                    }
                }
            } else {
                decoded
            };
            let color = ColorRange::from_ffmpeg(converted.color_range());
            let mut vf = self.to_video_frame(&converted, pts)?;
            if self.info.rotation_deg != 0
                && let Some(rotated) = rotate_frame(&vf, self.info.rotation_deg, &self.pool)
            {
                vf = rotated;
            }
            self.push_presented(vf, color);
            if pts >= self.target_us {
                return Ok(true);
            }
        }
    }

    /// Copy a decoded frame into a pooled [`VideoFrame`], keeping FFmpeg's
    /// strides (rows are never repacked).
    fn to_video_frame(
        &mut self,
        src: &VideoFrameFfmpeg,
        pts_us: i64,
    ) -> Result<VideoFrame, TrackError> {
        if let Some(scaler) = self.swscaler.as_mut() {
            let mut out = VideoFrameFfmpeg::empty();
            scaler
                .run(src, &mut out)
                .map_err(|e| TrackError::Decode(format!("swscale: {e}")))?;
            return copy_planes(&out, &self.pool, pts_us, PixelFormat::Yuv420p)
                .ok_or_else(|| TrackError::Decode("conversion produced no planes".into()));
        }
        let format = match src.format() {
            Pixel::YUV420P => PixelFormat::Yuv420p,
            Pixel::NV12 => PixelFormat::Nv12,
            other => {
                // Anything else that reached here is unexpected; convert once.
                if self.swscaler.is_none() {
                    self.swscaler = Some(
                        ffmpeg_next::software::scaling::Context::get(
                            other,
                            src.width(),
                            src.height(),
                            Pixel::YUV420P,
                            src.width(),
                            src.height(),
                            ffmpeg_next::software::scaling::Flags::BILINEAR,
                        )
                        .map_err(|e| TrackError::Decode(format!("swscale: {e}")))?,
                    );
                    self.shared.converted.store(true, Ordering::Relaxed);
                }
                let scaler = self.swscaler.as_mut().unwrap();
                let mut out = VideoFrameFfmpeg::empty();
                scaler
                    .run(src, &mut out)
                    .map_err(|e| TrackError::Decode(format!("swscale: {e}")))?;
                return copy_planes(&out, &self.pool, pts_us, PixelFormat::Yuv420p)
                    .ok_or_else(|| TrackError::Decode("conversion produced no planes".into()));
            }
        };
        copy_planes(src, &self.pool, pts_us, format)
            .ok_or_else(|| TrackError::Decode("decoder produced an empty frame".into()))
    }

    /// Transfer a hardware frame into an NV12/YUV420P system-memory frame.
    fn transfer_hw_frame(
        &mut self,
        src: &VideoFrameFfmpeg,
    ) -> Result<VideoFrameFfmpeg, TrackError> {
        // SAFETY: `src` is a live hardware frame owned by the decoder. We
        // allocate a plain AVFrame sized for it, transfer into it, then hand
        // ownership of that AVFrame to `VideoFrameFfmpeg::wrap`, which will
        // free it on drop.
        unsafe {
            let dst_format = ffi::AVPixelFormat::AV_PIX_FMT_NV12;
            let raw_dst = hwaccel::allocate_transfer_frame(src.as_ptr(), dst_format)
                .map_err(|e| TrackError::Decode(e.to_string()))?;
            if let Err(e) = hwaccel::transfer_to_system(src.as_ptr(), raw_dst) {
                hwaccel::free_frame(raw_dst);
                return Err(TrackError::Decode(e.to_string()));
            }
            Ok(VideoFrameFfmpeg::wrap(raw_dst))
        }
    }

    /// Decode and enqueue audio samples.
    fn receive_audio(&mut self) -> Result<(), TrackError> {
        let Some(adec) = self.audio_decoder.as_mut() else {
            return Ok(());
        };
        loop {
            let mut decoded = frame::Audio::empty();
            if adec.receive_frame(&mut decoded).is_err() {
                return Ok(());
            }
            let Some(res) = self.resampler.as_mut() else {
                return Ok(());
            };
            let mut out = frame::Audio::empty();
            if res.run(&decoded, &mut out).is_err() {
                continue;
            }
            let samples = out.samples();
            if samples == 0 {
                continue;
            }
            let bytes = out.data(0);
            let mut pcm = Vec::with_capacity(samples * 2);
            for i in 0..(samples * 2) {
                let start = i * 4;
                if start + 4 <= bytes.len() {
                    let v = f32::from_le_bytes([
                        bytes[start],
                        bytes[start + 1],
                        bytes[start + 2],
                        bytes[start + 3],
                    ]);
                    pcm.push(v);
                }
            }
            let pts_us = decoded
                .timestamp()
                .filter(|t| *t != i64::MIN)
                .map(|t| t.rescale(self.audio_tb, (1, 1_000_000)))
                .unwrap_or(0);
            let epoch = self.shared.seek_epoch.load(Ordering::Acquire);
            self.audio.push_audio(PcmChunk {
                pts_us,
                epoch,
                volume: 1.0,
                samples: pcm,
            });
        }
    }

    /// Push a decoded frame into the bounded queue and the history.
    fn push_presented(&mut self, frame: VideoFrame, color: ColorRange) {
        let epoch = self.shared.seek_epoch.load(Ordering::Acquire);
        if let Ok(mut hist) = self.shared.history.lock() {
            hist.push_back(HistoryEntry {
                frame: frame.clone(),
                color,
            });
            while hist.len() > HISTORY_FRAMES {
                hist.pop_front();
            }
        }
        let _ = self.frames.send(PresentedFrame {
            frame,
            color,
            epoch,
        });
        self.frame_count += 1;
    }

    /// Drop the hardware path and reopen a software decoder after a failure.
    fn recover_to_software(&mut self) -> Result<bool, TrackError> {
        if self.hw_failed || self.hw_candidate.is_none() {
            return Ok(false);
        }
        self.hw_failed = true;
        self.shared.set_backend(DecodeBackend::Software);
        self.info.backend = DecodeBackend::Software;
        let Some(input) = self.input.as_mut() else {
            return Ok(false);
        };
        let ctx = ffmpeg::codec::context::Context::from_parameters(
            stream_at(input, self.video_index)
                .ok_or_else(|| TrackError::Decode("video stream vanished".into()))?
                .parameters(),
        )
        .map_err(|e| TrackError::Decode(e.to_string()))?;
        let mut dec = ctx.decoder().video().map_err(|e| TrackError::Decode(e.to_string()))?;
        configure_software(&mut dec);
        self.decoder = Some(dec);
        self.hw_device = None;
        self.hw_candidate = None;
        let seek_to = self.shared.position_us.load(Ordering::Relaxed);
        let _ = self.do_seek(seek_to);
        Ok(true)
    }
}

/// Compute exact fps: prefer `avg_frame_rate`, fall back to `r_frame_rate`.
fn exact_fps(stream: &ffmpeg_next::Stream<'_>) -> (i32, i32) {
    let rr = stream.avg_frame_rate();
    let mut n = rr.numerator();
    let mut d = rr.denominator();
    if n <= 0 || d <= 0 {
        let rr = stream.rate();
        n = rr.numerator();
        d = rr.denominator();
    }
    if n <= 0 || d <= 0 {
        return (30, 1);
    }
    // Reduce with the GCD so 60000/1001 stays exact but 60/2 becomes 30/1.
    let g = gcd(n.unsigned_abs() as i64, d.unsigned_abs() as i64).max(1);
    ((n as i64 / g) as i32, (d as i64 / g) as i32)
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Rotation from the stream's display matrix side data, snapped to 90 degrees.
///
/// Mirrors `av_display_rotation_get` from `libavutil/display.c`: the matrix is
/// nine `int32` fixed-point values (16 fractional bits), and the reported angle
/// is the **negated** `atan2(b, a)` of the first row. A stream tagged 90 means
/// the picture must be rotated 90 degrees clockwise to display upright.
fn rotation_from_metadata(stream: &ffmpeg_next::Stream<'_>) -> i32 {
    for sd in stream.side_data() {
        // Stream side data is packet side data (`AV_PKT_DATA_*`), which has its
        // own `Type` enum -- frame side data's enum does not apply here.
        if sd.kind() == ffmpeg_next::packet::side_data::Type::DisplayMatrix {
            let data = sd.data();
            if data.len() >= 8 {
                let fixed = 1 << 16;
                let a = i32::from_le_bytes([data[0], data[1], data[2], data[3]]) as f64 / fixed as f64;
                let b = i32::from_le_bytes([data[4], data[5], data[6], data[7]]) as f64 / fixed as f64;
                if a != 0.0 || b != 0.0 {
                    let angle = -b.atan2(a).to_degrees();
                    // Snap to the nearest quarter turn, normalised to 0/90/180/270.
                    let snapped = ((angle / 90.0).round() as i32) * 90;
                    return snapped.rem_euclid(360);
                }
            }
        }
    }
    0
}

/// Convert a frame timestamp to microseconds in stream time.
fn frame_pts_us(frame: &VideoFrameFfmpeg, tb: Rational) -> i64 {
    let ts = frame.timestamp().unwrap_or(0);
    ts.rescale(tb, (1, 1_000_000))
}

/// Software decoder configuration: 4 threads, frame+slice threading.
fn configure_software(dec: &mut ffmpeg::decoder::Video) {
    // Built through `Default` and field assignment on purpose: the struct has
    // a `safe` field behind a now-obsolete feature flag, so a bare literal
    // would break the day that flag is removed.
    let mut config = ffmpeg_next::codec::threading::Config::default();
    config.kind = ffmpeg_next::codec::threading::Type::Frame;
    config.count = SOFTWARE_THREADS;
    dec.set_threading(config);
}

/// True when the pixel format can be displayed without swscale.
fn is_direct_yuv_format(p: Pixel) -> bool {
    matches!(p, Pixel::YUV420P | Pixel::NV12)
}

/// Copy a decoded frame into pooled planes, once, keeping strides.
fn copy_planes(
    src: &VideoFrameFfmpeg,
    pool: &Arc<PlanePool>,
    pts_us: i64,
    format: PixelFormat,
) -> Option<VideoFrame> {
    if src.width() == 0 || src.height() == 0 {
        return None;
    }
    let mut planes = [PlaneBuf::empty(), PlaneBuf::empty(), PlaneBuf::empty()];
    let mut strides = [0usize; 3];
    let count = format.plane_count();
    for i in 0..count {
        let stride = src.stride(i);
        let h = src.plane_height(i) as usize;
        let len = stride * h;
        let mut buf = PlaneBuf::acquire(pool, len);
        buf.copy_from(src.data(i), len);
        planes[i] = buf;
        strides[i] = stride;
    }
    Some(VideoFrame {
        format,
        width: src.width(),
        height: src.height(),
        planes,
        strides,
        pts_us,
    })
}

/// Rotate a decoded frame clockwise by `deg` (90, 180 or 270).
///
/// The display-matrix metadata says how far the picture must be turned
/// clockwise before it appears upright, and the decoder hands us the
/// *unrotated* picture. Width and height were already swapped when the stream
/// was probed for 90/270, so the planes have to actually be transposed here --
/// swapping the numbers alone would stretch the image.
///
/// Planes are copied with tight strides. Chroma planes of 4:2:0 formats are
/// subsampled in both axes, so the same index map turns them correctly; NV12's
/// interleaved UV plane is treated as pixels two bytes wide.
///
/// Returns `None` for an angle that is not a quarter turn, or a frame with no
/// planes, so callers fall back to the unrotated frame instead of panicking.
fn rotate_frame(frame: &VideoFrame, deg: i32, pool: &Arc<PlanePool>) -> Option<VideoFrame> {
    let deg = deg.rem_euclid(360);
    if deg != 90 && deg != 180 && deg != 270 {
        return None;
    }
    let src_extents = frame.plane_extents();
    let mut planes = [PlaneBuf::empty(), PlaneBuf::empty(), PlaneBuf::empty()];
    let mut strides = [0usize; 3];
    let (out_w, out_h) = if deg == 180 {
        (frame.width, frame.height)
    } else {
        (frame.height, frame.width)
    };

    for plane in 0..frame.format.plane_count() {
        let (pw, ph) = src_extents[plane];
        if pw == 0 || ph == 0 {
            continue;
        }
        // One interleaved UV pair counts as one texel of the NV12 chroma plane.
        let elem = usize::from(frame.format == PixelFormat::Nv12 && plane == 1) + 1;
        let (ow, oh) = if deg == 180 { (pw, ph) } else { (ph, pw) };
        let src_stride = frame.strides[plane].max(pw as usize * elem);
        let src = frame.planes[plane].as_slice();
        let dst_stride = ow as usize * elem;
        let mut dst = PlaneBuf::acquire(pool, dst_stride * oh as usize);
        {
            let out = dst.as_mut_slice();
            for y in 0..oh as usize {
                let row = y * dst_stride;
                for x in 0..ow as usize {
                    // dst(x, y) for a clockwise quarter turn is src(y, H-1-x);
                    // a 270 turn is the mirror image of that mapping.
                    let (sx, sy) = match deg {
                        90 => (y, ph as usize - 1 - x),
                        180 => (pw as usize - 1 - x, ph as usize - 1 - y),
                        _ => (pw as usize - 1 - y, x),
                    };
                    let si = sy * src_stride + sx * elem;
                    let di = row + x * elem;
                    if si + elem <= src.len() && di + elem <= out.len() {
                        out[di..di + elem].copy_from_slice(&src[si..si + elem]);
                    }
                }
            }
        }
        strides[plane] = dst_stride;
        planes[plane] = dst;
    }

    if planes[0].is_empty() {
        return None;
    }
    Some(VideoFrame {
        format: frame.format,
        width: out_w,
        height: out_h,
        planes,
        strides,
        pts_us: frame.pts_us,
    })
}

/// True when `ptr` points at a frame that lives in device memory.
///
/// A decoder-produced hardware frame always holds a `hw_frames_ctx` reference;
/// failing that, the format is compared against the hardware pixel formats by
/// value. Both checks stay in `c_int` space on purpose -- `av_pix_fmt_desc_get`
/// takes the rustified `AVPixelFormat` enum, so passing it a raw `int` would
/// need a transmute of a possibly-invalid discriminant.
///
/// # Safety
///
/// `ptr` must be a valid `AVFrame` pointer for the duration of the call.
unsafe fn is_hw_frame(ptr: *const ffi::AVFrame) -> bool {
    if ptr.is_null() {
        return false;
    }
    // SAFETY: non-null, owned by the decoder for this iteration.
    unsafe {
        if !(*ptr).hw_frames_ctx.is_null() {
            return true;
        }
        let fmt = (*ptr).format;
        if fmt < 0 {
            return false;
        }
        const HW_FORMATS: [ffi::AVPixelFormat; 8] = [
            ffi::AVPixelFormat::AV_PIX_FMT_VAAPI,
            ffi::AVPixelFormat::AV_PIX_FMT_VDPAU,
            ffi::AVPixelFormat::AV_PIX_FMT_CUDA,
            ffi::AVPixelFormat::AV_PIX_FMT_VULKAN,
            ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX,
            ffi::AVPixelFormat::AV_PIX_FMT_D3D11,
            ffi::AVPixelFormat::AV_PIX_FMT_DXVA2_VLD,
            ffi::AVPixelFormat::AV_PIX_FMT_MEDIACODEC,
        ];
        HW_FORMATS.iter().any(|f| *f as c_int == fmt)
    }
}

/// Open a source with a relink fallback: if the stored path is missing, the
/// caller is told so it can prompt.
pub fn source_available(path: &str) -> bool {
    if path.starts_with("http://") || path.starts_with("https://") || path.starts_with("rtmp://") {
        return true;
    }
    Path::new(path).exists()
}

/// Probe a file quickly (used by the Demo generator and by relink prompts).
pub fn quick_probe(path: &str) -> AppResult<MediaInfo> {
    ffmpeg::init().map_err(|e| AppError::new(e.to_string()))?;
    let input = ffmpeg::format::input(&path).map_err(|e| AppError::new(e.to_string()))?;
    let stream = input
        .streams()
        .best(ffmpeg::media::Type::Video)
        .ok_or_else(|| AppError::new("no video stream"))?;
    let (n, d) = exact_fps(&stream);
    let ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| AppError::new(e.to_string()))?;
    let dec = ctx.decoder().video().map_err(|e| AppError::new(e.to_string()))?;
    Ok(MediaInfo {
        fps: (n, d),
        width: dec.width(),
        height: dec.height(),
        duration_us: input.duration(),
        pixel_format: dec
            .format()
            .descriptor()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|| "unknown".into()),
        rotation_deg: 0,
        audio_channels: None,
        audio_sample_rate: None,
        converted: false,
        backend: DecodeBackend::Software,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_reduction_keeps_ntsc_exact() {
        let rr = Rational::new(60_000, 1_001);
        let g = gcd(60_000, 1_001);
        assert_eq!(g, 1);
        assert_eq!(rr, Rational::new(60_000, 1_001));
    }

    #[test]
    fn gcd_works() {
        assert_eq!(gcd(60000, 1001), 1);
        assert_eq!(gcd(60, 2), 2);
        assert_eq!(gcd(0, 5), 5);
    }

    #[test]
    fn media_info_frame_duration_is_sane() {
        let info = MediaInfo {
            fps: (30, 1),
            width: 1920,
            height: 1080,
            duration_us: 1,
            pixel_format: "yuv420p".into(),
            rotation_deg: 0,
            audio_channels: None,
            audio_sample_rate: None,
            converted: false,
            backend: DecodeBackend::Software,
        };
        assert_eq!(info.frame_duration_us(), 33_333);
        assert!((info.fps_f64() - 30.0).abs() < 1e-9);
    }

    #[test]
    fn invalid_fps_gives_a_safe_step() {
        let info = MediaInfo {
            fps: (0, 0),
            width: 0,
            height: 0,
            duration_us: 0,
            pixel_format: "none".into(),
            rotation_deg: 0,
            audio_channels: None,
            audio_sample_rate: None,
            converted: false,
            backend: DecodeBackend::Software,
        };
        assert_eq!(info.frame_duration_us(), 33_333);
        assert_eq!(info.fps_f64(), 0.0);
    }

    #[test]
    fn local_missing_source_is_unavailable() {
        assert!(!source_available("/definitely/not/here.mp4"));
        assert!(source_available("https://example.com/a.mp4"));
    }

    #[test]
    fn direct_formats_are_the_fast_path() {
        assert!(is_direct_yuv_format(Pixel::YUV420P));
        assert!(is_direct_yuv_format(Pixel::NV12));
        assert!(!is_direct_yuv_format(Pixel::YUV420P10LE));
        assert!(!is_direct_yuv_format(Pixel::YUV422P));
    }

    #[test]
    fn shared_backend_round_trips() {
        let shared = Shared::new(Arc::new(AtomicU64::new(0)));
        for b in [
            DecodeBackend::Vaapi,
            DecodeBackend::Vulkan,
            DecodeBackend::Vdpau,
            DecodeBackend::D3d11va,
            DecodeBackend::Dxva2,
            DecodeBackend::VideoToolbox,
            DecodeBackend::Software,
        ] {
            shared.set_backend(b);
            assert_eq!(shared.backend(), b);
        }
    }

    #[test]
    fn history_tail_returns_newest_first() {
        let pool = PlanePool::new();
        let frame = |pts| VideoFrame {
            format: PixelFormat::Yuv420p,
            width: 4,
            height: 4,
            planes: [
                PlaneBuf::acquire(&pool, 16),
                PlaneBuf::acquire(&pool, 4),
                PlaneBuf::acquire(&pool, 4),
            ],
            strides: [4, 2, 2],
            pts_us: pts,
        };
        let shared = Arc::new(Shared::new(Arc::new(AtomicU64::new(0))));
        {
            let mut hist = shared.history.lock().unwrap();
            for i in 0..20 {
                hist.push_back(HistoryEntry {
                    frame: frame(i * 1000),
                    color: ColorRange::Limited,
                });
                while hist.len() > HISTORY_FRAMES {
                    hist.pop_front();
                }
            }
        }
        let hist = shared.history.lock().unwrap();
        assert_eq!(hist.len(), HISTORY_FRAMES);
        assert_eq!(hist.front().unwrap().frame.pts_us, 5_000);
        assert_eq!(hist.back().unwrap().frame.pts_us, 19_000);
    }

    #[test]
    fn memory_budget_of_the_history() {
        // 1920x1080 YUV420P = 3,110,400 bytes; 15 frames = 46.6 MB per track.
        let frame_bytes = 1920 * 1080 + 2 * (960 * 540);
        let per_track = frame_bytes * HISTORY_FRAMES;
        assert!(per_track > 46_000_000 && per_track < 47_000_000);
        // Four layers of history: ~186 MB worst case.
        assert_eq!(per_track * 4, 186_624_000);
    }
}

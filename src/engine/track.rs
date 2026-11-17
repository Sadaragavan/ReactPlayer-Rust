//! One background worker per track: probe, decode, seek, and keep at most a
//! handful of frames in flight.
//!
//! The video worker owns its own demuxer and decoder; the audio worker opens
//! the same source a second time so a slow video queue can never starve the
//! sound. Both talk to the app through atomics and channels only - nothing here
//! ever blocks the UI thread.
//!
//! Unsafe appears where `ffmpeg-next`'s safe API stops: opening a decoder with a
//! hardware device attached, receiving frames by hand, transferring hardware
//! frames to system memory, and running swscale for source formats the
//! compositor does not sample natively. Each block says what makes it sound.

use std::collections::VecDeque;
use std::ffi::c_int;
use std::ptr;
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicI64, AtomicU64, AtomicU8, Ordering,
};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ffmpeg_next as ffmpeg;
use ffmpeg_next::ffi;

use super::audio::AudioSlot;
use super::audio_resample::FractionalResampler;
use super::hwaccel::{HwBackend, HwDecoder};
use super::video_frame::{
    BufferPool, ColorRange, FrameFormat, HISTORY_FRAMES, PlaneBuf, QUEUE_FRAMES, VideoFrame,
};
use crate::model::session::MediaSource;
use crate::model::settings::HwAccelPref;

/// `AV_NOPTS_VALUE`, spelled out so this does not depend on a bindgen name.
const NO_PTS: i64 = i64::MIN;

/// Everything probing tells the app about a source.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaInfo {
    pub duration_us: i64,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    /// The layout the decoder hands over natively.
    pub source_format: FrameFormat,
    /// True when the source had to be converted for the compositor.
    pub converted: bool,
    pub has_audio: bool,
    pub audio_channels: u16,
    pub audio_rate: u32,
    pub video_stream: usize,
    pub audio_stream: Option<usize>,
    /// Rotation from the display matrix, in degrees clockwise.
    pub rotation: i32,
}

impl MediaInfo {
    pub fn frame_us(&self) -> i64 {
        if self.fps_num == 0 {
            return 33_333;
        }
        1_000_000 * i64::from(self.fps_den.max(1)) / i64::from(self.fps_num.max(1))
    }
}

/// Probe a source without opening a decoder for playback. Used when a layer is
/// added so the timeline has a length immediately.
pub fn probe(source: &MediaSource) -> Result<MediaInfo, String> {
    let _ = ffmpeg::init();
    let input = ffmpeg::format::input(&source.location())
        .map_err(|e| format!("could not open {}: {e}", source.display()))?;

    let mut info = MediaInfo {
        duration_us: input.duration().max(0),
        width: 0,
        height: 0,
        fps_num: 30,
        fps_den: 1,
        source_format: FrameFormat::Yuv420p,
        converted: false,
        has_audio: false,
        audio_channels: 0,
        audio_rate: 0,
        video_stream: 0,
        audio_stream: None,
        rotation: 0,
    };

    if let Some(stream) = input.streams().best(ffmpeg::media::Type::Video) {
        info.video_stream = stream.index();
        let rate = stream.avg_frame_rate();
        if rate.numerator() > 0 && rate.denominator() > 0 {
            info.fps_num = rate.numerator() as u32;
            info.fps_den = rate.denominator() as u32;
        }
        let params = stream.parameters();
        if let Ok(context) = ffmpeg::codec::context::Context::from_parameters(params)
            && let Ok(video) = context.decoder().video() {
            info.width = video.width();
            info.height = video.height();
            (info.source_format, info.converted) = classify_pixel_format(video.format());
        }
        if info.duration_us == 0 {
            let tb = stream.time_base();
            let stream_duration = stream.duration();
            if stream_duration > 0 && tb.numerator() > 0 {
                info.duration_us = (stream_duration as f64 * f64::from(tb.numerator())
                    / f64::from(tb.denominator())
                    * 1_000_000.0) as i64;
            }
        }
    }

    if let Some(stream) = input.streams().best(ffmpeg::media::Type::Audio) {
        info.has_audio = true;
        info.audio_stream = Some(stream.index());
        let params = stream.parameters();
        if let Ok(context) = ffmpeg::codec::context::Context::from_parameters(params)
            && let Ok(audio) = context.decoder().audio() {
            info.audio_channels = audio.channels();
            info.audio_rate = audio.rate();
        }
    }

    if info.width == 0 || info.height == 0 {
        return Err(format!("{} has no video stream", source.display()));
    }
    Ok(info)
}

/// Which of the two native layouts a source pixel format maps to, and whether
/// it needs swscale first.
fn classify_pixel_format(format: ffmpeg::format::Pixel) -> (FrameFormat, bool) {
    match format {
        ffmpeg::format::Pixel::YUV420P => (FrameFormat::Yuv420p, false),
        ffmpeg::format::Pixel::NV12 => (FrameFormat::Nv12, false),
        _ => (FrameFormat::Yuv420p, true),
    }
}

/// Commands the app sends to a track's workers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    Play,
    Pause,
    /// Seek so that `target_us` of the source is on screen, then hold there.
    Seek { target_us: i64 },
    /// Move `delta` frames from the presented position and hold there.
    Step { delta: i64 },
    /// Rate factor for the audio nudge, clamped to 0.995..1.005.
    SetRate(f64),
    Shutdown,
}

/// State shared between a track's workers and the app.
pub struct TrackShared {
    /// Probe results, published once the decoder is open.
    info: Mutex<Option<MediaInfo>>,
    /// Anything that went wrong, shown in the layer row.
    error: Mutex<Option<String>>,
    /// Bumped on every seek; frames from an older epoch are dropped.
    pub epoch: AtomicU64,
    pub paused: AtomicBool,
    /// The backend actually in use (see `HwBackend::code`).
    backend: AtomicU8,
    /// True when the source had to be converted through swscale.
    converted: AtomicBool,
    /// Rotation from the display matrix, in degrees clockwise.
    rotation: AtomicI32,
    /// Decoded frames, oldest first, at most `HISTORY_FRAMES`.
    history: Mutex<VecDeque<Arc<VideoFrame>>>,
    /// Last frame the app presented, so stepping back needs no keyframe seek.
    last_presented_us: AtomicI64,
    ended: AtomicBool,
    pub frames_decoded: AtomicU64,
    stopping: AtomicBool,
}

impl TrackShared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            info: Mutex::new(None),
            error: Mutex::new(None),
            epoch: AtomicU64::new(0),
            paused: AtomicBool::new(true),
            backend: AtomicU8::new(HwBackend::Software.code()),
            converted: AtomicBool::new(false),
            rotation: AtomicI32::new(0),
            history: Mutex::new(VecDeque::with_capacity(HISTORY_FRAMES)),
            last_presented_us: AtomicI64::new(0),
            ended: AtomicBool::new(false),
            frames_decoded: AtomicU64::new(0),
            stopping: AtomicBool::new(false),
        })
    }

    pub fn info(&self) -> Option<MediaInfo> {
        self.info.lock().ok().and_then(|g| g.clone())
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|g| g.clone())
    }

    pub fn backend(&self) -> HwBackend {
        HwBackend::from_code(self.backend.load(Ordering::Relaxed))
    }

    pub fn converted(&self) -> bool {
        self.converted.load(Ordering::Relaxed)
    }

    pub fn rotation(&self) -> i32 {
        self.rotation.load(Ordering::Relaxed)
    }

    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::Relaxed)
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    pub fn last_presented_us(&self) -> i64 {
        self.last_presented_us.load(Ordering::Relaxed)
    }

    pub fn note_presented(&self, pts_us: i64) {
        self.last_presented_us.store(pts_us, Ordering::Relaxed);
    }

    /// Newest frame at least `step_us` older than `from_us`, taken from the
    /// decoded history. Stepping back while paused uses this and never seeks.
    pub fn frame_before(&self, from_us: i64, step_us: i64) -> Option<Arc<VideoFrame>> {
        let history = self.history.lock().ok()?;
        let wanted = from_us - step_us.max(1);
        history
            .iter()
            .filter(|f| f.pts_us <= wanted)
            .max_by_key(|f| f.pts_us)
            .cloned()
    }

    /// Oldest frame still in the history, used to hold the first picture while a
    /// layer has not started yet.
    pub fn history_first(&self) -> Option<Arc<VideoFrame>> {
        self.history.lock().ok()?.front().cloned()
    }

    /// A copy of the decoded history, oldest first. The studio keeps the last
    /// eight of these so a capture point can be re-examined frame by frame.
    pub fn history_snapshot(&self) -> Vec<Arc<VideoFrame>> {
        self.history
            .lock()
            .map(|history| history.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn history_len(&self) -> usize {
        self.history.lock().map(|h| h.len()).unwrap_or(0)
    }

    fn push_history(&self, frame: Arc<VideoFrame>) {
        if let Ok(mut history) = self.history.lock() {
            // Out-of-order frames mean a seek happened; start the history over.
            if history.back().is_some_and(|last| last.pts_us >= frame.pts_us) {
                history.clear();
            }
            history.push_back(frame);
            while history.len() > HISTORY_FRAMES {
                history.pop_front();
            }
        }
    }

    pub fn clear_history(&self) {
        if let Ok(mut history) = self.history.lock() {
            history.clear();
        }
    }

    fn set_error(&self, message: impl Into<String>) {
        if let Ok(mut slot) = self.error.lock() {
            *slot = Some(message.into());
        }
    }
}

/// A running track.
pub struct Track {
    pub shared: Arc<TrackShared>,
    senders: Vec<SyncSender<Command>>,
    video_thread: Option<JoinHandle<()>>,
    audio_thread: Option<JoinHandle<()>>,
    frames: Option<Receiver<(u64, Arc<VideoFrame>)>>,
    pool: Arc<BufferPool>,
}

/// What a track needs to start.
pub struct TrackConfig {
    pub source: MediaSource,
    pub hw_accel: HwAccelPref,
    /// Output device sample rate for the audio worker.
    pub audio_rate: u32,
    pub audio_slot: Option<Arc<AudioSlot>>,
}

impl Track {
    pub fn spawn(config: TrackConfig) -> Self {
        let shared = TrackShared::new();
        let pool = BufferPool::new(64);
        let (frame_tx, frame_rx) = sync_channel::<(u64, Arc<VideoFrame>)>(QUEUE_FRAMES);
        let (video_tx, video_rx) = sync_channel::<Command>(16);
        let (audio_tx, audio_rx) = sync_channel::<Command>(16);

        let video = VideoWorker {
            source: config.source.clone(),
            hw_accel: config.hw_accel,
            shared: shared.clone(),
            pool: pool.clone(),
            frames: frame_tx,
            commands: video_rx,
            decoder: None,
            paused: true,
            frames_decoded: 0,
            rotation_read: false,
        };
        let video_thread = thread::Builder::new()
            .name(format!("video:{}", config.source.display()))
            .spawn(move || video.run())
            .ok();

        let audio_thread = config.audio_slot.and_then(|slot| {
            let audio = AudioWorker {
                source: config.source.clone(),
                shared: shared.clone(),
                slot,
                rate: config.audio_rate.max(8_000),
                commands: audio_rx,
                paused: true,
            };
            thread::Builder::new()
                .name(format!("audio:{}", config.source.display()))
                .spawn(move || audio.run())
                .ok()
        });

        // Both workers take every command; a worker that was never started has
        // a disconnected channel, which the send ignores.
        let senders = vec![video_tx, audio_tx];
        Self {
            shared,
            senders,
            video_thread,
            audio_thread,
            frames: Some(frame_rx),
            pool,
        }
    }

    pub fn send(&self, command: Command) {
        for sender in &self.senders {
            let _ = sender.send(command);
        }
    }

    /// Send without ever blocking the caller; rate changes are dropped when a
    /// worker is busy rather than stalling a frame.
    pub fn try_send(&self, command: Command) {
        for sender in &self.senders {
            if let Err(TrySendError::Disconnected(_)) = sender.try_send(command) {
                break;
            }
        }
    }

    /// The next frame the worker finished, tagged with the epoch it belongs to.
    pub fn try_frame(&mut self) -> Option<(u64, Arc<VideoFrame>)> {
        let frames = self.frames.as_ref()?;
        match frames.try_recv() {
            Ok(frame) => Some(frame),
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }

    pub fn pool(&self) -> &Arc<BufferPool> {
        &self.pool
    }
}

impl Drop for Track {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Relaxed);
        for sender in &self.senders {
            let _ = sender.send(Command::Shutdown);
        }
        if let Some(handle) = self.video_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.audio_thread.take() {
            let _ = handle.join();
        }
    }
}

/// The video half of a track.
struct VideoWorker {
    source: MediaSource,
    hw_accel: HwAccelPref,
    shared: Arc<TrackShared>,
    pool: Arc<BufferPool>,
    frames: SyncSender<(u64, Arc<VideoFrame>)>,
    commands: Receiver<Command>,
    decoder: Option<Decoder>,
    paused: bool,
    frames_decoded: u64,
    rotation_read: bool,
}

impl VideoWorker {
    fn run(mut self) {
        match Decoder::open(&self.source, self.hw_accel) {
            Ok(mut decoder) => {
                let info = decoder.info.clone();
                if let Ok(mut slot) = self.shared.info.lock() {
                    *slot = Some(info);
                }
                self.shared
                    .backend
                    .store(decoder.backend.code(), Ordering::Relaxed);
                // Show the first frame immediately, even before play.
                if let Err(message) = self.seek_to(&mut decoder, 0) {
                    self.shared.set_error(message);
                }
                self.decoder = Some(decoder);
                self.loop_forever();
            }
            Err(message) => {
                self.shared.set_error(message);
            }
        }
    }

    fn loop_forever(&mut self) {
        loop {
            if self.shared.stopping.load(Ordering::Relaxed) {
                return;
            }
            loop {
                match self.commands.try_recv() {
                    Ok(command) => {
                        if self.apply(command) {
                            return;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }

            if self.paused {
                thread::sleep(Duration::from_millis(10));
                continue;
            }

            let Some(decoder) = self.decoder.as_mut() else {
                return;
            };
            match decoder.next_frame(&self.pool) {
                Ok(Some(frame)) => {
                    self.frames_decoded += 1;
                    self.shared
                        .frames_decoded
                        .store(self.frames_decoded, Ordering::Relaxed);
                    self.shared.ended.store(false, Ordering::Relaxed);
                    self.note_frame_flags(&frame);
                    if !self.deliver(frame) {
                        return;
                    }
                }
                Ok(None) => {
                    self.shared.ended.store(true, Ordering::Relaxed);
                    thread::sleep(Duration::from_millis(10));
                }
                Err(message) => {
                    self.shared.set_error(message);
                    self.paused = true;
                    self.shared.set_paused(true);
                }
            }
        }
    }

    /// Publish the "converted" badge and, once, the rotation.
    fn note_frame_flags(&mut self, frame: &VideoFrame) {
        self.shared
            .converted
            .store(frame.converted, Ordering::Relaxed);
        if !self.rotation_read
            && let Some(decoder) = self.decoder.as_ref() {
            let rotation = decoder.rotation;
            self.shared.rotation.store(rotation, Ordering::Relaxed);
            self.rotation_read = true;
        }
    }

    /// Handle one command; returns true when the worker should stop.
    fn apply(&mut self, command: Command) -> bool {
        match command {
            Command::Shutdown => return true,
            Command::Play => {
                self.paused = false;
                self.shared.set_paused(false);
                self.shared.ended.store(false, Ordering::Relaxed);
            }
            Command::Pause => {
                self.paused = true;
                self.shared.set_paused(true);
            }
            Command::Seek { target_us } => {
                // The decoder is taken out so `seek_to` can borrow the rest of
                // the worker while it moves the file position.
                if let Some(mut decoder) = self.decoder.take() {
                    // The epoch moves first so nothing from the old position can
                    // reach the screen.
                    self.shared.epoch.fetch_add(1, Ordering::AcqRel);
                    self.shared.clear_history();
                    let result = self.seek_to(&mut decoder, target_us);
                    self.decoder = Some(decoder);
                    if let Err(message) = result {
                        self.shared.set_error(message);
                    }
                }
            }
            Command::Step { delta } => {
                let frame_us = self
                    .decoder
                    .as_ref()
                    .map(|d| d.frame_us)
                    .unwrap_or(33_333);
                let from = self.shared.last_presented_us();
                let target = (from + delta.saturating_mul(frame_us)).max(0);
                if let Some(mut decoder) = self.decoder.take() {
                    self.shared.epoch.fetch_add(1, Ordering::AcqRel);
                    let result = self.seek_to(&mut decoder, target);
                    self.decoder = Some(decoder);
                    if let Err(message) = result {
                        self.shared.set_error(message);
                    }
                }
            }
            // The audio worker owns the rate; the video path picks frames
            // against the target instead of scaling time.
            Command::SetRate(_) => {}
        }
        false
    }

    /// Seek and decode until `target_us` is on screen, then deliver one frame.
    /// Discarding frames after the keyframe is what makes the seek exact.
    fn seek_to(&mut self, decoder: &mut Decoder, target_us: i64) -> Result<(), String> {
        decoder.seek(target_us)?;
        let mut last: Option<VideoFrame> = None;
        for _ in 0..MAX_SEEK_FRAMES {
            match decoder.next_frame(&self.pool) {
                Ok(Some(frame)) => {
                    let pts = frame.pts_us;
                    last = Some(frame);
                    if pts >= target_us {
                        break;
                    }
                }
                Ok(None) => break,
                Err(message) => return Err(message),
            }
        }
        if let Some(frame) = last {
            self.note_frame_flags(&frame);
            let frame = Arc::new(frame);
            self.shared.push_history(frame.clone());
            self.deliver_arc(frame);
        }
        Ok(())
    }

    fn deliver(&mut self, frame: VideoFrame) -> bool {
        let frame = Arc::new(frame);
        self.shared.push_history(frame.clone());
        self.deliver_arc(frame)
    }

    /// Hand a frame to the app, staying responsive while the queue is full.
    fn deliver_arc(&mut self, frame: Arc<VideoFrame>) -> bool {
        let epoch = self.shared.epoch();
        loop {
            if self.shared.stopping.load(Ordering::Relaxed) {
                return false;
            }
            match self.frames.try_send((epoch, frame.clone())) {
                Ok(()) => return true,
                Err(TrySendError::Full(_)) => {
                    // The app drains every frame; if it has not, wait a beat and
                    // keep watching for a shutdown or a seek.
                    while let Ok(command) = self.commands.try_recv() {
                        if self.apply(command) {
                            return false;
                        }
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
    }
}

/// How many frames a seek may decode before it gives up looking for the target.
const MAX_SEEK_FRAMES: usize = 900;

/// Demuxer, decoder and the scratch frames it needs.
///
/// The decoder itself is the safe `ffmpeg-next` wrapper; only the hardware
/// attach (before the context is opened) and the plane copy need raw access.
struct Decoder {
    input: ffmpeg::format::context::Input,
    stream_index: usize,
    decoder: ffmpeg::decoder::Video,
    time_base: ffmpeg::Rational,
    /// The frame as the decoder produced it (may live in device memory).
    raw: ffmpeg::frame::Video,
    /// Where a hardware frame is transferred to.
    transferred: ffmpeg::frame::Video,
    /// Where swscale puts a source format the compositor cannot sample.
    scaled: Option<ffmpeg::frame::Video>,
    sws: *mut ffi::SwsContext,
    sws_source: (u32, u32, ffmpeg::format::Pixel),
    /// The device pixel format, when hardware is in use.
    hw_format: Option<c_int>,
    info: MediaInfo,
    backend: HwBackend,
    frame_us: i64,
    rotation: i32,
    rotation_probed: bool,
}

// SAFETY: the only non-`Send` member is the swscale context, which is used
// exclusively by the worker thread that owns this decoder and freed in `Drop`.
unsafe impl Send for Decoder {}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: a non-null swscale context was created by `scale_to_yuv420`
        // and is released exactly once, here.
        unsafe {
            if !self.sws.is_null() {
                ffi::sws_freeContext(self.sws);
            }
        }
    }
}

impl Decoder {
    fn open(source: &MediaSource, hw_accel: HwAccelPref) -> Result<Self, String> {
        let _ = ffmpeg::init();
        let input = ffmpeg::format::input(&source.location())
            .map_err(|e| format!("could not open {}: {e}", source.display()))?;
        let stream = input
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or_else(|| format!("{} has no video stream", source.display()))?;
        let stream_index = stream.index();
        let time_base = stream.time_base();
        let parameters = stream.parameters();
        let media = probe(source)?;
        let codec = ffmpeg::codec::decoder::find(parameters.id())
            .ok_or_else(|| format!("{} uses a codec this build cannot decode", source.display()))?;

        // First attempt: hardware attached when the preference allows it.
        let mut context = ffmpeg::codec::context::Context::from_parameters(parameters.clone())
            .map_err(|e| format!("no decoder for {}: {e}", source.display()))?;
        // SAFETY: the context is not open yet, so its thread fields may be
        // written; `codec` is a static AVCodec owned by FFmpeg.
        let (ctx_ptr, codec_ptr) = unsafe {
            let ctx_ptr = context.as_mut_ptr();
            (*ctx_ptr).thread_count = 4;
            (*ctx_ptr).thread_type = ffi::FF_THREAD_FRAME | ffi::FF_THREAD_SLICE;
            (ctx_ptr, codec.as_ptr())
        };
        let mut hw = if hw_accel.allows_hardware() {
            // SAFETY: `ctx_ptr` is a valid, unopened context and `codec_ptr`
            // matches the id the stream parameters asked for. `attach` installs
            // the device and the `get_format` callback, or returns `None`.
            unsafe { HwDecoder::attach(ctx_ptr, codec_ptr, hw_accel) }
        } else {
            None
        };

        let decoder = match context.decoder().video() {
            Ok(decoder) => decoder,
            Err(first_error) => {
                if hw.is_none() {
                    return Err(format!(
                        "could not open the decoder for {}: {first_error}",
                        source.display()
                    ));
                }
                // The driver refused. The context was consumed by the failed
                // open, so start again without a device.
                hw = None;
                let mut context =
                    ffmpeg::codec::context::Context::from_parameters(parameters.clone())
                        .map_err(|e| format!("no decoder for {}: {e}", source.display()))?;
                // SAFETY: still unopened, so the thread fields are plain data.
                unsafe {
                    let ctx_ptr = context.as_mut_ptr();
                    (*ctx_ptr).thread_count = 4;
                    (*ctx_ptr).thread_type = ffi::FF_THREAD_FRAME | ffi::FF_THREAD_SLICE;
                }
                context.decoder().video().map_err(|e| {
                    format!(
                        "could not open the decoder for {} in software either: {e}",
                        source.display()
                    )
                })?
            }
        };

        let backend = hw.as_ref().map(|hw| hw.backend()).unwrap_or(HwBackend::Software);
        let hw_format = hw.as_ref().map(|hw| hw.device_pix_fmt() as c_int);
        let mut info = media.clone();
        info.converted = false;
        Ok(Self {
            input,
            stream_index,
            decoder,
            time_base,
            raw: ffmpeg::frame::Video::empty(),
            transferred: ffmpeg::frame::Video::empty(),
            scaled: None,
            sws: ptr::null_mut(),
            sws_source: (0, 0, ffmpeg::format::Pixel::None),
            hw_format,
            info,
            backend,
            frame_us: media.frame_us(),
            rotation: media.rotation,
            rotation_probed: false,
        })
    }

    /// Jump to the last keyframe at or before `target_us` and flush.
    fn seek(&mut self, target_us: i64) -> Result<(), String> {
        let tb = self.time_base;
        let ts = if tb.numerator() > 0 {
            (target_us as f64 * f64::from(tb.denominator()) / f64::from(tb.numerator())) as i64
        } else {
            target_us
        };
        // SAFETY: the input context is open and owned here; seeking with
        // `AVSEEK_FLAG_BACKWARD` is the documented way to land on the keyframe
        // at or before the timestamp.
        let ret = unsafe {
            ffi::av_seek_frame(
                self.input.as_mut_ptr(),
                self.stream_index as c_int,
                ts,
                ffi::AVSEEK_FLAG_BACKWARD as c_int,
            )
        };
        if ret < 0 {
            // Fall back to the safe wrapper, which uses `avformat_seek_file`
            // with the same intent (everything up to `ts` is acceptable).
            self.input
                .seek(ts, ..ts)
                .map_err(|e| format!("seek failed: {e}"))?;
        }
        // Everything decoded before the seek is meaningless now.
        self.decoder.flush();
        Ok(())
    }

    /// Decode until a frame is ready.
    fn next_frame(&mut self, pool: &Arc<BufferPool>) -> Result<Option<VideoFrame>, String> {
        loop {
            match self.receive(pool) {
                Ok(Some(frame)) => return Ok(Some(frame)),
                Ok(None) => match self.send_next_packet()? {
                    true => {}
                    false => return Ok(None),
                },
                Err(message) => return Err(message),
            }
        }
    }

    /// Feed one packet from the video stream. False means end of file.
    fn send_next_packet(&mut self) -> Result<bool, String> {
        loop {
            let Some((stream, packet)) = self.input.packets().next() else {
                // Telling the decoder the stream ended makes it drain.
                let _ = self.decoder.send_eof();
                return Ok(false);
            };
            if stream.index() != self.stream_index {
                continue;
            }
            match self.decoder.send_packet(&packet) {
                Ok(()) => return Ok(true),
                Err(ffmpeg::Error::Other {
                    errno: ffmpeg::error::EAGAIN,
                }) => {
                    // The decoder wants its output drained before it takes more
                    // input. `next_frame` receives first, so this only happens
                    // right after a seek; draining one frame and retrying keeps
                    // the packet.
                    let mut scratch = ffmpeg::frame::Video::empty();
                    let _ = self.decoder.receive_frame(&mut scratch);
                    let _ = self.decoder.send_packet(&packet);
                    return Ok(true);
                }
                Err(ffmpeg::Error::Eof) => return Ok(false),
                Err(error) => {
                    return Err(format!("decode error while sending a packet: {error}"));
                }
            }
        }
    }

    /// Pull one decoded frame out of the decoder, if it has one ready.
    fn receive(&mut self, pool: &Arc<BufferPool>) -> Result<Option<VideoFrame>, String> {
        match self.decoder.receive_frame(&mut self.raw) {
            Ok(()) => {}
            Err(ffmpeg::Error::Other {
                errno: ffmpeg::error::EAGAIN,
            })
            | Err(ffmpeg::Error::Eof) => return Ok(None),
            Err(error) => return Err(format!("decode error while receiving a frame: {error}")),
        }
        if !self.rotation_probed {
            // SAFETY: `raw` was just filled by the decoder; the display matrix,
            // if there is one, is owned by that frame.
            self.rotation = display_rotation(&self.raw);
            self.rotation_probed = true;
        }
        Ok(self.build_frame(pool))
    }

    /// Turn the frame the decoder just produced into a [`VideoFrame`], doing the
    /// hardware transfer or the swscale conversion if this source needs it.
    fn build_frame(&mut self, pool: &Arc<BufferPool>) -> Option<VideoFrame> {
        // SAFETY: `raw` was just filled by avcodec_receive_frame.
        let raw = unsafe { self.raw.as_ptr() };
        let raw_format = unsafe { (*raw).format };
        let mut source = raw;
        let mut format;
        let mut converted = false;

        if let Some(hw_format) = self.hw_format {
            if raw_format == hw_format {
                // SAFETY: `raw` holds device memory and `transferred` is an
                // allocated frame that the transfer fills in.
                unsafe { HwDecoder::transfer(raw, self.transferred.as_mut_ptr()) }.ok()?;
                // SAFETY: the transfer just filled `transferred`.
                source = unsafe { self.transferred.as_ptr() };
                let transferred_format = unsafe { (*source).format };
                format = pixel_format_to_frame_format(transferred_format);
            } else {
                format = pixel_format_to_frame_format(raw_format);
            }
        } else {
            format = pixel_format_to_frame_format(raw_format);
        }

        if format.is_none() {
            // SAFETY: `source` is a valid frame produced above.
            if !unsafe { self.scale_to_yuv420(source) } {
                return None;
            }
            source = self.scaled.as_ref().map(|frame| unsafe { frame.as_ptr() })?;
            format = Some(FrameFormat::Yuv420p);
            converted = true;
        }

        // SAFETY: `source` is a valid frame with width, height and planes set.
        unsafe { frame_to_video(source, format?, self.time_base, pool, converted) }
    }

    /// swscale into a reusable YUV420P frame.
    ///
    /// # Safety
    /// `source` must be a valid frame produced by this decoder.
    unsafe fn scale_to_yuv420(&mut self, source: *const ffi::AVFrame) -> bool {
        // SAFETY: the caller guarantees `source` is valid.
        let (width, height) = unsafe { ((*source).width.max(1), (*source).height.max(1)) };
        // Every frame this decoder hands back carries the decoder's pixel
        // format, which is the one swscale has to be configured for.
        let format = self.decoder.format();
        if self.sws.is_null() || self.sws_source != (width as u32, height as u32, format) {
            // SAFETY: a previous context, if any, is ours to free.
            unsafe {
                if !self.sws.is_null() {
                    ffi::sws_freeContext(self.sws);
                }
            }
            // SAFETY: all arguments describe the source and a YUV420P
            // destination of the same size; `BILINEAR` is a supported flag and
            // the three nulls are the default scaling callbacks.
            self.sws = unsafe {
                ffi::sws_getContext(
                    width,
                    height,
                    format.into(),
                    width,
                    height,
                    ffi::AVPixelFormat::AV_PIX_FMT_YUV420P,
                    ffmpeg::software::scaling::Flags::BILINEAR.bits(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            if self.sws.is_null() {
                return false;
            }
            self.sws_source = (width as u32, height as u32, format);
            self.scaled = Some(ffmpeg::frame::Video::new(
                ffmpeg::format::Pixel::YUV420P,
                width as u32,
                height as u32,
            ));
        }
        let Some(scaled) = self.scaled.as_mut() else {
            return false;
        };
        // SAFETY: the destination was allocated for exactly this size and
        // format, and the source is a valid frame of the same dimensions.
        unsafe {
            let destination = scaled.as_mut_ptr();
            let written = ffi::sws_scale(
                self.sws,
                (*source).data.as_ptr() as *const *const u8,
                (*source).linesize.as_ptr(),
                0,
                height,
                (*destination).data.as_ptr(),
                (*destination).linesize.as_ptr(),
            );
            written > 0
        }
    }
}

fn pixel_format_to_frame_format(format: c_int) -> Option<FrameFormat> {
    if format == ffi::AVPixelFormat::AV_PIX_FMT_NV12 as c_int {
        Some(FrameFormat::Nv12)
    } else if format == ffi::AVPixelFormat::AV_PIX_FMT_YUV420P as c_int {
        Some(FrameFormat::Yuv420p)
    } else {
        None
    }
}

/// Copy the planes out of an `AVFrame`, keeping FFmpeg's stride.
///
/// # Safety
/// `frame` must be a valid, decoded `AVFrame` whose format is `format`.
unsafe fn frame_to_video(
    frame: *const ffi::AVFrame,
    format: FrameFormat,
    time_base: ffmpeg::Rational,
    pool: &Arc<BufferPool>,
    converted: bool,
) -> Option<VideoFrame> {
    // SAFETY: the caller guarantees the frame is valid.
    let (width, height) = unsafe { ((*frame).width.max(1) as u32, (*frame).height.max(1) as u32) };
    let (chroma_w, chroma_h) = format.chroma_dims(width, height);
    let sizes = [(width, height), (chroma_w, chroma_h), (chroma_w, chroma_h)];
    let range = unsafe {
        if (*frame).color_range == ffi::AVColorRange::AVCOL_RANGE_JPEG {
            ColorRange::Full
        } else {
            ColorRange::Limited
        }
    };
    let pts = unsafe { frame_timestamp(frame, time_base) };

    let mut planes: [PlaneBuf; 3] = [pool.alloc(0), pool.alloc(0), pool.alloc(0)];
    let mut strides = [0usize; 3];
    for index in 0..format.plane_count() {
        let (plane_w, plane_h) = sizes[index];
        // SAFETY: the indices are inside a valid frame's fixed-size arrays, and
        // plane_count() never exceeds the planes FFmpeg set up for `format`.
        let (data, stride) = unsafe {
            let ptr = (*frame).data[index];
            let stride = (*frame).linesize[index];
            if ptr.is_null() || stride == 0 {
                (ptr::null_mut(), 0)
            } else {
                let packed = (stride.unsigned_abs() as usize).max(plane_w as usize);
                let base = if stride < 0 {
                    ptr.add((plane_h as usize - 1) * packed)
                } else {
                    ptr
                };
                (base, packed)
            }
        };
        if data.is_null() {
            return None;
        }
        let bytes = stride * plane_h as usize;
        let mut buffer = pool.alloc(bytes);
        // SAFETY: the source plane has `stride * plane_h` readable bytes as
        // described by `linesize`, and the destination was just allocated for
        // exactly that many bytes.
        unsafe {
            ptr::copy_nonoverlapping(data, buffer.as_mut_slice().as_mut_ptr(), bytes);
        }
        planes[index] = buffer;
        strides[index] = stride;
    }

    let mut frame = VideoFrame::from_planes(format, width, height, planes, strides, pts, range);
    frame.converted = converted;
    Some(frame)
}

/// Presentation timestamp in microseconds, from an `AVFrame`.
///
/// # Safety
/// `frame` must be a valid `AVFrame`.
unsafe fn frame_timestamp(frame: *const ffi::AVFrame, time_base: ffmpeg::Rational) -> i64 {
    unsafe {
        let mut ts = (*frame).best_effort_timestamp;
        if ts == NO_PTS {
            ts = (*frame).pts;
        }
        if ts == NO_PTS {
            return 0;
        }
        // The union timeline is 0-based, so a negative container start is
        // clamped rather than shifting every other layer.
        let ts = ts.max(0);
        if time_base.numerator() <= 0 {
            return ts;
        }
        (ts as f64 * f64::from(time_base.numerator()) / f64::from(time_base.denominator())
            * 1_000_000.0) as i64
    }
}

/// Rotation from the display matrix in the frame's side data, in degrees.
///
/// The matrix is a 9-entry row-major `i32` matrix, and its first two entries are
/// the top row of the 2×2 rotation block, which is enough for the four right
/// angles phones and cameras actually record.
fn display_rotation(frame: &ffmpeg::Frame) -> i32 {
    let Some(side) = frame.side_data(ffmpeg::frame::side_data::Type::DisplayMatrix) else {
        return 0;
    };
    let data = side.data();
    if data.len() < 8 {
        return 0;
    }
    let mut matrix = [0i32; 2];
    let (words, _) = data.as_chunks::<4>();
    for (slot, word) in matrix.iter_mut().zip(words) {
        *slot = i32::from_ne_bytes(*word);
    }
    let angle = -(matrix[1] as f64).atan2(matrix[0] as f64).to_degrees().round() as i32;
    match angle.rem_euclid(360) {
        0 => 0,
        90 => 90,
        180 => 180,
        270 => 270,
        _ => 0,
    }
}

/// The audio half of a track: its own demuxer, decoder and resampler, writing
/// interleaved stereo `f32` at the output device's rate into the slot's ring.
struct AudioWorker {
    source: MediaSource,
    shared: Arc<TrackShared>,
    slot: Arc<AudioSlot>,
    rate: u32,
    commands: Receiver<Command>,
    paused: bool,
}

impl AudioWorker {
    fn run(mut self) {
        let _ = ffmpeg::init();
        let mut input = match ffmpeg::format::input(&self.source.location()) {
            Ok(input) => input,
            Err(_) => return, // the video worker already reported the failure
        };
        let Some(stream) = input.streams().best(ffmpeg::media::Type::Audio) else {
            return;
        };
        let stream_index = stream.index();
        let time_base = stream.time_base();
        let Ok(context) = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        else {
            return;
        };
        let Ok(mut decoder) = context.decoder().audio() else {
            return;
        };
        let Ok(mut resampler) = ffmpeg::software::resampling::Context::get(
            decoder.format(),
            decoder.channel_layout(),
            decoder.rate(),
            ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
            ffmpeg::ChannelLayout::STEREO,
            self.rate,
        ) else {
            return;
        };

        let Some(mut producer) = self.slot.take_producer() else {
            return;
        };
        let mut fractional = FractionalResampler::new();
        let mut samples: Vec<f32> = Vec::new();
        let mut resampled: Vec<f32> = Vec::new();
        let mut frame = ffmpeg::frame::Audio::empty();

        loop {
            while let Ok(command) = self.commands.try_recv() {
                match command {
                    Command::Shutdown => return,
                    Command::Play => {
                        self.paused = false;
                        self.slot.set_active(true);
                    }
                    Command::Pause => {
                        self.paused = true;
                        self.slot.set_active(false);
                    }
                    Command::Seek { target_us } => {
                        let ts = if time_base.numerator() > 0 {
                            (target_us as f64 * f64::from(time_base.denominator())
                                / f64::from(time_base.numerator())) as i64
                        } else {
                            target_us
                        };
                        let _ = input.seek(ts, ..ts);
                        decoder.flush();
                        fractional.reset();
                        // Tell the mixer to throw away the old position.
                        self.slot.bump_epoch();
                    }
                    Command::SetRate(rate) => fractional.set_rate(rate),
                    Command::Step { .. } => {}
                }
            }
            if self.shared.stopping.load(Ordering::Relaxed) {
                return;
            }
            if self.paused {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            if !decode_audio_frame(&mut input, &mut decoder, stream_index, &mut frame) {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            let mut converted = ffmpeg::frame::Audio::empty();
            if resampler.run(&frame, &mut converted).is_err() {
                continue;
            }
            samples.clear();
            push_f32(&converted, &mut samples);
            resampled.clear();
            fractional.process(&samples, &mut resampled);
            if !push_blocking(&mut producer, &resampled, &self.shared) {
                return;
            }
        }
    }
}

/// Decode the next audio frame for `stream_index`, writing it into `frame`.
fn decode_audio_frame(
    input: &mut ffmpeg::format::context::Input,
    decoder: &mut ffmpeg::decoder::Audio,
    stream_index: usize,
    frame: &mut ffmpeg::frame::Audio,
) -> bool {
    loop {
        match decoder.receive_frame(frame) {
            Ok(()) => return true,
            Err(ffmpeg::Error::Other {
                errno: ffmpeg::error::EAGAIN,
            }) => {}
            Err(_) => return false,
        }
        let mut sent = false;
        for (stream, packet) in input.packets() {
            if stream.index() != stream_index {
                continue;
            }
            if decoder.send_packet(&packet).is_ok() {
                sent = true;
            }
            break;
        }
        if !sent {
            let _ = decoder.send_eof();
            match decoder.receive_frame(frame) {
                Ok(()) => return true,
                Err(_) => return false,
            }
        }
    }
}

/// Copy an audio frame's packed `f32` samples out.
fn push_f32(frame: &ffmpeg::frame::Audio, out: &mut Vec<f32>) {
    let data = frame.data(0);
    let (words, _) = data.as_chunks::<4>();
    out.extend(words.iter().map(|word| f32::from_le_bytes(*word)));
}

/// Push samples into the ring, waiting for room while staying responsive.
fn push_blocking(
    producer: &mut ringbuf::HeapProd<f32>,
    samples: &[f32],
    shared: &Arc<TrackShared>,
) -> bool {
    use ringbuf::traits::Producer;
    let mut offset = 0;
    while offset < samples.len() {
        if shared.stopping.load(Ordering::Relaxed) {
            return false;
        }
        let pushed = producer.push_slice(&samples[offset..]);
        if pushed == 0 {
            thread::sleep(Duration::from_millis(2));
        } else {
            offset += pushed;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_format_classification() {
        assert_eq!(
            classify_pixel_format(ffmpeg::format::Pixel::NV12),
            (FrameFormat::Nv12, false)
        );
        assert_eq!(
            classify_pixel_format(ffmpeg::format::Pixel::YUV420P),
            (FrameFormat::Yuv420p, false)
        );
        assert_eq!(
            classify_pixel_format(ffmpeg::format::Pixel::YUV420P10LE),
            (FrameFormat::Yuv420p, true)
        );
        assert_eq!(
            classify_pixel_format(ffmpeg::format::Pixel::YUVA444P),
            (FrameFormat::Yuv420p, true)
        );
    }

    #[test]
    fn media_info_frame_duration() {
        let info = MediaInfo {
            duration_us: 1_000_000,
            width: 1920,
            height: 1080,
            fps_num: 60_000,
            fps_den: 1_001,
            source_format: FrameFormat::Nv12,
            converted: false,
            has_audio: true,
            audio_channels: 2,
            audio_rate: 48_000,
            video_stream: 0,
            audio_stream: Some(1),
            rotation: 0,
        };
        assert_eq!(info.frame_us(), 16_683);
    }

    #[test]
    fn shared_state_starts_empty() {
        let shared = TrackShared::new();
        assert!(shared.info().is_none());
        assert!(shared.error().is_none());
        assert_eq!(shared.backend(), HwBackend::Software);
        assert_eq!(shared.epoch(), 0);
        assert_eq!(shared.history_len(), 0);
        assert!(shared.frame_before(0, 1).is_none());
        assert!(shared.history_first().is_none());
    }

    #[test]
    fn probe_rejects_a_missing_file() {
        let source = MediaSource::file("/nonexistent/never-there.mp4");
        let err = probe(&source).unwrap_err();
        assert!(err.contains("could not open"), "{err}");
    }

    #[test]
    fn commands_are_copy_and_comparable() {
        assert_eq!(Command::Play, Command::Play);
        assert_ne!(Command::Pause, Command::Play);
        assert_eq!(
            Command::Seek { target_us: 42 },
            Command::Seek { target_us: 42 }
        );
    }
}

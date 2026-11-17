//! Hardware-accelerated decoding via `ffmpeg-sys-next`.
//!
//! The safe `ffmpeg-next` API does not expose `AVHWDeviceContext`s, so this
//! module talks to the C API directly. Every `unsafe` block carries a safety
//! comment; in edition 2024 the blocks live *inside* `unsafe fn`s so that
//! `unsafe_op_in_unsafe_fn` stays quiet.
//!
//! ## Flow
//!
//! 1. [`device_candidates`] lists the devices to try, in platform order.
//! 2. [`probe_codec_hw_support`] walks `avcodec_get_hw_config()` to confirm the
//!    decoder supports the device pixel format.
//! 3. `av_hwdevice_ctx_create` creates the device; its refcounted handle is
//!    attached to the `AVCodecContext` as `hw_device_ctx`.
//! 4. A `get_format` callback is installed that picks the hardware pixel format.
//! 5. After `avcodec_send_packet`/`avcodec_receive_frame`, a frame in hardware
//!    memory is copied to system memory with `av_hwframe_transfer_data` into
//!    **NV12** (or YUV420P for `VAAPI` when the driver only offers that).
//!
//! Any failure along the way is reported as [`HwError`] and the caller silently
//! falls back to software decoding.

use std::ffi::{c_int, c_void};
use std::ptr;

use ffmpeg_next::ffi;

use crate::model::HardwareAccel;

/// Which decoder backend a track ended up using. The UI shows a badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DecodeBackend {
    /// VA-API (Linux).
    Vaapi,
    /// Vulkan video decode (Linux).
    Vulkan,
    /// VDPAU (Linux, X11 only).
    Vdpau,
    /// D3D11VA (Windows).
    D3d11va,
    /// DXVA2 (Windows, legacy).
    Dxva2,
    /// VideoToolbox (macOS).
    VideoToolbox,
    /// CPU decoding.
    #[default]
    Software,
}

impl DecodeBackend {
    /// Short badge label shown in the transport bar.
    pub fn badge(self) -> &'static str {
        match self {
            DecodeBackend::Vaapi => "VAAPI",
            DecodeBackend::Vulkan => "VULKAN",
            DecodeBackend::Vdpau => "VDPAU",
            DecodeBackend::D3d11va => "D3D11VA",
            DecodeBackend::Dxva2 => "DXVA2",
            DecodeBackend::VideoToolbox => "VT",
            DecodeBackend::Software => "SW",
        }
    }

    /// True when frames come from a hardware device.
    pub fn is_hardware(self) -> bool {
        self != DecodeBackend::Software
    }

    /// Relative quality ranking, used to pick the best badge when a session
    /// mixes backends across layers. Higher is better.
    pub fn rank(self) -> u8 {
        match self {
            DecodeBackend::Software => 0,
            // VDPAU and DXVA2 are the legacy paths; the rest are current.
            DecodeBackend::Vdpau => 1,
            DecodeBackend::Dxva2 => 1,
            DecodeBackend::Vaapi => 2,
            DecodeBackend::Vulkan => 3,
            DecodeBackend::D3d11va => 3,
            DecodeBackend::VideoToolbox => 3,
        }
    }
}

/// Colour range of a decoded frame, used by both the shader and the CPU path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorRange {
    /// Limited / MPEG range (16-235).
    #[default]
    Limited,
    /// Full / JPEG range (0-255).
    Full,
}

impl ColorRange {
    /// `1.0` selects limited range, `0.0` full range in the shader uniform.
    pub fn shader_flag(self) -> f32 {
        match self {
            ColorRange::Limited => 1.0,
            ColorRange::Full => 0.0,
        }
    }

    /// Interpret FFmpeg's range enum.
    pub fn from_ffmpeg(range: ffmpeg_next::color::Range) -> Self {
        match range {
            ffmpeg_next::color::Range::JPEG => ColorRange::Full,
            _ => ColorRange::Limited,
        }
    }
}

/// A hardware device type together with its pixel format and backend badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HwCandidate {
    /// The backend this candidate maps to.
    pub backend: DecodeBackend,
    /// FFmpeg device type, as the generated enum.
    pub device_type: ffi::AVHWDeviceType,
    /// Pixel format this device decodes to, as the generated enum.
    pub pix_fmt: ffi::AVPixelFormat,
    /// Name passed to `av_hwdevice_ctx_create`.
    pub device_name: &'static str,
    /// Config method bit to look for in `avcodec_get_hw_config`, a plain `int`
    /// in the C header and therefore a `c_int` here.
    pub config_method: c_int,
}

impl HwCandidate {
    /// Whether the decoder should be asked for a "reference" frame
    /// (`AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX`) or can decode fully inside
    /// the device.
    pub fn is_device_ctx(self) -> bool {
        self.config_method == ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as c_int
    }
}

/// Reasons hardware decoding could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HwError {
    /// The platform has no hardware candidates (or Software Only was chosen).
    NoCandidates,
    /// The decoder does not advertise support for any candidate.
    UnsupportedCodec,
    /// Creating the device failed. Contains the errno.
    DeviceCreation(i32),
    /// Attaching the device or installing `get_format` failed.
    Setup(String),
    /// Transferring a hardware frame to system memory failed.
    Transfer(i32),
    /// The caller disabled hardware decoding.
    Disabled,
}

impl std::fmt::Display for HwError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HwError::NoCandidates => write!(f, "no hardware candidates on this platform"),
            HwError::UnsupportedCodec => write!(f, "codec does not support hardware decode"),
            HwError::DeviceCreation(e) => write!(f, "av_hwdevice_ctx_create failed ({e})"),
            HwError::Setup(msg) => write!(f, "hardware setup failed: {msg}"),
            HwError::Transfer(e) => write!(f, "av_hwframe_transfer_data failed ({e})"),
            HwError::Disabled => write!(f, "hardware decoding disabled by the user"),
        }
    }
}

/// The device candidate order for the current platform.
///
/// * Linux: VAAPI -> Vulkan -> VDPAU
/// * Windows: D3D11VA -> DXVA2
/// * macOS: VideoToolbox
/// * anything else: none
pub fn device_candidates() -> Vec<HwCandidate> {
    let mut out = Vec::new();
    #[cfg(target_os = "linux")]
    {
        out.push(HwCandidate {
            backend: DecodeBackend::Vaapi,
            device_type: ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
            pix_fmt: ffi::AVPixelFormat::AV_PIX_FMT_VAAPI,
            device_name: "/dev/dri/renderD128",
            config_method: ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as c_int,
        });
        out.push(HwCandidate {
            backend: DecodeBackend::Vulkan,
            device_type: ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN,
            pix_fmt: ffi::AVPixelFormat::AV_PIX_FMT_VULKAN,
            device_name: "",
            config_method: ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as c_int,
        });
        out.push(HwCandidate {
            backend: DecodeBackend::Vdpau,
            device_type: ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VDPAU,
            pix_fmt: ffi::AVPixelFormat::AV_PIX_FMT_VDPAU,
            device_name: "",
            config_method: ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as c_int,
        });
    }
    #[cfg(target_os = "windows")]
    {
        out.push(HwCandidate {
            backend: DecodeBackend::D3d11va,
            device_type: ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
            pix_fmt: ffi::AVPixelFormat::AV_PIX_FMT_D3D11,
            device_name: "",
            config_method: ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as c_int,
        });
        out.push(HwCandidate {
            backend: DecodeBackend::Dxva2,
            device_type: ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_DXVA2,
            pix_fmt: ffi::AVPixelFormat::AV_PIX_FMT_DXVA2_VLD,
            device_name: "",
            config_method: ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as c_int,
        });
    }
    #[cfg(target_os = "macos")]
    {
        out.push(HwCandidate {
            backend: DecodeBackend::VideoToolbox,
            device_type: ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
            pix_fmt: ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX,
            device_name: "",
            config_method: ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as c_int,
        });
    }
    out
}

/// Check that `codec` advertises `candidate` in its hardware config list.
///
/// # Safety
///
/// `codec` must be a valid `AVCodec` pointer owned by FFmpeg (it is obtained
/// from `AVCodecContext::codec`, which is never NULL once the decoder context
/// has been created from stream parameters).
pub unsafe fn probe_codec_hw_support(
    codec: *const ffi::AVCodec,
    candidate: &HwCandidate,
) -> bool {
    if codec.is_null() {
        return false;
    }
    let mut seen = false;
    // SAFETY: `codec` was checked non-null above and points at a codec owned by
    // FFmpeg. `avcodec_get_hw_config` returns NULL once the list ends, so the
    // loop terminates and each non-null element is a valid config struct that
    // lives as long as the codec.
    unsafe {
        let mut index: c_int = 0;
        loop {
            let config = ffi::avcodec_get_hw_config(codec, index);
            if config.is_null() {
                break;
            }
            let cfg = &*config;
            if cfg.pix_fmt == candidate.pix_fmt
                && cfg.methods & candidate.config_method != 0
            {
                seen = true;
                break;
            }
            index += 1;
            // A decoder will never publish more than a handful of configs; the
            // guard keeps a malformed list from spinning forever.
            if index > 64 {
                break;
            }
        }
    }
    seen
}

/// Owns an `AVBufferRef*` returned by `av_hwdevice_ctx_create`.
pub struct HwDevice {
    /// The refcounted device context (`AVBufferRef*` whose `data` is the
    /// `AVHWDeviceContext`). Null only after `release`.
    raw: *mut ffi::AVBufferRef,
    /// Which candidate produced it.
    pub candidate: HwCandidate,
}

// SAFETY: av_hwdevice_ctx_create returns a refcounted AVBufferRef whose
// internal reference counting is thread safe (`av_buffer_ref`/`av_buffer_unref`
// are atomic). The device context itself is only used from the owning decode
// worker thread, which is the only thread that ever dereferences it.
unsafe impl Send for HwDevice {}

impl HwDevice {
    /// Create a hardware device for `candidate`.
    ///
    /// # Safety
    ///
    /// FFmpeg's global initialisation (`ffmpeg::init`) must have run, and the
    /// caller must not free the returned context except through `release`.
    pub unsafe fn create(candidate: HwCandidate) -> Result<Self, HwError> {
        let mut raw: *mut ffi::AVBufferRef = ptr::null_mut();
        let cname = std::ffi::CString::new(candidate.device_name)
            .map_err(|_| HwError::Setup("device name contains NUL".into()))?;
        let name_ptr = if candidate.device_name.is_empty() {
            ptr::null()
        } else {
            cname.as_ptr()
        };
        // SAFETY: `raw` is a valid out-pointer; `name_ptr` is either NULL or a
        // NUL-terminated C string that outlives the call; the options argument
        // is NULL which FFmpeg documents as "use defaults".
        let ret = unsafe {
            ffi::av_hwdevice_ctx_create(
                &mut raw,
                candidate.device_type,
                name_ptr,
                ptr::null_mut(),
                0,
            )
        };
        if ret < 0 || raw.is_null() {
            if !raw.is_null() {
                // SAFETY: `raw` was set by FFmpeg and is a valid refcounted
                // buffer that we own; releasing it balances the creation.
                unsafe { ffi::av_buffer_unref(&mut raw) };
            }
            return Err(HwError::DeviceCreation(ret));
        }
        Ok(Self { raw, candidate })
    }

    /// Take another reference suitable for storing in `AVCodecContext`. The
    /// returned pointer must be freed by FFmpeg when the codec context dies.
    ///
    /// # Safety
    ///
    /// The device must not have been released.
    pub unsafe fn clone_ref(&self) -> *mut ffi::AVBufferRef {
        if self.raw.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `self.raw` is a live refcounted buffer; `av_buffer_ref`
        // atomically increments the refcount and returns the same pointer.
        unsafe { ffi::av_buffer_ref(self.raw) }
    }

    /// Release the device reference. Safe to call twice.
    pub fn release(&mut self) {
        if !self.raw.is_null() {
            let mut raw = self.raw;
            // SAFETY: we own exactly one reference to this buffer, and set
            // `self.raw` to null afterwards so the reference is not released
            // twice.
            unsafe {
                ffi::av_buffer_unref(&mut raw);
            }
            self.raw = ptr::null_mut();
        }
    }
}

impl Drop for HwDevice {
    fn drop(&mut self) {
        self.release();
    }
}

/// The `get_format` callback installed on the codec context.
///
/// # Safety
///
/// Called by libavcodec while decoding. `ctx` is the `AVCodecContext` we
/// configured and `pix_fmts` is a NULL-terminated list owned by FFmpeg that is
/// valid for the duration of the call. We only read them.
pub unsafe extern "C" fn get_format_cb(
    ctx: *mut ffi::AVCodecContext,
    pix_fmts: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    if pix_fmts.is_null() {
        return ffi::AVPixelFormat::AV_PIX_FMT_NONE;
    }
    // SAFETY: `ctx` is the context FFmpeg is decoding with; it is alive for the
    // whole decode session. Reading its `opaque` field is a plain field access.
    let target = unsafe { (*ctx).opaque as usize as i32 };
    // SAFETY: `pix_fmts` is a NULL-terminated array of enum values supplied by
    // FFmpeg; we stop at the first AV_PIX_FMT_NONE sentinel. Discriminants are
    // read through `as i32` because `opaque` carries a plain integer.
    unsafe {
        let mut i = 0isize;
        loop {
            let fmt = *pix_fmts.offset(i);
            if fmt == ffi::AVPixelFormat::AV_PIX_FMT_NONE {
                break;
            }
            if fmt as i32 == target {
                return fmt;
            }
            i += 1;
            if i > 256 {
                break;
            }
        }
    }
    ffi::AVPixelFormat::AV_PIX_FMT_NONE
}

/// Attach `device` and the `get_format` callback to `ctx`.
///
/// # Safety
///
/// `ctx` must be the `AVCodecContext` of a decoder that has not been opened
/// yet (`avcodec_open2` must still be in the future), and `device` must be
/// alive. On success the codec context owns its own reference to the device.
pub unsafe fn attach_device(
    ctx: *mut ffi::AVCodecContext,
    device: &HwDevice,
    candidate: HwCandidate,
) -> Result<(), HwError> {
    if ctx.is_null() {
        return Err(HwError::Setup("null codec context".into()));
    }
    // SAFETY: `device` is alive and owns a reference; `clone_ref` hands us a
    // second reference whose ownership is transferred to the codec context,
    // which releases it in avcodec_close/avcodec_free_context.
    let cloned = unsafe { device.clone_ref() };
    if cloned.is_null() {
        return Err(HwError::Setup("could not reference the hw device".into()));
    }
    // SAFETY: `ctx` is a live codec context we exclusively own; storing the
    // cloned reference there transfers ownership to FFmpeg. `opaque` carries
    // the wanted pixel format into `get_format_cb`.
    unsafe {
        let old = (*ctx).hw_device_ctx;
        if !old.is_null() {
            let mut old = old;
            ffi::av_buffer_unref(&mut old);
        }
        (*ctx).hw_device_ctx = cloned;
        (*ctx).get_format = Some(get_format_cb);
        (*ctx).opaque = candidate.pix_fmt as usize as *mut c_void;
    }
    Ok(())
}

/// Allocate a system-memory `AVFrame` suitable for `av_hwframe_transfer_data`.
///
/// # Safety
///
/// The source `frame` must be a valid hardware frame; the returned frame must
/// be freed with `av_frame_free` (the caller does this through
/// `ffmpeg_next::util::frame::Video::wrap` after construction, or with
/// `av_frame_free` directly).
pub unsafe fn allocate_transfer_frame(
    src: *const ffi::AVFrame,
    dst_format: ffi::AVPixelFormat,
) -> Result<*mut ffi::AVFrame, HwError> {
    if src.is_null() {
        return Err(HwError::Setup("null source frame".into()));
    }
    // SAFETY: `src` is non-null; we read width/height to size the destination.
    let (w, h) = unsafe { ((*src).width, (*src).height) };
    if w <= 0 || h <= 0 {
        return Err(HwError::Setup("hardware frame has no dimensions".into()));
    }
    // SAFETY: `av_frame_alloc` returns an empty owned frame or NULL.
    let dst = unsafe { ffi::av_frame_alloc() };
    if dst.is_null() {
        return Err(HwError::Setup("av_frame_alloc failed".into()));
    }
    // SAFETY: `dst` is a valid owned frame; we set only scalar fields that
    // FFmpeg documents before calling av_frame_get_buffer.
    unsafe {
        // `AVFrame::format` is a plain `c_int` in the C struct.
        (*dst).format = dst_format as c_int;
        (*dst).width = w;
        (*dst).height = h;
        let ret = ffi::av_frame_get_buffer(dst, 1);
        if ret < 0 {
            ffi::av_frame_free(&mut { dst });
            return Err(HwError::Transfer(ret));
        }
    }
    Ok(dst)
}

/// Transfer a hardware frame into `dst` (a system-memory frame).
///
/// # Safety
///
/// Both frames must be valid and distinct.
pub unsafe fn transfer_to_system(
    src: *const ffi::AVFrame,
    dst: *mut ffi::AVFrame,
) -> Result<(), HwError> {
    // SAFETY: both pointers are valid frames owned by the caller and are
    // distinct buffers, as required by av_hwframe_transfer_data.
    let ret = unsafe { ffi::av_hwframe_transfer_data(dst, src, 0) };
    if ret < 0 {
        Err(HwError::Transfer(ret))
    } else {
        Ok(())
    }
}

/// Free a frame allocated by [`allocate_transfer_frame`].
///
/// # Safety
///
/// The pointer must come from `av_frame_alloc` and not be used afterwards.
pub unsafe fn free_frame(frame: *mut ffi::AVFrame) {
    if !frame.is_null() {
        // SAFETY: ownership of the frame is transferred into this call; the
        // local copy of the pointer is dropped immediately afterwards.
        unsafe {
            let mut f = frame;
            ffi::av_frame_free(&mut f);
        }
    }
}

/// Decide which candidates to try for a given preference.
pub fn candidates_for(preference: HardwareAccel) -> Vec<HwCandidate> {
    match preference {
        HardwareAccel::SoftwareOnly => Vec::new(),
        HardwareAccel::Auto | HardwareAccel::HardwarePreferred => device_candidates(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_badge_is_sw() {
        assert_eq!(DecodeBackend::Software.badge(), "SW");
        assert!(!DecodeBackend::Software.is_hardware());
    }

    #[test]
    fn all_backend_badges() {
        assert_eq!(DecodeBackend::Vaapi.badge(), "VAAPI");
        assert_eq!(DecodeBackend::Vulkan.badge(), "VULKAN");
        assert_eq!(DecodeBackend::Vdpau.badge(), "VDPAU");
        assert_eq!(DecodeBackend::D3d11va.badge(), "D3D11VA");
        assert_eq!(DecodeBackend::Dxva2.badge(), "DXVA2");
        assert_eq!(DecodeBackend::VideoToolbox.badge(), "VT");
    }

    #[test]
    fn software_only_has_no_candidates() {
        assert!(candidates_for(HardwareAccel::SoftwareOnly).is_empty());
    }

    #[test]
    fn auto_and_preferred_use_the_platform_order() {
        let auto = candidates_for(HardwareAccel::Auto);
        let preferred = candidates_for(HardwareAccel::HardwarePreferred);
        assert_eq!(auto, preferred);
        #[cfg(target_os = "linux")]
        {
            let backends: Vec<DecodeBackend> = auto.iter().map(|c| c.backend).collect();
            assert_eq!(
                backends,
                vec![
                    DecodeBackend::Vaapi,
                    DecodeBackend::Vulkan,
                    DecodeBackend::Vdpau
                ]
            );
        }
        #[cfg(target_os = "windows")]
        {
            let backends: Vec<DecodeBackend> = auto.iter().map(|c| c.backend).collect();
            assert_eq!(backends, vec![DecodeBackend::D3d11va, DecodeBackend::Dxva2]);
        }
        #[cfg(target_os = "macos")]
        {
            let backends: Vec<DecodeBackend> = auto.iter().map(|c| c.backend).collect();
            assert_eq!(backends, vec![DecodeBackend::VideoToolbox]);
        }
    }

    #[test]
    fn probing_a_null_codec_is_false() {
        let candidate = HwCandidate {
            backend: DecodeBackend::Vaapi,
            device_type: ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_NONE,
            pix_fmt: ffi::AVPixelFormat::AV_PIX_FMT_NONE,
            device_name: "",
            config_method: 0,
        };
        // SAFETY: the function explicitly handles a null codec pointer.
        assert!(!unsafe { probe_codec_hw_support(std::ptr::null(), &candidate) });
    }

    #[test]
    fn color_range_flags_match_shader_convention() {
        assert_eq!(ColorRange::Limited.shader_flag(), 1.0);
        assert_eq!(ColorRange::Full.shader_flag(), 0.0);
        assert_eq!(
            ColorRange::from_ffmpeg(ffmpeg_next::color::Range::JPEG),
            ColorRange::Full
        );
        assert_eq!(
            ColorRange::from_ffmpeg(ffmpeg_next::color::Range::MPEG),
            ColorRange::Limited
        );
        assert_eq!(
            ColorRange::from_ffmpeg(ffmpeg_next::color::Range::Unspecified),
            ColorRange::Limited
        );
    }

    #[test]
    fn hw_error_display_is_informative() {
        assert!(HwError::DeviceCreation(-5).to_string().contains("-5"));
        assert!(HwError::UnsupportedCodec.to_string().contains("hardware"));
    }
}

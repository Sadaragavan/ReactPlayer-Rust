//! Hardware decode devices.
//!
//! `ffmpeg-next`'s safe API does not expose `AVCodecContext::hw_device_ctx` or
//! the `get_format` callback, so this module talks to `ffmpeg-sys-next`
//! directly. Everything here is `unsafe` by necessity; each block states what
//! makes it sound.
//!
//! The flow for one decoder:
//! 1. [`HwBackend::candidates`] gives the platform's preference order.
//! 2. For each, check `avcodec_get_hw_config` for a hardware pixel format the
//!    codec supports through a device context.
//! 3. `av_hwdevice_ctx_create` opens the device, and `av_buffer_ref` shares it
//!    with the codec context.
//! 4. A `get_format` callback picks the hardware pixel format when the decoder
//!    offers it and the best software one when it does not.
//! 5. Frames come back in the hardware format; [`HwDecoder::transfer`] copies
//!    them into an NV12/YUV420P system-memory frame with
//!    `av_hwframe_transfer_data`.
//!
//! Any failure returns `None` and the caller decodes in software instead.

use std::cell::Cell;
use std::ffi::c_int;
use std::ptr;

use ffmpeg_next::ffi;

use crate::model::settings::HwAccelPref;

/// Which decoder actually produced the frames on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HwBackend {
    Vaapi,
    Vulkan,
    Vdpau,
    D3d11va,
    Dxva2,
    VideoToolbox,
    Software,
}

impl HwBackend {
    /// The badge the layer shows.
    pub fn badge(self) -> &'static str {
        match self {
            Self::Vaapi => "VAAPI",
            Self::Vulkan => "VULKAN",
            Self::Vdpau => "VDPAU",
            Self::D3d11va => "D3D11VA",
            Self::Dxva2 => "DXVA2",
            Self::VideoToolbox => "VT",
            Self::Software => "SW",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Vaapi => "VA-API",
            Self::Vulkan => "Vulkan video",
            Self::Vdpau => "VDPAU",
            Self::D3d11va => "D3D11VA",
            Self::Dxva2 => "DXVA2",
            Self::VideoToolbox => "VideoToolbox",
            Self::Software => "software",
        }
    }

    pub fn is_hardware(self) -> bool {
        !matches!(self, Self::Software)
    }

    /// Compact code for the atomics that publish this to the UI.
    pub fn code(self) -> u8 {
        match self {
            Self::Software => 0,
            Self::Vaapi => 1,
            Self::Vulkan => 2,
            Self::Vdpau => 3,
            Self::D3d11va => 4,
            Self::Dxva2 => 5,
            Self::VideoToolbox => 6,
        }
    }

    pub fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Vaapi,
            2 => Self::Vulkan,
            3 => Self::Vdpau,
            4 => Self::D3d11va,
            5 => Self::Dxva2,
            6 => Self::VideoToolbox,
            _ => Self::Software,
        }
    }

    /// Devices to try, in order, on this platform.
    pub fn candidates() -> &'static [HwBackend] {
        #[cfg(target_os = "linux")]
        {
            &[Self::Vaapi, Self::Vulkan, Self::Vdpau]
        }
        #[cfg(target_os = "windows")]
        {
            &[Self::D3d11va, Self::Dxva2]
        }
        #[cfg(target_os = "macos")]
        {
            &[Self::VideoToolbox]
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        {
            &[]
        }
    }

    fn device_type(self) -> ffi::AVHWDeviceType {
        match self {
            Self::Vaapi => ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
            Self::Vulkan => ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN,
            Self::Vdpau => ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VDPAU,
            Self::D3d11va => ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
            Self::Dxva2 => ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_DXVA2,
            Self::VideoToolbox => ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
            Self::Software => ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_NONE,
        }
    }
}

thread_local! {
    /// The pixel format the current thread's `get_format` should pick. Decoding
    /// happens on one thread per track, so a thread-local is enough and avoids
    /// stashing a pointer in `AVCodecContext::opaque`.
    static DESIRED: Cell<Option<ffi::AVPixelFormat>> = const { Cell::new(None) };
}

/// `get_format` callback: prefer the hardware format we set up, otherwise take
/// the decoder's first offer so playback continues in software.
///
/// # Safety
/// Called by FFmpeg with a valid, NUL-terminated list of pixel formats that
/// lives for the duration of the call.
unsafe extern "C" fn get_format(
    _ctx: *mut ffi::AVCodecContext,
    formats: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    if formats.is_null() {
        return ffi::AVPixelFormat::AV_PIX_FMT_NONE;
    }
    let desired = DESIRED.with(|d| d.get());
    let mut index = 0isize;
    // SAFETY: we stop at AV_PIX_FMT_NONE, which FFmpeg guarantees terminates
    // the array it passes in.
    unsafe {
        let first = *formats;
        loop {
            let format = *formats.offset(index);
            if format == ffi::AVPixelFormat::AV_PIX_FMT_NONE {
                break;
            }
            if Some(format) == desired {
                return format;
            }
            index += 1;
        }
        if first == ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            ffi::AVPixelFormat::AV_PIX_FMT_NONE
        } else {
            first
        }
    }
}

/// A hardware device shared with one codec context.
pub struct HwDecoder {
    backend: HwBackend,
    device_ctx: *mut ffi::AVBufferRef,
    pix_fmt: ffi::AVPixelFormat,
    /// The format the decoder hands back; frames in this format live in device
    /// memory and must be transferred before they can be sampled.
    device_pix_fmt: ffi::AVPixelFormat,
}

// SAFETY: the only non-Send member is the `AVBufferRef` owned by this struct,
// which FFmpeg refcounts and which nothing else touches after `attach`.
unsafe impl Send for HwDecoder {}

impl Drop for HwDecoder {
    fn drop(&mut self) {
        DESIRED.with(|d| d.set(None));
        if !self.device_ctx.is_null() {
            // SAFETY: `device_ctx` was produced by av_hwdevice_ctx_create /
            // av_buffer_ref and is dropped exactly once, here.
            unsafe { ffi::av_buffer_unref(&mut self.device_ctx) };
        }
    }
}

impl HwDecoder {
    pub fn backend(&self) -> HwBackend {
        self.backend
    }

    /// The format frames arrive in (device memory) when hardware decoding is
    /// really in use.
    pub fn device_pix_fmt(&self) -> ffi::AVPixelFormat {
        self.device_pix_fmt
    }

    pub fn hardware_pix_fmt(&self) -> ffi::AVPixelFormat {
        self.pix_fmt
    }

    /// Try every platform device in order and attach the first that works.
    ///
    /// # Safety
    /// `ctx` must be a valid, not yet opened `AVCodecContext` and `codec` the
    /// `AVCodec` it will be opened with. Both must outlive the returned value.
    pub unsafe fn attach(
        ctx: *mut ffi::AVCodecContext,
        codec: *const ffi::AVCodec,
        preference: HwAccelPref,
    ) -> Option<Self> {
        if !preference.allows_hardware() || ctx.is_null() || codec.is_null() {
            return None;
        }
        for backend in HwBackend::candidates() {
            // SAFETY: `codec` is a non-null AVCodec pointer owned by FFmpeg.
            let Some(pix_fmt) = (unsafe { supported_hw_format(codec, backend.device_type()) })
            else {
                continue;
            };
            let mut device_ctx: *mut ffi::AVBufferRef = ptr::null_mut();
            // SAFETY: the out-pointer is valid; the device string is NULL so
            // FFmpeg picks the platform default (for VAAPI, the first render
            // node), and the dictionary pointer is NULL for defaults.
            let created = unsafe {
                ffi::av_hwdevice_ctx_create(
                    &mut device_ctx,
                    backend.device_type(),
                    ptr::null(),
                    ptr::null_mut(),
                    0,
                )
            };
            if created < 0 || device_ctx.is_null() {
                continue;
            }
            // SAFETY: `device_ctx` is a fresh reference; av_buffer_ref makes a
            // second one for the codec context, which FFmpeg releases when the
            // context is closed.
            let shared = unsafe { ffi::av_buffer_ref(device_ctx) };
            if shared.is_null() {
                // SAFETY: we own the only reference to `device_ctx`.
                unsafe { ffi::av_buffer_unref(&mut device_ctx) };
                continue;
            }
            DESIRED.with(|d| d.set(Some(pix_fmt)));
            // SAFETY: the context is not open yet, so its fields may be set.
            unsafe {
                (*ctx).get_format = Some(get_format);
                (*ctx).hw_device_ctx = shared;
            }
            return Some(Self {
                backend: *backend,
                device_ctx,
                pix_fmt,
                device_pix_fmt: pix_fmt,
            });
        }
        None
    }

    /// Detach from a context that failed to open, so a software retry starts
    /// from a clean state.
    ///
    /// # Safety
    /// `ctx` must be the context this device was attached to and must not be
    /// open.
    pub unsafe fn detach(ctx: *mut ffi::AVCodecContext) {
        if ctx.is_null() {
            return;
        }
        // SAFETY: the codec context is not open, so writing these fields is
        // sound; av_buffer_unref drops the reference we created in `attach`.
        unsafe {
            (*ctx).get_format = None;
            if !(*ctx).hw_device_ctx.is_null() {
                ffi::av_buffer_unref(&mut (*ctx).hw_device_ctx);
            }
        }
    }

    /// Copy a device-memory frame into `dst` (an allocated, unreferenced
    /// `AVFrame`), converting to NV12 or YUV420P as the driver prefers.
    ///
    /// # Safety
    /// `src` must be a frame decoded by the codec this device was attached to,
    /// and `dst` must be a valid frame from `av_frame_alloc` that is currently
    /// unreferenced.
    pub unsafe fn transfer(src: *mut ffi::AVFrame, dst: *mut ffi::AVFrame) -> Result<(), String> {
        if src.is_null() || dst.is_null() {
            return Err("hardware transfer: null frame".to_owned());
        }
        // SAFETY: both frames are valid; FFmpeg writes into dst's buffers.
        let result = unsafe { ffi::av_hwframe_transfer_data(dst, src, 0) };
        if result < 0 {
            return Err(format!("av_hwframe_transfer_data failed ({result})"));
        }
        // SAFETY: dst is a valid frame; copying props keeps pts and color tags.
        unsafe {
            if ffi::av_frame_copy_props(dst, src) < 0 {
                // Not fatal: we read the timestamp we already know.
            }
        }
        Ok(())
    }
}

/// Look through the codec's hardware configurations for one that this device
/// type can drive, and return the hardware pixel format to ask for.
///
/// # Safety
/// `codec` must be a valid `AVCodec` pointer.
unsafe fn supported_hw_format(
    codec: *const ffi::AVCodec,
    device_type: ffi::AVHWDeviceType,
) -> Option<ffi::AVPixelFormat> {
    let mut index: c_int = 0;
    loop {
        // SAFETY: avcodec_get_hw_config returns NULL past the last entry.
        let config = unsafe { ffi::avcodec_get_hw_config(codec, index) };
        if config.is_null() {
            return None;
        }
        // SAFETY: the pointer was non-null and points at a static config.
        let config = unsafe { &*config };
        if config.device_type == device_type
            && (config.methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as c_int) != 0
        {
            return Some(config.pix_fmt);
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badges_match_the_spec() {
        assert_eq!(HwBackend::Vaapi.badge(), "VAAPI");
        assert_eq!(HwBackend::Vulkan.badge(), "VULKAN");
        assert_eq!(HwBackend::Vdpau.badge(), "VDPAU");
        assert_eq!(HwBackend::D3d11va.badge(), "D3D11VA");
        assert_eq!(HwBackend::Dxva2.badge(), "DXVA2");
        assert_eq!(HwBackend::VideoToolbox.badge(), "VT");
        assert_eq!(HwBackend::Software.badge(), "SW");
    }

    #[test]
    fn codes_round_trip() {
        for backend in [
            HwBackend::Software,
            HwBackend::Vaapi,
            HwBackend::Vulkan,
            HwBackend::Vdpau,
            HwBackend::D3d11va,
            HwBackend::Dxva2,
            HwBackend::VideoToolbox,
        ] {
            assert_eq!(HwBackend::from_code(backend.code()), backend);
        }
        assert_eq!(HwBackend::from_code(200), HwBackend::Software);
    }

    #[test]
    fn only_software_is_not_hardware() {
        assert!(!HwBackend::Software.is_hardware());
        assert!(HwBackend::Vaapi.is_hardware());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_prefers_vaapi_then_vulkan_then_vdpau() {
        assert_eq!(
            HwBackend::candidates(),
            &[HwBackend::Vaapi, HwBackend::Vulkan, HwBackend::Vdpau]
        );
    }

    #[test]
    fn software_only_never_opens_a_device() {
        // SAFETY: both pointers are null, which `attach` rejects before it
        // dereferences anything.
        let attached = unsafe {
            HwDecoder::attach(
                std::ptr::null_mut(),
                std::ptr::null(),
                HwAccelPref::SoftwareOnly,
            )
        };
        assert!(attached.is_none());
    }
}

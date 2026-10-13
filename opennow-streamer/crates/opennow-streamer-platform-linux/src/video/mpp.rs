use std::ffi::c_void;
use std::sync::{Arc, OnceLock};

use libloading::Library;

#[path = "mpp_ffi.rs"]
pub mod ffi;
use ffi::*;

use super::VideoDecoder;
use crate::{
    ChromaLocation, ColorMatrix, ColorRange, ColorTransfer, DecodedVideoFrame, EncodedVideoFrame,
    Error, FramePlane, PixelFormat, Result, StreamFormat, Subsystem, VideoCodec,
};

type FnMppCreate = unsafe extern "C" fn(*mut MppCtx, *mut *const MppApi) -> i32;
type FnMppInit = unsafe extern "C" fn(MppCtx, u32, u32) -> i32;
type FnMppDestroy = unsafe extern "C" fn(MppCtx) -> i32;
type FnMppCheckSupportFormat = unsafe extern "C" fn(u32, u32) -> i32;

type FnMppPacketInit = unsafe extern "C" fn(*mut MppPacket, *const c_void, usize) -> i32;
type FnMppPacketDeinit = unsafe extern "C" fn(*mut MppPacket) -> i32;
type FnMppPacketSetPts = unsafe extern "C" fn(MppPacket, i64);
type FnMppPacketSetEos = unsafe extern "C" fn(MppPacket);

type FnMppFrameInit = unsafe extern "C" fn(*mut MppFrame) -> i32;
type FnMppFrameDeinit = unsafe extern "C" fn(*mut MppFrame) -> i32;
type FnMppFrameGetWidth = unsafe extern "C" fn(MppFrame) -> u32;
type FnMppFrameGetHeight = unsafe extern "C" fn(MppFrame) -> u32;
type FnMppFrameGetHorStride = unsafe extern "C" fn(MppFrame) -> u32;
type FnMppFrameGetVerStride = unsafe extern "C" fn(MppFrame) -> u32;
type FnMppFrameGetBufSize = unsafe extern "C" fn(MppFrame) -> usize;
type FnMppFrameGetErrInfo = unsafe extern "C" fn(MppFrame) -> u32;
type FnMppFrameGetDiscard = unsafe extern "C" fn(MppFrame) -> u32;
type FnMppFrameGetPts = unsafe extern "C" fn(MppFrame) -> i64;
type FnMppFrameGetInfoChange = unsafe extern "C" fn(MppFrame) -> u32;
type FnMppFrameGetBuffer = unsafe extern "C" fn(MppFrame) -> MppBuffer;
type FnMppFrameGetFmt = unsafe extern "C" fn(MppFrame) -> u32;

type FnMppBufferGetFd = unsafe extern "C" fn(MppBuffer) -> i32;
type FnMppBufferGetPtr = unsafe extern "C" fn(MppBuffer) -> *mut c_void;
type FnMppBufferGetSize = unsafe extern "C" fn(MppBuffer) -> usize;
type FnMppBufferGetFdCaller = unsafe extern "C" fn(MppBuffer, *const i8) -> i32;
type FnMppBufferGetPtrCaller = unsafe extern "C" fn(MppBuffer, *const i8) -> *mut c_void;
type FnMppBufferGetSizeCaller = unsafe extern "C" fn(MppBuffer, *const i8) -> usize;
type FnMppBufferReadCaller =
    unsafe extern "C" fn(MppBuffer, usize, *mut c_void, usize, *const i8) -> i32;
type FnMppBufferSync = unsafe extern "C" fn(MppBuffer, i32, *const i8) -> i32;
type MppBufferGroup = *mut c_void;
type FnMppBufferGroupGet =
    unsafe extern "C" fn(*mut MppBufferGroup, u32, u32, *const i8, *const i8) -> i32;
type FnMppBufferGroupLimit = unsafe extern "C" fn(MppBufferGroup, usize, i32) -> i32;
type FnMppBufferGroupClear = unsafe extern "C" fn(MppBufferGroup) -> i32;
type FnMppBufferGroupPut = unsafe extern "C" fn(MppBufferGroup) -> i32;

#[allow(dead_code)]
pub struct MppLibrary {
    _lib: Library,
    pub mpp_create: FnMppCreate,
    pub mpp_init: FnMppInit,
    pub mpp_destroy: FnMppDestroy,
    pub mpp_check_support_format: Option<FnMppCheckSupportFormat>,
    pub mpp_packet_init: FnMppPacketInit,
    pub mpp_packet_deinit: FnMppPacketDeinit,
    pub mpp_packet_set_pts: FnMppPacketSetPts,
    pub mpp_packet_set_eos: FnMppPacketSetEos,
    pub mpp_frame_init: FnMppFrameInit,
    pub mpp_frame_deinit: FnMppFrameDeinit,
    pub mpp_frame_get_width: FnMppFrameGetWidth,
    pub mpp_frame_get_height: FnMppFrameGetHeight,
    pub mpp_frame_get_hor_stride: FnMppFrameGetHorStride,
    pub mpp_frame_get_ver_stride: FnMppFrameGetVerStride,
    pub mpp_frame_get_buf_size: FnMppFrameGetBufSize,
    pub mpp_frame_get_err_info: Option<FnMppFrameGetErrInfo>,
    pub mpp_frame_get_discard: FnMppFrameGetDiscard,
    pub mpp_frame_get_pts: FnMppFrameGetPts,
    pub mpp_frame_get_info_change: FnMppFrameGetInfoChange,
    pub mpp_frame_get_buffer: FnMppFrameGetBuffer,
    pub mpp_frame_get_fmt: FnMppFrameGetFmt,
    pub mpp_buffer_get_fd: Option<FnMppBufferGetFd>,
    pub mpp_buffer_get_ptr: Option<FnMppBufferGetPtr>,
    pub mpp_buffer_get_size: Option<FnMppBufferGetSize>,
    pub mpp_buffer_get_fd_caller: Option<FnMppBufferGetFdCaller>,
    pub mpp_buffer_get_ptr_caller: Option<FnMppBufferGetPtrCaller>,
    pub mpp_buffer_get_size_caller: Option<FnMppBufferGetSizeCaller>,
    #[allow(dead_code)]
    pub mpp_buffer_read_caller: Option<FnMppBufferReadCaller>,
    pub mpp_buffer_sync_begin: Option<FnMppBufferSync>,
    pub mpp_buffer_sync_end: Option<FnMppBufferSync>,
    pub mpp_buffer_group_get: FnMppBufferGroupGet,
    pub mpp_buffer_group_limit_config: FnMppBufferGroupLimit,
    pub mpp_buffer_group_clear: FnMppBufferGroupClear,
    pub mpp_buffer_group_put: FnMppBufferGroupPut,
}

impl MppLibrary {
    pub fn load() -> std::result::Result<Arc<Self>, String> {
        let mut library_names = Vec::new();
        if let Ok(executable) = std::fs::read_link("/proc/self/exe") {
            if let Some(directory) = executable.parent() {
                for name in ["librockchip_mpp.so.1", "librockchip_mpp.so"] {
                    library_names.push(directory.join(name).to_string_lossy().into_owned());
                }
            }
        }
        library_names.extend([
            "librockchip_mpp.so.1".to_owned(),
            "librockchip_mpp.so.0".to_owned(),
            "librockchip_mpp.so".to_owned(),
            "/usr/lib/librockchip_mpp.so.1".to_owned(),
            "/usr/lib/librockchip_mpp.so".to_owned(),
            "/usr/lib/aarch64-linux-gnu/librockchip_mpp.so.1".to_owned(),
            "/usr/lib/aarch64-linux-gnu/librockchip_mpp.so.0".to_owned(),
            "/usr/lib/aarch64-linux-gnu/librockchip_mpp.so".to_owned(),
            "libmpp.so.1".to_owned(),
            "libmpp.so".to_owned(),
        ]);
        let mut last_error = None;
        for name in &library_names {
            match unsafe { Library::new(name) } {
                Ok(lib) => {
                    let mpp_lib = Self::from_library(lib)?;
                    eprintln!("stream: loaded Rockchip MPP from {name}");
                    return Ok(Arc::new(mpp_lib));
                }
                Err(err) => {
                    last_error = Some(format!("{name}: {err}"));
                }
            }
        }
        Err(format!(
            "failed to load librockchip_mpp: {}",
            last_error.unwrap_or_else(|| "library not found".to_owned())
        ))
    }

    fn from_library(lib: Library) -> std::result::Result<Self, String> {
        unsafe {
            let mpp_create = *lib
                .get(b"mpp_create\0")
                .map_err(|e| format!("missing mpp_create: {e}"))?;
            let mpp_init = *lib
                .get(b"mpp_init\0")
                .map_err(|e| format!("missing mpp_init: {e}"))?;
            let mpp_destroy = *lib
                .get(b"mpp_destroy\0")
                .map_err(|e| format!("missing mpp_destroy: {e}"))?;
            let mpp_check_support_format = lib.get(b"mpp_check_support_format\0").ok().map(|s| *s);

            let mpp_packet_init = *lib
                .get(b"mpp_packet_init\0")
                .map_err(|e| format!("missing mpp_packet_init: {e}"))?;
            let mpp_packet_deinit = *lib
                .get(b"mpp_packet_deinit\0")
                .map_err(|e| format!("missing mpp_packet_deinit: {e}"))?;
            let mpp_packet_set_pts = *lib
                .get(b"mpp_packet_set_pts\0")
                .map_err(|e| format!("missing mpp_packet_set_pts: {e}"))?;
            let mpp_packet_set_eos = *lib
                .get(b"mpp_packet_set_eos\0")
                .map_err(|e| format!("missing mpp_packet_set_eos: {e}"))?;

            let mpp_frame_init = *lib
                .get(b"mpp_frame_init\0")
                .map_err(|e| format!("missing mpp_frame_init: {e}"))?;
            let mpp_frame_deinit = *lib
                .get(b"mpp_frame_deinit\0")
                .map_err(|e| format!("missing mpp_frame_deinit: {e}"))?;
            let mpp_frame_get_width = *lib
                .get(b"mpp_frame_get_width\0")
                .map_err(|e| format!("missing mpp_frame_get_width: {e}"))?;
            let mpp_frame_get_height = *lib
                .get(b"mpp_frame_get_height\0")
                .map_err(|e| format!("missing mpp_frame_get_height: {e}"))?;
            let mpp_frame_get_hor_stride = *lib
                .get(b"mpp_frame_get_hor_stride\0")
                .map_err(|e| format!("missing mpp_frame_get_hor_stride: {e}"))?;
            let mpp_frame_get_ver_stride = *lib
                .get(b"mpp_frame_get_ver_stride\0")
                .map_err(|e| format!("missing mpp_frame_get_ver_stride: {e}"))?;
            let mpp_frame_get_buf_size = *lib
                .get(b"mpp_frame_get_buf_size\0")
                .map_err(|e| format!("missing mpp_frame_get_buf_size: {e}"))?;
            let mpp_frame_get_err_info = lib
                .get(b"mpp_frame_get_errinfo\0")
                .or_else(|_| lib.get(b"mpp_frame_get_err_info\0"))
                .ok()
                .map(|symbol| *symbol);
            let mpp_frame_get_discard = *lib
                .get(b"mpp_frame_get_discard\0")
                .map_err(|e| format!("missing mpp_frame_get_discard: {e}"))?;
            let mpp_frame_get_pts = *lib
                .get(b"mpp_frame_get_pts\0")
                .map_err(|e| format!("missing mpp_frame_get_pts: {e}"))?;
            let mpp_frame_get_info_change = *lib
                .get(b"mpp_frame_get_info_change\0")
                .map_err(|e| format!("missing mpp_frame_get_info_change: {e}"))?;
            let mpp_frame_get_buffer = *lib
                .get(b"mpp_frame_get_buffer\0")
                .map_err(|e| format!("missing mpp_frame_get_buffer: {e}"))?;
            let mpp_frame_get_fmt = *lib
                .get(b"mpp_frame_get_fmt\0")
                .map_err(|e| format!("missing mpp_frame_get_fmt: {e}"))?;

            let mpp_buffer_get_fd = lib.get(b"mpp_buffer_get_fd\0").ok().map(|symbol| *symbol);
            let mpp_buffer_get_ptr = lib.get(b"mpp_buffer_get_ptr\0").ok().map(|symbol| *symbol);
            let mpp_buffer_get_size = lib.get(b"mpp_buffer_get_size\0").ok().map(|symbol| *symbol);
            let mpp_buffer_get_fd_caller = lib
                .get(b"mpp_buffer_get_fd_with_caller\0")
                .ok()
                .map(|symbol| *symbol);
            let mpp_buffer_get_ptr_caller = lib
                .get(b"mpp_buffer_get_ptr_with_caller\0")
                .ok()
                .map(|symbol| *symbol);
            let mpp_buffer_get_size_caller = lib
                .get(b"mpp_buffer_get_size_with_caller\0")
                .ok()
                .map(|symbol| *symbol);
            let mpp_buffer_read_caller = lib
                .get(b"mpp_buffer_read_with_caller\0")
                .ok()
                .map(|symbol| *symbol);
            let mpp_buffer_sync_begin = lib
                .get(b"mpp_buffer_sync_begin_f\0")
                .ok()
                .map(|symbol| *symbol);
            let mpp_buffer_sync_end = lib
                .get(b"mpp_buffer_sync_end_f\0")
                .ok()
                .map(|symbol| *symbol);
            let mpp_buffer_group_get = *lib
                .get(b"mpp_buffer_group_get\0")
                .map_err(|e| format!("missing mpp_buffer_group_get: {e}"))?;
            let mpp_buffer_group_limit_config = *lib
                .get(b"mpp_buffer_group_limit_config\0")
                .map_err(|e| format!("missing mpp_buffer_group_limit_config: {e}"))?;
            let mpp_buffer_group_clear = *lib
                .get(b"mpp_buffer_group_clear\0")
                .map_err(|e| format!("missing mpp_buffer_group_clear: {e}"))?;
            let mpp_buffer_group_put = *lib
                .get(b"mpp_buffer_group_put\0")
                .map_err(|e| format!("missing mpp_buffer_group_put: {e}"))?;
            if mpp_buffer_get_ptr.is_none()
                && mpp_buffer_get_ptr_caller.is_none()
                && mpp_buffer_get_fd.is_none()
                && mpp_buffer_get_fd_caller.is_none()
            {
                return Err(
                    "Rockchip MPP does not export a buffer pointer or dma-buf accessor".to_owned(),
                );
            }

            Ok(Self {
                _lib: lib,
                mpp_create,
                mpp_init,
                mpp_destroy,
                mpp_check_support_format,
                mpp_packet_init,
                mpp_packet_deinit,
                mpp_packet_set_pts,
                mpp_packet_set_eos,
                mpp_frame_init,
                mpp_frame_deinit,
                mpp_frame_get_width,
                mpp_frame_get_height,
                mpp_frame_get_hor_stride,
                mpp_frame_get_ver_stride,
                mpp_frame_get_buf_size,
                mpp_frame_get_err_info,
                mpp_frame_get_discard,
                mpp_frame_get_pts,
                mpp_frame_get_info_change,
                mpp_frame_get_buffer,
                mpp_frame_get_fmt,
                mpp_buffer_get_fd,
                mpp_buffer_get_ptr,
                mpp_buffer_get_size,
                mpp_buffer_get_fd_caller,
                mpp_buffer_get_ptr_caller,
                mpp_buffer_get_size_caller,
                mpp_buffer_read_caller,
                mpp_buffer_sync_begin,
                mpp_buffer_sync_end,
                mpp_buffer_group_get,
                mpp_buffer_group_limit_config,
                mpp_buffer_group_clear,
                mpp_buffer_group_put,
            })
        }
    }
}

static MPP_LIB: OnceLock<std::result::Result<Arc<MppLibrary>, String>> = OnceLock::new();

pub fn get_mpp_library() -> std::result::Result<Arc<MppLibrary>, String> {
    MPP_LIB.get_or_init(MppLibrary::load).clone()
}

pub fn probe_mpp(codec: VideoCodec) -> std::result::Result<String, String> {
    let lib = get_mpp_library()?;
    let coding = match codec {
        VideoCodec::H264 => MPP_VIDEO_CodingAVC,
        VideoCodec::H265 => MPP_VIDEO_CodingHEVC,
        _ => return Err(format!("Rockchip MPP does not support {}", codec.label())),
    };
    if let Some(check_fn) = lib.mpp_check_support_format {
        let ret = unsafe { check_fn(MPP_CTX_DEC, coding) };
        if ret != MPP_OK {
            eprintln!(
                "stream: MPP format check for {} returned {ret}; opening the decoder anyway",
                codec.label()
            );
        }
    }
    Ok(format!(
        "Rockchip MPP {} hardware decoder",
        codec.label().to_ascii_uppercase()
    ))
}

pub struct MppDecoder {
    ctx: MppCtx,
    mpi: *const MppApi,
    lib: Arc<MppLibrary>,
    format: StreamFormat,
    codec: VideoCodec,
    frame_group: MppBufferGroup,
    pending_format_change: Option<StreamFormat>,
    consecutive_decode_errors: u32,
    logged_backpressure: bool,
    skipped_frames: u64,
    /// `None` keeps the decoded frame. A box is the largest render size.
    render_limit: Option<(usize, usize)>,
}

unsafe impl Send for MppDecoder {}

impl MppDecoder {
    pub fn open(format: StreamFormat, codec: VideoCodec) -> Result<Self> {
        let lib = get_mpp_library().map_err(|e| Error::unavailable(Subsystem::V4l2, e))?;

        let mut ctx: MppCtx = std::ptr::null_mut();
        let mut mpi: *const MppApi = std::ptr::null();
        let ret = unsafe { (lib.mpp_create)(&mut ctx, &mut mpi) };
        if ret != MPP_OK || ctx.is_null() || mpi.is_null() {
            return Err(Error::backend(
                Subsystem::V4l2,
                format!("mpp_create failed ({ret})"),
            ));
        }

        let coding = match codec {
            VideoCodec::H264 => MPP_VIDEO_CodingAVC,
            VideoCodec::H265 => MPP_VIDEO_CodingHEVC,
            _ => {
                unsafe { (lib.mpp_destroy)(ctx) };
                return Err(Error::unavailable(
                    Subsystem::V4l2,
                    format!("unsupported codec for MPP: {}", codec.label()),
                ));
            }
        };

        // Non-blocking polls have to be set before mpp_init. Zero means do not wait.
        let mut timeout: i64 = 0;
        let input_timeout = unsafe {
            ((*mpi).control)(
                ctx,
                MPP_SET_INPUT_TIMEOUT,
                &mut timeout as *mut i64 as MppParam,
            )
        };
        let output_timeout = unsafe {
            ((*mpi).control)(
                ctx,
                MPP_SET_OUTPUT_TIMEOUT,
                &mut timeout as *mut i64 as MppParam,
            )
        };
        if input_timeout != MPP_OK || output_timeout != MPP_OK {
            unsafe { (lib.mpp_destroy)(ctx) };
            return Err(Error::backend(
                Subsystem::V4l2,
                format!("MPP timeout setup failed ({input_timeout}, {output_timeout})"),
            ));
        }

        let ret = unsafe { (lib.mpp_init)(ctx, MPP_CTX_DEC, coding) };
        if ret != MPP_OK {
            unsafe { (lib.mpp_destroy)(ctx) };
            return Err(Error::backend(
                Subsystem::V4l2,
                format!("mpp_init failed ({ret})"),
            ));
        }

        let mut output_format: u32 = MPP_FMT_YUV420SP;
        let format_ret = unsafe {
            ((*mpi).control)(
                ctx,
                MPP_DEC_SET_OUTPUT_FORMAT,
                &mut output_format as *mut u32 as MppParam,
            )
        };
        if format_ret != MPP_OK {
            unsafe { (lib.mpp_destroy)(ctx) };
            return Err(Error::backend(
                Subsystem::V4l2,
                format!("MPP NV12 output setup failed ({format_ret})"),
            ));
        }
        eprintln!("stream: using Rockchip MPP for {}", codec.label());
        let render_limit = render_limit_from_config();
        match render_limit {
            Some((width, height)) => eprintln!("stream: render limit {width}x{height}"),
            None => eprintln!("stream: render limit native"),
        }

        Ok(Self {
            ctx,
            mpi,
            lib,
            format,
            codec,
            pending_format_change: None,
            consecutive_decode_errors: 0,
            logged_backpressure: false,
            skipped_frames: 0,
            frame_group: std::ptr::null_mut(),
            render_limit,
        })
    }

    fn acknowledge_info_change(&mut self, frame: MppFrame) -> Result<()> {
        let width = unsafe { (self.lib.mpp_frame_get_width)(frame) };
        let height = unsafe { (self.lib.mpp_frame_get_height)(frame) };
        let buffer_size = unsafe { (self.lib.mpp_frame_get_buf_size)(frame) };
        if width == 0 || height == 0 || buffer_size == 0 {
            return Err(Error::backend(
                Subsystem::V4l2,
                "MPP info change did not include a frame size".to_owned(),
            ));
        }
        let tag = b"opennow\0".as_ptr().cast();
        // GreenOvercast creates an internal DRM pool and hands it to the
        // decoder on the info-change frame. ION is the fallback when DRM
        // cannot be created, which is the older Hantro allocator.
        if self.frame_group.is_null() {
            let mut created = MPP_NOK;
            let mut chosen = MPP_BUFFER_TYPE_DRM;
            for buffer_type in [MPP_BUFFER_TYPE_DRM, MPP_BUFFER_TYPE_ION] {
                created = unsafe {
                    (self.lib.mpp_buffer_group_get)(
                        &mut self.frame_group,
                        buffer_type,
                        MPP_BUFFER_INTERNAL,
                        tag,
                        tag,
                    )
                };
                if created == MPP_OK && !self.frame_group.is_null() {
                    chosen = buffer_type;
                    break;
                }
                self.frame_group = std::ptr::null_mut();
            }
            if created != MPP_OK || self.frame_group.is_null() {
                return Err(Error::backend(
                    Subsystem::V4l2,
                    format!("MPP frame-buffer group creation failed ({created})"),
                ));
            }
            eprintln!(
                "stream: MPP picture pool is {}",
                if chosen == MPP_BUFFER_TYPE_DRM { "DRM" } else { "ION" }
            );
        } else {
            let cleared = unsafe { (self.lib.mpp_buffer_group_clear)(self.frame_group) };
            if cleared != MPP_OK {
                return Err(Error::backend(
                    Subsystem::V4l2,
                    format!("MPP frame-buffer group reset failed ({cleared})"),
                ));
            }
        }
        let limited = unsafe {
            (self.lib.mpp_buffer_group_limit_config)(
                self.frame_group,
                buffer_size,
                MPP_FRAME_LIMIT,
            )
        };
        if limited != MPP_OK {
            return Err(Error::backend(
                Subsystem::V4l2,
                format!("MPP frame-buffer limit failed ({limited})"),
            ));
        }
        let attached = unsafe {
            ((*self.mpi).control)(
                self.ctx,
                MPP_DEC_SET_EXT_BUF_GROUP,
                self.frame_group as MppParam,
            )
        };
        if attached != MPP_OK {
            return Err(Error::backend(
                Subsystem::V4l2,
                format!("MPP frame-buffer group attachment failed ({attached})"),
            ));
        }
        let ready = unsafe {
            ((*self.mpi).control)(self.ctx, MPP_DEC_SET_INFO_CHANGE_READY, std::ptr::null_mut())
        };
        if ready != MPP_OK {
            return Err(Error::backend(
                Subsystem::V4l2,
                format!("MPP info-change acknowledgement failed ({ready})"),
            ));
        }
        if width != self.format.width || height != self.format.height {
            self.format.width = width;
            self.format.height = height;
            self.pending_format_change = Some(self.format);
        }
        eprintln!(
            "stream: MPP output NV12 {width}x{height}, buffer {buffer_size} bytes"
        );
        Ok(())
    }

    /// Copies the newest picture out and returns every MPP buffer before doing
    /// any other work. GreenOvercast drains this way: the VPU pool is small,
    /// and a buffer that is still checked out stops the decoder.
    fn release_ready_frames(&mut self) -> Result<Option<ReleasedPicture>> {
        let mut latest = None;
        for _ in 0..32 {
            let mut mpp_frame: MppFrame = std::ptr::null_mut();
            let ret = unsafe { ((*self.mpi).decode_get_frame)(self.ctx, &mut mpp_frame) };
            if ret != MPP_OK || mpp_frame.is_null() {
                if !mpp_frame.is_null() {
                    unsafe { (self.lib.mpp_frame_deinit)(&mut mpp_frame) };
                }
                break;
            }

            let info_change = unsafe { (self.lib.mpp_frame_get_info_change)(mpp_frame) };
            if info_change != 0 {
                let ready = self.acknowledge_info_change(mpp_frame);
                unsafe { (self.lib.mpp_frame_deinit)(&mut mpp_frame) };
                ready?;
                continue;
            }

            let discard = unsafe { (self.lib.mpp_frame_get_discard)(mpp_frame) };
            if discard != 0 {
                self.note_skip("discard");
                unsafe { (self.lib.mpp_frame_deinit)(&mut mpp_frame) };
                continue;
            }
            let err_info = self
                .lib
                .mpp_frame_get_err_info
                .map(|read_error| unsafe { read_error(mpp_frame) })
                .unwrap_or(0);
            if err_info != 0 {
                self.note_skip(&format!("errinfo={err_info:#x}"));
                unsafe { (self.lib.mpp_frame_deinit)(&mut mpp_frame) };
                continue;
            }

            let buffer = unsafe { (self.lib.mpp_frame_get_buffer)(mpp_frame) };
            let copied = if buffer.is_null() {
                None
            } else {
                let width = unsafe { (self.lib.mpp_frame_get_width)(mpp_frame) } as usize;
                let height = unsafe { (self.lib.mpp_frame_get_height)(mpp_frame) } as usize;
                let hor_stride = unsafe { (self.lib.mpp_frame_get_hor_stride)(mpp_frame) } as usize;
                let ver_stride = unsafe { (self.lib.mpp_frame_get_ver_stride)(mpp_frame) } as usize;
                let buf_size = unsafe { (self.lib.mpp_frame_get_buf_size)(mpp_frame) };
                let pts = unsafe { (self.lib.mpp_frame_get_pts)(mpp_frame) };
                copy_nv12(
                    self.lib.as_ref(),
                    buffer,
                    width,
                    height,
                    hor_stride,
                    ver_stride,
                    buf_size,
                    pts,
                )
            };
            unsafe { (self.lib.mpp_frame_deinit)(&mut mpp_frame) };
            match copied {
                Some(picture) => latest = Some(picture),
                None => self.note_skip("unreadable"),
            }
        }
        Ok(latest)
    }

    fn note_skip(&mut self, reason: &str) {
        self.skipped_frames = self.skipped_frames.saturating_add(1);
        if self.skipped_frames == 1 || self.skipped_frames == 30 || self.skipped_frames % 300 == 0
        {
            eprintln!(
                "stream: MPP released a picture without showing it ({reason}); skipped {}",
                self.skipped_frames
            );
        }
    }

    fn present(
        self_format: StreamFormat,
        picture: ReleasedPicture,
        render_limit: Option<(usize, usize)>,
    ) -> Option<DecodedVideoFrame> {
        let ReleasedPicture { nv12, width, height, hor_stride, ver_stride, pts } = picture;
        let (out_width, out_height) = panel_size(width, height, render_limit);
        let rgba = rgba_for_panel(&nv12, width, height, hor_stride, ver_stride, out_width, out_height)?;
        static LOGGED_PANEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if LOGGED_PANEL
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_ok()
        {
            eprintln!("stream: showing {out_width}x{out_height} from {width}x{height}");
        }
        Some(DecodedVideoFrame {
            format: StreamFormat {
                width: out_width as u32,
                height: out_height as u32,
                pixel_format: PixelFormat::Rgba8,
                color_range: ColorRange::Full,
                color_matrix: ColorMatrix::Bt601,
                color_transfer: ColorTransfer::Sdr,
                color_primaries: self_format.color_primaries,
                chroma_location: ChromaLocation::Center,
            },
            planes: vec![FramePlane {
                data: Arc::<[u8]>::from(rgba),
                stride: out_width * 4,
                rows: out_height,
            }],
            dmabuf: None,
            vulkan: None,
            timestamp_us: if pts > 0 { pts as u64 } else { 0 },
        })
    }

    /// Puts one access unit. A full input queue is not fatal: the caller
    /// releases pictures and tries the same bytes again, which is what
    /// GreenOvercast does for up to 64 attempts.
    fn submit_packet(&mut self, data: &[u8], pts: i64) -> Result<bool> {
        let mut packet: MppPacket = std::ptr::null_mut();
        let ret = unsafe { (self.lib.mpp_packet_init)(&mut packet, data.as_ptr().cast(), data.len()) };
        if ret != MPP_OK || packet.is_null() {
            return Err(Error::backend(
                Subsystem::V4l2,
                format!("mpp_packet_init failed ({ret})"),
            ));
        }
        unsafe { (self.lib.mpp_packet_set_pts)(packet, pts) };
        let put_ret = unsafe { ((*self.mpi).decode_put_packet)(self.ctx, packet) };
        unsafe { (self.lib.mpp_packet_deinit)(&mut packet) };
        if put_ret == MPP_OK {
            self.consecutive_decode_errors = 0;
            return Ok(true);
        }
        if matches!(put_ret, MPP_NOK | MPP_ERR_TIMEOUT | MPP_ERR_BUFFER_FULL) {
            return Ok(false);
        }
        self.consecutive_decode_errors += 1;
        if self.consecutive_decode_errors > 30 {
            return Err(Error::backend(
                Subsystem::V4l2,
                format!("mpp decode_put_packet failed repeatedly ({put_ret})"),
            ));
        }
        Ok(false)
    }
}

impl VideoDecoder for MppDecoder {
    fn decode(&mut self, frame: &EncodedVideoFrame) -> Result<Vec<DecodedVideoFrame>> {
        let mut latest = None;
        let mut accepted = false;
        for attempt in 0..64 {
            let picture = self.release_ready_frames()?;
            let produced = picture.is_some();
            if let Some(picture) = picture {
                latest = Some(picture);
            }
            accepted = self.submit_packet(frame.data.as_ref(), frame.timestamp_us as i64)?;
            if accepted {
                break;
            }
            if !produced && attempt + 1 < 64 {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        if let Some(picture) = self.release_ready_frames()? {
            latest = Some(picture);
        }
        if !accepted {
            if !self.logged_backpressure {
                self.logged_backpressure = true;
                eprintln!("stream: MPP input stayed full after releasing queued pictures");
            }
            return Err(Error::ReferenceLost {
                subsystem: Subsystem::V4l2,
                reason: "MPP did not accept the picture after its buffers were released".into(),
            });
        }
        let format = self.format;
        let render_limit = self.render_limit;
        Ok(latest.and_then(|picture| Self::present(format, picture, render_limit)).into_iter().collect())
    }

    fn poll(&mut self) -> Result<Vec<DecodedVideoFrame>> {
        let format = self.format;
        let picture = self.release_ready_frames()?;
        let render_limit = self.render_limit;
        Ok(picture.and_then(|picture| Self::present(format, picture, render_limit)).into_iter().collect())
    }

    fn flush(&mut self) -> Result<Vec<DecodedVideoFrame>> {
        let format = self.format;
        let picture = self.release_ready_frames()?;
        unsafe {
            ((*self.mpi).reset)(self.ctx);
        }
        let render_limit = self.render_limit;
        Ok(picture.and_then(|picture| Self::present(format, picture, render_limit)).into_iter().collect())
    }

    fn take_format_change(&mut self) -> Option<StreamFormat> {
        self.pending_format_change.take()
    }
}

struct ReleasedPicture {
    nv12: Vec<u8>,
    width: usize,
    height: usize,
    hor_stride: usize,
    ver_stride: usize,
    pts: i64,
}

/// Fits a frame into the render box from `stream.json`. `None` keeps the frame.
fn panel_size(width: usize, height: usize, limit: Option<(usize, usize)>) -> (usize, usize) {
    let Some((panel_width, panel_height)) = limit else {
        return (width.max(2) & !1, height.max(2) & !1);
    };
    let panel_width = panel_width.max(2) & !1;
    let panel_height = panel_height.max(2) & !1;
    if width <= panel_width && height <= panel_height {
        return (width.max(2) & !1, height.max(2) & !1);
    }
    if width.saturating_mul(panel_height) >= height.saturating_mul(panel_width) {
        let out_height = (height.saturating_mul(panel_width) / width).max(2) & !1;
        (panel_width, out_height.max(2))
    } else {
        let out_width = (width.saturating_mul(panel_height) / height).max(2) & !1;
        (out_width.max(2), panel_height)
    }
}

fn render_limit_from_config() -> Option<(usize, usize)> {
    let Some(path) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join("stream.json"))) else {
        return Some((640, 480));
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Some((640, 480));
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Some((640, 480));
    };
    let Some(text) = value.get("renderResolution").and_then(|item| item.as_str()) else {
        return Some((640, 480));
    };
    if text.eq_ignore_ascii_case("native") {
        return None;
    }
    let Some((width, height)) = text.split_once('x') else {
        return Some((640, 480));
    };
    match (width.parse::<usize>(), height.parse::<usize>()) {
        (Ok(width), Ok(height)) if width >= 2 && height >= 2 => Some((width & !1, height & !1)),
        _ => Some((640, 480)),
    }
}

fn copy_nv12(
    lib: &MppLibrary,
    buffer: MppBuffer,
    width: usize,
    height: usize,
    hor_stride: usize,
    ver_stride: usize,
    buf_size: usize,
    pts: i64,
) -> Option<ReleasedPicture> {
    if width < 2 || height < 2 || hor_stride < width || ver_stride < height {
        return None;
    }
    let y_size = hor_stride.saturating_mul(ver_stride);
    let uv_rows = height.div_ceil(2);
    let needed = y_size.saturating_add(hor_stride.saturating_mul(uv_rows));
    if needed == 0 {
        return None;
    }
    let caller = b"opennow\0".as_ptr().cast();
    if let Some(sync) = lib.mpp_buffer_sync_begin {
        unsafe { sync(buffer, 1, caller) };
    }
    let (pointer, len, mapped) = map_nv12(lib, buffer, hor_stride, ver_stride, buf_size)?;
    if len < needed {
        if let Some(address) = mapped {
            unsafe { libc::munmap(address, len) };
        }
        return None;
    }
    let mut nv12 = vec![0_u8; needed];
    unsafe { std::ptr::copy_nonoverlapping(pointer, nv12.as_mut_ptr(), needed) };
    if let Some(address) = mapped {
        unsafe { libc::munmap(address, len) };
    }
    if let Some(sync) = lib.mpp_buffer_sync_end {
        unsafe { sync(buffer, 1, caller) };
    }
    Some(ReleasedPicture { nv12, width, height, hor_stride, ver_stride, pts })
}

fn map_nv12(
    lib: &MppLibrary,
    buffer: MppBuffer,
    hor_stride: usize,
    ver_stride: usize,
    buf_size: usize,
) -> Option<(*const u8, usize, Option<*mut c_void>)> {
    let needed = hor_stride.saturating_mul(ver_stride).saturating_mul(3) / 2;
    let size = if buf_size > 0 { buf_size.max(needed) } else { needed };
    if size == 0 {
        return None;
    }
    let caller = b"opennow\0".as_ptr().cast();
    if let Some(get_ptr) = lib.mpp_buffer_get_ptr_caller {
        let pointer = unsafe { get_ptr(buffer, caller) };
        if !pointer.is_null() {
            return Some((pointer.cast(), size, None));
        }
    }
    if let Some(get_ptr) = lib.mpp_buffer_get_ptr {
        let pointer = unsafe { get_ptr(buffer) };
        if !pointer.is_null() {
            return Some((pointer.cast(), size, None));
        }
    }
    let fd = if let Some(get_fd) = lib.mpp_buffer_get_fd_caller {
        unsafe { get_fd(buffer, caller) }
    } else if let Some(get_fd) = lib.mpp_buffer_get_fd {
        unsafe { get_fd(buffer) }
    } else {
        -1
    };
    if fd < 0 {
        return None;
    }
    let mapped = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if mapped == libc::MAP_FAILED {
        return None;
    }
    Some((mapped.cast(), size, Some(mapped)))
}

fn rgba_for_panel(
    nv12: &[u8],
    width: usize,
    height: usize,
    hor_stride: usize,
    ver_stride: usize,
    out_width: usize,
    out_height: usize,
) -> Option<Vec<u8>> {
    if width < 2 || height < 2 || hor_stride < width || ver_stride < height || out_width < 2 || out_height < 2 {
        return None;
    }
    let uv_offset = hor_stride.saturating_mul(ver_stride);
    if nv12.len() < uv_offset.saturating_add(hor_stride.saturating_mul(height / 2)) {
        return None;
    }
    let mut rgba = vec![0_u8; out_width * out_height * 4];
    for row in 0..out_height {
        let src_y = row * height / out_height;
        let chroma_row = src_y / 2;
        for col in 0..out_width {
            let src_x = col * width / out_width;
            let y = i32::from(nv12[src_y * hor_stride + src_x]);
            let chroma = uv_offset + chroma_row * hor_stride + (src_x & !1);
            let u = i32::from(nv12[chroma]) - 128;
            let v = i32::from(nv12[chroma + 1]) - 128;
            let r = (y + ((1436 * v) >> 10)).clamp(0, 255) as u8;
            let g = (y - ((352 * u) >> 10) - ((731 * v) >> 10)).clamp(0, 255) as u8;
            let b = (y + ((1815 * u) >> 10)).clamp(0, 255) as u8;
            let index = (row * out_width + col) * 4;
            rgba[index] = r;
            rgba[index + 1] = g;
            rgba[index + 2] = b;
            rgba[index + 3] = 255;
        }
    }
    Some(rgba)
}

impl Drop for MppDecoder {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            unsafe {
                ((*self.mpi).reset)(self.ctx);
                (self.lib.mpp_destroy)(self.ctx);
            }
            self.ctx = std::ptr::null_mut();
        }
        if !self.frame_group.is_null() {
            unsafe { (self.lib.mpp_buffer_group_put)(self.frame_group) };
            self.frame_group = std::ptr::null_mut();
        }
    }
}

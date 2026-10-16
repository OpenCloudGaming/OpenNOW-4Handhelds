#![allow(non_camel_case_types, non_upper_case_globals, dead_code)]

use std::ffi::c_void;

pub const MPP_OK: i32 = 0;
pub const MPP_NOK: i32 = -1;
pub const MPP_ERR_UNKNOW: i32 = -2;
pub const MPP_ERR_NULL_PTR: i32 = -3;
pub const MPP_ERR_MALLOC: i32 = -4;
pub const MPP_ERR_OPEN_FILE: i32 = -5;
pub const MPP_ERR_VALUE: i32 = -6;
pub const MPP_ERR_READ_BIT: i32 = -7;
pub const MPP_ERR_TIMEOUT: i32 = -8;
pub const MPP_ERR_PERM: i32 = -9;
pub const MPP_ERR_BUFFER_FULL: i32 = -1012;

// MppCtxType
pub const MPP_CTX_DEC: u32 = 0;
pub const MPP_CTX_ENC: u32 = 1;
pub const MPP_CTX_ISP: u32 = 2;

// MppCodingType
pub const MPP_VIDEO_CodingUnused: u32 = 0;
pub const MPP_VIDEO_CodingAutoDetect: u32 = 1;
pub const MPP_VIDEO_CodingMPEG2: u32 = 2;
pub const MPP_VIDEO_CodingH263: u32 = 3;
pub const MPP_VIDEO_CodingMPEG4: u32 = 4;
pub const MPP_VIDEO_CodingWMV: u32 = 5;
pub const MPP_VIDEO_CodingRV: u32 = 6;
pub const MPP_VIDEO_CodingAVC: u32 = 7; // H.264
pub const MPP_VIDEO_CodingMJPEG: u32 = 8;
pub const MPP_VIDEO_CodingVP8: u32 = 9;
pub const MPP_VIDEO_CodingVP9: u32 = 10;
pub const MPP_VIDEO_CodingHEVC: u32 = 11; // H.265
pub const MPP_VIDEO_CodingAVSplus: u32 = 12;
pub const MPP_VIDEO_CodingAVS: u32 = 13;
pub const MPP_VIDEO_CodingAV1: u32 = 14;

// Command ids from Rockchip's rk_mpi_cmd.h. The module and context live in
// the high bits; a packed sequential enum does not reach the decoder.
pub const MPP_SET_INPUT_TIMEOUT: u32 = 0x0020_0006;
pub const MPP_SET_OUTPUT_TIMEOUT: u32 = 0x0020_0007;
pub const MPP_DEC_CMD_BASE: u32 = 0x0031_0000;
pub const MPP_DEC_SET_EXT_BUF_GROUP: u32 = MPP_DEC_CMD_BASE + 2;
pub const MPP_DEC_SET_INFO_CHANGE_READY: u32 = MPP_DEC_CMD_BASE + 3;
pub const MPP_DEC_SET_OUTPUT_FORMAT: u32 = MPP_DEC_CMD_BASE + 10;

pub const MPP_BUFFER_INTERNAL: u32 = 0;
pub const MPP_BUFFER_TYPE_ION: u32 = 1;
pub const MPP_BUFFER_TYPE_DRM: u32 = 3;
pub const MPP_FRAME_LIMIT: i32 = 24;

// MppFrameFormat
pub const MPP_FMT_YUV420SP: u32 = 0; // NV12
pub const MPP_FRAME_FMT_MASK: u32 = 0x000f_ffff;
pub const MPP_FRAME_FBC_MASK: u32 = 0x00f0_0000;
pub const MPP_FRAME_TILE_FLAG: u32 = 0x0200_0000;
pub const MPP_FMT_YUV420SP_10BIT: u32 = 1; // P010
pub const MPP_FMT_YUV422SP: u32 = 2;
pub const MPP_FMT_YUV420P: u32 = 4; // I420

pub type MppCtx = *mut c_void;
pub type MppPacket = *mut c_void;
pub type MppFrame = *mut c_void;
pub type MppBuffer = *mut c_void;
pub type MpiCmd = u32;
pub type MppParam = *mut c_void;

#[repr(C)]
pub struct MppApi {
    pub size: u32,
    pub version: u32,
    pub decode: unsafe extern "C" fn(MppCtx, MppPacket, *mut MppFrame) -> i32,
    pub decode_put_packet: unsafe extern "C" fn(MppCtx, MppPacket) -> i32,
    pub decode_get_frame: unsafe extern "C" fn(MppCtx, *mut MppFrame) -> i32,
    pub encode: unsafe extern "C" fn(MppCtx, MppFrame, *mut MppPacket) -> i32,
    pub encode_put_frame: unsafe extern "C" fn(MppCtx, MppFrame) -> i32,
    pub encode_get_packet: unsafe extern "C" fn(MppCtx, *mut MppPacket) -> i32,
    pub isp: unsafe extern "C" fn(MppCtx, MppFrame, MppFrame) -> i32,
    pub isp_put_frame: unsafe extern "C" fn(MppCtx, MppFrame) -> i32,
    pub isp_get_frame: unsafe extern "C" fn(MppCtx, *mut MppFrame) -> i32,
    pub poll: unsafe extern "C" fn(MppCtx, u32, u32) -> i32,
    pub dequeue: unsafe extern "C" fn(MppCtx, u32, *mut *mut c_void) -> i32,
    pub enqueue: unsafe extern "C" fn(MppCtx, u32, *mut c_void) -> i32,
    pub reset: unsafe extern "C" fn(MppCtx) -> i32,
    pub control: unsafe extern "C" fn(MppCtx, MpiCmd, MppParam) -> i32,
}

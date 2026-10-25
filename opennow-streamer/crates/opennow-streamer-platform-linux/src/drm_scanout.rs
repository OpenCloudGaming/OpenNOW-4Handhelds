//! Scans decoded NV12 frames out on a KMS overlay plane.
//!
//! The decoder keeps writing into its own buffers. This holds at most three of
//! them, shows the newest, and lets the display controller scale it to the
//! panel. SDL keeps the primary plane for the menu.

use std::collections::VecDeque;
use std::ffi::CStr;
use std::os::raw::{c_int, c_ulong};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use libloading::Library;

use crate::format::{DecodedVideoFrame, DmaBufFrame};

const DRM_FORMAT_NV12: u32 = u32::from_le_bytes(*b"NV12");
const DRM_MODE_CONNECTED: u32 = 1;
const DRM_MODE_OBJECT_PLANE: u32 = 0xeeee_eeee;
const DRM_MODE_PROP_RANGE: u32 = 1 << 1;
const DRM_MODE_PROP_IMMUTABLE: u32 = 1 << 2;
const DRM_MODE_PROP_ENUM: u32 = 1 << 3;
const DRM_IOCTL_GEM_CLOSE: c_ulong = (1 << 30) | ((b'd' as c_ulong) << 8) | 0x09 | (8 << 16);
const DRM_IOCTL_MODE_CREATE_DUMB: c_ulong =
    (3 << 30) | ((b'd' as c_ulong) << 8) | 0xB2 | (32 << 16);
const DRM_IOCTL_MODE_DESTROY_DUMB: c_ulong = (1 << 30) | ((b'd' as c_ulong) << 8) | 0xB4 | (4 << 16);
const DRM_PRIME_RDWR_CLOEXEC: u32 = 0x80002;
const RGA_FORMAT_NV12: i32 = 0xa;
const RGA_ROTATE_90: i32 = 0x4;
const RGA_ROTATE_270: i32 = 0x7;
const MAX_HELD_FRAMES: usize = 3;

static SUSPENDED: AtomicBool = AtomicBool::new(false);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
static SCANOUT: Mutex<Option<Scanout>> = Mutex::new(None);

#[repr(C)]
struct DrmModeRes {
    count_fbs: i32,
    _pad0: i32,
    fbs: *mut u32,
    count_crtcs: i32,
    _pad1: i32,
    crtcs: *mut u32,
    count_connectors: i32,
    _pad2: i32,
    connectors: *mut u32,
    count_encoders: i32,
    _pad3: i32,
    encoders: *mut u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

#[repr(C)]
struct DrmModeConnectorHead {
    connector_id: u32,
    encoder_id: u32,
    connector_type: u32,
    connector_type_id: u32,
    connection: u32,
}

#[repr(C)]
struct DrmModeEncoder {
    encoder_id: u32,
    encoder_type: u32,
    crtc_id: u32,
    possible_crtcs: u32,
    possible_clones: u32,
}

#[repr(C)]
struct DrmModeCrtcHead {
    crtc_id: u32,
    buffer_id: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
struct DrmModePlane {
    count_formats: u32,
    _pad: u32,
    formats: *mut u32,
    plane_id: u32,
    crtc_id: u32,
    fb_id: u32,
    crtc_x: u32,
    crtc_y: u32,
    x: u32,
    y: u32,
    possible_crtcs: u32,
    gamma_size: u32,
}

#[repr(C)]
struct DrmModePlaneRes {
    count_planes: u32,
    _pad: u32,
    planes: *mut u32,
}

#[repr(C)]
struct DrmModeObjectProperties {
    count_props: u32,
    _pad: u32,
    props: *mut u32,
    prop_values: *mut u64,
}

#[repr(C)]
struct DrmModeProperty {
    prop_id: u32,
    flags: u32,
    name: [u8; 32],
    count_values: i32,
    _pad: i32,
    values: *mut u64,
    count_enums: i32,
    _pad2: i32,
    enums: *mut DrmModeEnum,
}

#[repr(C)]
struct DrmModeEnum {
    value: u64,
    name: [u8; 32],
}

#[repr(C)]
struct DrmGemClose {
    handle: u32,
    pad: u32,
}

#[repr(C)]
struct DrmCreateDumb {
    height: u32,
    width: u32,
    bpp: u32,
    flags: u32,
    handle: u32,
    pitch: u32,
    size: u64,
}

#[repr(C)]
struct DrmDestroyDumb {
    handle: u32,
}

#[repr(C)]
struct RgaRect {
    xoffset: i32,
    yoffset: i32,
    width: i32,
    height: i32,
    wstride: i32,
    hstride: i32,
    format: i32,
    size: i32,
}

/// Prefix shared by the old and current librga `rga_info_t`. The call itself
/// passes a larger zeroed buffer so a newer library does not read off the end.
#[repr(C)]
struct RgaInfoPrefix {
    fd: i32,
    _pad_fd: i32,
    vir_addr: *mut std::ffi::c_void,
    phy_addr: *mut std::ffi::c_void,
    hnd: u32,
    format: i32,
    rect: RgaRect,
    blend: u32,
    buffer_size: i32,
    rotation: i32,
    color: i32,
    test_log: i32,
    mmu_flag: i32,
}

struct Drm {
    _library: Library,
    get_resources: unsafe extern "C" fn(c_int) -> *mut DrmModeRes,
    free_resources: unsafe extern "C" fn(*mut DrmModeRes),
    get_connector: unsafe extern "C" fn(c_int, u32) -> *mut DrmModeConnectorHead,
    free_connector: unsafe extern "C" fn(*mut DrmModeConnectorHead),
    get_encoder: unsafe extern "C" fn(c_int, u32) -> *mut DrmModeEncoder,
    free_encoder: unsafe extern "C" fn(*mut DrmModeEncoder),
    get_crtc: unsafe extern "C" fn(c_int, u32) -> *mut DrmModeCrtcHead,
    free_crtc: unsafe extern "C" fn(*mut DrmModeCrtcHead),
    get_plane_resources: unsafe extern "C" fn(c_int) -> *mut DrmModePlaneRes,
    free_plane_resources: unsafe extern "C" fn(*mut DrmModePlaneRes),
    get_plane: unsafe extern "C" fn(c_int, u32) -> *mut DrmModePlane,
    free_plane: unsafe extern "C" fn(*mut DrmModePlane),
    get_properties: unsafe extern "C" fn(c_int, u32, u32) -> *mut DrmModeObjectProperties,
    free_properties: unsafe extern "C" fn(*mut DrmModeObjectProperties),
    get_property: unsafe extern "C" fn(c_int, u32) -> *mut DrmModeProperty,
    free_property: unsafe extern "C" fn(*mut DrmModeProperty),
    set_property: unsafe extern "C" fn(c_int, u32, u32, u32, u64) -> c_int,
    prime_fd_to_handle: unsafe extern "C" fn(c_int, c_int, *mut u32) -> c_int,
    prime_handle_to_fd: unsafe extern "C" fn(c_int, u32, u32, *mut c_int) -> c_int,
    add_fb2: unsafe extern "C" fn(
        c_int,
        u32,
        u32,
        u32,
        *const u32,
        *const u32,
        *const u32,
        *mut u32,
        u32,
    ) -> c_int,
    rm_fb: unsafe extern "C" fn(c_int, u32) -> c_int,
    set_plane: unsafe extern "C" fn(
        c_int,
        u32,
        u32,
        u32,
        u32,
        i32,
        i32,
        u32,
        u32,
        u32,
        u32,
        u32,
        u32,
    ) -> c_int,
}

struct Held {
    fb: u32,
    handle: u32,
    _frame: std::sync::Arc<DmaBufFrame>,
}

struct TurnedBuffer {
    handle: u32,
    fb: u32,
    prime_fd: c_int,
    pitch: u32,
    width: u32,
    height: u32,
}

struct Scanout {
    drm: &'static Drm,
    fd: c_int,
    crtc_id: u32,
    plane_id: u32,
    crtc_width: u32,
    crtc_height: u32,
    /// Degrees clockwise to turn a landscape picture so it matches the menus.
    /// The display cannot turn a plane, so the chip's 2D unit turns the pixels.
    /// 0 means the mode is already the same way up as the window.
    turn: u16,
    turned: Vec<TurnedBuffer>,
    turned_cursor: usize,
    held: VecDeque<Held>,
    failed: bool,
    window: Instant,
    window_frames: u64,
}

fn load_symbol<T: Copy>(library: &Library, name: &[u8]) -> Option<T> {
    unsafe { library.get::<T>(name) }.ok().map(|symbol| *symbol)
}

fn load_drm() -> Option<Drm> {
    let library = unsafe { Library::new("libdrm.so.2") }.ok()?;
    Some(Drm {
        get_resources: load_symbol(&library, b"drmModeGetResources\0")?,
        free_resources: load_symbol(&library, b"drmModeFreeResources\0")?,
        get_connector: load_symbol(&library, b"drmModeGetConnector\0")?,
        free_connector: load_symbol(&library, b"drmModeFreeConnector\0")?,
        get_encoder: load_symbol(&library, b"drmModeGetEncoder\0")?,
        free_encoder: load_symbol(&library, b"drmModeFreeEncoder\0")?,
        get_crtc: load_symbol(&library, b"drmModeGetCrtc\0")?,
        free_crtc: load_symbol(&library, b"drmModeFreeCrtc\0")?,
        get_plane_resources: load_symbol(&library, b"drmModeGetPlaneResources\0")?,
        free_plane_resources: load_symbol(&library, b"drmModeFreePlaneResources\0")?,
        get_plane: load_symbol(&library, b"drmModeGetPlane\0")?,
        free_plane: load_symbol(&library, b"drmModeFreePlane\0")?,
        get_properties: load_symbol(&library, b"drmModeObjectGetProperties\0")?,
        free_properties: load_symbol(&library, b"drmModeFreeObjectProperties\0")?,
        get_property: load_symbol(&library, b"drmModeGetProperty\0")?,
        free_property: load_symbol(&library, b"drmModeFreeProperty\0")?,
        set_property: load_symbol(&library, b"drmModeObjectSetProperty\0")?,
        prime_fd_to_handle: load_symbol(&library, b"drmPrimeFDToHandle\0")?,
        prime_handle_to_fd: load_symbol(&library, b"drmPrimeHandleToFD\0")?,
        add_fb2: load_symbol(&library, b"drmModeAddFB2\0")?,
        rm_fb: load_symbol(&library, b"drmModeRmFB\0")?,
        set_plane: load_symbol(&library, b"drmModeSetPlane\0")?,
        _library: library,
    })
}

fn drm() -> Option<&'static Drm> {
    static DRM: OnceLock<Option<Drm>> = OnceLock::new();
    DRM.get_or_init(load_drm).as_ref()
}

struct Rga {
    _library: Library,
    blit: unsafe extern "C" fn(
        *mut std::ffi::c_void,
        *mut std::ffi::c_void,
        *mut std::ffi::c_void,
    ) -> c_int,
}

fn load_rga() -> Option<Rga> {
    let library = unsafe { Library::new("librga.so.2") }
        .or_else(|_| unsafe { Library::new("librga.so") })
        .ok()?;
    let init = load_symbol::<unsafe extern "C" fn() -> c_int>(&library, b"c_RkRgaInit\0")?;
    let blit = load_symbol(&library, b"c_RkRgaBlit\0")?;
    if unsafe { init() } < 0 {
        return None;
    }
    Some(Rga {
        _library: library,
        blit,
    })
}

fn rga() -> Option<&'static Rga> {
    static RGA: OnceLock<Option<Rga>> = OnceLock::new();
    RGA.get_or_init(load_rga).as_ref()
}

fn rga_turn(
    rga: &Rga,
    src_fd: c_int,
    src_w: i32,
    src_h: i32,
    src_stride: i32,
    src_coded_h: i32,
    dst_fd: c_int,
    dst_w: i32,
    dst_h: i32,
    dst_stride: i32,
    degrees: u16,
) -> c_int {
    let rotation = match degrees {
        90 => RGA_ROTATE_90,
        270 => RGA_ROTATE_270,
        _ => return -1,
    };
    let mut src_raw = [0u8; 4096];
    let mut dst_raw = [0u8; 4096];
    unsafe {
        let src = &mut *(src_raw.as_mut_ptr() as *mut RgaInfoPrefix);
        src.fd = src_fd;
        src.format = RGA_FORMAT_NV12;
        src.rect = RgaRect {
            xoffset: 0,
            yoffset: 0,
            width: src_w,
            height: src_h,
            wstride: src_stride,
            hstride: src_coded_h,
            format: RGA_FORMAT_NV12,
            size: 0,
        };
        src.rotation = rotation;
        src.mmu_flag = 1;
        let dst = &mut *(dst_raw.as_mut_ptr() as *mut RgaInfoPrefix);
        dst.fd = dst_fd;
        dst.format = RGA_FORMAT_NV12;
        dst.rect = RgaRect {
            xoffset: 0,
            yoffset: 0,
            width: dst_w,
            height: dst_h,
            wstride: dst_stride,
            hstride: dst_h,
            format: RGA_FORMAT_NV12,
            size: 0,
        };
        dst.mmu_flag = 1;
        (rga.blit)(
            src_raw.as_mut_ptr().cast(),
            dst_raw.as_mut_ptr().cast(),
            std::ptr::null_mut(),
        )
    }
}

fn c_name(bytes: &[u8]) -> String {
    CStr::from_bytes_until_nul(bytes)
        .unwrap_or(c"<bad>")
        .to_string_lossy()
        .into_owned()
}

impl Drm {
    fn plane_type_name(&self, fd: c_int, plane_id: u32) -> Option<String> {
        let properties = unsafe { (self.get_properties)(fd, plane_id, DRM_MODE_OBJECT_PLANE) };
        if properties.is_null() {
            return None;
        }
        let count = unsafe { (*properties).count_props as isize };
        let ids = unsafe { (*properties).props };
        let values = unsafe { (*properties).prop_values };
        let mut name = None;
        for index in 0..count {
            let prop_id = unsafe { *ids.offset(index) };
            let current = unsafe { *values.offset(index) };
            let property = unsafe { (self.get_property)(fd, prop_id) };
            if property.is_null() {
                continue;
            }
            let is_type = c_name(unsafe { &(*property).name }) == "type"
                && unsafe { (*property).flags } & DRM_MODE_PROP_ENUM != 0;
            if is_type {
                let enums = unsafe { (*property).count_enums as isize };
                let table = unsafe { (*property).enums };
                if !table.is_null() {
                    for enum_index in 0..enums {
                        let entry = unsafe { &*table.offset(enum_index) };
                        if entry.value == current {
                            name = Some(c_name(&entry.name));
                        }
                    }
                }
            }
            unsafe { (self.free_property)(property) };
            if name.is_some() {
                break;
            }
        }
        unsafe { (self.free_properties)(properties) };
        name
    }

    fn plane_type_is_overlay(&self, fd: c_int, plane_id: u32) -> bool {
        self.plane_type_name(fd, plane_id).as_deref() == Some("Overlay")
    }

    fn raise_plane(&self, fd: c_int, plane_id: u32) {
        let properties = unsafe { (self.get_properties)(fd, plane_id, DRM_MODE_OBJECT_PLANE) };
        if properties.is_null() {
            return;
        }
        let count = unsafe { (*properties).count_props as isize };
        let ids = unsafe { (*properties).props };
        for index in 0..count {
            let prop_id = unsafe { *ids.offset(index) };
            let property = unsafe { (self.get_property)(fd, prop_id) };
            if property.is_null() {
                continue;
            }
            let flags = unsafe { (*property).flags };
            let is_zpos = c_name(unsafe { &(*property).name }) == "zpos";
            let top = if is_zpos && flags & DRM_MODE_PROP_IMMUTABLE == 0 && flags & DRM_MODE_PROP_RANGE != 0
            {
                let count_values = unsafe { (*property).count_values };
                let values = unsafe { (*property).values };
                if count_values >= 2 && !values.is_null() {
                    Some(unsafe { *values.add(1) })
                } else {
                    None
                }
            } else {
                None
            };
            unsafe { (self.free_property)(property) };
            if let Some(top) = top {
                unsafe {
                    (self.set_property)(fd, plane_id, DRM_MODE_OBJECT_PLANE, prop_id, top);
                }
                break;
            }
        }
        unsafe { (self.free_properties)(properties) };
    }
}

fn active_crtc(drm: &Drm, fd: c_int) -> Option<(u32, usize, u32, u32)> {
    let resources = unsafe { (drm.get_resources)(fd) };
    if resources.is_null() {
        return None;
    }
    let mut chosen = None;
    let connectors = unsafe { (*resources).count_connectors };
    let connector_ids = unsafe { (*resources).connectors };
    for index in 0..connectors {
        let connector_id = unsafe { *connector_ids.offset(index as isize) };
        let connector = unsafe { (drm.get_connector)(fd, connector_id) };
        if connector.is_null() {
            continue;
        }
        let connected = unsafe { (*connector).connection } == DRM_MODE_CONNECTED;
        let encoder_id = unsafe { (*connector).encoder_id };
        unsafe { (drm.free_connector)(connector) };
        if !connected || encoder_id == 0 {
            continue;
        }
        let encoder = unsafe { (drm.get_encoder)(fd, encoder_id) };
        if encoder.is_null() {
            continue;
        }
        let crtc_id = unsafe { (*encoder).crtc_id };
        unsafe { (drm.free_encoder)(encoder) };
        if crtc_id == 0 {
            continue;
        }
        let crtc_count = unsafe { (*resources).count_crtcs };
        let crtc_ids = unsafe { (*resources).crtcs };
        let mut crtc_index = None;
        for candidate in 0..crtc_count {
            if unsafe { *crtc_ids.offset(candidate as isize) } == crtc_id {
                crtc_index = Some(candidate as usize);
                break;
            }
        }
        let Some(crtc_index) = crtc_index else {
            continue;
        };
        let crtc = unsafe { (drm.get_crtc)(fd, crtc_id) };
        if crtc.is_null() {
            continue;
        }
        let width = unsafe { (*crtc).width };
        let height = unsafe { (*crtc).height };
        unsafe { (drm.free_crtc)(crtc) };
        chosen = Some((crtc_id, crtc_index, width, height));
        break;
    }
    unsafe { (drm.free_resources)(resources) };
    chosen
}

fn overlay_plane(drm: &Drm, fd: c_int, crtc_index: usize) -> Option<u32> {
    let resources = unsafe { (drm.get_plane_resources)(fd) };
    if resources.is_null() {
        return None;
    }
    let count = unsafe { (*resources).count_planes };
    let ids = unsafe { (*resources).planes };
    let mask = 1u32 << crtc_index;
    let mut fallback = None;
    let mut overlay = None;
    for index in 0..count {
        let plane_id = unsafe { *ids.offset(index as isize) };
        let plane = unsafe { (drm.get_plane)(fd, plane_id) };
        if plane.is_null() {
            continue;
        }
        let possible = unsafe { (*plane).possible_crtcs };
        let formats = unsafe { (*plane).count_formats as isize };
        let format_ptr = unsafe { (*plane).formats };
        let mut nv12 = false;
        if possible & mask != 0 && !format_ptr.is_null() {
            for format_index in 0..formats {
                if unsafe { *format_ptr.offset(format_index) } == DRM_FORMAT_NV12 {
                    nv12 = true;
                    break;
                }
            }
        }
        unsafe { (drm.free_plane)(plane) };
        if !nv12 {
            continue;
        }
        if drm.plane_type_is_overlay(fd, plane_id) {
            overlay = Some(plane_id);
            break;
        }
        fallback.get_or_insert(plane_id);
    }
    unsafe { (drm.free_plane_resources)(resources) };
    overlay.or(fallback)
}

/// Reads the panel size and leaves the picture on the GPU.
///
/// The stream is letterboxed into the SDL window, which is already oriented
/// for the user. An NV12 plane in panel space covers the menu.
pub fn arm_display_scanout(drm_fd: c_int, width: u32, height: u32) -> bool {
    let Some(drm) = drm() else {
        eprintln!("stream: display plane unavailable, libdrm did not load");
        return false;
    };
    let fd = unsafe { libc::dup(drm_fd) };
    if fd < 0 {
        eprintln!("stream: display plane could not use the SDL DRM device");
        return false;
    }
    let Some((_crtc_id, _crtc_index, crtc_width, crtc_height)) = active_crtc(drm, fd) else {
        eprintln!("stream: display plane found no active screen");
        unsafe { libc::close(fd) };
        return false;
    };
    let crtc_width = if crtc_width >= 2 { crtc_width } else { width.max(2) };
    let crtc_height = if crtc_height >= 2 { crtc_height } else { height.max(2) };
    eprintln!(
        "stream: display {crtc_width}x{crtc_height}, window {width}x{height}, picture stays on the GPU"
    );
    unsafe { libc::close(fd) };
    false
}

pub fn set_display_scanout_suspended(suspended: bool) {
    let was = SUSPENDED.swap(suspended, Ordering::AcqRel);
    if suspended && !was {
        if let Some(scanout) = SCANOUT.lock().unwrap_or_else(|error| error.into_inner()).as_mut() {
            scanout.hide();
            eprintln!("stream: display plane paused");
        }
    }
}

pub fn display_scanout_sequence() -> u64 {
    SEQUENCE.load(Ordering::Acquire)
}

pub fn display_scanout_failed() -> bool {
    SCANOUT
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .is_some_and(|scanout| scanout.failed)
}

/// Shows `frame` on the overlay. A `false` return leaves the GLES path in charge.
pub fn present_display_plane(frame: &DecodedVideoFrame) -> bool {
    if SUSPENDED.load(Ordering::Acquire) {
        return false;
    }
    let mut guard = SCANOUT.lock().unwrap_or_else(|error| error.into_inner());
    let Some(scanout) = guard.as_mut() else {
        return false;
    };
    if scanout.failed {
        return false;
    }
    match scanout.present(frame) {
        Ok(()) => true,
        Err(message) => {
            scanout.failed = true;
            scanout.hide();
            eprintln!("stream: display plane stopped ({message}); using the GPU path");
            false
        }
    }
}

impl Scanout {
    fn hide(&mut self) {
        unsafe {
            (self.drm.set_plane)(
                self.fd,
                self.plane_id,
                self.crtc_id,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            );
        }
        while let Some(held) = self.held.pop_front() {
            self.release_held(held);
        }
    }

    fn release_held(&self, held: Held) {
        unsafe {
            (self.drm.rm_fb)(self.fd, held.fb);
            let mut close = DrmGemClose {
                handle: held.handle,
                pad: 0,
            };
            libc::ioctl(self.fd, DRM_IOCTL_GEM_CLOSE, &mut close);
        }
    }

    fn present(&mut self, frame: &DecodedVideoFrame) -> Result<(), String> {
        if self.turn != 0 {
            return Err("picture stays on the GPU".to_owned());
        }
        let dmabuf = frame
            .dmabuf
            .as_ref()
            .ok_or_else(|| "decoded frame has no display buffer".to_owned())?;
        let layer = dmabuf
            .layers
            .first()
            .filter(|layer| layer.format == DRM_FORMAT_NV12 && layer.planes.len() >= 2)
            .ok_or_else(|| "decoded frame is not NV12".to_owned())?;
        let luma = &layer.planes[0];
        let chroma = &layer.planes[1];
        if luma.object_index != chroma.object_index {
            return Err("NV12 planes are not in one buffer".to_owned());
        }
        let object = dmabuf
            .objects
            .get(luma.object_index)
            .ok_or_else(|| "NV12 buffer is missing".to_owned())?;
        let mut handle = 0u32;
        let imported = unsafe { (self.drm.prime_fd_to_handle)(self.fd, object.fd, &mut handle) };
        if imported != 0 || handle == 0 {
            return Err(format!("could not import the decoder buffer ({imported})"));
        }
        let handles = [handle, handle, 0, 0];
        let pitches = [luma.pitch as u32, chroma.pitch as u32, 0, 0];
        let offsets = [luma.offset as u32, chroma.offset as u32, 0, 0];
        let mut fb = 0u32;
        let added = unsafe {
            (self.drm.add_fb2)(
                self.fd,
                frame.format.width,
                frame.format.height,
                DRM_FORMAT_NV12,
                handles.as_ptr(),
                pitches.as_ptr(),
                offsets.as_ptr(),
                &mut fb,
                0,
            )
        };
        if added != 0 || fb == 0 {
            unsafe {
                let mut close = DrmGemClose { handle, pad: 0 };
                libc::ioctl(self.fd, DRM_IOCTL_GEM_CLOSE, &mut close);
            }
            return Err(format!("could not wrap the decoder buffer ({added})"));
        }
        let (crtc_x, crtc_y, crtc_w, crtc_h) = letterbox(
            frame.format.width,
            frame.format.height,
            self.crtc_width,
            self.crtc_height,
        );
        let flipped = unsafe {
            (self.drm.set_plane)(
                self.fd,
                self.plane_id,
                self.crtc_id,
                fb,
                0,
                crtc_x,
                crtc_y,
                crtc_w,
                crtc_h,
                0,
                0,
                frame.format.width << 16,
                frame.format.height << 16,
            )
        };
        if flipped != 0 {
            unsafe {
                (self.drm.rm_fb)(self.fd, fb);
                let mut close = DrmGemClose { handle, pad: 0 };
                libc::ioctl(self.fd, DRM_IOCTL_GEM_CLOSE, &mut close);
            }
            return Err(format!("the display rejected the frame ({flipped})"));
        }
        self.held.push_back(Held {
            fb,
            handle,
            _frame: std::sync::Arc::clone(dmabuf),
        });
        while self.held.len() > MAX_HELD_FRAMES {
            if let Some(old) = self.held.pop_front() {
                self.release_held(old);
            }
        }
        SEQUENCE.fetch_add(1, Ordering::Release);
        self.window_frames = self.window_frames.saturating_add(1);
        let elapsed = self.window.elapsed();
        if elapsed.as_secs_f64() >= 1.0 {
            let fps = self.window_frames as f64 / elapsed.as_secs_f64();
            eprintln!("stream: display plane {fps:.0} fps");
            self.window_frames = 0;
            self.window = Instant::now();
        }
        Ok(())
    }

    fn present_turned(&mut self, frame: &DecodedVideoFrame) -> Result<(), String> {
        let src_w = frame.format.width;
        let src_h = frame.format.height;
        if src_w < 2 || src_h < 2 || src_w % 2 != 0 || src_h % 2 != 0 {
            return Err("picture size cannot be turned".to_owned());
        }
        let dst_w = src_h;
        let dst_h = src_w;
        self.ensure_turned(dst_w, dst_h)?;
        let index = self.turned_cursor % self.turned.len();
        self.turned_cursor = self.turned_cursor.wrapping_add(1);
        self.fill_turned(index, frame)?;
        let fb = self.turned[index].fb;
        let (crtc_x, crtc_y, crtc_w, crtc_h) =
            letterbox(dst_w, dst_h, self.crtc_width, self.crtc_height);
        let flipped = unsafe {
            (self.drm.set_plane)(
                self.fd,
                self.plane_id,
                self.crtc_id,
                fb,
                0,
                crtc_x,
                crtc_y,
                crtc_w,
                crtc_h,
                0,
                0,
                dst_w << 16,
                dst_h << 16,
            )
        };
        if flipped != 0 {
            return Err(format!("the display rejected the turned frame ({flipped})"));
        }
        SEQUENCE.fetch_add(1, Ordering::Release);
        self.window_frames = self.window_frames.saturating_add(1);
        let elapsed = self.window.elapsed();
        if elapsed.as_secs_f64() >= 1.0 {
            let fps = self.window_frames as f64 / elapsed.as_secs_f64();
            eprintln!("stream: display plane {fps:.0} fps");
            self.window_frames = 0;
            self.window = Instant::now();
        }
        Ok(())
    }

    fn ensure_turned(&mut self, width: u32, height: u32) -> Result<(), String> {
        if self
            .turned
            .first()
            .is_some_and(|buffer| buffer.width == width && buffer.height == height)
        {
            return Ok(());
        }
        self.destroy_turned();
        for _ in 0..MAX_HELD_FRAMES {
            match self.create_turned(width, height) {
                Ok(buffer) => self.turned.push(buffer),
                Err(message) => {
                    self.destroy_turned();
                    return Err(message);
                }
            }
        }
        Ok(())
    }

    fn create_turned(&self, width: u32, height: u32) -> Result<TurnedBuffer, String> {
        let stride = (width + 15) & !15;
        let mut create = DrmCreateDumb {
            height: height * 3 / 2,
            width: stride,
            bpp: 8,
            flags: 0,
            handle: 0,
            pitch: 0,
            size: 0,
        };
        let created = unsafe { libc::ioctl(self.fd, DRM_IOCTL_MODE_CREATE_DUMB, &mut create) };
        if created != 0 || create.handle == 0 || create.pitch < width || create.size < 16 {
            return Err(format!(
                "could not allocate a turned picture ({})",
                std::io::Error::last_os_error()
            ));
        }
        let handles = [create.handle, create.handle, 0, 0];
        let pitches = [create.pitch, create.pitch, 0, 0];
        let offsets = [0, create.pitch * height, 0, 0];
        let mut fb = 0u32;
        let added = unsafe {
            (self.drm.add_fb2)(
                self.fd,
                width,
                height,
                DRM_FORMAT_NV12,
                handles.as_ptr(),
                pitches.as_ptr(),
                offsets.as_ptr(),
                &mut fb,
                0,
            )
        };
        if added != 0 || fb == 0 {
            self.destroy_dumb(create.handle);
            return Err(format!("could not wrap a turned picture ({added})"));
        }
        let mut prime_fd = -1;
        let exported = unsafe {
            (self.drm.prime_handle_to_fd)(
                self.fd,
                create.handle,
                DRM_PRIME_RDWR_CLOEXEC,
                &mut prime_fd,
            )
        };
        if exported != 0 || prime_fd < 0 {
            unsafe { (self.drm.rm_fb)(self.fd, fb) };
            self.destroy_dumb(create.handle);
            return Err(format!(
                "could not share a turned picture ({})",
                std::io::Error::last_os_error()
            ));
        }
        Ok(TurnedBuffer {
            handle: create.handle,
            fb,
            prime_fd,
            pitch: create.pitch,
            width,
            height,
        })
    }

    fn destroy_dumb(&self, handle: u32) {
        let mut destroy = DrmDestroyDumb { handle };
        unsafe {
            libc::ioctl(self.fd, DRM_IOCTL_MODE_DESTROY_DUMB, &mut destroy);
        }
    }

    fn destroy_turned(&mut self) {
        for buffer in self.turned.drain(..) {
            unsafe {
                if buffer.fb != 0 {
                    (self.drm.rm_fb)(self.fd, buffer.fb);
                }
                if buffer.prime_fd >= 0 {
                    libc::close(buffer.prime_fd);
                }
                if buffer.handle != 0 {
                    let mut destroy = DrmDestroyDumb {
                        handle: buffer.handle,
                    };
                    libc::ioctl(self.fd, DRM_IOCTL_MODE_DESTROY_DUMB, &mut destroy);
                }
            }
        }
        self.turned_cursor = 0;
    }

    fn fill_turned(&mut self, index: usize, frame: &DecodedVideoFrame) -> Result<(), String> {
        let Some(rga) = rga() else {
            return Err("the 2D unit is not available".to_owned());
        };
        let dmabuf = frame
            .dmabuf
            .as_ref()
            .ok_or_else(|| "decoded frame has no display buffer".to_owned())?;
        let layer = dmabuf
            .layers
            .first()
            .filter(|layer| layer.format == DRM_FORMAT_NV12 && layer.planes.len() >= 2)
            .ok_or_else(|| "decoded frame is not NV12".to_owned())?;
        let luma = layer.planes[0];
        let chroma = layer.planes[1];
        if luma.object_index != chroma.object_index || luma.offset != 0 || luma.pitch == 0 {
            return Err("NV12 planes are not in one buffer".to_owned());
        }
        if chroma.pitch != luma.pitch || chroma.offset % luma.pitch != 0 {
            return Err("NV12 stride cannot be turned".to_owned());
        }
        let object = dmabuf
            .objects
            .get(luma.object_index)
            .ok_or_else(|| "NV12 buffer is missing".to_owned())?;
        let coded_h = (chroma.offset / luma.pitch) as i32;
        let visible_h = frame.format.height as i32;
        if coded_h < visible_h {
            return Err("NV12 stride cannot be turned".to_owned());
        }
        let buffer = &self.turned[index];
        let turned = rga_turn(
            rga,
            object.fd,
            frame.format.width as i32,
            visible_h,
            luma.pitch as i32,
            coded_h,
            buffer.prime_fd,
            buffer.width as i32,
            buffer.height as i32,
            buffer.pitch as i32,
            self.turn,
        );
        if turned != 0 {
            return Err(format!("the 2D unit rejected the turn ({turned})"));
        }
        Ok(())
    }
}

fn letterbox(width: u32, height: u32, crtc_width: u32, crtc_height: u32) -> (i32, i32, u32, u32) {
    if width < 2 || height < 2 || crtc_width < 2 || crtc_height < 2 {
        return (0, 0, crtc_width.max(2), crtc_height.max(2));
    }
    let frame_ratio = width as f32 / height as f32;
    let screen_ratio = crtc_width as f32 / crtc_height as f32;
    if screen_ratio > frame_ratio {
        let fitted = ((frame_ratio / screen_ratio) * crtc_width as f32) as u32 & !1;
        let fitted = fitted.max(2).min(crtc_width);
        let x = ((crtc_width - fitted) / 2) as i32;
        (x, 0, fitted, crtc_height)
    } else {
        let fitted = ((screen_ratio / frame_ratio) * crtc_height as f32) as u32 & !1;
        let fitted = fitted.max(2).min(crtc_height);
        let y = ((crtc_height - fitted) / 2) as i32;
        (0, y, crtc_width, fitted)
    }
}

impl Drop for Scanout {
    fn drop(&mut self) {
        self.hide();
        self.destroy_turned();
        unsafe { libc::close(self.fd) };
    }
}


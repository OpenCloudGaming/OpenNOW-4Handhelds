//! In-process NVST streamer. The shared library owns the network session,
//! decode, and audio. This module hands it the current OpenGL ES context and
//! reads back one RGBA texture per frame, and forwards the gamepad.

use crate::core_client::executable_dir;
use libloading::Library;
use serde_json::{Value, json};
use std::ffi::c_void;
use std::os::raw::c_char;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

const ABI_VERSION: u32 = 11;
const GRAPHICS_CONTEXT_VERSION: u32 = 3;
const RENDER_COMMAND_VERSION: u32 = 3;
const GRAPHICS_API_OPENGL_ES: u32 = 4;
const PROTOCOL_VERSION: u64 = 7;
// All embedded Linux video publishers currently allocate at least four GLES
// slots. Rotating them avoids reusing one DMA-BUF import every frame, which
// otherwise forces the presenter to drain the GPU before releasing it.
const GLES_FRAME_SLOTS: u32 = 4;

type Status = i32;
type Callback = unsafe extern "C" fn(*const u8, usize, *mut c_void);
type FrameCallback = unsafe extern "C" fn(*mut c_void);

#[repr(C)]
struct Config {
    abi_version: u32,
    struct_size: usize,
    command_queue_capacity: usize,
    response_queue_capacity: usize,
    event_queue_capacity: usize,
    max_command_bytes: usize,
    response_callback: Callback,
    event_callback: Callback,
    frame_available_callback: FrameCallback,
    cursor_callback: Callback,
    user_data: *mut c_void,
    vulkan_device: *const c_void,
    windows_adapter_luid: u64,
}

#[repr(C)]
struct GraphicsContext {
    version: u32,
    struct_size: usize,
    graphics_api: u32,
    instance: *mut c_void,
    physical_device: *mut c_void,
    device: *mut c_void,
    queue: *mut c_void,
    queue_family_index: u32,
    enabled_capabilities: u32,
}

#[repr(C)]
struct RecordCommand {
    version: u32,
    struct_size: usize,
    command_buffer: *mut c_void,
    frame_slot: u32,
    upscale_width: u32,
    upscale_height: u32,
    upscale_sharpness: u32,
    upscale_denoise: u32,
}

#[repr(C)]
#[derive(Default)]
struct FrameInfo {
    width: u32,
    height: u32,
    sequence: u64,
    presentation_time_ns: u64,
}

#[repr(C)]
#[derive(Default)]
struct RecordedFrame {
    resource: u64,
    resource_view: u64,
    graphics_api: u32,
    texture_format: u32,
    color_space: u32,
    width: u32,
    height: u32,
    frame_slot: u32,
    generation: u64,
    presentation_time_ns: u64,
}

struct Api {
    create: unsafe extern "C" fn(*const Config, *mut *mut c_void) -> Status,
    destroy: unsafe extern "C" fn(*mut c_void) -> Status,
    send: unsafe extern "C" fn(*const c_void, *const u8, usize) -> Status,
    set_graphics_context: unsafe extern "C" fn(*const c_void, *const GraphicsContext) -> Status,
    acquire_latest_frame: unsafe extern "C" fn(*const c_void, *mut *mut c_void, *mut FrameInfo) -> Status,
    record_frame: unsafe extern "C" fn(*const c_void, *const c_void, *const RecordCommand, *mut RecordedFrame) -> Status,
    release_frame: unsafe extern "C" fn(*mut c_void) -> Status,
    scene_graph_shutdown: unsafe extern "C" fn(*const c_void) -> Status,
    submit_gamepad: unsafe extern "C" fn(*const c_void, u8, u16, u16, u8, u8, i16, i16, i16, i16) -> Status,
    submit_mouse_absolute: unsafe extern "C" fn(*const c_void, u16, u16, u16, u16) -> Status,
    submit_mouse_button: unsafe extern "C" fn(*const c_void, u8, bool) -> Status,
    set_capture: unsafe extern "C" fn(*const c_void, bool, bool, usize, *mut bool) -> Status,
    _library: Library,
}

pub struct Picture {
    pub texture: u32,
    pub width: u32,
    pub height: u32,
}

struct HeldFrame {
    pointer: *mut c_void,
    picture: Picture,
}

/// The loaded streamer and the gamepad snapshot sent with it.
pub struct NativeStream {
    api: Api,
    handle: *mut c_void,
    messages: Receiver<Value>,
    /// Kept alive because the callbacks borrow the heap allocation.
    _outgoing: Box<Sender<Value>>,
    graphics_ready: bool,
    next_frame_slot: u32,
    held: Option<HeldFrame>,
    pad_buttons: u16,
    pad_left: (i16, i16),
    pad_right: (i16, i16),
    pad_triggers: (u8, u8),
    pad_sent: Option<Instant>,
}

fn library_path() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("OPENNOW_STREAMER") {
        return path.into();
    }
    let name = if cfg!(windows) { "opennow_streamer_ffi.dll" } else { "libopennow_streamer_ffi.so" };
    let bundled = executable_dir().join("libraries").join(&name);
    if bundled.is_file() {
        return bundled;
    }
    executable_dir().join(name)
}

impl NativeStream {
    pub fn open() -> Result<Self, String> {
        let path = library_path();
        let library = unsafe { Library::new(&path) }.map_err(|error| format!("Could not load {}: {error}", path.display()))?;
        let api = unsafe {
            Api {
                create: *library.get(b"opennow_streamer_create\0").map_err(|error| error.to_string())?,
                destroy: *library.get(b"opennow_streamer_destroy\0").map_err(|error| error.to_string())?,
                send: *library.get(b"opennow_streamer_send\0").map_err(|error| error.to_string())?,
                set_graphics_context: *library.get(b"opennow_streamer_set_graphics_context\0").map_err(|error| error.to_string())?,
                acquire_latest_frame: *library.get(b"opennow_streamer_acquire_latest_frame\0").map_err(|error| error.to_string())?,
                record_frame: *library.get(b"opennow_streamer_record_frame\0").map_err(|error| error.to_string())?,
                release_frame: *library.get(b"opennow_streamer_release_frame\0").map_err(|error| error.to_string())?,
                scene_graph_shutdown: *library.get(b"opennow_streamer_scene_graph_shutdown\0").map_err(|error| error.to_string())?,
                submit_gamepad: *library.get(b"opennow_streamer_submit_gamepad\0").map_err(|error| error.to_string())?,
                submit_mouse_absolute: *library.get(b"opennow_streamer_submit_mouse_absolute\0").map_err(|error| error.to_string())?,
                submit_mouse_button: *library.get(b"opennow_streamer_submit_mouse_button\0").map_err(|error| error.to_string())?,
                set_capture: *library.get(b"opennow_streamer_set_capture_active\0").map_err(|error| error.to_string())?,
                _library: library,
            }
        };
        let (sender, messages) = channel();
        let outgoing = Box::new(sender);
        let user_data = (&*outgoing as *const Sender<Value>).cast_mut().cast();
        let mut stream = Self {
            api,
            handle: std::ptr::null_mut(),
            messages,
            _outgoing: outgoing,
            graphics_ready: false,
            next_frame_slot: 0,
            held: None,
            pad_buttons: 0,
            pad_left: (0, 0),
            pad_right: (0, 0),
            pad_triggers: (0, 0),
            pad_sent: None,
        };
        let config = Config {
            abi_version: ABI_VERSION,
            struct_size: size_of::<Config>(),
            command_queue_capacity: 32,
            response_queue_capacity: 32,
            event_queue_capacity: 64,
            max_command_bytes: 1024 * 1024,
            response_callback: on_message,
            event_callback: on_message,
            frame_available_callback: on_frame,
            cursor_callback: on_message,
            user_data,
            vulkan_device: std::ptr::null(),
            windows_adapter_luid: 0,
        };
        let mut handle = std::ptr::null_mut();
        let status = unsafe { (stream.api.create)(&config, &mut handle) };
        if status != 0 || handle.is_null() {
            return Err(format!("The streamer runtime did not start ({status})"));
        }
        stream.handle = handle;
        let log = executable_dir().join("log.txt");
        if let Ok(bytes) = std::ffi::CString::new(log.to_string_lossy().as_bytes()) {
            unsafe {
                if let Ok(set_log) = stream.api._library.get::<unsafe extern "C" fn(*const c_char) -> Status>(b"opennow_streamer_set_log_file\0") {
                    set_log(bytes.as_ptr());
                }
            }
        }
        stream.send_json(&json!({"id": "hello", "type": "hello", "protocolVersion": PROTOCOL_VERSION}))?;
        Ok(stream)
    }

    pub fn poll(&self) -> Vec<Value> {
        self.messages.try_iter().collect()
    }

    pub fn start(&mut self, context: &Value) -> Result<(), String> {
        let mut raw = false;
        unsafe { (self.api.set_capture)(self.handle, true, false, 0, &mut raw) };
        self.send_json(&json!({
            "id": "start",
            "type": "start",
            "context": context,
            "microphoneEnabled": false,
        }))
    }

    pub fn stop_media(&mut self) {
        let _ = self.send_json(&json!({"id": "stop", "type": "stop", "reason": "ended from the handheld"}));
        self.release_held();
        self.pad_buttons = 0;
    }

    /// Records the newest decoded frame into a texture. The OpenGL ES context
    /// must be current on this thread. Call `release_picture` after drawing.
    pub fn picture(&mut self) -> Option<Picture> {
        self.release_held();
        if !self.graphics_ready {
            let context = GraphicsContext {
                version: GRAPHICS_CONTEXT_VERSION,
                struct_size: size_of::<GraphicsContext>(),
                graphics_api: GRAPHICS_API_OPENGL_ES,
                instance: std::ptr::null_mut(),
                physical_device: std::ptr::null_mut(),
                // The GLES presenter uses the current context, but the runtime
                // rejects a null device and queue.
                device: 1 as *mut c_void,
                queue: 1 as *mut c_void,
                queue_family_index: 0,
                enabled_capabilities: 0,
            };
            let status = unsafe { (self.api.set_graphics_context)(self.handle, &context) };
            if status != 0 {
                eprintln!("stream: graphics context rejected ({status})");
                return None;
            }
            eprintln!("stream: GLES frame output rotates across {GLES_FRAME_SLOTS} slots");
            self.graphics_ready = true;
        }
        let mut frame = std::ptr::null_mut();
        let mut info = FrameInfo::default();
        let status = unsafe { (self.api.acquire_latest_frame)(self.handle, &mut frame, &mut info) };
        if status != 0 || frame.is_null() {
            return None;
        }
        let command = RecordCommand {
            version: RENDER_COMMAND_VERSION,
            struct_size: size_of::<RecordCommand>(),
            command_buffer: 1 as *mut c_void,
            frame_slot: self.next_frame_slot,
            upscale_width: 0,
            upscale_height: 0,
            upscale_sharpness: 0,
            upscale_denoise: 0,
        };
        self.next_frame_slot = (self.next_frame_slot + 1) % GLES_FRAME_SLOTS;
        let mut recorded = RecordedFrame::default();
        let status = unsafe { (self.api.record_frame)(self.handle, frame, &command, &mut recorded) };
        if status != 0 || recorded.resource == 0 {
            unsafe { (self.api.release_frame)(frame) };
            return None;
        }
        let picture = Picture {
            texture: recorded.resource as u32,
            width: recorded.width,
            height: recorded.height,
        };
        self.held = Some(HeldFrame { pointer: frame, picture: Picture { texture: picture.texture, width: picture.width, height: picture.height } });
        Some(picture)
    }

    pub fn release_picture(&mut self) {
        self.release_held();
    }

    pub fn button(&mut self, button: u8, pressed: bool, raw_joystick: bool, xbox_layout: bool) {
        let Some(mask) = button_mask(button, raw_joystick, xbox_layout) else {
            return;
        };
        if pressed {
            self.pad_buttons |= mask;
        } else {
            self.pad_buttons &= !mask;
        }
        self.submit_pad();
    }

    pub fn axis(&mut self, axis: u8, value: i16) {
        match axis {
            0 => self.pad_left.0 = value,
            1 => self.pad_left.1 = invert_axis(value),
            2 => self.pad_right.0 = value,
            3 => self.pad_right.1 = invert_axis(value),
            4 => self.pad_triggers.0 = trigger(value),
            5 => self.pad_triggers.1 = trigger(value),
            _ => return,
        }
        self.submit_pad();
    }

    /// The remote drops a silent pad, so repeat the last snapshot.
    pub fn keep_pad(&mut self) {
        if self.pad_sent.is_none_or(|sent| sent.elapsed() >= Duration::from_millis(100)) {
            self.submit_pad();
        }
    }

    pub fn release_pad(&mut self) {
        self.pad_buttons = 0;
        self.pad_left = (0, 0);
        self.pad_right = (0, 0);
        self.pad_triggers = (0, 0);
        self.submit_pad();
    }

    pub fn mouse_absolute(&mut self, x: u16, y: u16, width: u16, height: u16) {
        if self.handle.is_null() || width == 0 || height == 0 {
            return;
        }
        unsafe { (self.api.submit_mouse_absolute)(self.handle, x, y, width, height) };
    }

    /// Buttons follow SDL numbering: 1 is left, 2 is middle, and 3 is right.
    pub fn mouse_button(&mut self, button: u8, pressed: bool) {
        if self.handle.is_null() {
            return;
        }
        unsafe { (self.api.submit_mouse_button)(self.handle, button, pressed) };
    }

    fn submit_pad(&mut self) {
        if self.handle.is_null() {
            return;
        }
        unsafe {
            (self.api.submit_gamepad)(
                self.handle,
                0,
                0x0101,
                self.pad_buttons,
                self.pad_triggers.0,
                self.pad_triggers.1,
                self.pad_left.0,
                self.pad_left.1,
                self.pad_right.0,
                self.pad_right.1,
            );
        }
        self.pad_sent = Some(Instant::now());
    }

    fn release_held(&mut self) {
        if let Some(held) = self.held.take() {
            unsafe { (self.api.release_frame)(held.pointer) };
        }
    }

    pub fn set_bitrate(&self, kbps: u32) -> Result<(), String> {
        self.send_json(&json!({
            "id": format!("bitrate-{kbps}"),
            "type": "bitrate",
            "maxBitrateKbps": kbps,
        }))
    }

    fn send_json(&self, value: &Value) -> Result<(), String> {
        let bytes = value.to_string();
        let status = unsafe { (self.api.send)(self.handle, bytes.as_ptr(), bytes.len()) };
        if status != 0 {
            Err(format!("The streamer rejected a command ({status})"))
        } else {
            Ok(())
        }
    }
}

impl Drop for NativeStream {
    fn drop(&mut self) {
        self.release_held();
        if self.handle.is_null() {
            return;
        }
        unsafe {
            // Graphics objects were created on this thread. Release them here,
            // before the worker is joined, or the Mali driver frees them twice.
            (self.api.scene_graph_shutdown)(self.handle);
            (self.api.destroy)(self.handle);
            self.handle = std::ptr::null_mut();
        }
    }
}

/// SDL reports stick-up as a negative Y. The stream expects the opposite.
fn invert_axis(value: i16) -> i16 {
    value.saturating_neg()
}

fn trigger(value: i16) -> u8 {
    let normalized = (value as f32 / 32767.0).clamp(0.0, 1.0);
    (normalized * 255.0).round() as u8
}

/// SDL game-controller buttons, or the RK3326 raw joystick layout.
/// These handhelds already use Switch labels: A east, B south, X north, Y west.
/// A mapped pad's SDL A/B/X/Y follow that, so Nintendo keeps them.
/// Xbox layout swaps the pairs.
fn button_mask(button: u8, raw_joystick: bool, xbox_layout: bool) -> Option<u16> {
    if raw_joystick {
        return Some(match button {
            1 => if xbox_layout { 0x2000 } else { 0x1000 },
            0 => if xbox_layout { 0x1000 } else { 0x2000 },
            2 => if xbox_layout { 0x8000 } else { 0x4000 },
            3 => if xbox_layout { 0x4000 } else { 0x8000 },
            12 => 0x0020, // Select
            13 => 0x0010, // Start
            14 => 0x0040, // Left stick click
            15 => 0x0080, // Right stick click
            8 => 0x0001,
            9 => 0x0002,
            10 => 0x0004,
            11 => 0x0008,
            _ => return None,
        });
    }
    Some(match button {
        0 => if xbox_layout { 0x2000 } else { 0x1000 },
        1 => if xbox_layout { 0x1000 } else { 0x2000 },
        2 => if xbox_layout { 0x8000 } else { 0x4000 },
        3 => if xbox_layout { 0x4000 } else { 0x8000 },
        4 => 0x0020,
        6 => 0x0010,
        7 => 0x0040,
        8 => 0x0080,
        9 => 0x0100,
        10 => 0x0200,
        11 => 0x0001,
        12 => 0x0002,
        13 => 0x0004,
        14 => 0x0008,
        _ => return None,
    })
}

unsafe extern "C" fn on_message(bytes: *const u8, length: usize, user: *mut c_void) {
    if bytes.is_null() || user.is_null() || length == 0 {
        return;
    }
    let text = unsafe { std::slice::from_raw_parts(bytes, length) };
    let Ok(value) = serde_json::from_slice::<Value>(text) else {
        return;
    };
    let sender = unsafe { &*(user as *const Sender<Value>) };
    let _ = sender.send(value);
}

unsafe extern "C" fn on_frame(_user: *mut c_void) {}

/// Letterboxed rectangle for a frame inside the design canvas.
pub fn fitted_rect(frame_width: u32, frame_height: u32, canvas: (f32, f32)) -> (f32, f32, f32, f32) {
    let frame_width = frame_width.max(1) as f32;
    let frame_height = frame_height.max(1) as f32;
    let scale = (canvas.0 / frame_width).min(canvas.1 / frame_height);
    let width = frame_width * scale;
    let height = frame_height * scale;
    ((canvas.0 - width) / 2.0, (canvas.1 - height) / 2.0, width, height)
}

pub fn message_text(value: &Value) -> Option<String> {
    value["message"].as_str().filter(|text| !text.is_empty()).map(str::to_owned).or_else(|| {
        value["error"].as_str().map(str::to_owned)
    })
}

pub fn is_error(value: &Value) -> bool {
    value["type"].as_str() == Some("error")
}

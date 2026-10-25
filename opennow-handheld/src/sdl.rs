//! Runtime-loaded system SDL2. It drives the panel through KMSDRM and the Mali
//! EGL driver, so no window manager is involved. `OPENNOW_SDL2` can point at
//! another build. Controller mappings come from PortMaster's `get_controls`.

use libloading::Library;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

static GL_RESOLVER: AtomicUsize = AtomicUsize::new(0);

pub fn gl_resolver() -> Option<unsafe extern "C" fn(*const c_char) -> *mut c_void> {
    let pointer = GL_RESOLVER.load(Ordering::Acquire);
    if pointer == 0 {
        None
    } else {
        Some(unsafe { std::mem::transmute(pointer) })
    }
}

const INIT_VIDEO: u32 = 0x20;
const INIT_JOYSTICK: u32 = 0x200;
const INIT_GAMECONTROLLER: u32 = 0x2000;
const INIT_EVENTS: u32 = 0x4000;

const WINDOW_FULLSCREEN: u32 = 0x1;
const WINDOW_OPENGL: u32 = 0x2;
const WINDOW_SHOWN: u32 = 0x4;
const WINDOW_RESIZABLE: u32 = 0x20;
const WINDOWPOS_CENTERED: c_int = 0x2FFF_0000;

const GL_DOUBLEBUFFER: c_int = 5;
const GL_CONTEXT_MAJOR_VERSION: c_int = 17;
const GL_CONTEXT_MINOR_VERSION: c_int = 18;
const GL_CONTEXT_PROFILE_MASK: c_int = 21;
const GL_CONTEXT_PROFILE_COMPATIBILITY: c_int = 2;
const GL_CONTEXT_PROFILE_ES: c_int = 4;

const EVENT_QUIT: u32 = 0x100;
const EVENT_KEYDOWN: u32 = 0x300;
const EVENT_JOYAXISMOTION: u32 = 0x600;
const EVENT_JOYHATMOTION: u32 = 0x602;
const EVENT_JOYBUTTONDOWN: u32 = 0x603;
const EVENT_JOYBUTTONUP: u32 = 0x604;
const EVENT_JOYDEVICEADDED: u32 = 0x605;
const EVENT_CONTROLLERAXISMOTION: u32 = 0x650;
const EVENT_CONTROLLERBUTTONDOWN: u32 = 0x651;
const EVENT_CONTROLLERBUTTONUP: u32 = 0x652;

#[repr(C)]
#[derive(Default)]
struct DisplayMode {
    format: u32,
    w: c_int,
    h: c_int,
    refresh_rate: c_int,
    driver_data: usize,
}

#[repr(C, align(8))]
struct RawEvent {
    bytes: [u8; 56],
}

impl RawEvent {
    fn u32_at(&self, offset: usize) -> u32 {
        u32::from_ne_bytes(self.bytes[offset..offset + 4].try_into().unwrap())
    }
    fn i32_at(&self, offset: usize) -> i32 {
        self.u32_at(offset) as i32
    }
    fn i16_at(&self, offset: usize) -> i16 {
        i16::from_ne_bytes(self.bytes[offset..offset + 2].try_into().unwrap())
    }
}

pub enum Event {
    Quit,
    Key(i32),
    ControllerButton {
        button: u8,
        pressed: bool,
    },
    ControllerAxis {
        axis: u8,
        value: i16,
    },
    JoyButton {
        which: i32,
        button: u8,
        pressed: bool,
    },
    JoyHat {
        which: i32,
        value: u8,
    },
    JoyAxis {
        which: i32,
        axis: u8,
        value: i16,
    },
    Other,
}

pub struct Sdl {
    init: unsafe extern "C" fn(u32) -> c_int,
    quit: unsafe extern "C" fn(),
    get_error: unsafe extern "C" fn() -> *const c_char,
    current_video_driver: unsafe extern "C" fn() -> *const c_char,
    desktop_display_mode: unsafe extern "C" fn(c_int, *mut DisplayMode) -> c_int,
    gl_set_attribute: unsafe extern "C" fn(c_int, c_int) -> c_int,
    create_window:
        unsafe extern "C" fn(*const c_char, c_int, c_int, c_int, c_int, u32) -> *mut c_void,
    destroy_window: unsafe extern "C" fn(*mut c_void),
    show_cursor: unsafe extern "C" fn(c_int) -> c_int,
    gl_create_context: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    gl_delete_context: unsafe extern "C" fn(*mut c_void),
    gl_make_current: unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int,
    gl_set_swap_interval: unsafe extern "C" fn(c_int) -> c_int,
    gl_swap_window: unsafe extern "C" fn(*mut c_void),
    gl_get_proc_address: unsafe extern "C" fn(*const c_char) -> *mut c_void,
    gl_get_drawable_size: unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int),
    poll_event: unsafe extern "C" fn(*mut RawEvent) -> c_int,
    is_game_controller: unsafe extern "C" fn(c_int) -> c_int,
    game_controller_open: unsafe extern "C" fn(c_int) -> *mut c_void,
    game_controller_name: unsafe extern "C" fn(*mut c_void) -> *const c_char,
    game_controller_joystick: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    joystick_open: unsafe extern "C" fn(c_int) -> *mut c_void,
    joystick_name: unsafe extern "C" fn(*mut c_void) -> *const c_char,
    joystick_num_buttons: unsafe extern "C" fn(*mut c_void) -> c_int,
    joystick_instance_id: unsafe extern "C" fn(*mut c_void) -> i32,
    game_controller_mapping: Option<unsafe extern "C" fn(*mut c_void) -> *mut c_char>,
    sdl_free: Option<unsafe extern "C" fn(*mut c_void)>,
    mapped_pads: Vec<MappedPad>,
    _library: Library,
}

struct MappedPad {
    instance: i32,
    /// Raw buttons the SDL mapping already reports as stick clicks.
    /// `None` means that mapping string could not be read.
    stick_clicks: Option<(Option<u8>, Option<u8>)>,
}

pub struct Window {
    window: *mut c_void,
    context: *mut c_void,
    pub width: i32,
    pub height: i32,
}

fn symbol<T: Copy>(library: &Library, name: &str) -> Result<T, String> {
    unsafe { library.get::<T>(name.as_bytes()) }
        .map(|symbol| *symbol)
        .map_err(|error| format!("SDL2 is missing {name}: {error}"))
}

fn optional_symbol<T: Copy>(library: &Library, name: &str) -> Option<T> {
    unsafe { library.get::<T>(name.as_bytes()) }
        .ok()
        .map(|symbol| *symbol)
}

fn parse_stick_clicks(mapping: &str) -> (Option<u8>, Option<u8>) {
    let mut left = None;
    let mut right = None;
    for part in mapping.split(',') {
        let Some((name, value)) = part.split_once(':') else {
            continue;
        };
        let Some(button) = value
            .strip_prefix('b')
            .and_then(|text| text.parse::<u8>().ok())
        else {
            continue;
        };
        match name {
            "leftstick" => left = Some(button),
            "rightstick" => right = Some(button),
            _ => {}
        }
    }
    (left, right)
}

fn text(pointer: *const c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned()
}

impl Sdl {
    pub fn load() -> Result<Self, String> {
        let mut names = Vec::new();
        if let Ok(path) = std::env::var("OPENNOW_SDL2") {
            names.push(path);
        }
        if cfg!(windows) {
            names.push("SDL2.dll".into());
        } else {
            names.extend([
                "libSDL2-2.0.so.0".into(),
                "libSDL2-2.0.so".into(),
                "libSDL2.so".into(),
            ]);
        }
        let mut failures = Vec::new();
        let library = names
            .iter()
            .find_map(|name| match unsafe { Library::new(name) } {
                Ok(library) => Some(library),
                Err(error) => {
                    failures.push(format!("{name}: {error}"));
                    None
                }
            })
            .ok_or_else(|| format!("could not load SDL2 ({})", failures.join("; ")))?;
        Ok(Self {
            init: symbol(&library, "SDL_Init")?,
            quit: symbol(&library, "SDL_Quit")?,
            get_error: symbol(&library, "SDL_GetError")?,
            current_video_driver: symbol(&library, "SDL_GetCurrentVideoDriver")?,
            desktop_display_mode: symbol(&library, "SDL_GetDesktopDisplayMode")?,
            gl_set_attribute: symbol(&library, "SDL_GL_SetAttribute")?,
            create_window: symbol(&library, "SDL_CreateWindow")?,
            destroy_window: symbol(&library, "SDL_DestroyWindow")?,
            show_cursor: symbol(&library, "SDL_ShowCursor")?,
            gl_create_context: symbol(&library, "SDL_GL_CreateContext")?,
            gl_delete_context: symbol(&library, "SDL_GL_DeleteContext")?,
            gl_make_current: symbol(&library, "SDL_GL_MakeCurrent")?,
            gl_set_swap_interval: symbol(&library, "SDL_GL_SetSwapInterval")?,
            gl_swap_window: symbol(&library, "SDL_GL_SwapWindow")?,
            gl_get_proc_address: {
                let resolver = symbol(&library, "SDL_GL_GetProcAddress")?;
                GL_RESOLVER.store(resolver as usize, Ordering::Release);
                resolver
            },
            gl_get_drawable_size: symbol(&library, "SDL_GL_GetDrawableSize")?,
            poll_event: symbol(&library, "SDL_PollEvent")?,
            is_game_controller: symbol(&library, "SDL_IsGameController")?,
            game_controller_open: symbol(&library, "SDL_GameControllerOpen")?,
            game_controller_name: symbol(&library, "SDL_GameControllerName")?,
            game_controller_joystick: symbol(&library, "SDL_GameControllerGetJoystick")?,
            joystick_open: symbol(&library, "SDL_JoystickOpen")?,
            joystick_name: symbol(&library, "SDL_JoystickName")?,
            joystick_num_buttons: symbol(&library, "SDL_JoystickNumButtons")?,
            joystick_instance_id: symbol(&library, "SDL_JoystickInstanceID")?,
            game_controller_mapping: optional_symbol(&library, "SDL_GameControllerMapping"),
            sdl_free: optional_symbol(&library, "SDL_free"),
            mapped_pads: Vec::new(),
            _library: library,
        })
    }

    pub fn error(&self) -> String {
        text(unsafe { (self.get_error)() })
    }

    pub fn init(&self) -> Result<(), String> {
        // Face buttons stay in Xbox positions (A south, B east). The Nintendo
        // layout then swaps them onto the labels printed on the handheld.
        // SDL's label hint would swap them first and undo that default.
        // Called once on this thread before SDL or any other thread starts.
        unsafe { std::env::set_var("SDL_GAMECONTROLLER_USE_BUTTON_LABELS", "0") };
        let flags = INIT_VIDEO | INIT_JOYSTICK | INIT_GAMECONTROLLER | INIT_EVENTS;
        if unsafe { (self.init)(flags) } != 0 {
            return Err(format!("SDL_Init failed: {}", self.error()));
        }
        Ok(())
    }

    pub fn video_driver(&self) -> String {
        text(unsafe { (self.current_video_driver)() })
    }

    /// Creates the window and an OpenGL ES 3 context, which the video
    /// presenter needs. ES 2, then desktop OpenGL 2.1, are the fallbacks; the
    /// interface shaders run on all three.
    pub fn create_gl_window(
        &self,
        title: &CStr,
        windowed: Option<(i32, i32)>,
    ) -> Result<Window, String> {
        let mut mode = DisplayMode::default();
        if unsafe { (self.desktop_display_mode)(0, &mut mode) } != 0 {
            return Err(format!("no display mode: {}", self.error()));
        }
        unsafe {
            (self.gl_set_attribute)(GL_CONTEXT_PROFILE_MASK, GL_CONTEXT_PROFILE_ES);
            (self.gl_set_attribute)(GL_CONTEXT_MAJOR_VERSION, 3);
            (self.gl_set_attribute)(GL_CONTEXT_MINOR_VERSION, 0);
            (self.gl_set_attribute)(GL_DOUBLEBUFFER, 1);
        }
        let (width, height, flags) = match windowed {
            Some((width, height)) => (
                width,
                height,
                WINDOW_OPENGL | WINDOW_SHOWN | WINDOW_RESIZABLE,
            ),
            None => (
                mode.w,
                mode.h,
                WINDOW_FULLSCREEN | WINDOW_OPENGL | WINDOW_SHOWN,
            ),
        };
        let window = unsafe {
            (self.create_window)(
                title.as_ptr(),
                WINDOWPOS_CENTERED,
                WINDOWPOS_CENTERED,
                width,
                height,
                flags,
            )
        };
        if window.is_null() {
            return Err(format!("SDL_CreateWindow failed: {}", self.error()));
        }
        let mut context = unsafe { (self.gl_create_context)(window) };
        if context.is_null() {
            let es3_error = self.error();
            eprintln!("OpenGL ES 3 context failed ({es3_error}); trying OpenGL ES 2");
            unsafe {
                (self.gl_set_attribute)(GL_CONTEXT_MAJOR_VERSION, 2);
                (self.gl_set_attribute)(GL_CONTEXT_MINOR_VERSION, 0);
                context = (self.gl_create_context)(window);
            }
        }
        if context.is_null() {
            let es_error = self.error();
            eprintln!("OpenGL ES 2 context failed ({es_error}); trying OpenGL 2.1");
            unsafe {
                (self.gl_set_attribute)(GL_CONTEXT_PROFILE_MASK, GL_CONTEXT_PROFILE_COMPATIBILITY);
                (self.gl_set_attribute)(GL_CONTEXT_MAJOR_VERSION, 2);
                (self.gl_set_attribute)(GL_CONTEXT_MINOR_VERSION, 1);
                context = (self.gl_create_context)(window);
            }
            if context.is_null() {
                let error = self.error();
                unsafe { (self.destroy_window)(window) };
                return Err(format!("OpenGL context failed: {error}"));
            }
        }
        unsafe {
            (self.gl_make_current)(window, context);
            (self.show_cursor)(0);
        }
        let (mut width, mut height) = (0, 0);
        unsafe { (self.gl_get_drawable_size)(window, &mut width, &mut height) };
        if width <= 0 || height <= 0 {
            (width, height) = (mode.w, mode.h);
        }
        eprintln!(
            "display: {}x{} @ {} Hz, drawable {width}x{height}",
            mode.w, mode.h, mode.refresh_rate
        );
        Ok(Window {
            window,
            context,
            width,
            height,
        })
    }

    pub fn set_vsync(&self, enabled: bool) -> bool {
        let interval = if enabled { 1 } else { 0 };
        unsafe { (self.gl_set_swap_interval)(interval) == 0 && enabled }
    }

    pub fn refresh_drawable_size(&self, window: &mut Window) -> bool {
        let (mut width, mut height) = (0, 0);
        unsafe { (self.gl_get_drawable_size)(window.window, &mut width, &mut height) };
        if width <= 0 || height <= 0 || (width, height) == (window.width, window.height) {
            return false;
        }
        window.width = width;
        window.height = height;
        true
    }

    pub fn swap(&self, window: &Window) {
        unsafe { (self.gl_swap_window)(window.window) };
    }

    /// DRM file descriptor for the KMSDRM window, shared with the streamer's
    /// overlay plane. Other video drivers have no plane to borrow.
    pub fn drm_fd(&self, window: &Window) -> Option<i32> {
        #[repr(C)]
        struct Version {
            major: u8,
            minor: u8,
            patch: u8,
        }
        #[repr(C)]
        struct WmInfo {
            major: u8,
            minor: u8,
            patch: u8,
            _pad: u8,
            subsystem: i32,
            dev_index: i32,
            drm_fd: i32,
            _rest: [u8; 192],
        }
        const KMSDRM: i32 = 13;
        let get_version = unsafe {
            self._library
                .get::<unsafe extern "C" fn(*mut Version)>(b"SDL_GetVersion\0")
                .ok()?
        };
        let get_wm = unsafe {
            self._library
                .get::<unsafe extern "C" fn(*mut c_void, *mut WmInfo) -> c_int>(
                    b"SDL_GetWindowWMInfo\0",
                )
                .ok()?
        };
        let mut info = WmInfo {
            major: 0,
            minor: 0,
            patch: 0,
            _pad: 0,
            subsystem: 0,
            dev_index: 0,
            drm_fd: -1,
            _rest: [0; 192],
        };
        let mut version = Version {
            major: 0,
            minor: 0,
            patch: 0,
        };
        unsafe { get_version(&mut version) };
        info.major = version.major;
        info.minor = version.minor;
        info.patch = version.patch;
        let ok = unsafe { get_wm(window.window, &mut info) };
        if ok == 0 || info.subsystem != KMSDRM || info.drm_fd < 0 {
            None
        } else {
            Some(info.drm_fd)
        }
    }

    pub fn gl_proc(&self, name: &str) -> *mut c_void {
        debug_assert!(name.ends_with('\0'));
        unsafe { (self.gl_get_proc_address)(name.as_ptr().cast()) }
    }

    pub fn destroy(&self, window: Window) {
        unsafe {
            (self.gl_delete_context)(window.context);
            (self.destroy_window)(window.window);
        }
    }

    /// Opens a newly attached device as a game controller when SDL has a
    /// mapping for it, otherwise as a raw joystick. The bool is whether
    /// physical button 16 (FN) exists on that pad.
    pub fn open_device(&mut self, index: i32) -> (String, bool) {
        unsafe {
            if (self.is_game_controller)(index) != 0 {
                let controller = (self.game_controller_open)(index);
                if !controller.is_null() {
                    let joystick = (self.game_controller_joystick)(controller);
                    let instance = (self.joystick_instance_id)(joystick);
                    let mapping = self.controller_mapping(controller);
                    let stick_clicks = mapping.as_deref().map(parse_stick_clicks);
                    let name = text((self.game_controller_name)(controller));
                    eprintln!(
                        "pad: {name} config {}",
                        mapping.as_deref().unwrap_or("none")
                    );
                    self.mapped_pads.push(MappedPad {
                        instance,
                        stick_clicks,
                    });
                    let buttons = (self.joystick_num_buttons)(joystick);
                    return (name, buttons > 16);
                }
            }
            let joystick = (self.joystick_open)(index);
            if joystick.is_null() {
                return (format!("device {index} failed: {}", self.error()), false);
            }
            let buttons = (self.joystick_num_buttons)(joystick);
            let name = text((self.joystick_name)(joystick));
            eprintln!("pad: {name} config none");
            (format!("{name} (no SDL mapping)"), buttons > 16)
        }
    }

    fn is_mapped(&self, which: i32) -> bool {
        self.mapped_pads.iter().any(|pad| pad.instance == which)
    }

    /// Face buttons and sticks already arrive through the controller API.
    /// Select, Start, and FN stay raw. These boards click the sticks with
    /// buttons 14 and 15. PortMaster's mapping already names those, so they
    /// are left to the controller events. System SDL, used when PortMaster is
    /// not installed, often names different buttons or none, so the raw clicks
    /// are sent as well.
    fn forward_raw_button(&self, which: i32, button: u8) -> bool {
        let Some(pad) = self.mapped_pads.iter().find(|pad| pad.instance == which) else {
            return true;
        };
        if matches!(button, 12 | 13 | 16) {
            return true;
        }
        if !matches!(button, 14 | 15) {
            return false;
        }
        let Some((left, right)) = pad.stick_clicks else {
            return true;
        };
        left != Some(button) && right != Some(button)
    }

    fn controller_mapping(&self, controller: *mut c_void) -> Option<String> {
        let (Some(mapping), Some(free)) = (self.game_controller_mapping, self.sdl_free) else {
            return None;
        };
        let pointer = unsafe { mapping(controller) };
        if pointer.is_null() {
            return None;
        }
        let text = unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned();
        unsafe { free(pointer.cast()) };
        Some(text)
    }

    pub fn poll(&self) -> Option<(Event, Option<i32>)> {
        let mut event = RawEvent { bytes: [0; 56] };
        if unsafe { (self.poll_event)(&mut event) } == 0 {
            return None;
        }
        let kind = event.u32_at(0);
        let which = event.i32_at(8);
        let parsed = match kind {
            EVENT_QUIT => Event::Quit,
            EVENT_JOYDEVICEADDED => return Some((Event::Other, Some(which))),
            EVENT_KEYDOWN if event.bytes[13] == 0 => Event::Key(event.i32_at(20)),
            EVENT_CONTROLLERAXISMOTION => Event::ControllerAxis {
                axis: event.bytes[12],
                value: event.i16_at(16),
            },
            // Select and Start stay on raw buttons 12 and 13. The PortMaster
            // map also names them, and forwarding both presses each one twice.
            EVENT_CONTROLLERBUTTONDOWN | EVENT_CONTROLLERBUTTONUP
                if matches!(event.bytes[12], 4 | 6) =>
            {
                Event::Other
            }
            EVENT_CONTROLLERBUTTONDOWN | EVENT_CONTROLLERBUTTONUP => Event::ControllerButton {
                button: event.bytes[12],
                pressed: kind == EVENT_CONTROLLERBUTTONDOWN,
            },
            EVENT_JOYBUTTONDOWN | EVENT_JOYBUTTONUP
                if self.forward_raw_button(which, event.bytes[12]) =>
            {
                Event::JoyButton {
                    which,
                    button: event.bytes[12],
                    pressed: kind == EVENT_JOYBUTTONDOWN,
                }
            }
            EVENT_JOYHATMOTION if !self.is_mapped(which) => Event::JoyHat {
                which,
                value: event.bytes[13],
            },
            EVENT_JOYAXISMOTION if !self.is_mapped(which) => Event::JoyAxis {
                which,
                axis: event.bytes[12],
                value: event.i16_at(16),
            },
            _ if self.is_mapped(which) => Event::Other,
            _ => Event::Other,
        };
        Some((parsed, None))
    }
}

impl Drop for Sdl {
    fn drop(&mut self) {
        unsafe { (self.quit)() };
    }
}

#[cfg(test)]
mod tests {
    use super::parse_stick_clicks;

    #[test]
    fn stick_clicks_come_from_the_mapping_buttons() {
        let mapping = "GO-Super Gamepad,a:b1,b:b0,leftstick:b10,rightstick:b11,dpup:h0.1,platform:Linux";
        assert_eq!(parse_stick_clicks(mapping), (Some(10), Some(11)));
        let handheld = "a:b0,b:b1,leftstick:b14,rightstick:b15,platform:Linux";
        assert_eq!(parse_stick_clicks(handheld), (Some(14), Some(15)));
        assert_eq!(parse_stick_clicks("a:b0,b:b1"), (None, None));
    }
}

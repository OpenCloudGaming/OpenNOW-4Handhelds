//! Runtime-loaded SDL2. Handheld ports use PortMaster's libSDL2 when
//! `OPENNOW_SDL2` points at it. dArkOS otherwise uses the system SDL2, which
//! drives the panel through KMSDRM and the Mali EGL driver, so no window
//! manager is involved.

use libloading::Library;
use std::ffi::{CStr, c_char, c_int, c_void};

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
    controller_joysticks: Vec<i32>,
    _library: Library,
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
            gl_get_proc_address: symbol(&library, "SDL_GL_GetProcAddress")?,
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
            controller_joysticks: Vec::new(),
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
                    self.controller_joysticks
                        .push((self.joystick_instance_id)(joystick));
                    let buttons = (self.joystick_num_buttons)(joystick);
                    return (text((self.game_controller_name)(controller)), buttons > 16);
                }
            }
            let joystick = (self.joystick_open)(index);
            if joystick.is_null() {
                return (format!("device {index} failed: {}", self.error()), false);
            }
            let buttons = (self.joystick_num_buttons)(joystick);
            (
                format!("{} (no SDL mapping)", text((self.joystick_name)(joystick))),
                buttons > 16,
            )
        }
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
            EVENT_CONTROLLERBUTTONDOWN | EVENT_CONTROLLERBUTTONUP => Event::ControllerButton {
                button: event.bytes[12],
                pressed: kind == EVENT_CONTROLLERBUTTONDOWN,
            },
            // A mapped pad already reports face buttons, the D-pad, and the
            // sticks through the controller API. Forwarding the raw copies
            // presses each one twice. Xbox layout sends those two copies to
            // different face buttons, so both A and B fire. Select, Start, and
            // FN stay raw: this port does not take those from the controller map.
            EVENT_JOYBUTTONDOWN | EVENT_JOYBUTTONUP
                if !self.controller_joysticks.contains(&which)
                    || matches!(event.bytes[12], 12 | 13 | 16) =>
            {
                Event::JoyButton {
                    which,
                    button: event.bytes[12],
                    pressed: kind == EVENT_JOYBUTTONDOWN,
                }
            }
            EVENT_JOYHATMOTION if !self.controller_joysticks.contains(&which) => Event::JoyHat {
                which,
                value: event.bytes[13],
            },
            EVENT_JOYAXISMOTION if !self.controller_joysticks.contains(&which) => Event::JoyAxis {
                which,
                axis: event.bytes[12],
                value: event.i16_at(16),
            },
            _ if self.controller_joysticks.contains(&which) => Event::Other,
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

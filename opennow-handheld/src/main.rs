//! OpenNOW handheld front end. It brings up a fullscreen OpenGL ES 2 surface
//! through the system SDL2 (KMSDRM on dArkOS, no window manager), runs the
//! OpenNOW core for sign-in and the library, and draws the console screens.
//! Streaming is not wired in yet.

mod app;
mod artwork;
mod assets;
mod core_client;
mod gl;
mod home;
mod library;
mod sdl;
mod settings;
mod signin;
mod store;
mod stream;
mod system;
mod text;
mod theme;

use app::App;
use gl::{Gl, Painter};
use home::{Action, Status};
use sdl::{Event, Sdl};
use std::time::{Duration, Instant};

const AXIS_THRESHOLD: i16 = 16_000;
const REPEAT_DELAY: Duration = Duration::from_millis(320);
const REPEAT_RATE: Duration = Duration::from_millis(80);

/// Held menu directions. The first press is delivered immediately; keeping the
/// D-pad or stick held repeats after a short pause.
struct NavRepeat {
    /// Up, down, left, right from the D-pad buttons.
    dpad: [bool; 4],
    /// Up, down, left, right from a joystick hat.
    hat: [bool; 4],
    stick_x: i8,
    stick_y: i8,
    next: Instant,
}

impl NavRepeat {
    fn new() -> Self {
        Self { dpad: [false; 4], hat: [false; 4], stick_x: 0, stick_y: 0, next: Instant::now() }
    }

    fn set_button(&mut self, index: usize, held: bool) {
        if self.dpad[index] == held {
            return;
        }
        let idle = !self.any();
        self.dpad[index] = held;
        if held && idle {
            self.next = Instant::now() + REPEAT_DELAY;
        }
    }

    fn set_hat(&mut self, value: u8) {
        let next = [value & 1 != 0, value & 4 != 0, value & 8 != 0, value & 2 != 0];
        if self.hat == next {
            return;
        }
        let idle = !self.any();
        let pressed = next.iter().any(|held| *held);
        self.hat = next;
        if pressed && idle {
            self.next = Instant::now() + REPEAT_DELAY;
        }
    }

    fn sync_stick(&mut self, x: i8, y: i8) {
        if self.stick_x == x && self.stick_y == y {
            return;
        }
        let idle = !self.any();
        self.stick_x = x;
        self.stick_y = y;
        if idle && self.any() {
            self.next = Instant::now() + REPEAT_DELAY;
        }
    }

    fn tick(&mut self, now: Instant) -> [Option<Action>; 4] {
        let mut actions = [None; 4];
        if !self.any() || now < self.next {
            return actions;
        }
        self.next = now + REPEAT_RATE;
        let dirs = [Action::Up, Action::Down, Action::Left, Action::Right];
        let mut count = 0;
        for (index, action) in dirs.into_iter().enumerate() {
            if self.held(index) && !self.held(index ^ 1) {
                actions[count] = Some(action);
                count += 1;
            }
        }
        actions
    }

    fn any(&self) -> bool {
        (0..4).any(|index| self.held(index))
    }

    fn held(&self, index: usize) -> bool {
        self.dpad[index] || self.hat[index] || match index {
            0 => self.stick_y < 0,
            1 => self.stick_y > 0,
            2 => self.stick_x < 0,
            3 => self.stick_x > 0,
            _ => false,
        }
    }
}

fn direction_index(button: u8, raw_joystick: bool) -> Option<usize> {
    if raw_joystick {
        return match button {
            8 => Some(0),
            9 => Some(1),
            10 => Some(2),
            11 => Some(3),
            _ => None,
        };
    }
    match button {
        11 => Some(0),
        12 => Some(1),
        13 => Some(2),
        14 => Some(3),
        _ => None,
    }
}

/// Turns analog stick motion into single D-pad steps.
#[derive(Default)]
struct Stick {
    x: i8,
    y: i8,
}

impl Stick {
    fn step(&mut self, axis: u8, value: i16) -> Option<Action> {
        let direction = if value <= -AXIS_THRESHOLD {
            -1
        } else if value >= AXIS_THRESHOLD {
            1
        } else {
            0
        };
        let (state, negative, positive) = match axis {
            0 => (&mut self.x, Action::Left, Action::Right),
            1 => (&mut self.y, Action::Up, Action::Down),
            _ => return None,
        };
        if *state == direction {
            return None;
        }
        *state = direction;
        match direction {
            -1 => Some(negative),
            1 => Some(positive),
            _ => None,
        }
    }
}

fn key_action(symbol: i32) -> Option<Action> {
    match symbol {
        0x4000_0052 => Some(Action::Up),
        0x4000_0051 => Some(Action::Down),
        0x4000_0050 => Some(Action::Left),
        0x4000_004F => Some(Action::Right),
        13 | 32 => Some(Action::Confirm),
        8 => Some(Action::Back),
        120 => Some(Action::Options),
        27 => Some(Action::Escape),
        _ => None,
    }
}

fn controller_action(button: u8, pressed: bool, xbox_layout: bool) -> Option<Action> {
    // Nintendo: A east, B south, X top. Xbox: A south, B east, X on the printed Y.
    let (confirm, back, options) = if xbox_layout { (0, 1, 3) } else { (1, 0, 2) };
    match (button, pressed) {
        (button, true) if button == confirm => Some(Action::Confirm),
        (button, true) if button == back => Some(Action::Back),
        (button, true) if button == options => Some(Action::Options),
        (4, held) => Some(Action::Select(held)),
        (6, held) => Some(Action::Start(held)),
        (11, true) => Some(Action::Up),
        (12, true) => Some(Action::Down),
        (13, true) => Some(Action::Left),
        (14, true) => Some(Action::Right),
        _ => None,
    }
}

/// Fallback for pads SDL has no mapping for, using the common RK3326 joypad
/// layout. The System panel shows raw indices when a device differs.
fn raw_button_action(button: u8, pressed: bool, xbox_layout: bool) -> Option<Action> {
    // Raw indices already follow the printed Nintendo labels. Xbox swaps them.
    let (confirm, back, options) = if xbox_layout { (0, 1, 3) } else { (1, 0, 2) };
    match (button, pressed) {
        (button, true) if button == confirm => Some(Action::Confirm),
        (button, true) if button == back => Some(Action::Back),
        (button, true) if button == options => Some(Action::Options),
        (8, true) => Some(Action::Up),
        (9, true) => Some(Action::Down),
        (10, true) => Some(Action::Left),
        (11, true) => Some(Action::Right),
        (12, held) => Some(Action::Select(held)),
        (13, held) => Some(Action::Start(held)),
        _ => None,
    }
}

fn hat_action(value: u8) -> Option<Action> {
    match value {
        1 => Some(Action::Up),
        4 => Some(Action::Down),
        8 => Some(Action::Left),
        2 => Some(Action::Right),
        _ => None,
    }
}

/// Maps an event to an action and records it for the System panel and log.
/// Returns `None` for the window-close request, which quits at once.
fn translate(event: Event, stick: &mut Stick, repeat: &mut NavRepeat, status: &mut Status, xbox_layout: bool) -> Option<Option<Action>> {
    let action = match event {
        Event::Quit => return None,
        Event::Key(symbol) => {
            record(status, format!("Key {symbol}"));
            key_action(symbol)
        }
        Event::ControllerButton { button, pressed } => {
            if let Some(index) = direction_index(button, false) {
                repeat.set_button(index, pressed);
            }
            if pressed {
                record(status, format!("Controller button {button}"));
            }
            controller_action(button, pressed, xbox_layout)
        }
        Event::JoyButton { which, button, pressed } => {
            if let Some(index) = direction_index(button, true) {
                repeat.set_button(index, pressed);
            }
            if pressed {
                record(status, format!("Joystick {which} button {button}"));
            }
            raw_button_action(button, pressed, xbox_layout)
        }
        Event::JoyHat { which, value } => {
            repeat.set_hat(value);
            record(status, format!("Joystick {which} hat {value}"));
            hat_action(value)
        }
        Event::ControllerAxis { axis, value } => {
            let action = stick.step(axis, value);
            repeat.sync_stick(stick.x, stick.y);
            action
        }
        Event::JoyAxis { which, axis, value } => {
            let action = stick.step(axis, value);
            repeat.sync_stick(stick.x, stick.y);
            if action.is_some() {
                record(status, format!("Joystick {which} axis {axis} {value}"));
            }
            action
        }
        Event::Other => None,
    };
    Some(action)
}

fn record(status: &mut Status, input: String) {
    eprintln!("input: {input}");
    status.last_input = input;
}

struct Options {
    limit: Option<Duration>,
    windowed: bool,
}

/// `--seconds N` quits after N seconds. Handhelds default to fullscreen and
/// desktops to a 960×720 window; `--fullscreen` and `--window` override that.
fn parse_options() -> Options {
    let mut options = Options { limit: None, windowed: !cfg!(target_os = "linux") };
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--seconds" => {
                options.limit = arguments.next().and_then(|value| value.parse().ok()).map(Duration::from_secs);
            }
            "--window" => options.windowed = true,
            "--fullscreen" => options.windowed = false,
            _ => eprintln!("ignoring unknown argument {argument}"),
        }
    }
    options
}

fn run() -> Result<(), String> {
    let options = parse_options();
    let mut sdl = Sdl::load()?;
    sdl.init()?;
    let driver = sdl.video_driver();
    eprintln!("video driver: {driver}");
    let window = sdl.create_gl_window(c"OpenNOW", options.windowed.then_some((960, 720)))?;
    let gl = Gl::load(|name| sdl.gl_proc(name))?;
    let renderer = gl.string(gl::RENDERER);
    eprintln!("gl: {} | {} | {}", gl.string(gl::VENDOR), renderer, gl.string(gl::VERSION));
    let mut painter = Painter::new(gl)?;
    let mut app = App::new();
    let mut vsync = sdl.set_vsync(app.vsync_enabled());
    eprintln!("vsync: {}", if app.vsync_enabled() { "on" } else { "off" });

    let mut status = Status {
        driver,
        resolution: format!("{} × {}", window.width, window.height),
        renderer,
        devices: Vec::new(),
        last_input: "None yet".into(),
    };
    let mut stick = Stick::default();
    let mut repeat = NavRepeat::new();
    let started = Instant::now();
    let mut last_frame = Instant::now();
    let (mut frames, mut window_start) = (0u32, Instant::now());

    'running: while !app.quit {
        while let Some((event, added)) = sdl.poll() {
            if let Some(index) = added {
                let (description, fn_button) = sdl.open_device(index);
                eprintln!("input: {description}");
                if fn_button {
                    eprintln!("input: FN is button 16");
                }
                app.note_fn_button(fn_button);
                status.devices.push(description);
            }
            match &event {
                Event::ControllerButton { button, pressed } => app.pad_button(*button, *pressed, false),
                Event::ControllerAxis { axis, value } => app.pad_axis(*axis, *value),
                Event::JoyButton { button, pressed, .. } => app.pad_button(*button, *pressed, true),
                Event::JoyAxis { axis, value, .. } => app.pad_axis(*axis, *value),
                _ => {}
            }
            match translate(event, &mut stick, &mut repeat, &mut status, app.xbox_layout()) {
                None => break 'running,
                Some(Some(action)) => app.act(action, &status),
                Some(None) => {}
            }
        }
        if app.repeats_navigation() {
            for action in repeat.tick(Instant::now()).into_iter().flatten() {
                app.act(action, &status);
            }
        }
        if options.limit.is_some_and(|limit| started.elapsed() >= limit) {
            break;
        }

        let now = Instant::now();
        app.update(now - last_frame, &mut painter);
        if let Some(enabled) = app.take_vsync_change() {
            vsync = sdl.set_vsync(enabled);
            eprintln!("vsync: {}", if enabled { "on" } else { "off" });
        }
        last_frame = now;
        painter.begin((window.width, window.height), theme::CANVAS, theme::BACKGROUND);
        app.draw(&mut painter, &status);
        painter.end();
        app.release_picture();
        sdl.swap(&window);
        if !vsync {
            std::thread::sleep(Duration::from_millis(16).saturating_sub(now.elapsed()));
        }

        frames += 1;
        if window_start.elapsed() >= Duration::from_secs(5) {
            eprintln!("fps: {:.1}", frames as f32 / window_start.elapsed().as_secs_f32());
            (frames, window_start) = (0, Instant::now());
        }
    }

    eprintln!("exiting after {:.1}s", started.elapsed().as_secs_f32());
    app.shutdown(&mut painter);
    drop(painter);
    sdl.destroy(window);
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("opennow-handheld: {error}");
        std::process::exit(1);
    }
}

//! Application flow: starts the core, restores or signs in an account, loads
//! its library, and routes input to the current screen. Quitting and signing
//! out always go through a confirmation dialog.

use crate::core_client::{self as core, Core, CoreError, Message};
use crate::gl::Painter;
use crate::home::{Action, Home, HomeView, LibraryStatus, Request, Status};
use crate::library::{Artwork, Game};
use crate::settings::{self, Profile};
use crate::signin::{self, Challenge, Phase};
use crate::store::{self, Store};
use crate::stream::{self, NativeStream};
use crate::system;
use crate::text::Weight;
use crate::theme::{self, *};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TOAST_TIME: Duration = Duration::from_millis(2200);
const LIBRARY_PAGE: u64 = 100;
/// Upper bound on games kept in memory on a 1 GB handheld.
const LIBRARY_LIMIT: usize = 2000;
/// Concurrent `artwork.resolve` requests; the core runs four background workers.
const ARTWORK_IN_FLIGHT: usize = 3;

enum Screen {
    Connecting,
    CoreFailed(String),
    SignIn,
    Home,
    Settings,
    Store,
    Stream,
}

#[derive(Clone, Copy)]
enum DialogKind {
    Quit,
    SignOut,
    AddToLibrary,
    EndStream,
}

struct Dialog {
    kind: DialogKind,
    confirm_selected: bool,
}

enum Pending {
    Hello,
    Session,
    SignInStart,
    SignInPoll,
    SignInComplete,
    SignInCancel,
    Library,
    Artwork(String),
    Logout,
    StorePage,
    Own(String),
    Launch,
    StreamSetting,
    StreamInspect,
    SessionCreate,
    SessionList,
    SessionClaim,
    SessionPoll,
    StreamPrepare,
    SessionStop,
    Regions,
    SettingsRead,
    RegionSet,
}

struct Region {
    name: String,
    url: String,
}

#[derive(Clone, Copy, PartialEq)]
enum PlayPhase {
    Idle,
    Settings,
    Inspect,
    Create,
    Claim,
    Poll,
    Attach,
    Live,
    Stop,
}

#[derive(Default)]
struct LibraryPage {
    traversal: String,
    cursor: String,
    revision: Value,
    context: Value,
    restarts: u32,
}

pub struct App {
    core: Option<Core>,
    pending: HashMap<u64, Pending>,
    screen: Screen,
    dialog: Option<Dialog>,
    /// Session menu drawn over the live stream. The picture stays up.
    session_menu: bool,
    /// 0 is Mouse, 1 is End session, 2 exits OpenNOW after the session stops.
    session_menu_index: u8,
    /// Sticks move a pointer and the right trigger clicks.
    mouse_mode: bool,
    mouse_x: f32,
    mouse_y: f32,
    mouse_down: bool,
    /// Select (SDL buttons 4 and 12) is held, for the fallback menu chord.
    pad_select: bool,
    /// X (SDL button 2) is held, for the fallback menu chord.
    pad_x: bool,
    /// The connected pad has physical button 16, the FN key.
    fn_button: bool,
    toast: Option<(String, Instant)>,
    start_held: bool,
    select_held: bool,
    clock: Clock,
    clock_checked: Option<Instant>,
    started: Instant,
    home: Home,
    account: String,
    games: Vec<Game>,
    library: LibraryStatus,
    page: LibraryPage,
    artwork: Artwork,
    store: Store,
    store_games: Vec<Game>,
    store_status: store::Status,
    store_cursor: String,
    store_seen: Vec<String>,
    store_has_more: bool,
    store_query: String,
    refresh_library: bool,
    scope: Value,
    pending_add: Option<(String, String)>,
    notice: Option<String>,
    stream: Option<NativeStream>,
    /// Last presented frame. The streamer only hands over a picture when a new
    /// one is decoded, and the shell must not paint its loading chrome in the gaps.
    stream_frame: Option<(u32, u32, u32)>,
    stream_capabilities: Option<Value>,
    cloud_session: Option<Value>,
    play: PlayPhase,
    play_settings_left: u8,
    play_message: String,
    launch_catalog: String,
    launch_variant: String,
    launch_title: String,
    play_deadline: Option<Instant>,
    next_session_poll: Option<Instant>,
    return_to_store: bool,
    sign_in: Phase,
    next_poll: Option<Instant>,
    profile: Profile,
    settings_index: usize,
    servers: Vec<Region>,
    /// 0 is Automatic. Later entries match `servers`.
    server_index: usize,
    region_url: String,
    region_provider: String,
    /// Lowest queue position seen for this session. Higher readings are other zones.
    queue_position: Option<i64>,
    vsync_apply: bool,
    /// Set when the session menu asks to leave OpenNOW. The process quits after session.stop finishes.
    quit_after_session: bool,
    pub quit: bool,
}

/// Prefers the game that was just launched, then any other running seat.
fn resumable_session(value: &Value, variant_id: &str) -> Option<Value> {
    let sessions = value["sessions"].as_array()?;
    let same_game = sessions.iter().find(|session| app_id_matches(session, variant_id));
    same_game.or_else(|| sessions.first()).cloned()
}

fn app_id_matches(session: &Value, variant_id: &str) -> bool {
    session["appId"].as_str() == Some(variant_id)
        || session["appId"].as_i64().is_some_and(|id| id.to_string() == variant_id)
        || session["appId"].as_u64().is_some_and(|id| id.to_string() == variant_id)
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|time| time.as_millis() as u64).unwrap_or(0)
}

fn data_dir() -> PathBuf {
    std::env::var_os("OPENNOW_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| core::executable_dir().join("data"))
}

impl App {
    pub fn new() -> Self {
        let mut app = Self {
            core: None,
            pending: HashMap::new(),
            screen: Screen::Connecting,
            dialog: None,
            session_menu: false,
            session_menu_index: 0,
            mouse_mode: false,
            mouse_x: CANVAS.0 / 2.0,
            mouse_y: CANVAS.1 / 2.0,
            mouse_down: false,
            pad_select: false,
            pad_x: false,
            fn_button: false,
            toast: None,
            start_held: false,
            select_held: false,
            clock: Clock::default(),
            clock_checked: None,
            started: Instant::now(),
            home: Home::new(),
            account: String::new(),
            games: Vec::new(),
            library: LibraryStatus::Loading,
            page: LibraryPage::default(),
            artwork: Artwork::new(),
            store: Store::new(),
            store_games: Vec::new(),
            store_status: store::Status::Loading,
            store_cursor: String::new(),
            store_seen: Vec::new(),
            store_has_more: true,
            store_query: String::new(),
            refresh_library: false,
            scope: Value::Null,
            pending_add: None,
            notice: None,
            stream: None,
            stream_frame: None,
            stream_capabilities: None,
            cloud_session: None,
            play: PlayPhase::Idle,
            play_settings_left: 0,
            play_message: String::new(),
            launch_catalog: String::new(),
            launch_variant: String::new(),
            launch_title: String::new(),
            play_deadline: None,
            next_session_poll: None,
            return_to_store: false,
            sign_in: Phase::Starting,
            next_poll: None,
            profile: settings::load(),
            settings_index: 0,
            servers: Vec::new(),
            server_index: 0,
            region_url: String::new(),
            region_provider: String::new(),
            queue_position: None,
            vsync_apply: false,
            quit_after_session: false,
            quit: false,
        };
        app.start_core();
        app
    }

    fn start_core(&mut self) {
        self.core = None;
        self.pending.clear();
        self.screen = Screen::Connecting;
        let binary = core::default_binary();
        let data = data_dir();
        eprintln!("core: starting {} with data in {}", binary.display(), data.display());
        match Core::spawn(&binary, &data) {
            Ok(mut core) => {
                let id = core.hello();
                self.pending.insert(id, Pending::Hello);
                self.core = Some(core);
            }
            Err(message) => self.screen = Screen::CoreFailed(message),
        }
    }

    fn send(&mut self, method: &str, params: Value, timeout: Duration, pending: Pending) {
        if let Some(core) = self.core.as_mut() {
            let id = core.request(method, params, timeout);
            self.pending.insert(id, pending);
        }
    }

    fn notify(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now()));
    }

    // Input

    pub fn act(&mut self, action: Action, status: &Status) {
        match action {
            Action::Start(held) => self.start_held = held,
            Action::Select(held) => self.select_held = held,
            _ => {}
        }
        if self.start_held && self.select_held && !matches!(self.screen, Screen::Stream) {
            self.quit = true;
            return;
        }
        if let Some(dialog) = self.dialog.as_mut() {
            match action {
                Action::Left | Action::Right => dialog.confirm_selected = !dialog.confirm_selected,
                Action::Confirm if dialog.confirm_selected => {
                    let kind = dialog.kind;
                    self.dialog = None;
                    self.confirmed(kind);
                }
                Action::Confirm | Action::Back | Action::Escape => {
                    self.dialog = None;
                    self.pending_add = None;
                }
                _ => {}
            }
            return;
        }
        if self.session_menu {
            match action {
                Action::Up => self.session_menu_index = if self.session_menu_index == 0 { 2 } else { self.session_menu_index - 1 },
                Action::Down => self.session_menu_index = (self.session_menu_index + 1) % 3,
                Action::Confirm => {
                    let choice = self.session_menu_index;
                    self.session_menu = false;
                    if let Some(stream) = self.stream.as_mut() {
                        stream.release_pad();
                    }
                    match choice {
                        0 => self.enter_mouse_mode(),
                        2 => self.exit_after_session(),
                        _ => self.open_dialog(DialogKind::EndStream),
                    }
                }
                Action::Back | Action::Escape => self.session_menu = false,
                _ => {}
            }
            return;
        }
        if self.notice.is_some() && matches!(action, Action::Confirm | Action::Back | Action::Escape) {
            self.notice = None;
            return;
        }
        match &self.screen {
            Screen::Home => {
                let view = HomeView {
                    games: &self.games,
                    artwork: &self.artwork,
                    library: &self.library,
                    account: &self.account,
                    status,
                    clock: &self.clock,
                    seconds: 0.0,
                };
                match self.home.act(action, &view) {
                    Some(Request::Quit) => self.open_dialog(DialogKind::Quit),
                    Some(Request::SignOut) => self.open_dialog(DialogKind::SignOut),
                    Some(Request::OpenStore) => self.open_store(),
                    Some(Request::OpenSettings) => self.open_settings(),
                    Some(Request::Play) => {
                        if let Some(index) = self.home.selected_game(self.games.len()) {
                            let game = &self.games[index];
                            self.begin_stream(&game.id.clone(), &game.variant_id.clone(), &game.title.clone(), false);
                        }
                    }
                    Some(Request::RetryLibrary) => self.load_library(),
                    Some(Request::Toast(message)) => self.notify(message),
                    None => {}
                }
            }
            Screen::Settings => self.act_settings(action),
            Screen::Store => self.act_store(action),
            Screen::Stream => {
                if matches!(action, Action::Escape) {
                    if let Some(stream) = self.stream.as_mut() {
                        stream.release_pad();
                    }
                    self.open_dialog(DialogKind::EndStream);
                }
            }
            Screen::SignIn => match action {
                Action::Confirm if matches!(self.sign_in, Phase::Waiting(_) | Phase::Failed(_)) => self.begin_sign_in(),
                Action::Escape => self.open_dialog(DialogKind::Quit),
                _ => {}
            },
            Screen::CoreFailed(_) => match action {
                Action::Confirm => self.start_core(),
                Action::Escape => self.open_dialog(DialogKind::Quit),
                _ => {}
            },
            Screen::Connecting => {
                if matches!(action, Action::Escape) {
                    self.open_dialog(DialogKind::Quit);
                }
            }
        }
    }

    fn open_dialog(&mut self, kind: DialogKind) {
        self.dialog = Some(Dialog { kind, confirm_selected: false });
    }

    fn confirmed(&mut self, kind: DialogKind) {
        match kind {
            DialogKind::Quit => self.quit = true,
            DialogKind::SignOut => {
                self.send("auth.logout", json!({}), Duration::from_secs(20), Pending::Logout);
                self.notify("Signing out…");
            }
            DialogKind::AddToLibrary => self.submit_add(),
            DialogKind::EndStream => self.end_stream("Ended from the handheld"),
        }
    }

    /// Home, settings, the store, and open menus repeat a held direction.
    /// A live game keeps the D-pad and sticks for itself.
    pub fn repeats_navigation(&self) -> bool {
        if self.mouse_mode {
            return false;
        }
        match self.screen {
            Screen::Stream => self.session_menu || self.dialog.is_some(),
            _ => true,
        }
    }

    pub fn xbox_layout(&self) -> bool {
        self.profile.xbox_layout
    }

    pub fn vsync_enabled(&self) -> bool {
        self.profile.vsync
    }

    pub fn take_vsync_change(&mut self) -> Option<bool> {
        if !self.vsync_apply {
            return None;
        }
        self.vsync_apply = false;
        Some(self.profile.vsync)
    }

    pub fn note_fn_button(&mut self, present: bool) {
        if present {
            self.fn_button = true;
        }
    }

    pub fn pad_button(&mut self, button: u8, pressed: bool, raw_joystick: bool) {
        if !matches!(self.screen, Screen::Stream) {
            return;
        }
        if button == 16 {
            if pressed {
                self.fn_button = true;
                if self.mouse_mode {
                    self.leave_mouse_mode();
                } else {
                    self.toggle_session_menu();
                }
            }
            return;
        }
        if button == 4 || button == 12 {
            self.pad_select = pressed;
        }
        if button == 2 {
            self.pad_x = pressed;
        }
        if !self.fn_button && pressed && self.pad_select && self.pad_x {
            self.toggle_session_menu();
            return;
        }
        if self.mouse_mode || self.session_menu || self.dialog.is_some() {
            return;
        }
        if let Some(stream) = self.stream.as_mut() {
            stream.button(button, pressed, raw_joystick, self.profile.xbox_layout);
        }
    }

    fn toggle_session_menu(&mut self) {
        if self.dialog.is_some() || self.mouse_mode {
            return;
        }
        self.session_menu = !self.session_menu;
        if self.session_menu {
            self.session_menu_index = 0;
        }
        if let Some(stream) = self.stream.as_mut() {
            stream.release_pad();
        }
    }

    fn enter_mouse_mode(&mut self) {
        self.mouse_mode = true;
        self.mouse_down = false;
        self.mouse_x = CANVAS.0 / 2.0;
        self.mouse_y = CANVAS.1 / 2.0;
        if let Some(stream) = self.stream.as_mut() {
            stream.release_pad();
        }
        self.send_mouse();
    }

    fn leave_mouse_mode(&mut self) {
        if self.mouse_down {
            if let Some(stream) = self.stream.as_mut() {
                stream.mouse_button(1, false);
            }
            self.mouse_down = false;
        }
        self.mouse_mode = false;
        if let Some(stream) = self.stream.as_mut() {
            stream.release_pad();
        }
    }

    fn send_mouse(&mut self) {
        let (width, height) = self.stream_frame.map(|(_, width, height)| (width, height)).unwrap_or((1024, 768));
        let x = (self.mouse_x / CANVAS.0 * width as f32).round().clamp(0.0, width.saturating_sub(1) as f32) as u16;
        let y = (self.mouse_y / CANVAS.1 * height as f32).round().clamp(0.0, height.saturating_sub(1) as f32) as u16;
        if let Some(stream) = self.stream.as_mut() {
            stream.mouse_absolute(x, y, width as u16, height as u16);
        }
    }

    fn mouse_axis(&mut self, axis: u8, value: i16) {
        const DEADZONE: i16 = 4_500;
        const SPEED: f32 = 0.00025;
        match axis {
            0 | 2 => {
                if value.abs() >= DEADZONE {
                    self.mouse_x = (self.mouse_x + value as f32 * SPEED).clamp(0.0, CANVAS.0 - 1.0);
                    self.send_mouse();
                }
            }
            1 | 3 => {
                if value.abs() >= DEADZONE {
                    // Stick up is a negative SDL value, and the screen's up is a smaller Y.
                    self.mouse_y = (self.mouse_y + value as f32 * SPEED).clamp(0.0, CANVAS.1 - 1.0);
                    self.send_mouse();
                }
            }
            5 => {
                let press = value > 12_000;
                let release = value < 4_000;
                if press && !self.mouse_down {
                    self.mouse_down = true;
                    if let Some(stream) = self.stream.as_mut() {
                        stream.mouse_button(1, true);
                    }
                } else if release && self.mouse_down {
                    self.mouse_down = false;
                    if let Some(stream) = self.stream.as_mut() {
                        stream.mouse_button(1, false);
                    }
                }
            }
            _ => {}
        }
    }

    pub fn pad_axis(&mut self, axis: u8, value: i16) {
        if !matches!(self.screen, Screen::Stream) {
            return;
        }
        if self.mouse_mode {
            self.mouse_axis(axis, value);
            return;
        }
        if self.session_menu || self.dialog.is_some() {
            return;
        }
        if let Some(stream) = self.stream.as_mut() {
            stream.axis(axis, value);
        }
    }

    fn act_store(&mut self, action: Action) {
        let request = {
            let view = store::View {
                games: &self.store_games,
                artwork: &self.artwork,
                status: &self.store_status,
                has_more: self.store_has_more,
                query: &self.store_query,
                seconds: 0.0,
            };
            self.store.act(action, &view)
        };
        match request {
            Some(store::Request::Close) => {
                self.screen = Screen::Home;
                if self.refresh_library {
                    self.refresh_library = false;
                    self.load_library();
                }
            }
            Some(store::Request::Search(query)) => self.browse_store(&query),
            Some(store::Request::LoadMore) | Some(store::Request::Retry) => self.request_store_page(),
            Some(store::Request::Play { id, variant_id }) => {
                let title = self.store_games.iter().find(|game| game.id == id).map(|game| game.title.clone()).unwrap_or_default();
                self.begin_stream(&id, &variant_id, &title, true);
            }
            Some(store::Request::Add { id, variant_id }) => {
                self.pending_add = Some((id, variant_id));
                self.open_dialog(DialogKind::AddToLibrary);
            }
            None => {}
        }
    }

    // Core traffic

    pub fn update(&mut self, elapsed: Duration, painter: &mut Painter) {
        self.refresh_clock();
        if self.toast.as_ref().is_some_and(|(_, shown)| shown.elapsed() > TOAST_TIME) {
            self.toast = None;
        }
        let messages = self.core.as_mut().map(Core::poll).unwrap_or_default();
        for message in messages {
            match message {
                Message::Response { id, result } => {
                    if let Some(pending) = self.pending.remove(&id) {
                        self.responded(pending, result);
                    }
                }
                Message::Event { name, payload } => self.event(&name, &payload),
            }
        }
        if let Some(reason) = self.core.as_ref().and_then(Core::closed).map(str::to_owned) {
            if !matches!(self.screen, Screen::CoreFailed(_)) {
                self.screen = Screen::CoreFailed(reason);
                self.pending.clear();
            }
        }
        self.poll_sign_in();
        self.advance_play();
        let mut keep_owned = Vec::new();
        match self.screen {
            Screen::Home => {
                self.home.update(elapsed, self.games.len());
                for index in self.home.art_order(self.games.len()) {
                    if let Some(url) = self.games[index].artwork.clone() {
                        keep_owned.push(url);
                    }
                }
            }
            Screen::Store => {
                self.store.update(elapsed, self.store_games.len());
                for index in self.store.art_order(self.store_games.len()) {
                    if let Some(url) = self.store_games[index].artwork.clone() {
                        keep_owned.push(url);
                    }
                }
            }
            _ => {}
        }
        self.request_artwork(&keep_owned);
        let keep: Vec<&str> = keep_owned.iter().map(String::as_str).collect();
        self.artwork.update(painter, &keep);
    }

    fn refresh_clock(&mut self) {
        if self.clock_checked.is_some_and(|checked| checked.elapsed() < Duration::from_secs(5)) {
            return;
        }
        self.clock_checked = Some(Instant::now());
        self.clock.battery = system::battery_percent();
        self.clock.time = system::local_time()
            .map(|time| {
                let hour = match time.hour % 12 {
                    0 => 12,
                    hour => hour,
                };
                let suffix = if time.hour < 12 { "AM" } else { "PM" };
                format!("{hour}:{:02} {suffix}", time.minute)
            })
            .unwrap_or_default();
    }

    fn responded(&mut self, pending: Pending, result: Result<Value, CoreError>) {
        match (pending, result) {
            (Pending::Hello, Ok(value)) => {
                if value["protocolVersion"].as_i64() != Some(core::PROTOCOL_VERSION) {
                    self.screen = Screen::CoreFailed("The OpenNOW core uses a different protocol version".into());
                    return;
                }
                eprintln!("core: connected to {}", value["coreVersion"].as_str().unwrap_or("unknown version"));
                self.send("auth.session.get", json!({}), Duration::from_secs(60), Pending::Session);
            }
            (Pending::Hello, Err(error)) => self.screen = Screen::CoreFailed(error.message),
            (Pending::Session | Pending::SignInComplete | Pending::Logout, Ok(envelope)) => self.apply_session(&envelope),
            (Pending::Session, Err(error)) => {
                eprintln!("auth: session restore failed: {}", error.message);
                self.show_sign_in();
            }
            (Pending::SignInComplete, Err(error)) => self.sign_in = Phase::Failed(error.message),
            (Pending::Logout, Err(error)) => {
                eprintln!("auth: logout reported {}", error.message);
                self.show_sign_in();
            }
            (Pending::SignInStart, Ok(value)) => self.challenge_started(&value),
            (Pending::SignInStart | Pending::SignInPoll, Err(error)) => self.sign_in = Phase::Failed(error.message),
            (Pending::SignInPoll, Ok(value)) => self.polled(&value),
            (Pending::SignInCancel, _) => {}
            (Pending::Library, Ok(value)) => self.library_page(&value),
            (Pending::Library, Err(error)) => self.library_failed(error),
            (Pending::Artwork(_), Ok(value)) => self.artwork.resolved(&value),
            (Pending::Artwork(url), Err(_)) => self.artwork.resolve_failed(&url),
            (Pending::StorePage, Ok(value)) => self.store_page(&value),
            (Pending::StorePage, Err(error)) => self.store_failed(error),
            (Pending::Own(id), Ok(value)) => self.owned(&id, &value),
            (Pending::Own(_), Err(error)) => self.notice = Some(error.message),
            (Pending::Launch, Ok(value)) => self.launch_checked(&value),
            (Pending::Launch, Err(error)) => self.notice = Some(error.message),
            (Pending::StreamSetting, result) => self.stream_setting_finished(result),
            (Pending::StreamInspect, Ok(value)) => self.stream_inspected(&value),
            (Pending::StreamInspect, Err(error)) => self.fail_stream(error.message),
            (Pending::SessionCreate, Ok(value)) => self.session_created(&value),
            (Pending::SessionCreate, Err(error)) => self.session_create_failed(error),
            (Pending::SessionList, Ok(value)) => self.remote_sessions_listed(&value),
            (Pending::SessionList, Err(error)) => self.fail_stream(error.message),
            (Pending::SessionClaim, Ok(value)) => self.session_claimed(&value),
            (Pending::SessionClaim, Err(error)) => self.fail_stream(error.message),
            (Pending::SessionPoll, Ok(value)) => self.session_polled(&value),
            (Pending::SessionPoll, Err(error)) => self.fail_stream(error.message),
            (Pending::StreamPrepare, Ok(value)) => self.stream_prepared(&value),
            (Pending::StreamPrepare, Err(error)) => self.fail_stream(error.message),
            (Pending::SessionStop, _) => {
                self.cloud_session = None;
                self.stream = None;
                self.session_menu = false;
                self.pad_select = false;
                self.pad_x = false;
                if self.quit_after_session {
                    self.quit = true;
                } else if self.play == PlayPhase::Stop || self.play == PlayPhase::Idle {
                    self.finish_stream_screen();
                }
            }
            (Pending::Regions, Ok(value)) => self.regions_loaded(&value),
            (Pending::Regions, Err(error)) => {
                eprintln!("settings: server list failed: {}", error.message);
                if self.servers.is_empty() {
                    self.notify("Couldn't load server locations");
                }
            }
            (Pending::SettingsRead, Ok(value)) => self.settings_loaded(&value),
            (Pending::SettingsRead, Err(error)) => eprintln!("settings: could not read saved server: {}", error.message),
            (Pending::RegionSet, Ok(_)) => eprintln!("settings: server saved"),
            (Pending::RegionSet, Err(error)) => {
                self.notify(error.message);
                self.send("settings.get", json!({}), Duration::from_secs(10), Pending::SettingsRead);
            }
        }
    }

    fn event(&mut self, name: &str, payload: &Value) {
        match name {
            "artwork.ready" => self.artwork.resolved(payload),
            "auth.session.changed" => {
                if payload["session"].is_null() && matches!(self.screen, Screen::Home | Screen::Store) {
                    self.show_sign_in();
                }
            }
            _ => {}
        }
    }

    // Accounts

    fn apply_session(&mut self, envelope: &Value) {
        let user = &envelope["session"]["user"];
        if !user.is_object() {
            self.show_sign_in();
            return;
        }
        self.account = user["displayName"].as_str().unwrap_or("Signed in").to_owned();
        self.scope = json!({
            "generation": envelope["generation"].clone(),
            "userId": user["userId"].clone(),
            "providerIdpId": envelope["session"]["provider"]["idpId"].clone(),
        });
        eprintln!("auth: signed in, membership {}", user["membershipTier"].as_str().unwrap_or("unknown"));
        self.screen = Screen::Home;
        self.home.reset();
        self.load_library();
    }

    fn show_sign_in(&mut self) {
        self.account.clear();
        self.scope = Value::Null;
        self.games.clear();
        self.store_games.clear();
        self.store.reset();
        self.refresh_library = false;
        self.notice = None;
        self.screen = Screen::SignIn;
        self.begin_sign_in();
    }

    fn begin_sign_in(&mut self) {
        if let Phase::Waiting(challenge) = &self.sign_in {
            let params = json!({"attemptId": challenge.attempt_id});
            self.send("auth.device.cancel", params, Duration::from_secs(5), Pending::SignInCancel);
        }
        self.pending.retain(|_, pending| !matches!(pending, Pending::SignInStart | Pending::SignInPoll | Pending::SignInComplete));
        self.sign_in = Phase::Starting;
        self.next_poll = None;
        self.send("auth.device.start", json!({}), Duration::from_secs(30), Pending::SignInStart);
    }

    fn challenge_started(&mut self, value: &Value) {
        let qr = value["qrRows"]
            .as_array()
            .map(|rows| rows.iter().filter_map(Value::as_str).map(|row| row.chars().map(|cell| cell == '1').collect()).collect())
            .unwrap_or_default();
        let (Some(attempt_id), Some(user_code)) = (value["attemptId"].as_str(), value["userCode"].as_str()) else {
            self.sign_in = Phase::Failed("The sign-in code was incomplete".into());
            return;
        };
        let interval = value["intervalSeconds"].as_u64().unwrap_or(5).clamp(1, 60);
        self.next_poll = Some(Instant::now() + Duration::from_secs(interval));
        self.sign_in = Phase::Waiting(Challenge {
            attempt_id: attempt_id.to_owned(),
            user_code: user_code.to_owned(),
            verification_uri: value["verificationUri"].as_str().unwrap_or_default().to_owned(),
            qr,
            expires_at: value["expiresAt"].as_u64().unwrap_or(0),
        });
    }

    fn poll_sign_in(&mut self) {
        let Phase::Waiting(challenge) = &self.sign_in else {
            return;
        };
        if self.next_poll.is_none_or(|at| Instant::now() < at) {
            return;
        }
        if challenge.expires_at != 0 && now_ms() >= challenge.expires_at {
            self.sign_in = Phase::Failed("The code expired.".into());
            return;
        }
        self.next_poll = None;
        let params = json!({"attemptId": challenge.attempt_id});
        self.send("auth.device.poll", params, Duration::from_secs(30), Pending::SignInPoll);
    }

    fn polled(&mut self, value: &Value) {
        let Phase::Waiting(challenge) = &self.sign_in else {
            return;
        };
        match value["status"].as_str() {
            Some("pending" | "slow_down") => {
                let delay = value["retryAfterMs"].as_u64().unwrap_or(5000).clamp(1000, 60_000);
                self.next_poll = Some(Instant::now() + Duration::from_millis(delay));
            }
            Some("authorized") => {
                let params = json!({"attemptId": challenge.attempt_id, "staySignedIn": true});
                self.sign_in = Phase::Completing;
                self.send("auth.device.complete", params, Duration::from_secs(60), Pending::SignInComplete);
            }
            _ => {
                let message = value["error"].as_str().unwrap_or("Sign-in was cancelled.").to_owned();
                self.sign_in = Phase::Failed(message);
            }
        }
    }

    // Library

    fn load_library(&mut self) {
        self.pending.retain(|_, pending| !matches!(pending, Pending::Library));
        self.games.clear();
        self.library = LibraryStatus::Loading;
        let restarts = self.page.restarts;
        self.page = LibraryPage { traversal: format!("handheld-{:x}", now_ms()), restarts, ..LibraryPage::default() };
        self.request_library_page();
    }

    fn request_library_page(&mut self) {
        let mut params = json!({"limit": LIBRARY_PAGE, "traversalId": self.page.traversal});
        if !self.page.cursor.is_empty() {
            params["cursor"] = json!(self.page.cursor);
            params["catalogRevision"] = self.page.revision.clone();
            params["catalogContext"] = self.page.context.clone();
        }
        self.send("catalog.library.list", params, Duration::from_secs(60), Pending::Library);
    }

    fn library_page(&mut self, value: &Value) {
        let games = value["games"].as_array().map(Vec::as_slice).unwrap_or_default();
        self.games.extend(games.iter().filter_map(Game::parse));
        self.games.truncate(LIBRARY_LIMIT);
        let next = value["nextCursor"].as_str().unwrap_or_default();
        if value["hasNextPage"].as_bool() == Some(true) && !next.is_empty() && self.games.len() < LIBRARY_LIMIT {
            self.page.cursor = next.to_owned();
            self.page.revision = value["catalogRevision"].clone();
            self.page.context = value["catalogContext"].clone();
            self.request_library_page();
        } else {
            self.page.restarts = 0;
            self.library = LibraryStatus::Loaded;
            eprintln!("library: {} games", self.games.len());
        }
    }

    fn library_failed(&mut self, error: CoreError) {
        eprintln!("library: {} ({})", error.message, error.code);
        if error.code == "catalog_changed" && self.page.restarts < 2 {
            self.page.restarts += 1;
            self.load_library();
            return;
        }
        self.page.restarts = 0;
        self.library = LibraryStatus::Failed(error.message);
    }

    fn request_artwork(&mut self, urls: &[String]) {
        let mut in_flight = self.pending.values().filter(|pending| matches!(pending, Pending::Artwork(_))).count();
        for url in urls {
            if in_flight >= ARTWORK_IN_FLIGHT {
                return;
            }
            if self.artwork.begin_resolve(url) {
                self.send("artwork.resolve", json!({"sourceUrl": url}), Duration::from_secs(60), Pending::Artwork(url.clone()));
                in_flight += 1;
            }
        }
    }

    fn open_store(&mut self) {
        self.notice = None;
        self.screen = Screen::Store;
        if self.store_games.is_empty() && !self.pending.values().any(|pending| matches!(pending, Pending::StorePage)) {
            self.browse_store("");
        }
    }

    fn browse_store(&mut self, query: &str) {
        self.cancel_pending(|pending| matches!(pending, Pending::StorePage));
        self.store_query = query.to_owned();
        self.store_games.clear();
        self.store_cursor.clear();
        self.store_seen.clear();
        self.store_has_more = true;
        self.store_status = store::Status::Loading;
        self.store.reset();
        self.request_store_page();
    }

    fn request_store_page(&mut self) {
        if !self.store_has_more || self.pending.values().any(|pending| matches!(pending, Pending::StorePage)) {
            return;
        }
        if !self.store_cursor.is_empty() {
            if self.store_seen.iter().any(|cursor| cursor == &self.store_cursor) {
                self.store_has_more = false;
                return;
            }
            self.store_seen.push(self.store_cursor.clone());
        }
        let mut params = json!({"limit": 100, "cursor": self.store_cursor, "searchQuery": self.store_query});
        if self.store_query.is_empty() {
            params["browse"] = json!("all-games");
        }
        self.store_status = store::Status::Loading;
        self.send("catalog.store.list", params, Duration::from_secs(60), Pending::StorePage);
    }

    fn store_page(&mut self, value: &Value) {
        let incoming = value["games"].as_array().map(Vec::as_slice).unwrap_or_default();
        for game in incoming.iter().filter_map(Game::parse) {
            let duplicate = !game.id.is_empty() && self.store_games.iter().any(|have| have.id == game.id);
            if !duplicate && self.store_games.len() < LIBRARY_LIMIT {
                self.store_games.push(game);
            }
        }
        let next = value["nextCursor"].as_str().unwrap_or_default();
        let more = value["hasNextPage"].as_bool() == Some(true)
            && !next.is_empty()
            && self.store_games.len() < LIBRARY_LIMIT
            && !self.store_seen.iter().any(|cursor| cursor == next);
        self.store_has_more = more;
        if more {
            self.store_cursor = next.to_owned();
        }
        self.store_status = store::Status::Ready;
        eprintln!("store: {} games, more={more}", self.store_games.len());
    }

    fn store_failed(&mut self, error: CoreError) {
        eprintln!("store: {} ({})", error.message, error.code);
        if self.store_games.is_empty() {
            self.store_status = store::Status::Failed(error.message);
            self.store_has_more = true;
            self.store_cursor.clear();
            self.store_seen.clear();
        } else {
            self.store_status = store::Status::Ready;
            self.store_has_more = false;
            self.notify(error.message);
        }
    }

    fn submit_add(&mut self) {
        let Some((id, variant_id)) = self.pending_add.clone() else {
            return;
        };
        if !self.scope.is_object() {
            self.notice = Some("Sign in again before adding a game.".into());
            return;
        }
        self.send(
            "catalog.ownership.add",
            json!({"appId": id, "variantId": variant_id, "confirmedExistingLicense": true, "scope": self.scope}),
            Duration::from_secs(60),
            Pending::Own(id),
        );
        self.notify("Adding to your library…");
    }

    fn owned(&mut self, id: &str, value: &Value) {
        if value["reconciliation"].as_str() == Some("confirmed") {
            for game in self.store_games.iter_mut().chain(self.games.iter_mut()) {
                if game.id == id {
                    game.in_library = true;
                }
            }
            self.refresh_library = true;
            self.notify("Added to your library");
        } else {
            let message = value["message"].as_str().unwrap_or("Could not confirm the library update.");
            self.notice = Some(message.to_owned());
        }
    }

    fn launch_checked(&mut self, value: &Value) {
        let status = value["decision"]["status"].as_str().unwrap_or("");
        let message = value["decision"]["message"].as_str().filter(|text| !text.is_empty()).unwrap_or("This game can't be started.");
        self.notice = Some(if status == "ready" {
            format!("{message} Video streaming isn't connected in this build yet, so no session was started.")
        } else {
            message.to_owned()
        });
    }

    fn cancel_pending(&mut self, mut matches_pending: impl FnMut(&Pending) -> bool) {
        let ids: Vec<u64> = self.pending.iter().filter(|(_, pending)| matches_pending(pending)).map(|(id, _)| *id).collect();
        for id in ids {
            if let Some(core) = self.core.as_mut() {
                core.cancel(id);
            }
            self.pending.remove(&id);
        }
    }

    fn act_settings(&mut self, action: Action) {
        match action {
            Action::Up => self.settings_index = self.settings_index.saturating_sub(1),
            Action::Down => self.settings_index = (self.settings_index + 1).min(settings::ROWS - 1),
            Action::Left => self.adjust_setting(-1),
            Action::Right | Action::Confirm => self.adjust_setting(1),
            Action::Back | Action::Escape => self.screen = Screen::Home,
            _ => {}
        }
    }

    fn adjust_setting(&mut self, delta: i32) {
        if self.settings_index == settings::SERVER_ROW {
            self.cycle_server(delta);
            return;
        }
        let vsync = self.profile.vsync;
        self.profile.nudge(self.settings_index, delta);
        if self.profile.vsync != vsync {
            self.vsync_apply = true;
        }
        self.profile.save();
    }

    fn open_settings(&mut self) {
        self.settings_index = 0;
        self.screen = Screen::Settings;
        if !self.pending.values().any(|pending| matches!(pending, Pending::Regions)) {
            self.send("network.regions.list", json!({}), Duration::from_secs(30), Pending::Regions);
        }
        if !self.pending.values().any(|pending| matches!(pending, Pending::SettingsRead)) {
            self.send("settings.get", json!({}), Duration::from_secs(10), Pending::SettingsRead);
        }
    }

    fn regions_loaded(&mut self, value: &Value) {
        if let Some(provider) = value["providerIdpId"].as_str().filter(|provider| !provider.is_empty()) {
            self.region_provider = provider.to_owned();
        }
        self.servers = value["regions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let name = entry["name"].as_str().filter(|name| !name.is_empty())?;
                let url = entry["url"].as_str().filter(|url| url.starts_with("https://"))?;
                Some(Region { name: name.to_owned(), url: url.to_owned() })
            })
            .collect();
        self.sync_server_index();
        eprintln!("settings: {} server locations", self.servers.len());
    }

    fn settings_loaded(&mut self, value: &Value) {
        let settings = &value["settings"];
        self.region_url = settings["region"].as_str().unwrap_or("").to_owned();
        if let Some(provider) = settings["regionProviderIdpId"].as_str().filter(|provider| !provider.is_empty()) {
            self.region_provider = provider.to_owned();
        }
        self.sync_server_index();
    }

    fn sync_server_index(&mut self) {
        self.server_index = self
            .servers
            .iter()
            .position(|server| server.url == self.region_url)
            .map(|index| index + 1)
            .unwrap_or(0);
    }

    fn server_label(&self) -> String {
        if self.servers.is_empty() {
            return if self.pending.values().any(|pending| matches!(pending, Pending::Regions)) {
                "Loading…".into()
            } else if self.region_url.is_empty() {
                "Automatic".into()
            } else {
                "Saved".into()
            };
        }
        if self.server_index == 0 {
            "Automatic".into()
        } else {
            self.servers.get(self.server_index - 1).map(|server| server.name.clone()).unwrap_or_else(|| "Automatic".into())
        }
    }

    fn cycle_server(&mut self, delta: i32) {
        if self.servers.is_empty() || self.region_provider.is_empty() {
            self.notify("Server list isn't ready yet");
            return;
        }
        let count = self.servers.len() + 1;
        let next = (self.server_index as i32 + delta).rem_euclid(count as i32) as usize;
        let url = if next == 0 { String::new() } else { self.servers[next - 1].url.clone() };
        self.server_index = next;
        self.region_url = url.clone();
        eprintln!("settings: server {}", self.server_label());
        self.send(
            "settings.set",
            json!({"key": "region", "value": url, "providerIdpId": self.region_provider}),
            Duration::from_secs(10),
            Pending::RegionSet,
        );
    }

    fn begin_stream(&mut self, catalog_id: &str, variant_id: &str, title: &str, from_store: bool) {
        if self.play != PlayPhase::Idle {
            return;
        }
        if !self.scope.is_object() {
            self.notice = Some("Sign in before playing.".into());
            return;
        }
        if variant_id.is_empty() || !variant_id.bytes().all(|byte| byte.is_ascii_digit()) {
            self.notice = Some("This game has no numeric GeForce NOW launch id.".into());
            return;
        }
        let stream = match NativeStream::open() {
            Ok(stream) => stream,
            Err(message) => {
                self.notice = Some(message);
                return;
            }
        };
        self.stream = Some(stream);
        self.stream_capabilities = None;
        self.cloud_session = None;
        self.queue_position = None;
        self.launch_catalog = catalog_id.to_owned();
        self.launch_variant = variant_id.to_owned();
        self.launch_title = title.to_owned();
        self.return_to_store = from_store;
        let profile = self.profile.clone();
        self.play = PlayPhase::Settings;
        self.play_message = format!("Preparing {title} at {}…", profile.label());
        self.play_deadline = Some(Instant::now() + Duration::from_secs(180));
        self.screen = Screen::Stream;
        let settings = profile.settings();
        self.play_settings_left = settings.len() as u8;
        for (key, value) in settings {
            self.send("settings.set", json!({"key": key, "value": value}), Duration::from_secs(10), Pending::StreamSetting);
        }
    }

    fn advance_play(&mut self) {
        let messages = self.stream.as_ref().map(NativeStream::poll).unwrap_or_default();
        for message in messages {
            if stream::is_error(&message) {
                let text = stream::message_text(&message).unwrap_or_else(|| "The stream failed.".into());
                self.fail_stream(text);
                return;
            }
            if message["type"].as_str() == Some("ready") && message["capabilities"].is_object() {
                self.stream_capabilities = Some(message["capabilities"].clone());
                eprintln!("stream: runtime ready");
            }
            if message["id"].as_str() == Some("start") {
                self.play = PlayPhase::Live;
                self.play_deadline = None;
                self.play_message = self.launch_title.clone();
                eprintln!("stream: media started");
            }
        }
        if !matches!(self.play, PlayPhase::Idle | PlayPhase::Live | PlayPhase::Stop | PlayPhase::Poll)
            && self.play_deadline.is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.fail_stream("The cloud session took too long to start.".into());
            return;
        }
        if self.play == PlayPhase::Settings && self.play_settings_left == 0 {
            if self.stream_capabilities.is_none() {
                return;
            }
            self.play = PlayPhase::Inspect;
            self.play_message = "Checking this game on GeForce NOW…".into();
            self.send(
                "catalog.launch.inspect",
                json!({"appId": self.launch_catalog, "variantId": self.launch_variant}),
                Duration::from_secs(45),
                Pending::StreamInspect,
            );
        }
        if self.play == PlayPhase::Poll && self.next_session_poll.is_none_or(|at| Instant::now() >= at) {
            let poll_busy = self.pending.values().any(|pending| matches!(pending, Pending::SessionPoll));
            if !poll_busy {
                self.next_session_poll = Some(Instant::now() + Duration::from_secs(1));
                if let Some(session) = self.cloud_session.clone() {
                    self.send(
                        "session.poll",
                        json!({"sessionId": session["sessionId"], "streamingBaseUrl": session["streamingBaseUrl"]}),
                        Duration::from_secs(20),
                        Pending::SessionPoll,
                    );
                }
            }
        }
        if self.play == PlayPhase::Live && !self.mouse_mode {
            if let Some(stream) = self.stream.as_mut() {
                stream.keep_pad();
            }
        }
    }

    fn stream_setting_finished(&mut self, result: Result<Value, CoreError>) {
        self.play_settings_left = self.play_settings_left.saturating_sub(1);
        if let Err(error) = result {
            self.fail_stream(error.message);
        }
    }

    fn stream_inspected(&mut self, value: &Value) {
        let status = value["decision"]["status"].as_str().unwrap_or("");
        if status != "ready" {
            let message = value["decision"]["message"].as_str().unwrap_or("This game can't be started.").to_owned();
            self.fail_stream(message);
            return;
        }
        let Some(capabilities) = self.stream_capabilities.clone() else {
            self.fail_stream("The video runtime is not ready.".into());
            return;
        };
        self.play = PlayPhase::Create;
        self.play_message = "Requesting a cloud gaming seat…".into();
        eprintln!("stream: requesting Steam Big Picture for this session");
        self.send(
            "session.create",
            json!({
                "appId": self.launch_variant,
                "catalogAppId": self.launch_catalog,
                "variantId": self.launch_variant,
                "scope": self.scope,
                "title": self.launch_title,
                "runtimeCapabilities": capabilities,
                "maxEntitledFps": 60,
                "appLaunchMode": "gamepadFriendly",
            }),
            Duration::from_secs(60),
            Pending::SessionCreate,
        );
    }

    fn session_created(&mut self, value: &Value) {
        let session = if value["session"].is_object() { value["session"].clone() } else { value.clone() };
        self.note_session(&session);
    }

    fn session_create_failed(&mut self, error: CoreError) {
        if error.code == "session_conflict" && self.play == PlayPhase::Create {
            eprintln!("stream: a GeForce NOW session is already running; claiming it");
            self.play = PlayPhase::Claim;
            self.play_message = "A game is already running. Resuming it…".into();
            self.send("session.remote.list", json!({}), Duration::from_secs(30), Pending::SessionList);
            return;
        }
        self.fail_stream(error.message);
    }

    fn remote_sessions_listed(&mut self, value: &Value) {
        if self.play != PlayPhase::Claim {
            return;
        }
        let Some(session) = resumable_session(value, &self.launch_variant) else {
            self.fail_stream("A GeForce NOW session is already active, but it could not be resumed.".into());
            return;
        };
        let session_id = session["sessionId"].as_str().unwrap_or("");
        if session_id.is_empty() {
            self.fail_stream("The running GeForce NOW session has no id.".into());
            return;
        }
        eprintln!("stream: claiming session {session_id}");
        self.play_message = "Reconnecting to your running game…".into();
        self.send(
            "session.claim",
            json!({
                "sessionId": session_id,
                "streamingBaseUrl": session["streamingBaseUrl"],
                "appId": session["appId"],
                "recoveryMode": true,
            }),
            Duration::from_secs(35),
            Pending::SessionClaim,
        );
    }

    fn session_claimed(&mut self, value: &Value) {
        let session = if value["session"].is_object() { value["session"].clone() } else { value.clone() };
        self.note_session(&session);
    }

    fn session_polled(&mut self, value: &Value) {
        if value.get("session").is_some_and(Value::is_null) {
            self.fail_stream("The GeForce NOW session ended before it could be resumed.".into());
            return;
        }
        let session = if value["session"].is_object() { value["session"].clone() } else { value.clone() };
        self.note_session(&session);
    }

    fn note_session(&mut self, session: &Value) {
        if !matches!(self.play, PlayPhase::Create | PlayPhase::Claim | PlayPhase::Poll) {
            return;
        }
        self.cloud_session = Some(session.clone());
        let status = session["status"].as_i64().unwrap_or(0);
        if status == 7 || session["phase"].as_str() == Some("failed") {
            self.fail_stream("GeForce NOW closed the session before it was ready.".into());
            return;
        }
        // A claim keeps the previous signaling addresses until a fresh poll
        // reports the seat ready. Connecting to those addresses returns HTTP 404.
        let resume_pending = session["resumePending"].as_bool() == Some(true);
        let ready = !resume_pending
            && matches!(status, 2 | 3)
            && session["rtspsEndpoints"].as_array().is_some_and(|endpoints| !endpoints.is_empty());
        if !ready {
            self.play = PlayPhase::Poll;
            self.play_deadline = Some(Instant::now() + Duration::from_secs(30 * 60));
            let reported = session["seatSetupInfo"]["queuePosition"]
                .as_i64()
                .or_else(|| session["queuePosition"].as_i64())
                .filter(|position| *position > 0);
            let position = match (self.queue_position, reported) {
                (Some(current), Some(next)) if next <= current => next,
                (Some(current), _) => current,
                (None, Some(next)) => next,
                (None, None) => {
                    self.play_message = if resume_pending {
                        "Waiting for the running game to hand over…".into()
                    } else {
                        "Waiting for a free machine…".into()
                    };
                    return;
                }
            };
            self.queue_position = Some(position);
            self.play_message = if resume_pending {
                "Waiting for the running game to hand over…".into()
            } else {
                format!("Queue position {position}")
            };
            return;
        }
        self.play = PlayPhase::Attach;
        self.play_deadline = Some(Instant::now() + Duration::from_secs(90));
        self.queue_position = None;
        self.play_message = "Your seat is ready. Connecting video…".into();
        let Some(capabilities) = self.stream_capabilities.clone() else {
            self.fail_stream("The video runtime is not ready.".into());
            return;
        };
        self.send(
            "streamer.prepare",
            json!({"session": session, "runtimeCapabilities": capabilities}),
            Duration::from_secs(20),
            Pending::StreamPrepare,
        );
    }

    fn stream_prepared(&mut self, value: &Value) {
        let Some(context) = value.get("context").cloned() else {
            self.fail_stream("The stream context was incomplete.".into());
            return;
        };
        if let Some(stream) = self.stream.as_mut() {
            if let Err(message) = stream.start(&context) {
                self.fail_stream(message);
                return;
            }
        }
        self.play = PlayPhase::Attach;
        self.play_message = format!("Starting {}…", self.launch_title);
    }

    fn fail_stream(&mut self, message: String) {
        if self.play == PlayPhase::Stop {
            self.notice = Some(message);
            return;
        }
        eprintln!("stream: {message}");
        self.leave_mouse_mode();
        self.queue_position = None;
        let session = self.cloud_session.clone();
        self.play = PlayPhase::Idle;
        self.play_message.clear();
        self.notice = Some(message);
        if let Some(stream) = self.stream.as_mut() {
            stream.stop_media();
        }
        self.stream = None;
        self.session_menu = false;
        self.pad_select = false;
        self.pad_x = false;
        if let Some(session) = session {
            self.play = PlayPhase::Stop;
            self.request_session_stop(&session);
        } else {
            self.finish_stream_screen();
        }
    }

    fn end_stream(&mut self, message: &str) {
        let _ = message;
        self.leave_mouse_mode();
        self.queue_position = None;
        self.session_menu = false;
        self.pad_select = false;
        self.pad_x = false;
        self.stream_frame = None;
        if let Some(mut stream) = self.stream.take() {
            stream.stop_media();
            stream.release_pad();
            stream.release_picture();
        }
        let stopping = if let Some(session) = self.cloud_session.clone() {
            self.play = PlayPhase::Stop;
            self.request_session_stop(&session);
            self.pending.values().any(|pending| matches!(pending, Pending::SessionStop))
        } else {
            self.play = PlayPhase::Idle;
            false
        };
        if self.quit_after_session {
            self.return_to_store = false;
            self.play_message = "Closing OpenNOW…".into();
            self.screen = Screen::Stream;
            if !stopping {
                self.quit = true;
            }
            return;
        }
        self.return_to_store = false;
        self.finish_stream_screen();
        self.load_library();
    }

    fn exit_after_session(&mut self) {
        self.quit_after_session = true;
        self.end_stream("exit");
    }

    fn request_session_stop(&mut self, session: &Value) {
        self.send(
            "session.stop",
            json!({
                "sessionId": session["sessionId"],
                "streamingBaseUrl": session["streamingBaseUrl"],
                "serverIp": session["serverIp"],
            }),
            Duration::from_secs(20),
            Pending::SessionStop,
        );
    }

    fn finish_stream_screen(&mut self) {
        self.play = PlayPhase::Idle;
        self.cloud_session = None;
        self.stream_capabilities = None;
        self.stream_frame = None;
        self.session_menu = false;
        self.screen = if self.return_to_store { Screen::Store } else { Screen::Home };
    }

    pub fn release_picture(&mut self) {
        if let Some(stream) = self.stream.as_mut() {
            stream.release_picture();
        }
    }

    // Drawing

    pub fn draw(&mut self, painter: &mut Painter, status: &Status) {
        let seconds = self.started.elapsed().as_secs_f32();
        match &self.screen {
            Screen::Connecting => {
                theme::draw_header(painter, &self.clock);
                theme::draw_spinner(painter, CANVAS.0 / 2.0, 220.0, 22.0, seconds);
                let label = "Starting OpenNOW…";
                let width = painter.measure(label, 17.0, Weight::Bold);
                painter.text((CANVAS.0 - width) / 2.0, 262.0, 17.0, Weight::Bold, label, MUTED);
                theme::draw_footer(painter, "Connecting to the OpenNOW core", &[]);
            }
            Screen::CoreFailed(message) => {
                theme::draw_header(painter, &self.clock);
                painter.text(INSET_X, 120.0, 24.0, Weight::Black, "OpenNOW couldn't start", TEXT);
                painter.text_fit(INSET_X, 164.0, 15.0, Weight::SemiBold, message, CANVAS.0 - INSET_X * 2.0, ERROR);
                painter.text(INSET_X, 200.0, 15.0, Weight::SemiBold, "Details are in opennow.log next to the app.", MUTED);
                theme::draw_footer(painter, "", &[("A", "Retry")]);
            }
            Screen::SignIn => {
                signin::draw(painter, &self.sign_in, &self.clock, now_ms(), seconds);
                theme::draw_footer(painter, "OpenNOW is not affiliated with NVIDIA", &signin::hints(&self.sign_in));
            }
            Screen::Home => {
                let view = HomeView {
                    games: &self.games,
                    artwork: &self.artwork,
                    library: &self.library,
                    account: &self.account,
                    status,
                    clock: &self.clock,
                    seconds,
                };
                self.home.draw(painter, &view);
            }
            Screen::Settings => {
                let server = self.server_label();
                settings::draw(painter, &self.clock, &self.profile, self.settings_index, &server);
            }
            Screen::Store => {
                let view = store::View {
                    games: &self.store_games,
                    artwork: &self.artwork,
                    status: &self.store_status,
                    has_more: self.store_has_more,
                    query: &self.store_query,
                    seconds,
                };
                self.store.draw(painter, &view);
            }
            Screen::Stream => {
                let covered = self.session_menu || self.dialog.is_some();
                if self.stream.is_none() {
                    self.stream_frame = None;
                } else if !covered {
                    if let Some(picture) = self.stream.as_mut().and_then(NativeStream::picture) {
                        self.stream_frame = Some((picture.texture, picture.width, picture.height));
                    }
                }
                if let Some((texture, width, height)) = self.stream_frame {
                    let (x, y, fitted_width, fitted_height) = stream::fitted_rect(width, height, CANVAS);
                    painter.video(texture, x, y, fitted_width, fitted_height);
                } else {
                    theme::draw_spinner(painter, CANVAS.0 / 2.0, 190.0, 22.0, seconds);
                    let width = painter.measure(&self.play_message, 20.0, Weight::Bold);
                    painter.text_fit(((CANVAS.0 - width) / 2.0).max(EDGE), 236.0, 20.0, Weight::Bold, &self.play_message, CANVAS.0 - EDGE * 2.0, TEXT);
                    theme::draw_footer(
                        painter,
                        if self.fn_button {
                            "FN opens the session menu"
                        } else {
                            "Select + X opens the session menu"
                        },
                        &[],
                    );
                }
            }
        }
        if self.session_menu {
            theme::draw_session_menu(painter, self.session_menu_index as usize);
        }
        if self.mouse_mode {
            painter.text(18.0, 14.0, 14.0, Weight::Bold, "Mouse    sticks move    RT click    FN gamepad", TEXT);
            let x = self.mouse_x - 7.0;
            let y = self.mouse_y - 7.0;
            painter.fill(x, y, 14.0, 14.0, 7.0, BLACK);
            painter.fill(x + 3.0, y + 3.0, 8.0, 8.0, 4.0, if self.mouse_down { FOCUS } else { TEXT });
        }
        if let Some(message) = &self.notice {
            draw_notice(painter, message);
        }
        if let Some(dialog) = &self.dialog {
            let (title, body, confirm) = match dialog.kind {
                DialogKind::Quit => ("Quit OpenNOW?", "You can open it again from Ports.", "Quit"),
                DialogKind::SignOut => ("Sign out?", "You'll need to scan a new code to sign in again.", "Sign out"),
                DialogKind::AddToLibrary => {
                    ("Add to your library?", "Confirms you already own it. Nothing is purchased.", "Add")
                }
                DialogKind::EndStream => ("End the session?", "This closes the GeForce NOW game.", "End"),
            };
            theme::draw_confirm(painter, title, body, confirm, dialog.confirm_selected);
        }
        if let Some((message, shown)) = &self.toast {
            theme::draw_toast(painter, message, TOAST_TIME.saturating_sub(shown.elapsed()).as_secs_f32());
        }
    }

    pub fn shutdown(&mut self, painter: &mut Painter) {
        if self.cloud_session.is_some() || self.stream.is_some() {
            self.end_stream("quit");
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline && self.pending.values().any(|pending| matches!(pending, Pending::SessionStop)) {
                if let Some(core) = self.core.as_mut() {
                    let _ = core.poll();
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        self.release_picture();
        self.artwork.clear(painter);
        self.core = None;
    }
}

const INSET_X: f32 = 32.0;

fn draw_notice(painter: &mut Painter, message: &str) {
    let (x, y, width, height) = (70.0, 110.0, 500.0, 230.0);
    theme::draw_panel(painter, x, y, width, height);
    painter.text(x + 28.0, y + 24.0, 22.0, Weight::Black, "Play", TEXT);
    let mut line = String::new();
    let mut line_y = y + 68.0;
    for word in message.split_whitespace() {
        let candidate = if line.is_empty() { word.to_owned() } else { format!("{line} {word}") };
        if !line.is_empty() && painter.measure(&candidate, 15.0, Weight::SemiBold) > width - 56.0 {
            painter.text(x + 28.0, line_y, 15.0, Weight::SemiBold, &line, MUTED);
            line_y += 22.0;
            if line_y > y + height - 70.0 {
                line = format!("{word}…");
                break;
            }
            line = word.to_owned();
        } else {
            line = candidate;
        }
    }
    if !line.is_empty() {
        painter.text_fit(x + 28.0, line_y, 15.0, Weight::SemiBold, &line, width - 56.0, MUTED);
    }
    painter.text(x + 28.0, y + height - 42.0, 14.0, Weight::Bold, "Press B to close", TEXT);
}

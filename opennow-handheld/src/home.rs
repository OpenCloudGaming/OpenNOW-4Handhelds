//! Console home screen, modelled on the Qt shell's `HomeScreen.qml`: a shelf
//! of square game tiles from the account library, a round dock, and a System
//! panel. Session-level actions are returned to the app as requests.

use crate::assets::Image;
use crate::gl::{Painter, with_alpha};
use crate::library::{Artwork, Game};
use crate::text::Weight;
use crate::theme::{self, *};
use std::time::Duration;

const INSET: f32 = 32.0;
const TILE: f32 = 172.0;
const GAP: f32 = 14.0;
const TITLE_Y: f32 = 90.0;
const SHELF_Y: f32 = 130.0;
const DOCK: f32 = 50.0;
const DOCK_SPACING: f32 = 24.0;
const DOCK_Y: f32 = 336.0;
/// Tiles either side of the selection whose art is loaded ahead of time.
const ART_BEHIND: usize = 2;
const ART_AHEAD: usize = 5;

const DOCK_ITEMS: [(&str, Image); 3] = [
    ("Store", Image::Store),
    ("Settings", Image::Settings),
    ("Power", Image::Power),
];

#[derive(Clone, Copy)]
pub enum Action {
    Up,
    Down,
    Left,
    Right,
    Confirm,
    Back,
    Options,
    /// Escape on a keyboard: closes an overlay, or offers to quit.
    Escape,
    Start(bool),
    Select(bool),
}

pub enum Request {
    Quit,
    SignOut,
    OpenStore,
    OpenSettings,
    RetryLibrary,
    Play,
    Toast(&'static str),
}

pub enum LibraryStatus {
    Loading,
    Loaded,
    Failed(String),
}

/// What the device reported at startup, shown in the System panel.
pub struct Status {
    pub driver: String,
    pub resolution: String,
    pub renderer: String,
    pub devices: Vec<String>,
    pub last_input: String,
}

pub struct HomeView<'a> {
    pub games: &'a [Game],
    pub artwork: &'a Artwork,
    pub library: &'a LibraryStatus,
    pub account: &'a str,
    pub status: &'a Status,
    pub clock: &'a Clock,
    pub seconds: f32,
}

#[derive(PartialEq)]
enum Focus {
    Shelf,
    Dock,
}

pub struct Home {
    shelf_index: usize,
    dock_index: usize,
    focus: Focus,
    system_open: bool,
    scroll: f32,
}

fn tile_x(index: usize) -> f32 {
    INSET + index as f32 * (TILE + GAP)
}

impl Home {
    pub fn new() -> Self {
        Self { shelf_index: 0, dock_index: 0, focus: Focus::Shelf, system_open: false, scroll: 0.0 }
    }

    /// Returns to the first tile, for a new account or a reloaded library.
    /// Shelf selection when a game tile is focused.
    pub fn selected_game(&self, games: usize) -> Option<usize> {
        if self.focus == Focus::Shelf && self.shelf_index < games { Some(self.shelf_index) } else { None }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Indices of the games whose art should be loaded, nearest first.
    pub fn art_order(&self, games: usize) -> Vec<usize> {
        let mut order = Vec::new();
        for distance in 0..=ART_AHEAD.max(ART_BEHIND) {
            if distance <= ART_AHEAD && self.shelf_index + distance < games {
                order.push(self.shelf_index + distance);
            }
            if distance > 0 && distance <= ART_BEHIND && self.shelf_index >= distance {
                order.push(self.shelf_index - distance);
            }
        }
        order
    }

    pub fn act(&mut self, action: Action, view: &HomeView) -> Option<Request> {
        let games = view.games.len();
        let shelf_len = games + 1;
        self.shelf_index = self.shelf_index.min(shelf_len - 1);
        if self.system_open {
            match action {
                Action::Confirm | Action::Back | Action::Escape => self.system_open = false,
                Action::Options => {
                    self.system_open = false;
                    return Some(Request::SignOut);
                }
                _ => {}
            }
            return None;
        }
        match (action, &self.focus) {
            (Action::Escape, _) => return Some(Request::Quit),
            (Action::Left, Focus::Shelf) => self.shelf_index = self.shelf_index.saturating_sub(1),
            (Action::Right, Focus::Shelf) => self.shelf_index = (self.shelf_index + 1).min(shelf_len - 1),
            (Action::Down, Focus::Shelf) => self.focus = Focus::Dock,
            (Action::Confirm, Focus::Shelf) => {
                if let LibraryStatus::Failed(_) = view.library {
                    if games == 0 {
                        return Some(Request::RetryLibrary);
                    }
                }
                if self.shelf_index < games {
                    return Some(Request::Play);
                }
                return Some(Request::Toast("The store isn't connected yet"));
            }
            (Action::Options, Focus::Shelf) if self.shelf_index < games => {
                return Some(Request::Toast("Tile options are coming soon"));
            }
            (Action::Left, Focus::Dock) => self.dock_index = self.dock_index.saturating_sub(1),
            (Action::Right, Focus::Dock) => self.dock_index = (self.dock_index + 1).min(DOCK_ITEMS.len() - 1),
            (Action::Up, Focus::Dock) => self.focus = Focus::Shelf,
            (Action::Options, Focus::Dock) => self.system_open = true,
            (Action::Confirm, Focus::Dock) => match DOCK_ITEMS[self.dock_index].1 {
                Image::Store => return Some(Request::OpenStore),
                Image::Settings => return Some(Request::OpenSettings),
                _ => return Some(Request::Quit),
            },
            _ => {}
        }
        None
    }

    pub fn update(&mut self, elapsed: Duration, games: usize) {
        self.shelf_index = self.shelf_index.min(games);
        let target = tile_x(self.shelf_index) - INSET;
        let blend = (elapsed.as_secs_f32() * 16.0).min(1.0);
        self.scroll += (target - self.scroll) * blend;
        if (target - self.scroll).abs() < 0.25 {
            self.scroll = target;
        }
    }

    pub fn draw(&self, painter: &mut Painter, view: &HomeView) {
        theme::draw_header(painter, view.clock);
        self.draw_shelf(painter, view);
        self.draw_dock(painter);
        let status = match view.library {
            LibraryStatus::Loading if view.games.is_empty() => "Loading your library…".to_owned(),
            LibraryStatus::Loading => format!("{} · loading more games…", view.account),
            LibraryStatus::Loaded => format!("{} · {} games", view.account, view.games.len()),
            LibraryStatus::Failed(message) => message.clone(),
        };
        if self.system_open {
            draw_system(painter, view);
        }
        theme::draw_footer(painter, &status, &self.hints(view));
    }

    fn hints(&self, view: &HomeView) -> Vec<(&'static str, &'static str)> {
        if self.system_open {
            return vec![("X", "Sign out"), ("B", "Close")];
        }
        match self.focus {
            Focus::Dock => vec![("X", "System"), ("A", "Select")],
            Focus::Shelf if view.games.is_empty() && matches!(view.library, LibraryStatus::Failed(_)) => vec![("A", "Retry")],
            Focus::Shelf if self.shelf_index >= view.games.len() => vec![("A", "Add")],
            Focus::Shelf => vec![("X", "Options"), ("A", "Start")],
        }
    }

    fn draw_shelf(&self, painter: &mut Painter, view: &HomeView) {
        let games = view.games;
        let title = match (games.get(self.shelf_index), view.library) {
            (Some(game), _) => game.title.as_str(),
            (None, LibraryStatus::Loading) if games.is_empty() => "Your library",
            (None, LibraryStatus::Failed(_)) if games.is_empty() => "Library unavailable",
            (None, _) => "Add a game",
        };
        painter.text_fit(INSET, TITLE_Y, 22.0, Weight::SemiBold, title, CANVAS.0 - INSET - EDGE, TITLE);

        if games.is_empty() {
            match view.library {
                LibraryStatus::Loading => {
                    draw_message_tile(painter, tile_x(0) - self.scroll, "Loading…", None);
                    let x = tile_x(0) - self.scroll;
                    theme::draw_spinner(painter, x + TILE / 2.0, SHELF_Y + 66.0, 18.0, view.seconds);
                    return;
                }
                LibraryStatus::Failed(_) => {
                    let x = tile_x(0) - self.scroll;
                    draw_message_tile(painter, x, "Try again", Some("Press A"));
                    self.draw_focus(painter, 0, x);
                    return;
                }
                LibraryStatus::Loaded => {}
            }
        }

        for index in 0..=games.len() {
            let x = tile_x(index) - self.scroll;
            if x > CANVAS.0 || x + TILE < 0.0 {
                continue;
            }
            match games.get(index) {
                Some(game) => draw_game_tile(painter, game, view.artwork, x),
                None => draw_add_tile(painter, x),
            }
            self.draw_focus(painter, index, x);
        }
    }

    fn draw_focus(&self, painter: &mut Painter, index: usize, x: f32) {
        if index == self.shelf_index && self.focus == Focus::Shelf {
            painter.stroke(x - 8.0, SHELF_Y - 8.0, TILE + 16.0, TILE + 16.0, 9.0, 4.5, FOCUS);
        }
    }

    fn draw_dock(&self, painter: &mut Painter) {
        let width = DOCK_ITEMS.len() as f32 * DOCK + (DOCK_ITEMS.len() - 1) as f32 * DOCK_SPACING;
        let left = (CANVAS.0 - width) / 2.0;
        for (index, (label, icon)) in DOCK_ITEMS.iter().enumerate() {
            let x = left + index as f32 * (DOCK + DOCK_SPACING);
            painter.shadow(x, DOCK_Y + 4.0, DOCK, DOCK, DOCK / 2.0, 8.0, with_alpha(BLACK, 0.4));
            painter.fill(x, DOCK_Y, DOCK, DOCK, DOCK / 2.0, DOCK_FACE);
            let icon_size = DOCK * 0.5;
            painter.image(*icon, x + (DOCK - icon_size) / 2.0, DOCK_Y + (DOCK - icon_size) / 2.0, icon_size, icon_size, WHITE);
            if self.focus == Focus::Dock && index == self.dock_index {
                painter.stroke(x - 5.0, DOCK_Y - 5.0, DOCK + 10.0, DOCK + 10.0, DOCK / 2.0 + 5.0, 3.0, FOCUS);
                let label_width = painter.measure(label, 14.0, Weight::Bold);
                painter.text(x + (DOCK - label_width) / 2.0, DOCK_Y + DOCK + 10.0, 14.0, Weight::Bold, label, TEXT);
            }
        }
    }
}

fn draw_game_tile(painter: &mut Painter, game: &Game, artwork: &Artwork, x: f32) {
    let y = SHELF_Y;
    match game.artwork.as_deref().and_then(|url| artwork.texture(url)) {
        Some(texture) => painter.texture(texture, x, y, TILE, TILE, 5.0, WHITE),
        None => {
            painter.fill(x, y, TILE, TILE, 5.0, game.color);
            let initial: String = game.title.chars().take(1).collect();
            let initial_width = painter.measure(&initial, 96.0, Weight::Black);
            painter.text(x + (TILE - initial_width) / 2.0, y + 18.0, 96.0, Weight::Black, &initial, with_alpha(WHITE, 0.14));
        }
    }
    painter.gradient(x, y, TILE, TILE, 5.0, y, with_alpha(BLACK, 0.35), with_alpha(BLACK, 0.0));
    painter.gradient(x, y, TILE, TILE, 5.0, y + TILE * 0.4, with_alpha(BLACK, 0.0), with_alpha(BLACK, 0.7));
    painter.text(x + 12.0, y + 11.0, 11.0, Weight::ExtraBold, &game.store, with_alpha(WHITE, 0.8));
    painter.text_fit(x + 12.0, y + TILE - 42.0, 16.0, Weight::Black, &game.title, TILE - 24.0, WHITE);
    painter.text_fit(x + 12.0, y + TILE - 22.0, 12.0, Weight::Bold, &game.status, TILE - 24.0, with_alpha(WHITE, 0.7));
}

fn draw_message_tile(painter: &mut Painter, x: f32, title: &str, detail: Option<&str>) {
    let y = SHELF_Y;
    painter.fill(x, y, TILE, TILE, 5.0, ADD_TILE);
    let width = painter.measure(title, 17.0, Weight::ExtraBold);
    painter.text(x + (TILE - width) / 2.0, y + 104.0, 17.0, Weight::ExtraBold, title, TEXT);
    if let Some(detail) = detail {
        let width = painter.measure(detail, 13.0, Weight::Bold);
        painter.text(x + (TILE - width) / 2.0, y + 130.0, 13.0, Weight::Bold, detail, MUTED);
    }
}

fn draw_add_tile(painter: &mut Painter, x: f32) {
    let y = SHELF_Y;
    painter.fill(x, y, TILE, TILE, 5.0, ADD_TILE);
    let circle = 44.0;
    let (cx, cy) = (x + (TILE - circle) / 2.0, y + 46.0);
    painter.fill(cx, cy, circle, circle, circle / 2.0, SEPARATOR);
    painter.fill(cx + 13.0, cy + circle / 2.0 - 1.5, circle - 26.0, 3.0, 1.5, TEXT);
    painter.fill(cx + circle / 2.0 - 1.5, cy + 13.0, 3.0, circle - 26.0, 1.5, TEXT);
    let label = "Add a game";
    let width = painter.measure(label, 17.0, Weight::ExtraBold);
    painter.text(x + (TILE - width) / 2.0, cy + circle + 14.0, 17.0, Weight::ExtraBold, label, TEXT);
}

fn draw_system(painter: &mut Painter, view: &HomeView) {
    let status = view.status;
    let (x, y, width, height) = (60.0, 56.0, 520.0, 352.0);
    theme::draw_panel(painter, x, y, width, height);
    painter.text(x + 26.0, y + 22.0, 24.0, Weight::Black, "System", TEXT);
    let devices = if status.devices.is_empty() { "None found".to_owned() } else { status.devices.join(", ") };
    let rows = [
        ("Account", view.account),
        ("Video driver", status.driver.as_str()),
        ("Resolution", status.resolution.as_str()),
        ("GPU", status.renderer.as_str()),
        ("Controllers", devices.as_str()),
        ("Last input", status.last_input.as_str()),
    ];
    let value_x = x + 160.0;
    for (index, (label, value)) in rows.iter().enumerate() {
        let row_y = y + 70.0 + index as f32 * 44.0;
        painter.text(x + 26.0, row_y, 15.0, Weight::Bold, label, MUTED);
        painter.text_fit(value_x, row_y, 15.0, Weight::SemiBold, value, x + width - 26.0 - value_x, TEXT);
        if index + 1 < rows.len() {
            painter.fill(x + 26.0, row_y + 31.0, width - 52.0, 1.0, 0.0, SEPARATOR);
        }
    }
}

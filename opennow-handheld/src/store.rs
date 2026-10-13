//! GeForce NOW store: a three-column grid of the live catalog, an on-screen
//! search keyboard, and a per-game menu. Adding a game asserts an existing
//! store license; it does not buy anything.

use crate::gl::{Painter, with_alpha};
use crate::home::Action;
use crate::library::{Artwork, Game};
use crate::text::Weight;
use crate::theme::{self, *};

const COLUMNS: usize = 3;
const MARGIN: f32 = 16.0;
const GAP: f32 = 14.0;
const TILE: f32 = (CANVAS.0 - MARGIN * 2.0 - GAP * (COLUMNS as f32 - 1.0)) / COLUMNS as f32;
const GRID_TOP: f32 = 16.0;
const GRID_BOTTOM: f32 = SEPARATOR_Y;
const MAX_QUERY: usize = 40;

const KEY_ROWS: [&[&str]; 4] = [
    &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p"],
    &["a", "s", "d", "f", "g", "h", "j", "k", "l"],
    &["z", "x", "c", "v", "b", "n", "m", "⌫"],
    &["space", "done"],
];

pub enum Status {
    Loading,
    Ready,
    Failed(String),
}

pub struct View<'a> {
    pub games: &'a [Game],
    pub artwork: &'a Artwork,
    pub status: &'a Status,
    pub has_more: bool,
    pub query: &'a str,
    pub seconds: f32,
}

pub enum Request {
    Close,
    Search(String),
    LoadMore,
    Retry,
    Play { id: String, variant_id: String },
    Add { id: String, variant_id: String },
}

enum KeyResult {
    Stay,
    Close,
    Submit(String),
}

struct Keyboard {
    row: usize,
    column: usize,
    draft: String,
}

pub struct Store {
    index: usize,
    scroll: f32,
    menu: Option<usize>,
    keyboard: Option<Keyboard>,
}

fn grid_left() -> f32 {
    MARGIN
}

fn tile_origin(index: usize, scroll: f32) -> (f32, f32) {
    let column = (index % COLUMNS) as f32;
    let row = (index / COLUMNS) as f32;
    (grid_left() + column * (TILE + GAP), GRID_TOP + row * (TILE + GAP) - scroll)
}

impl Store {
    pub fn new() -> Self {
        Self { index: 0, scroll: 0.0, menu: None, keyboard: None }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Tile indices whose art should be loaded, nearest the selection first.
    pub fn art_order(&self, count: usize) -> Vec<usize> {
        if count == 0 {
            return Vec::new();
        }
        let index = self.index.min(count - 1);
        let start = index.saturating_sub(COLUMNS * 2);
        let end = (index + COLUMNS * 4).min(count);
        let mut order: Vec<usize> = (start..end).collect();
        order.sort_by_key(|item| item.abs_diff(index));
        order
    }

    pub fn act(&mut self, action: Action, view: &View) -> Option<Request> {
        let count = view.games.len();
        if count > 0 {
            self.index = self.index.min(count - 1);
        }
        let keys = self.keyboard.as_mut().map(|keyboard| keyboard_action(keyboard, action));
        match keys {
            Some(KeyResult::Stay) => return None,
            Some(KeyResult::Close) => {
                self.keyboard = None;
                return None;
            }
            Some(KeyResult::Submit(query)) => {
                self.keyboard = None;
                return Some(Request::Search(query));
            }
            None => {}
        }
        if let Some(row) = self.menu {
            return self.menu_action(action, row, view);
        }
        match action {
            Action::Back | Action::Escape => return Some(Request::Close),
            Action::Options => {
                self.keyboard = Some(Keyboard { row: 0, column: 0, draft: view.query.to_owned() });
                return None;
            }
            Action::Left if self.index % COLUMNS != 0 => self.index -= 1,
            Action::Right if self.index % COLUMNS != COLUMNS - 1 && self.index + 1 < count => {
                self.index += 1;
            }
            Action::Up if self.index >= COLUMNS => self.index -= COLUMNS,
            Action::Down => {
                if self.index + COLUMNS < count {
                    self.index += COLUMNS;
                } else if view.has_more {
                    return Some(Request::LoadMore);
                }
            }
            Action::Confirm if count == 0 && matches!(view.status, Status::Failed(_)) => {
                return Some(Request::Retry);
            }
            Action::Confirm if count > 0 => {
                self.menu = Some(0);
                return None;
            }
            _ => {}
        }
        None
    }

    fn menu_action(&mut self, action: Action, row: usize, view: &View) -> Option<Request> {
        let Some(game) = view.games.get(self.index) else {
            self.menu = None;
            return None;
        };
        let rows = menu_rows(game);
        match action {
            Action::Up => self.menu = Some(row.saturating_sub(1)),
            Action::Down => self.menu = Some((row + 1).min(rows.len() - 1)),
            Action::Back | Action::Escape => self.menu = None,
            Action::Confirm => {
                let (kind, enabled) = rows.get(row).map(|item| (item.kind, item.enabled)).unwrap_or((MenuKind::Close, true));
                if !enabled {
                    return None;
                }
                let id = game.id.clone();
                let variant_id = game.variant_id.clone();
                self.menu = None;
                return match kind {
                    MenuKind::Play => Some(Request::Play { id, variant_id }),
                    MenuKind::Add => Some(Request::Add { id, variant_id }),
                    MenuKind::Close => None,
                };
            }
            _ => {}
        }
        None
    }

    pub fn update(&mut self, elapsed: std::time::Duration, count: usize) {
        if count == 0 {
            self.scroll = 0.0;
            return;
        }
        self.index = self.index.min(count - 1);
        let rows = count.div_ceil(COLUMNS) as f32;
        let content = rows * (TILE + GAP) - GAP;
        let view = GRID_BOTTOM - GRID_TOP;
        let row_y = (self.index / COLUMNS) as f32 * (TILE + GAP);
        let target = (row_y - (view - TILE) / 2.0).clamp(0.0, (content - view).max(0.0));
        let blend = (elapsed.as_secs_f32() * 16.0).min(1.0);
        self.scroll += (target - self.scroll) * blend;
        if (target - self.scroll).abs() < 0.25 {
            self.scroll = target;
        }
    }

    pub fn draw(&self, painter: &mut Painter, view: &View) {
        painter.clip(0.0, 0.0, CANVAS.0, SEPARATOR_Y);
        self.draw_grid(painter, view);
        painter.clear_clip();
        painter.fill(0.0, SEPARATOR_Y, CANVAS.0, CANVAS.1 - SEPARATOR_Y, 0.0, BACKGROUND);
        theme::draw_footer(painter, &footer(view), &self.hints(view));
        if self.menu.is_some() {
            self.draw_menu(painter, view);
        }
        if let Some(keyboard) = &self.keyboard {
            draw_keyboard(painter, keyboard);
        }
    }

    fn hints(&self, view: &View) -> Vec<(&'static str, &'static str)> {
        if self.keyboard.is_some() {
            return vec![("A", "Type"), ("B", "Close")];
        }
        if self.menu.is_some() {
            return vec![("A", "Select"), ("B", "Close")];
        }
        if view.games.is_empty() && matches!(view.status, Status::Ready) {
            return vec![("X", "Search"), ("B", "Home")];
        }
        if view.games.is_empty() && matches!(view.status, Status::Failed(_)) {
            return vec![("A", "Retry"), ("B", "Home")];
        }
        vec![("X", "Search"), ("A", "Options"), ("B", "Home")]
    }

    fn draw_grid(&self, painter: &mut Painter, view: &View) {
        if view.games.is_empty() {
            match view.status {
                Status::Loading => {
                    theme::draw_spinner(painter, CANVAS.0 / 2.0, 230.0, 20.0, view.seconds);
                    let label = "Loading the store…";
                    let width = painter.measure(label, 16.0, Weight::Bold);
                    painter.text((CANVAS.0 - width) / 2.0, 268.0, 16.0, Weight::Bold, label, MUTED);
                }
                Status::Failed(_) => {
                    painter.text(EDGE, 200.0, 18.0, Weight::Bold, "The store didn't load", ERROR);
                }
                Status::Ready => {
                    let label = if view.query.is_empty() { "The store is empty" } else { "No games match" };
                    let width = painter.measure(label, 18.0, Weight::Bold);
                    painter.text((CANVAS.0 - width) / 2.0, 220.0, 18.0, Weight::Bold, label, MUTED);
                }
            }
            return;
        }
        let focused = self.keyboard.is_none() && self.menu.is_none();
        for (index, game) in view.games.iter().enumerate() {
            let (x, y) = tile_origin(index, self.scroll);
            if y > GRID_BOTTOM || y + TILE < GRID_TOP {
                continue;
            }
            draw_tile(painter, game, view.artwork, x, y);
            if focused && index == self.index {
                painter.stroke(x - 6.0, y - 6.0, TILE + 12.0, TILE + 12.0, 8.0, 3.5, FOCUS);
            }
        }
    }

    fn draw_menu(&self, painter: &mut Painter, view: &View) {
        let Some(game) = view.games.get(self.index) else {
            return;
        };
        let rows = menu_rows(game);
        let (x, width, row_h) = (120.0, 400.0, 52.0);
        let height = 78.0 + rows.len() as f32 * row_h;
        let y = (CANVAS.1 - height) / 2.0;
        theme::draw_panel(painter, x, y, width, height);
        painter.text_fit(x + 24.0, y + 20.0, 20.0, Weight::Black, &game.title, width - 48.0, TEXT);
        let selected = self.menu.unwrap_or(0);
        for (index, item) in rows.iter().enumerate() {
            let row_y = y + 64.0 + index as f32 * row_h;
            if index == selected {
                painter.fill(x + 16.0, row_y, width - 32.0, row_h - 8.0, 12.0, if item.enabled { FOCUS } else { BUTTON });
            }
            let color = if !item.enabled {
                MUTED
            } else if index == selected {
                BACKGROUND
            } else {
                TEXT
            };
            painter.text(x + 32.0, row_y + 10.0, 16.0, Weight::ExtraBold, item.label, color);
        }
    }
}

struct MenuItem {
    label: &'static str,
    kind: MenuKind,
    enabled: bool,
}

#[derive(Clone, Copy)]
enum MenuKind {
    Play,
    Add,
    Close,
}

fn menu_rows(game: &Game) -> Vec<MenuItem> {
    let playable = !game.id.is_empty() && !game.variant_id.is_empty();
    vec![
        MenuItem { label: "Play", kind: MenuKind::Play, enabled: playable },
        MenuItem {
            label: if game.in_library { "In your library" } else { "Add to library" },
            kind: MenuKind::Add,
            enabled: playable && !game.in_library,
        },
        MenuItem { label: "Close", kind: MenuKind::Close, enabled: true },
    ]
}

fn footer(view: &View) -> String {
    match view.status {
        Status::Loading if view.games.is_empty() => "Loading the store…".to_owned(),
        Status::Loading => "Loading more games…".to_owned(),
        Status::Failed(message) if view.games.is_empty() => message.clone(),
        Status::Ready | Status::Failed(_) if !view.query.is_empty() => format!("Search · {}", view.query),
        _ if view.has_more => format!("Store · {} games · more below", view.games.len()),
        _ => format!("Store · {} games", view.games.len()),
    }
}

fn draw_tile(painter: &mut Painter, game: &Game, artwork: &Artwork, x: f32, y: f32) {
    match game.artwork.as_deref().and_then(|url| artwork.texture(url)) {
        Some(texture) => painter.texture(texture, x, y, TILE, TILE, 5.0, WHITE),
        None => {
            painter.fill(x, y, TILE, TILE, 5.0, game.color);
            let initial: String = game.title.chars().take(1).collect();
            let width = painter.measure(&initial, 72.0, Weight::Black);
            painter.text(x + (TILE - width) / 2.0, y + 24.0, 72.0, Weight::Black, &initial, with_alpha(WHITE, 0.14));
        }
    }
    painter.gradient(x, y, TILE, TILE, 5.0, y + TILE * 0.42, with_alpha(BLACK, 0.0), with_alpha(BLACK, 0.72));
    let caption = if game.in_library { "In your library" } else { game.store.as_str() };
    painter.text_fit(x + 10.0, y + TILE - 36.0, 14.0, Weight::Black, &game.title, TILE - 20.0, WHITE);
    painter.text_fit(x + 10.0, y + TILE - 18.0, 11.0, Weight::Bold, caption, TILE - 20.0, with_alpha(WHITE, 0.75));
}

fn draw_keyboard(painter: &mut Painter, keyboard: &Keyboard) {
    let (x, y, width, height) = (36.0, 150.0, 568.0, 250.0);
    theme::draw_panel(painter, x, y, width, height);
    let shown = if keyboard.draft.is_empty() { "Type a game name".to_owned() } else { keyboard.draft.clone() };
    let color = if keyboard.draft.is_empty() { MUTED } else { TEXT };
    painter.text_fit(x + 24.0, y + 16.0, 18.0, Weight::Bold, &shown, width - 48.0, color);
    let top = y + 58.0;
    for (row_index, row) in KEY_ROWS.iter().enumerate() {
        let key_w = 46.0;
        let key_h = 36.0;
        let gap = 6.0;
        let span_of = |label: &str| if label == "space" || label == "done" { key_w * 2.0 + gap } else { key_w };
        let row_width = row.iter().map(|label| span_of(label)).sum::<f32>() + (row.len().saturating_sub(1)) as f32 * gap;
        let mut key_x = x + (width - row_width) / 2.0;
        let key_y = top + row_index as f32 * (key_h + gap);
        for (column, label) in row.iter().enumerate() {
            let span = span_of(label);
            let selected = keyboard.row == row_index && keyboard.column == column;
            painter.fill(key_x, key_y, span, key_h, 8.0, if selected { FOCUS } else { BUTTON });
            let text = match *label {
                "space" => "space",
                "done" => "search",
                other => other,
            };
            let text_width = painter.measure(text, 15.0, Weight::ExtraBold);
            painter.text(key_x + (span - text_width) / 2.0, key_y + 8.0, 15.0, Weight::ExtraBold, text, if selected { BACKGROUND } else { TEXT });
            key_x += span + gap;
        }
    }
}

fn keyboard_action(keyboard: &mut Keyboard, action: Action) -> KeyResult {
    let row = &KEY_ROWS[keyboard.row];
    match action {
        Action::Left => keyboard.column = keyboard.column.saturating_sub(1),
        Action::Right => keyboard.column = (keyboard.column + 1).min(row.len() - 1),
        Action::Up | Action::Down => {
            let going_up = matches!(action, Action::Up);
            let next = if going_up { keyboard.row.saturating_sub(1) } else { (keyboard.row + 1).min(KEY_ROWS.len() - 1) };
            let columns = KEY_ROWS[next].len();
            keyboard.column = (keyboard.column * columns / row.len()).min(columns - 1);
            keyboard.row = next;
        }
        Action::Back | Action::Escape => return KeyResult::Close,
        Action::Confirm => match row[keyboard.column] {
            "⌫" => {
                keyboard.draft.pop();
            }
            "space" if keyboard.draft.len() < MAX_QUERY => keyboard.draft.push(' '),
            "done" => return KeyResult::Submit(keyboard.draft.trim().to_owned()),
            letter if keyboard.draft.len() < MAX_QUERY && letter.chars().count() == 1 => keyboard.draft.push_str(letter),
            _ => {}
        },
        _ => {}
    }
    KeyResult::Stay
}

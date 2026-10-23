//! GeForce NOW store: a three-column grid of the live catalog, an on-screen
//! search keyboard, and a per-game menu. Adding a game asserts an existing
//! store license; it does not buy anything.

use crate::gl::{with_alpha, Painter};
use crate::home::Action;
use crate::library::{Artwork, Game};
use crate::text::Weight;
use crate::theme::{self, *};
use serde_json::Value;

const COLUMNS: usize = 3;
const MARGIN: f32 = 16.0;
const GAP: f32 = 14.0;
fn tile_size() -> f32 {
    (canvas().0 - MARGIN * 2.0 - GAP * (COLUMNS as f32 - 1.0)) / COLUMNS as f32
}
const GRID_TOP: f32 = 16.0;
fn grid_bottom() -> f32 {
    separator_y()
}
const MAX_QUERY: usize = 40;
const GAMEPAD_FILTER_ID: &str = "opennow.gamepadCompatible";

const CELL: f32 = 42.0;
const CELL_GAP: f32 = 3.0;
const KEY_COLS: usize = 13;
const KEY_ROWS: usize = 5;

/// One cell of the EmulationStation gamepad keyboard. Wide keys span columns
/// the way that keyboard does, so a direction skips the whole key.
struct Key {
    row: u8,
    col: u8,
    w: u8,
    h: u8,
    text: &'static str,
    upper: &'static str,
    alt: &'static str,
    alt_shift: &'static str,
}

macro_rules! k {
    ($r:literal, $c:literal, $t:literal) => {
        Key {
            row: $r,
            col: $c,
            w: 1,
            h: 1,
            text: $t,
            upper: $t,
            alt: "",
            alt_shift: "",
        }
    };
    ($r:literal, $c:literal, $w:literal x $h:literal, $t:literal) => {
        Key {
            row: $r,
            col: $c,
            w: $w,
            h: $h,
            text: $t,
            upper: $t,
            alt: "",
            alt_shift: "",
        }
    };
    ($r:literal, $c:literal, $t:literal => $u:literal) => {
        Key {
            row: $r,
            col: $c,
            w: 1,
            h: 1,
            text: $t,
            upper: $u,
            alt: "",
            alt_shift: "",
        }
    };
    ($r:literal, $c:literal, $t:literal => $u:literal, $a:literal => $as:literal) => {
        Key {
            row: $r,
            col: $c,
            w: 1,
            h: 1,
            text: $t,
            upper: $u,
            alt: $a,
            alt_shift: $as,
        }
    };
}

const KEYS: &[Key] = &[
    k!(0, 0, "1" => "!"),
    k!(0, 1, "2" => "@"),
    k!(0, 2, "3" => "#"),
    k!(0, 3, "4" => "$"),
    k!(0, 4, "5" => "%"),
    k!(0, 5, "6" => "^"),
    k!(0, 6, "7" => "&"),
    k!(0, 7, "8" => "*"),
    k!(0, 8, "9" => "("),
    k!(0, 9, "0" => ")"),
    k!(0, 10, "_" => "-"),
    k!(0, 11, "+" => "="),
    k!(0, 12, "del"),
    k!(1, 0, "q" => "Q", "à" => "à"),
    k!(1, 1, "w" => "W", "ä" => "ä"),
    k!(1, 2, "e" => "E", "è" => "è"),
    k!(1, 3, "r" => "R", "ë" => "ë"),
    k!(1, 4, "t" => "T", "ì" => "ì"),
    k!(1, 5, "y" => "Y", "ï" => "ï"),
    k!(1, 6, "u" => "U", "ò" => "ò"),
    k!(1, 7, "i" => "I", "ö" => "ö"),
    k!(1, 8, "o" => "O", "ù" => "ù"),
    k!(1, 9, "p" => "P", "ü" => "ü"),
    k!(1, 10, "{" => "[", "¨" => "¨"),
    k!(1, 11, "}" => "]", "¿" => "¿"),
    k!(1, 12, 1 x 2, "ok"),
    k!(2, 0, "a" => "A", "á" => "á"),
    k!(2, 1, "s" => "S", "â" => "â"),
    k!(2, 2, "d" => "D", "é" => "é"),
    k!(2, 3, "f" => "F", "ê" => "ê"),
    k!(2, 4, "g" => "G", "í" => "í"),
    k!(2, 5, "h" => "H", "î" => "î"),
    k!(2, 6, "j" => "J", "ó" => "ó"),
    k!(2, 7, "k" => "K", "ô" => "ô"),
    k!(2, 8, "l" => "L", "ú" => "ú"),
    k!(2, 9, ";" => ":", "û" => "û"),
    k!(2, 10, "\"" => "'", "ñ" => "ñ"),
    k!(2, 11, "|" => "\\", "¡" => "¡"),
    k!(3, 0, "~" => "`", "€" => "€"),
    k!(3, 1, "z" => "Z"),
    k!(3, 2, "x" => "X"),
    k!(3, 3, "c" => "C"),
    k!(3, 4, "v" => "V"),
    k!(3, 5, "b" => "B"),
    k!(3, 6, "n" => "N"),
    k!(3, 7, "m" => "M"),
    k!(3, 8, "," => "<"),
    k!(3, 9, "." => ">"),
    k!(3, 10, "?" => "/"),
    k!(3, 11, 2 x 1, "alt"),
    k!(4, 0, 2 x 1, "shift"),
    k!(4, 2, 7 x 1, "space"),
    k!(4, 9, 2 x 1, "reset"),
    k!(4, 11, 2 x 1, "cancel"),
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
    Play {
        id: String,
        variant_id: String,
    },
    Add {
        id: String,
        variant_id: String,
    },
    ChooseStore {
        id: String,
        variant_id: String,
        owned: bool,
    },
    LoadStores {
        id: String,
    },
    LoadFilters,
    ApplyFilters,
}

struct FilterOption {
    id: String,
    label: String,
}

struct FilterGroup {
    label: String,
    options: Vec<FilterOption>,
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
    shift: bool,
    alt: bool,
}

pub struct Store {
    index: usize,
    scroll: f32,
    menu: Option<usize>,
    /// The open menu is the list of store variants, not Play / Add / Close.
    picking_store: bool,
    /// The store list is waiting for the full catalog game.
    stores_loading: bool,
    keyboard: Option<Keyboard>,
    filters: Vec<FilterGroup>,
    selected_filters: Vec<String>,
    filters_open: bool,
    filters_loading: bool,
    expanded_filter: Option<usize>,
    filter_index: usize,
}

fn grid_left() -> f32 {
    MARGIN
}

fn tile_origin(index: usize, scroll: f32) -> (f32, f32) {
    let column = (index % COLUMNS) as f32;
    let row = (index / COLUMNS) as f32;
    (
        grid_left() + column * (tile_size() + GAP),
        GRID_TOP + row * (tile_size() + GAP) - scroll,
    )
}

impl Store {
    pub fn new() -> Self {
        Self {
            index: 0,
            scroll: 0.0,
            menu: None,
            picking_store: false,
            stores_loading: false,
            keyboard: None,
            filters: Vec::new(),
            selected_filters: Vec::new(),
            filters_open: false,
            filters_loading: false,
            expanded_filter: None,
            filter_index: 0,
        }
    }

    /// Shows or hides the spinner inside the store list.
    pub fn note_store_load(&mut self, loading: bool) {
        self.stores_loading = loading && self.picking_store;
    }

    pub fn reset(&mut self) {
        self.index = 0;
        self.scroll = 0.0;
        self.menu = None;
        self.picking_store = false;
        self.stores_loading = false;
        self.keyboard = None;
    }

    pub fn set_filter_definitions(&mut self, value: &Value) {
        self.filters = value["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|group| {
                group["id"].as_str()?;
                let label = group["label"].as_str()?.to_owned();
                let options = group["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|option| {
                        if !option["expression"].is_object() && !option["filters"].is_array() {
                            return None;
                        }
                        Some(FilterOption {
                            id: option["id"].as_str()?.to_owned(),
                            label: option["label"].as_str()?.to_owned(),
                        })
                    })
                    .collect::<Vec<_>>();
                (!options.is_empty()).then_some(FilterGroup { label, options })
            })
            .collect();
        let features = self
            .filters
            .iter_mut()
            .find(|group| group.label.eq_ignore_ascii_case("Features"));
        let option = FilterOption {
            id: GAMEPAD_FILTER_ID.to_owned(),
            label: "Gamepad compatible".to_owned(),
        };
        if let Some(group) = features {
            if !group.options.iter().any(|existing| existing.id == GAMEPAD_FILTER_ID) {
                group.options.push(option);
            }
        } else {
            self.filters.insert(
                0,
                FilterGroup {
                    label: "Features".to_owned(),
                    options: vec![option],
                },
            );
        }
        self.filters_loading = false;
    }

    pub fn filter_load_failed(&mut self) {
        self.filters_loading = false;
    }

    pub fn selected_filter_ids(&self) -> Vec<String> {
        self.selected_filters
            .iter()
            .filter(|id| id.as_str() != GAMEPAD_FILTER_ID)
            .cloned()
            .collect()
    }

    pub fn gamepad_filter_selected(&self) -> bool {
        self.selected_filters
            .iter()
            .any(|id| id == GAMEPAD_FILTER_ID)
    }

    /// Repeats deletion only while the delete key remains selected.
    pub fn repeat_delete(&mut self) {
        let Some(keyboard) = self.keyboard.as_mut() else {
            return;
        };
        if key_at(keyboard.row, keyboard.column).text == "del" {
            keyboard.draft.pop();
        }
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
        if self.filters_open {
            return self.filter_action(action);
        }
        let keys = self
            .keyboard
            .as_mut()
            .map(|keyboard| keyboard_action(keyboard, action));
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
            Action::Y => {
                self.filters_open = true;
                if self.filters.is_empty() && !self.filters_loading {
                    self.filters_loading = true;
                    return Some(Request::LoadFilters);
                }
                return None;
            }
            Action::Options => {
                self.keyboard = Some(Keyboard {
                    row: 1,
                    column: 0,
                    draft: view.query.to_owned(),
                    shift: false,
                    alt: false,
                });
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

    fn filter_rows(&self) -> Vec<(Option<usize>, Option<usize>)> {
        let mut rows = vec![(None, None)];
        for (group_index, group) in self.filters.iter().enumerate() {
            rows.push((Some(group_index), None));
            if self.expanded_filter == Some(group_index) {
                rows.extend(
                    (0..group.options.len())
                        .map(|option_index| (Some(group_index), Some(option_index))),
                );
            }
        }
        rows
    }

    fn filter_action(&mut self, action: Action) -> Option<Request> {
        let rows = self.filter_rows();
        self.filter_index = self.filter_index.min(rows.len().saturating_sub(1));
        match action {
            Action::Back | Action::Escape | Action::Options | Action::Y => {
                self.filters_open = false;
                self.expanded_filter = None;
            }
            Action::Up => self.filter_index = self.filter_index.saturating_sub(1),
            Action::Down => {
                self.filter_index = (self.filter_index + 1).min(rows.len().saturating_sub(1))
            }
            Action::Left => {
                if let Some(group) = self.expanded_filter.take() {
                    self.filter_index = self
                        .filter_rows()
                        .iter()
                        .position(|row| *row == (Some(group), None))
                        .unwrap_or(0);
                } else {
                    self.filters_open = false;
                }
            }
            Action::Right => {
                if let Some((Some(group), None)) = rows.get(self.filter_index).copied() {
                    self.expanded_filter = Some(group);
                }
            }
            Action::Confirm => {
                let Some((group, option)) = rows.get(self.filter_index).copied() else {
                    return None;
                };
                match (group, option) {
                    (None, None) => {
                        let changed = !self.selected_filters.is_empty();
                        self.selected_filters.clear();
                        if changed {
                            return Some(Request::ApplyFilters);
                        }
                    }
                    (Some(group), None) => {
                        self.expanded_filter =
                            (self.expanded_filter != Some(group)).then_some(group);
                    }
                    (Some(group), Some(option)) => {
                        let id = self.filters[group].options[option].id.clone();
                        if let Some(index) = self
                            .selected_filters
                            .iter()
                            .position(|selected| selected == &id)
                        {
                            self.selected_filters.remove(index);
                        } else {
                            self.selected_filters.push(id);
                        }
                        return Some(Request::ApplyFilters);
                    }
                    (None, Some(_)) => {}
                }
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
        if self.stores_loading && self.picking_store {
            if matches!(action, Action::Back | Action::Escape) {
                self.stores_loading = false;
                self.picking_store = false;
                self.menu = Some(2);
            }
            return None;
        }
        let rows = menu_rows(game, self.picking_store);
        match action {
            Action::Up => self.menu = Some(row.saturating_sub(1)),
            Action::Down => self.menu = Some((row + 1).min(rows.len().saturating_sub(1))),
            Action::Back | Action::Escape | Action::Select(true) => {
                if self.picking_store {
                    self.picking_store = false;
                    self.menu = Some(2);
                } else {
                    self.menu = None;
                }
            }
            Action::Confirm => {
                let (kind, enabled) = rows
                    .get(row)
                    .map(|item| (item.kind, item.enabled))
                    .unwrap_or((MenuKind::Back, true));
                if !enabled {
                    return None;
                }
                let id = game.id.clone();
                let variant_id = game.variant_id.clone();
                return match kind {
                    MenuKind::Play => {
                        self.menu = None;
                        Some(Request::Play { id, variant_id })
                    }
                    MenuKind::Add => {
                        self.menu = None;
                        Some(Request::Add { id, variant_id })
                    }
                    MenuKind::OpenStores => {
                        self.picking_store = true;
                        self.stores_loading = true;
                        self.menu = Some(0);
                        Some(Request::LoadStores { id })
                    }
                    MenuKind::Store(index) => {
                        self.menu = None;
                        self.picking_store = false;
                        game.versions
                            .get(index)
                            .map(|version| Request::ChooseStore {
                                id,
                                variant_id: version.id.clone(),
                                owned: version.in_library,
                            })
                    }
                    MenuKind::Back => {
                        self.picking_store = false;
                        self.menu = Some(2);
                        None
                    }
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
        let content = rows * (tile_size() + GAP) - GAP;
        let view = grid_bottom() - GRID_TOP;
        let row_y = (self.index / COLUMNS) as f32 * (tile_size() + GAP);
        let target = (row_y - (view - tile_size()) / 2.0).clamp(0.0, (content - view).max(0.0));
        let blend = (elapsed.as_secs_f32() * 16.0).min(1.0);
        self.scroll += (target - self.scroll) * blend;
        if (target - self.scroll).abs() < 0.25 {
            self.scroll = target;
        }
    }

    pub fn draw(&self, painter: &mut Painter, view: &View) {
        painter.clip(0.0, 0.0, canvas().0, separator_y());
        self.draw_grid(painter, view);
        painter.clear_clip();
        painter.fill(
            0.0,
            separator_y(),
            canvas().0,
            canvas().1 - separator_y(),
            0.0,
            BACKGROUND,
        );
        theme::draw_footer(painter, &footer(view), &self.hints(view));
        if self.menu.is_some() {
            self.draw_menu(painter, view);
        }
        if let Some(keyboard) = &self.keyboard {
            draw_keyboard(painter, keyboard);
        }
        if self.filters_open {
            self.draw_filters(painter);
        }
    }

    fn hints(&self, view: &View) -> Vec<(&'static str, &'static str)> {
        if self.filters_open {
            return vec![("A", "Select"), ("↑↓", "Navigate")];
        }
        if self.keyboard.is_some() {
            return vec![("X", "Shift"), ("A", "Type")];
        }
        if self.stores_loading {
            return vec![("B", "Back")];
        }
        if self.picking_store {
            return vec![("A", "Select"), ("B", "Back")];
        }
        if self.menu.is_some() {
            return vec![("A", "Select")];
        }
        if view.games.is_empty() && matches!(view.status, Status::Ready) {
            return vec![("X", "Search"), ("B", "Home")];
        }
        if view.games.is_empty() && matches!(view.status, Status::Failed(_)) {
            return vec![("A", "Retry"), ("B", "Home")];
        }
        vec![
            ("X", "Search"),
            ("A", "Options"),
            ("Y", "Filters"),
            ("B", "Home"),
        ]
    }

    fn draw_filters(&self, painter: &mut Painter) {
        let width = 320.0_f32.min(canvas().0);
        let rows = self.filter_rows();
        let row_h = 44.0;
        let height = canvas().1;
        let x = canvas().0 - width;
        let panel_y = 0.0;
        painter.fill(x, panel_y, width, height, 0.0, BACKGROUND);
        painter.text(x + 24.0, panel_y + 20.0, 20.0, Weight::Black, "Filters", TITLE);
        painter.stroke(x + 12.0, panel_y + 54.0, width - 24.0, 1.0, 0.0, 1.0, SEPARATOR);
        if self.filters_loading {
            let label = "Loading filters…";
            let text_width = painter.measure(label, 15.0, Weight::Bold);
            painter.text(
                x + (width - text_width) / 2.0,
                panel_y + 82.0,
                15.0,
                Weight::Bold,
                label,
                MUTED,
            );
            return;
        }
        if self.filters.is_empty() {
            painter.text(
                x + 24.0,
                panel_y + 82.0,
                14.0,
                Weight::Bold,
                "Filters unavailable",
                MUTED,
            );
            return;
        }
        let top = panel_y + 64.0;
        let available = height - 76.0;
        let visible_rows = (available / row_h).floor().max(1.0) as usize;
        let centered_first = self.filter_index.saturating_sub(visible_rows / 2);
        let first = centered_first.min(rows.len().saturating_sub(visible_rows));
        painter.clip(x + 8.0, top, width - 16.0, available);
        for (visible, (group_index, option_index)) in rows.iter().enumerate().skip(first) {
            let y = top + (visible - first) as f32 * row_h;
            if y > canvas().1 {
                break;
            }
            let selected = visible == self.filter_index;
            let (label, indent, checked, expandable) = match (*group_index, *option_index) {
                (None, _) => ("Clear all".to_owned(), 24.0, false, false),
                (Some(group), None) => {
                    let group = &self.filters[group];
                    (group.label.clone(), 24.0, false, true)
                }
                (Some(group), Some(option)) => {
                    let option = &self.filters[group].options[option];
                    (
                        option.label.clone(),
                        40.0,
                        self.selected_filters.contains(&option.id),
                        false,
                    )
                }
            };
            if selected {
                painter.fill(
                    x + 8.0,
                    y + 7.0,
                    3.0,
                    row_h - 14.0,
                    1.5,
                    FOCUS,
                );
            }
            let label_color = if selected { FOCUS } else { TEXT };
            painter.text_fit(
                x + indent,
                y + 15.0,
                14.0,
                Weight::Bold,
                &label,
                width - indent - 54.0,
                label_color,
            );
            if checked {
                painter.fill(x + width - 34.0, y + 16.0, 12.0, 12.0, 6.0, FOCUS);
            } else if option_index.is_some() {
                painter.stroke(x + width - 34.0, y + 16.0, 12.0, 12.0, 6.0, 1.5, with_alpha(TEXT, 0.55));
            } else if expandable {
                let expanded = self.expanded_filter == *group_index;
                if expanded {
                    painter.fill(x + width - 34.0, y + 16.0, 12.0, 12.0, 6.0, FOCUS);
                } else {
                    painter.stroke(
                        x + width - 34.0,
                        y + 16.0,
                        12.0,
                        12.0,
                        6.0,
                        1.5,
                        with_alpha(TEXT, 0.55),
                    );
                }
            }
            painter.stroke(
                x + 8.0,
                y + row_h - 1.0,
                width - 16.0,
                1.0,
                0.0,
                1.0,
                SEPARATOR,
            );
        }
        painter.clear_clip();
    }

    fn draw_grid(&self, painter: &mut Painter, view: &View) {
        if view.games.is_empty() {
            match view.status {
                Status::Loading => {
                    let offset_y = (canvas().1 - 480.0) / 2.0;
                    theme::draw_spinner(
                        painter,
                        canvas().0 / 2.0,
                        230.0 + offset_y,
                        20.0,
                        view.seconds,
                    );
                    let label = "Loading the store…";
                    let width = painter.measure(label, 16.0, Weight::Bold);
                    painter.text(
                        (canvas().0 - width) / 2.0,
                        268.0 + offset_y,
                        16.0,
                        Weight::Bold,
                        label,
                        MUTED,
                    );
                }
                Status::Failed(_) => {
                    painter.text(
                        EDGE,
                        200.0 + (canvas().1 - 480.0) / 2.0,
                        18.0,
                        Weight::Bold,
                        "The store didn't load",
                        ERROR,
                    );
                }
                Status::Ready => {
                    let label = if view.query.is_empty() {
                        "The store is empty"
                    } else {
                        "No games match"
                    };
                    let width = painter.measure(label, 18.0, Weight::Bold);
                    painter.text(
                        (canvas().0 - width) / 2.0,
                        220.0 + (canvas().1 - 480.0) / 2.0,
                        18.0,
                        Weight::Bold,
                        label,
                        MUTED,
                    );
                }
            }
            return;
        }
        let focused = self.keyboard.is_none() && self.menu.is_none();
        for (index, game) in view.games.iter().enumerate() {
            let (x, y) = tile_origin(index, self.scroll);
            if y > grid_bottom() || y + tile_size() < GRID_TOP {
                continue;
            }
            draw_tile(painter, game, view.artwork, x, y);
            if focused && index == self.index {
                painter.stroke(
                    x - 6.0,
                    y - 6.0,
                    tile_size() + 12.0,
                    tile_size() + 12.0,
                    8.0,
                    3.5,
                    FOCUS,
                );
            }
        }
    }

    fn draw_menu(&self, painter: &mut Painter, view: &View) {
        let Some(game) = view.games.get(self.index) else {
            return;
        };
        if self.stores_loading && self.picking_store {
            draw_store_loading(painter, view.seconds);
            return;
        }
        let rows = menu_rows(game, self.picking_store);
        let (width, row_h) = (460.0, 46.0);
        let x = (canvas().0 - width) / 2.0;
        let height = 72.0 + rows.len() as f32 * row_h;
        let y = ((canvas().1 - height) / 2.0).max(24.0);
        theme::draw_panel(painter, x, y, width, height);
        let heading = if self.picking_store {
            "Select store"
        } else {
            game.title.as_str()
        };
        painter.text_fit(
            x + 24.0,
            y + 18.0,
            20.0,
            Weight::Black,
            heading,
            width - 48.0,
            TEXT,
        );
        let selected = self.menu.unwrap_or(0);
        for (index, item) in rows.iter().enumerate() {
            let row_y = y + 64.0 + index as f32 * row_h;
            if index == selected {
                painter.fill(
                    x + 16.0,
                    row_y,
                    width - 32.0,
                    row_h - 8.0,
                    12.0,
                    if item.enabled { FOCUS } else { BUTTON },
                );
            }
            let color = if !item.enabled {
                MUTED
            } else if index == selected {
                BACKGROUND
            } else {
                TEXT
            };
            painter.text_fit(
                x + 32.0,
                row_y + 8.0,
                16.0,
                Weight::ExtraBold,
                &item.label,
                width - 64.0,
                color,
            );
        }
    }
}

fn draw_store_loading(painter: &mut Painter, seconds: f32) {
    let (width, height) = (460.0, 180.0);
    let x = (canvas().0 - width) / 2.0;
    let y = (canvas().1 - height) / 2.0;
    theme::draw_panel(painter, x, y, width, height);
    painter.text(
        x + 24.0,
        y + 18.0,
        20.0,
        Weight::Black,
        "Select store",
        TEXT,
    );
    theme::draw_spinner(painter, x + width / 2.0, y + 96.0, 16.0, seconds);
    let label = "Loading stores…";
    let label_width = painter.measure(label, 15.0, Weight::Bold);
    painter.text(
        x + (width - label_width) / 2.0,
        y + 132.0,
        15.0,
        Weight::Bold,
        label,
        MUTED,
    );
}

struct MenuItem {
    label: String,
    kind: MenuKind,
    enabled: bool,
}

#[derive(Clone, Copy)]
enum MenuKind {
    Play,
    Add,
    OpenStores,
    Store(usize),
    Back,
}

fn menu_rows(game: &Game, picking_store: bool) -> Vec<MenuItem> {
    if picking_store {
        let any_default = game.versions.iter().any(|version| version.selected);
        let mut rows = game
            .versions
            .iter()
            .enumerate()
            .map(|(index, version)| {
                let name = if version.store.is_empty() {
                    "Store".to_owned()
                } else {
                    version.store.clone()
                };
                let label = if version.selected || (!any_default && version.id == game.variant_id) {
                    format!("{name} · default")
                } else {
                    name
                };
                MenuItem {
                    label,
                    kind: MenuKind::Store(index),
                    enabled: true,
                }
            })
            .collect::<Vec<_>>();
        if rows.is_empty() {
            rows.push(MenuItem {
                label: "Loading stores…".into(),
                kind: MenuKind::Back,
                enabled: true,
            });
        }
        rows.push(MenuItem {
            label: "Back".into(),
            kind: MenuKind::Back,
            enabled: true,
        });
        return rows;
    }
    let playable = !game.id.is_empty() && !game.variant_id.is_empty();
    let current = game
        .versions
        .iter()
        .find(|version| version.selected)
        .map(|version| version.store.as_str())
        .filter(|store| !store.is_empty())
        .unwrap_or(game.store.as_str());
    let select_label = if current.is_empty() {
        "Select store".into()
    } else {
        format!("Select store · {current}")
    };
    vec![
        MenuItem {
            label: "Play".into(),
            kind: MenuKind::Play,
            enabled: playable,
        },
        MenuItem {
            label: if game.in_library {
                "In your library".into()
            } else {
                "Add to library".into()
            },
            kind: MenuKind::Add,
            enabled: playable && !game.in_library,
        },
        MenuItem {
            label: select_label,
            kind: MenuKind::OpenStores,
            enabled: !game.id.is_empty(),
        },
    ]
}

fn footer(view: &View) -> String {
    match view.status {
        Status::Loading if view.games.is_empty() => "Loading the store…".to_owned(),
        Status::Loading => "Loading more games…".to_owned(),
        Status::Failed(message) if view.games.is_empty() => message.clone(),
        Status::Ready | Status::Failed(_) if !view.query.is_empty() => {
            format!("Search · {}", view.query)
        }
        _ if view.has_more => format!("Store · {} games · more below", view.games.len()),
        _ => format!("Store · {} games", view.games.len()),
    }
}

fn draw_tile(painter: &mut Painter, game: &Game, artwork: &Artwork, x: f32, y: f32) {
    match game.artwork.as_deref().and_then(|url| artwork.texture(url)) {
        Some(texture) => painter.texture(texture, x, y, tile_size(), tile_size(), 5.0, WHITE),
        None => {
            painter.fill(x, y, tile_size(), tile_size(), 5.0, game.color);
            let initial: String = game.title.chars().take(1).collect();
            let width = painter.measure(&initial, 72.0, Weight::Black);
            painter.text(
                x + (tile_size() - width) / 2.0,
                y + 24.0,
                72.0,
                Weight::Black,
                &initial,
                with_alpha(WHITE, 0.14),
            );
        }
    }
    painter.gradient(
        x,
        y,
        tile_size(),
        tile_size(),
        5.0,
        y + tile_size() * 0.42,
        with_alpha(BLACK, 0.0),
        with_alpha(BLACK, 0.72),
    );
    let caption = if game.in_library {
        "In your library"
    } else {
        game.store.as_str()
    };
    painter.text_fit(
        x + 10.0,
        y + tile_size() - 36.0,
        14.0,
        Weight::Black,
        &game.title,
        tile_size() - 20.0,
        WHITE,
    );
    painter.text_fit(
        x + 10.0,
        y + tile_size() - 18.0,
        11.0,
        Weight::Bold,
        caption,
        tile_size() - 20.0,
        with_alpha(WHITE, 0.75),
    );
}

fn covers(key: &Key, row: usize, col: usize) -> bool {
    row >= key.row as usize
        && row < key.row as usize + key.h as usize
        && col >= key.col as usize
        && col < key.col as usize + key.w as usize
}

fn key_at(row: usize, col: usize) -> &'static Key {
    KEYS.iter()
        .find(|key| covers(key, row, col))
        .expect("every keyboard cell is a key")
}

fn face<'a>(key: &'a Key, shift: bool, alt: bool) -> &'a str {
    if alt {
        let swapped = if shift { key.alt_shift } else { key.alt };
        if !swapped.is_empty() {
            return swapped;
        }
    }
    if shift && !key.upper.is_empty() {
        key.upper
    } else {
        key.text
    }
}

fn key_box(origin: (f32, f32), key: &Key) -> (f32, f32, f32, f32) {
    let x = origin.0 + key.col as f32 * (CELL + CELL_GAP);
    let y = origin.1 + key.row as f32 * (CELL + CELL_GAP);
    let width = key.w as f32 * CELL + (key.w - 1) as f32 * CELL_GAP;
    let height = key.h as f32 * CELL + (key.h - 1) as f32 * CELL_GAP;
    (x, y, width, height)
}

fn move_cursor(row: usize, col: usize, action: Action) -> (usize, usize) {
    let key = key_at(row, col);
    match action {
        Action::Left => (
            row,
            if key.col == 0 {
                KEY_COLS - 1
            } else {
                key.col as usize - 1
            },
        ),
        Action::Right => {
            let next = key.col as usize + key.w as usize;
            (row, if next >= KEY_COLS { 0 } else { next })
        }
        Action::Up if key.row > 0 => (key.row as usize - 1, col),
        Action::Down if key.row as usize + (key.h as usize) < KEY_ROWS => {
            (key.row as usize + key.h as usize, col)
        }
        _ => (row, col),
    }
}

fn draw_keyboard(painter: &mut Painter, keyboard: &Keyboard) {
    let grid_w = KEY_COLS as f32 * CELL + (KEY_COLS - 1) as f32 * CELL_GAP;
    let grid_h = KEY_ROWS as f32 * CELL + (KEY_ROWS - 1) as f32 * CELL_GAP;
    let pad_x = 14.0;
    let width = grid_w + pad_x * 2.0;
    let height = 14.0 + 36.0 + 10.0 + grid_h + 12.0;
    let x = (canvas().0 - width) / 2.0;
    let y = (separator_y() - 12.0 - height).max(64.0);
    theme::draw_panel(painter, x, y, width, height);

    let field_y = y + 14.0;
    painter.fill(x + pad_x, field_y, grid_w, 36.0, 6.0, BUTTON);
    if keyboard.draft.is_empty() {
        painter.text(
            x + pad_x + 12.0,
            field_y + 8.0,
            16.0,
            Weight::Bold,
            "Type a game name",
            MUTED,
        );
    } else {
        let text_x = x + pad_x + 12.0;
        let drawn = painter.text(
            text_x,
            field_y + 8.0,
            16.0,
            Weight::Bold,
            &keyboard.draft,
            TEXT,
        );
        painter.fill(text_x + drawn + 2.0, field_y + 9.0, 2.0, 18.0, 0.0, FOCUS);
    }

    let origin = (x + pad_x, field_y + 46.0);
    let selected = key_at(keyboard.row, keyboard.column);
    for key in KEYS {
        let (key_x, key_y, key_w, key_h) = key_box(origin, key);
        let focused = key.row == selected.row && key.col == selected.col;
        let latched =
            (key.text == "shift" && keyboard.shift) || (key.text == "alt" && keyboard.alt);
        let fill = if focused {
            FOCUS
        } else if latched {
            rgb(0x1C6C78)
        } else {
            BUTTON
        };
        painter.fill(key_x, key_y, key_w, key_h, 4.0, fill);
        let text = face(key, keyboard.shift, keyboard.alt);
        let mut size = if text.chars().count() == 1 || key.h > 1 {
            20.0
        } else {
            14.0
        };
        let limit = (key_w - 8.0).max(8.0);
        while size > 9.0 && painter.measure(text, size, Weight::ExtraBold) > limit {
            size -= 1.0;
        }
        let text_width = painter.measure(text, size, Weight::ExtraBold);
        let text_y = key_y + (key_h - size * 1.52) / 2.0;
        let ink = if focused { BACKGROUND } else { TEXT };
        painter.text(
            key_x + (key_w - text_width) / 2.0,
            text_y,
            size,
            Weight::ExtraBold,
            text,
            ink,
        );
    }
}

fn keyboard_action(keyboard: &mut Keyboard, action: Action) -> KeyResult {
    match action {
        Action::Left | Action::Right | Action::Up | Action::Down => {
            let (row, column) = move_cursor(keyboard.row, keyboard.column, action);
            keyboard.row = row;
            keyboard.column = column;
        }
        Action::Back | Action::Escape => return KeyResult::Close,
        Action::Options => keyboard.shift = !keyboard.shift,
        // D-pad left is also delivered as Start on the raw joystick path, so
        // directions and Start stay in the grid. OK submits the search.
        Action::Confirm => return press_key(keyboard),
        _ => {}
    }
    KeyResult::Stay
}

fn press_key(keyboard: &mut Keyboard) -> KeyResult {
    let key = key_at(keyboard.row, keyboard.column);
    match key.text {
        "del" => {
            keyboard.draft.pop();
        }
        "space" if keyboard.draft.chars().count() < MAX_QUERY => keyboard.draft.push(' '),
        "ok" => return KeyResult::Submit(keyboard.draft.trim().to_owned()),
        "cancel" => return KeyResult::Close,
        "reset" => keyboard.draft.clear(),
        "shift" => keyboard.shift = !keyboard.shift,
        "alt" => {
            if keyboard.shift {
                keyboard.shift = false;
            }
            keyboard.alt = !keyboard.alt;
        }
        _ => {
            let text = face(key, keyboard.shift, keyboard.alt);
            if keyboard.draft.chars().count() + text.chars().count() <= MAX_QUERY {
                keyboard.draft.push_str(text);
            }
        }
    }
    KeyResult::Stay
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_covers_every_cell_once() {
        for row in 0..KEY_ROWS {
            for col in 0..KEY_COLS {
                let hits = KEYS.iter().filter(|key| covers(key, row, col)).count();
                assert_eq!(hits, 1, "cell {row},{col} covered {hits} times");
            }
        }
    }

    #[test]
    fn directions_follow_the_emulationstation_grid() {
        assert_eq!(key_at(1, 12).text, "ok");
        let (row, col) = move_cursor(1, 11, Action::Right);
        assert_eq!(key_at(row, col).text, "ok");
        let (row, col) = move_cursor(1, 12, Action::Down);
        assert_eq!((row, col), (3, 12));
        assert_eq!(key_at(row, col).text, "alt");
        let (row, col) = move_cursor(1, 5, Action::Down);
        assert_eq!(key_at(row, col).text, "h");
        let (row, col) = move_cursor(3, 5, Action::Down);
        assert_eq!(key_at(row, col).text, "space");
        let (row, col) = move_cursor(4, 5, Action::Up);
        assert_eq!(key_at(row, col).text, "b");
        let (row, col) = move_cursor(4, 2, Action::Right);
        assert_eq!(key_at(row, col).text, "reset");
        let (row, col) = move_cursor(4, 0, Action::Left);
        assert_eq!(key_at(row, col).text, "cancel");
        let (row, col) = move_cursor(0, 0, Action::Left);
        assert_eq!(key_at(row, col).text, "del");
        let (row, col) = move_cursor(0, 12, Action::Right);
        assert_eq!(key_at(row, col).text, "1");
        let (row, col) = move_cursor(2, 11, Action::Right);
        assert_eq!(key_at(row, col).text, "ok");
        let (row, col) = move_cursor(2, 12, Action::Left);
        assert_eq!((row, col), (2, 11));
        assert_eq!(key_at(row, col).text, "|");
        let (row, col) = move_cursor(2, 12, Action::Up);
        assert_eq!(key_at(row, col).text, "del");
    }

    #[test]
    fn moving_left_keeps_the_keyboard_open() {
        let mut keyboard = Keyboard {
            row: 1,
            column: 0,
            draft: "zelda".into(),
            shift: false,
            alt: false,
        };
        assert!(matches!(
            keyboard_action(&mut keyboard, Action::Left),
            KeyResult::Stay
        ));
        assert_eq!(keyboard.draft, "zelda");
        assert_eq!((keyboard.row, keyboard.column), (1, 12));
        assert!(matches!(
            keyboard_action(&mut keyboard, Action::Start(true)),
            KeyResult::Stay
        ));
        assert_eq!(keyboard.draft, "zelda");
    }

    #[test]
    fn shift_and_alt_change_the_typed_character() {
        let mut keyboard = Keyboard {
            row: 1,
            column: 0,
            draft: String::new(),
            shift: false,
            alt: false,
        };
        assert!(matches!(press_key(&mut keyboard), KeyResult::Stay));
        assert_eq!(keyboard.draft, "q");
        keyboard.shift = true;
        press_key(&mut keyboard);
        assert_eq!(keyboard.draft, "qQ");
        keyboard.shift = false;
        keyboard.alt = true;
        press_key(&mut keyboard);
        assert_eq!(keyboard.draft, "qQà");
        keyboard.column = 12;
        assert!(matches!(press_key(&mut keyboard), KeyResult::Submit(query) if query == "qQà"));
    }
}

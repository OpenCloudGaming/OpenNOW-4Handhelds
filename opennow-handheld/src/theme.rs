//! Colours, metrics, and chrome shared by every screen, following the Qt
//! console theme: the header, button hints, panels, and dialogs.

use crate::assets::Image;
use crate::gl::{Color, Painter, with_alpha};
use crate::text::Weight;
use std::sync::atomic::{AtomicU32, Ordering};

const DESIGN_WIDTH: f32 = 640.0;
const DESIGN_HEIGHT: f32 = 480.0;
static CANVAS_WIDTH: AtomicU32 = AtomicU32::new(DESIGN_WIDTH.to_bits());
static CANVAS_HEIGHT: AtomicU32 = AtomicU32::new(DESIGN_HEIGHT.to_bits());

pub fn canvas() -> (f32, f32) {
    (
        f32::from_bits(CANVAS_WIDTH.load(Ordering::Relaxed)),
        f32::from_bits(CANVAS_HEIGHT.load(Ordering::Relaxed)),
    )
}

/// Extend the design canvas to match the display while retaining a uniform UI scale.
pub fn set_canvas_for_display(width: i32, height: i32) {
    if width <= 0 || height <= 0 {
        return;
    }
    let scale = (width as f32 / DESIGN_WIDTH).min(height as f32 / DESIGN_HEIGHT);
    CANVAS_WIDTH.store((width as f32 / scale).to_bits(), Ordering::Relaxed);
    CANVAS_HEIGHT.store((height as f32 / scale).to_bits(), Ordering::Relaxed);
}

pub const fn rgb(hex: u32) -> Color {
    [
        ((hex >> 16) & 0xFF) as f32 / 255.0,
        ((hex >> 8) & 0xFF) as f32 / 255.0,
        (hex & 0xFF) as f32 / 255.0,
        1.0,
    ]
}

pub const BACKGROUND: Color = rgb(0x17191C);
pub const TEXT: Color = rgb(0xF2F4F6);
pub const MUTED: Color = rgb(0x9AA0A6);
pub const TITLE: Color = rgb(0x76EF36);
pub const FOCUS: Color = rgb(0x76EF36);
pub const DOCK_FACE: Color = rgb(0xF4F4F5);
pub const SEPARATOR: Color = rgb(0x3A3E46);
pub const ADD_TILE: Color = rgb(0x2A2D33);
pub const PANEL: Color = rgb(0x22252A);
pub const BUTTON: Color = rgb(0x34383F);
pub const ERROR: Color = rgb(0xFF8A80);
pub const WHITE: Color = [1.0, 1.0, 1.0, 1.0];
pub const BLACK: Color = [0.0, 0.0, 0.0, 1.0];

pub const EDGE: f32 = 20.0;
pub fn separator_y() -> f32 {
    canvas().1 - 56.0
}

/// Clock text and battery level, refreshed by the app every few seconds.
#[derive(Default)]
pub struct Clock {
    pub time: String,
    pub battery: Option<u8>,
}

pub fn draw_header(painter: &mut Painter, clock: &Clock) {
    painter.image(Image::Logo, EDGE, 22.0, 52.0, 30.0, WHITE);
    let name_width = painter.text(
        EDGE + 62.0,
        28.0,
        16.0,
        Weight::Black,
        "OpenNOW-4Handhelds",
        TEXT,
    );
    painter.text(
        EDGE + 62.0 + name_width + 8.0,
        31.0,
        13.0,
        Weight::Bold,
        "v0.1.0",
        MUTED,
    );

    let mut right = canvas().0 - EDGE;
    if let Some(percent) = clock.battery {
        let (width, height, top) = (24.0, 13.0, 31.0);
        let body = right - width - 3.0;
        painter.fill(body + width + 0.5, top + 3.5, 2.5, 6.0, 1.0, TEXT);
        painter.stroke(body, top, width, height, 3.5, 2.0, TEXT);
        let level = (width - 7.0) * percent as f32 / 100.0;
        painter.fill(
            body + 3.5,
            top + 3.5,
            level.max(1.5),
            height - 7.0,
            1.5,
            TEXT,
        );
        let label = format!("{percent}%");
        let label_width = painter.measure(&label, 16.0, Weight::Bold);
        right = body - 8.0 - label_width;
        painter.text(right, 26.0, 16.0, Weight::Bold, &label, TEXT);
        right -= 18.0;
    }
    if !clock.time.is_empty() {
        let width = painter.measure(&clock.time, 18.0, Weight::Bold);
        painter.text(right - width, 25.0, 18.0, Weight::Bold, &clock.time, TEXT);
    }
}

/// The footer: a separator, a status line on the left, and button hints on
/// the right, listed left to right.
pub fn draw_footer(painter: &mut Painter, status: &str, hints: &[(&str, &str)]) {
    painter.fill(0.0, separator_y(), canvas().0, 1.0, 0.0, SEPARATOR);
    let y = separator_y() + 15.0;
    let mut right = canvas().0 - EDGE;
    for (glyph, label) in hints.iter().rev() {
        let label_width = painter.measure(label, 15.0, Weight::Bold);
        right -= label_width;
        painter.text(right, y + 1.0, 15.0, Weight::Bold, label, TEXT);
        right -= 8.0 + 22.0;
        draw_button_glyph(painter, right, y, glyph);
        right -= 20.0;
    }
    painter.text_fit(
        EDGE,
        separator_y() + 18.0,
        13.0,
        Weight::Bold,
        status,
        right - EDGE,
        MUTED,
    );
}

pub fn draw_centered_footer(painter: &mut Painter, status: &str) {
    painter.fill(0.0, separator_y(), canvas().0, 1.0, 0.0, SEPARATOR);
    let width = painter.measure(status, 13.0, Weight::Bold);
    painter.text_fit(
        (canvas().0 - width) / 2.0,
        separator_y() + 18.0,
        13.0,
        Weight::Bold,
        status,
        canvas().0 - EDGE * 2.0,
        MUTED,
    );
}

/// Draws `text` on as many lines as the words need. A line that is still too
/// wide is shortened with an ellipsis.
fn draw_wrapped(
    painter: &mut Painter,
    x: f32,
    y: f32,
    size: f32,
    weight: Weight,
    text: &str,
    max_width: f32,
    line_gap: f32,
    color: Color,
) {
    let mut line = String::new();
    let mut line_y = y;
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_owned()
        } else {
            format!("{line} {word}")
        };
        if line.is_empty() || painter.measure(&candidate, size, weight) <= max_width {
            line = candidate;
            continue;
        }
        painter.text(x, line_y, size, weight, &line, color);
        line_y += line_gap;
        line = word.to_owned();
    }
    if !line.is_empty() {
        painter.text_fit(x, line_y, size, weight, &line, max_width, color);
    }
}

fn draw_button_glyph(painter: &mut Painter, x: f32, y: f32, glyph: &str) {
    painter.fill(x, y, 22.0, 22.0, 11.0, TEXT);
    let width = painter.measure(glyph, 13.0, Weight::Black);
    painter.text(
        x + (22.0 - width) / 2.0,
        y + 3.0,
        13.0,
        Weight::Black,
        glyph,
        BACKGROUND,
    );
}

/// Dims the screen and draws an empty modal panel.
pub fn draw_panel(painter: &mut Painter, x: f32, y: f32, width: f32, height: f32) {
    painter.fill(
        0.0,
        0.0,
        canvas().0,
        canvas().1,
        0.0,
        with_alpha(BLACK, 0.6),
    );
    painter.shadow(
        x,
        y + 8.0,
        width,
        height,
        18.0,
        24.0,
        with_alpha(BLACK, 0.5),
    );
    painter.fill(x, y, width, height, 18.0, PANEL);
    painter.stroke(x, y, width, height, 18.0, 1.0, SEPARATOR);
}

/// A two-button confirmation. Cancel is on the left and focused by default.
pub fn draw_confirm(
    painter: &mut Painter,
    title: &str,
    body: &str,
    confirm: &str,
    confirm_selected: bool,
) {
    let (width, height) = (380.0, 190.0);
    let x = (canvas().0 - width) / 2.0;
    let y = (canvas().1 - height) / 2.0;
    draw_panel(painter, x, y, width, height);
    painter.text(x + 26.0, y + 24.0, 24.0, Weight::Black, title, TEXT);
    draw_wrapped(
        painter,
        x + 26.0,
        y + 62.0,
        15.0,
        Weight::SemiBold,
        body,
        width - 52.0,
        20.0,
        MUTED,
    );
    for (index, label) in ["Cancel", confirm].iter().enumerate() {
        let selected = (index == 1) == confirm_selected;
        let (button_x, button_y, button_width) =
            (x + 26.0 + index as f32 * 176.0, y + 118.0, 152.0);
        painter.fill(
            button_x,
            button_y,
            button_width,
            44.0,
            22.0,
            if selected { FOCUS } else { BUTTON },
        );
        let label_width = painter.measure(label, 16.0, Weight::ExtraBold);
        let color = if selected { BACKGROUND } else { TEXT };
        painter.text(
            button_x + (button_width - label_width) / 2.0,
            button_y + 11.0,
            16.0,
            Weight::ExtraBold,
            label,
            color,
        );
    }
}

pub fn draw_session_menu(painter: &mut Painter, selected: usize, time_left: &str) {
    let rows = ["End session", "Exit OpenNOW", "Mouse mode", "Stats"];
    let (width, row_h) = (380.0, 56.0);
    let x = (canvas().0 - width) / 2.0;
    let header_h = 64.0;
    let bottom_padding = 12.0;
    let height = header_h + rows.len() as f32 * row_h + bottom_padding;
    let y = (canvas().1 - height) / 2.0;
    draw_panel(painter, x, y, width, height);
    let remaining = format!("Session time remaining: {time_left}");
    let remaining_width = painter.measure(&remaining, 16.0, Weight::SemiBold);
    painter.text(
        x + (width - remaining_width) / 2.0,
        y + 28.0,
        16.0,
        Weight::SemiBold,
        &remaining,
        MUTED,
    );
    for (index, label) in rows.iter().enumerate() {
        let focused = index == selected;
        let button_y = y + header_h + index as f32 * row_h;
        let button_width = width - 52.0;
        painter.fill(
            x + 26.0,
            button_y,
            button_width,
            48.0,
            22.0,
            if focused { FOCUS } else { BUTTON },
        );
        let label_width = painter.measure(label, 16.0, Weight::ExtraBold);
        painter.text(
            x + 26.0 + (button_width - label_width) / 2.0,
            button_y + 13.0,
            16.0,
            Weight::ExtraBold,
            label,
            if focused { BACKGROUND } else { TEXT },
        );
    }
}
pub fn draw_toast(painter: &mut Painter, message: &str, remaining: f32) {
    let alpha = (remaining / 0.3).clamp(0.0, 1.0);
    let width = painter.measure(message, 15.0, Weight::Bold) + 40.0;
    let x = (canvas().0 - width) / 2.0;
    painter.shadow(
        x,
        22.0,
        width,
        36.0,
        18.0,
        10.0,
        with_alpha(BLACK, 0.45 * alpha),
    );
    painter.fill(x, 20.0, width, 36.0, 18.0, with_alpha(PANEL, alpha));
    painter.stroke(
        x,
        20.0,
        width,
        36.0,
        18.0,
        1.0,
        with_alpha(SEPARATOR, alpha),
    );
    painter.text(
        x + 20.0,
        28.0,
        15.0,
        Weight::Bold,
        message,
        with_alpha(TEXT, alpha),
    );
}

/// Draws a slowly rotating ring of dots as a busy indicator.
pub fn draw_spinner(painter: &mut Painter, cx: f32, cy: f32, radius: f32, seconds: f32) {
    let count = 8;
    let head = (seconds * 10.0) as usize % count;
    for index in 0..count {
        let angle = index as f32 / count as f32 * std::f32::consts::TAU;
        let age = (head + count - index) % count;
        let alpha = 1.0 - age as f32 / count as f32 * 0.85;
        let dot = radius * 0.28;
        let (x, y) = (cx + angle.sin() * radius, cy - angle.cos() * radius);
        painter.fill(
            x - dot / 2.0,
            y - dot / 2.0,
            dot,
            dot,
            dot / 2.0,
            with_alpha(FOCUS, alpha),
        );
    }
}

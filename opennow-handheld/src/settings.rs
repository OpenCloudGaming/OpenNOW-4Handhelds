//! Stream settings edited from the home dock and stored in `stream.json`.
//! Bit rate, frame rate, and the GeForce NOW resolution are sent on the next
//! session. Render size, VSync, and the controller layout stay on the device.

use crate::core_client::{self as core};
use crate::gl::Painter;
use crate::text::Weight;
use crate::theme::{self, *};
use serde_json::{Value, json};
use std::path::PathBuf;

pub const ROWS: usize = 8;
pub const SERVER_ROW: usize = 6;
pub const SIGN_OUT_ROW: usize = 7;

const BITRATES: [f64; 20] = [
    5.0, 10.0, 15.0, 20.0, 25.0, 30.0, 35.0, 40.0, 45.0, 50.0, 55.0, 60.0, 65.0, 70.0, 75.0, 80.0, 85.0, 90.0, 95.0, 100.0,
];
const FRAME_RATES: [u64; 3] = [30, 60, 120];

#[derive(Clone)]
pub struct Profile {
    pub codec: String,
    pub color_quality: String,
    pub resolution: String,
    pub aspect_ratio: String,
    pub fps: u64,
    pub max_bitrate_mbps: f64,
    /// Size the decoded picture is color-converted to. `native` keeps the frame.
    pub render_resolution: String,
    /// Entitled GeForce NOW sizes, smallest first. Empty until the account is read.
    pub resolutions: Vec<String>,
    pub vsync: bool,
    /// Off uses the Nintendo face labels printed on most handhelds.
    pub xbox_layout: bool,
}

impl Profile {
    fn fallback() -> Self {
        Self {
            codec: "h264".into(),
            color_quality: "8bit_420".into(),
            resolution: String::new(),
            aspect_ratio: "16:9".into(),
            fps: 30,
            max_bitrate_mbps: 10.0,
            render_resolution: "640x480".into(),
            resolutions: Vec::new(),
            vsync: false,
            xbox_layout: false,
        }
    }

    pub fn label(&self) -> String {
        format!("{} @ {} fps", self.resolution.replace('x', "×"), self.fps)
    }

    /// Values the core understands. Local display settings stay out of this list.
    pub fn settings(&self) -> [(&'static str, Value); 6] {
        [
            ("codec", json!(self.codec)),
            ("colorQuality", json!(self.color_quality)),
            ("resolution", json!(self.resolution)),
            ("aspectRatio", json!(self.aspect_ratio)),
            ("fps", json!(self.fps)),
            ("maxBitrateMbps", bitrate_json(self.max_bitrate_mbps)),
        ]
    }

    /// 640×480 and Native stay available. The account's GeForce NOW sizes are added between them.
    fn render_choices(&self) -> Vec<String> {
        let mut choices = vec!["640x480".to_owned()];
        for size in &self.resolutions {
            if !choices.iter().any(|existing| existing.eq_ignore_ascii_case(size)) {
                choices.push(size.clone());
            }
        }
        choices.push("native".to_owned());
        choices
    }

    pub fn nudge(&mut self, row: usize, delta: i32) {
        match row {
            0 => self.max_bitrate_mbps = step_f64(&BITRATES, self.max_bitrate_mbps, delta),
            1 => self.fps = step_u64(&FRAME_RATES, self.fps, delta),
            2 => {
                let choices: Vec<&str> = self.resolutions.iter().map(String::as_str).collect();
                if choices.is_empty() {
                    return;
                }
                self.resolution = step_str(&choices, &self.resolution, delta).to_owned();
                self.aspect_ratio = aspect_for(&self.resolution);
            }
            3 => {
                let choices = self.render_choices();
                let choices: Vec<&str> = choices.iter().map(String::as_str).collect();
                self.render_resolution = step_str(&choices, &self.render_resolution, delta).to_owned();
            }
            4 => self.vsync = !self.vsync,
            5 => self.xbox_layout = !self.xbox_layout,
            _ => {}
        }
    }

    fn row_value(&self, row: usize) -> String {
        match row {
            0 => format!("{} Mbps", bitrate_text(self.max_bitrate_mbps)),
            1 => format!("{} FPS", self.fps),
            2 => {
                if self.resolution.is_empty() {
                    "Loading…".into()
                } else {
                    pretty_size(&self.resolution)
                }
            }
            3 => {
                if self.render_resolution.eq_ignore_ascii_case("native") {
                    "Native".into()
                } else {
                    pretty_size(&self.render_resolution)
                }
            }
            4 => if self.vsync { "On".into() } else { "Off".into() },
            5 => if self.xbox_layout { "Xbox".into() } else { "Nintendo".into() },
            _ => String::new(),
        }
    }

    fn document(&self) -> String {
        serde_json::to_string_pretty(&json!({
            "codec": self.codec,
            "colorQuality": self.color_quality,
            "resolution": self.resolution,
            "aspectRatio": self.aspect_ratio,
            "fps": self.fps,
            "maxBitrateMbps": bitrate_json(self.max_bitrate_mbps),
            "renderResolution": self.render_resolution,
            "vsync": self.vsync,
            "xboxLayout": self.xbox_layout,
        }))
        .unwrap_or_else(|_| "{}".into())
    }

    pub fn apply_resolutions(&mut self, resolutions: Vec<String>) {
        self.resolutions = resolutions;
        if self.resolutions.iter().any(|item| item.eq_ignore_ascii_case(&self.resolution)) {
            return;
        }
        let Some(lowest) = self.resolutions.first().cloned() else {
            return;
        };
        eprintln!("settings: resolution {} is not on this account, using {lowest}", if self.resolution.is_empty() { "unset" } else { self.resolution.as_str() });
        self.resolution = lowest;
        self.aspect_ratio = aspect_for(&self.resolution);
        self.save();
    }

    pub fn save(&self) {
        let path = config_path();
        match std::fs::write(&path, self.document()) {
            Ok(()) => eprintln!("settings: wrote {}", path.display()),
            Err(error) => eprintln!("settings: could not write {}: {error}", path.display()),
        }
    }
}

pub fn load() -> Profile {
    let path = config_path();
    let fallback = Profile::fallback();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) => return fallback,
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        eprintln!("settings: {} is not JSON, using the defaults", path.display());
        return fallback;
    };
    let mut profile = Profile {
        codec: text_or(&value, "codec", &fallback.codec),
        color_quality: text_or(&value, "colorQuality", &fallback.color_quality),
        resolution: text_or(&value, "resolution", &fallback.resolution),
        aspect_ratio: text_or(&value, "aspectRatio", &fallback.aspect_ratio),
        fps: value
            .get("fps")
            .and_then(|item| item.as_u64().or_else(|| item.as_f64().map(|fps| fps.round() as u64)))
            .filter(|fps| (30..=360).contains(fps))
            .unwrap_or(fallback.fps),
        max_bitrate_mbps: value
            .get("maxBitrateMbps")
            .and_then(Value::as_f64)
            .filter(|bitrate| bitrate.is_finite() && (5.0..=100.0).contains(bitrate))
            .map(snap_bitrate)
            .unwrap_or(fallback.max_bitrate_mbps),
        render_resolution: text_or(&value, "renderResolution", &fallback.render_resolution),
        resolutions: Vec::new(),
        vsync: value.get("vsync").and_then(Value::as_bool).unwrap_or(fallback.vsync),
        xbox_layout: value.get("xboxLayout").and_then(Value::as_bool).unwrap_or(false),
    };
    if !resolution_ok(&profile.resolution) {
        profile.resolution.clear();
    }
    if !matches!(profile.aspect_ratio.as_str(), "16:9" | "16:10" | "21:9" | "32:9" | "4:3") {
        profile.aspect_ratio = aspect_for(&profile.resolution);
    }
    if !render_ok(&profile.render_resolution) {
        profile.render_resolution = fallback.render_resolution.clone();
    }
    eprintln!(
        "settings: {} · {} at {} fps, {} Mbps, render {}, vsync {}, {}",
        profile.resolution,
        profile.codec,
        profile.fps,
        bitrate_text(profile.max_bitrate_mbps),
        if profile.render_resolution.eq_ignore_ascii_case("native") {
            "native".to_owned()
        } else {
            profile.render_resolution.clone()
        },
        if profile.vsync { "on" } else { "off" },
        if profile.xbox_layout { "xbox layout" } else { "nintendo layout" }
    );
    let missing_local = value.get("renderResolution").is_none() || value.get("vsync").is_none() || value.get("xboxLayout").is_none();
    if missing_local {
        profile.save();
    }
    profile
}

#[derive(Clone, Copy)]
pub struct Playtime {
    pub remaining: f64,
    pub total: f64,
    pub unlimited: bool,
}

pub fn playtime_from_subscription(value: &Value) -> Option<Playtime> {
    let subscription = value.get("subscription").filter(|item| item.is_object())?;
    Some(Playtime {
        remaining: subscription["remainingHours"].as_f64().unwrap_or(0.0).max(0.0),
        total: subscription["totalHours"].as_f64().unwrap_or(0.0).max(0.0),
        unlimited: subscription["isUnlimited"].as_bool().unwrap_or(false),
    })
}

pub fn draw(
    painter: &mut Painter,
    clock: &theme::Clock,
    profile: &Profile,
    selected: usize,
    server: &str,
    playtime: Option<Playtime>,
) {
    theme::draw_header(painter, clock);
    painter.text(EDGE, 68.0, 20.0, Weight::Black, "Settings", TEXT);
    let list_top = draw_playtime(painter, playtime);
    let row_height = 46.0;
    let row_fill = 42.0;
    let text_size = 17.0;
    let list_height = SEPARATOR_Y - list_top;
    let visible = (list_height / row_height).floor().max(1.0) as usize;
    let start = list_start(selected, visible);
    painter.clip(0.0, list_top, CANVAS.0, list_height);
    for row in start..(start + visible).min(ROWS) {
        let y = list_top + (row - start) as f32 * row_height;
        if row == selected {
            painter.fill(EDGE, y, CANVAS.0 - EDGE * 2.0, row_fill, 8.0, BUTTON);
        }
        let color = if row == selected { TEXT } else { MUTED };
        let label_color = if row == SIGN_OUT_ROW { ERROR } else { color };
        painter.text(32.0, y + 11.0, text_size, Weight::SemiBold, row_label(row), label_color);
        if row == SIGN_OUT_ROW {
            continue;
        }
        let value = if row == SERVER_ROW { server.to_owned() } else { profile.row_value(row) };
        let value_width = painter.measure(&value, text_size, Weight::Bold);
        let right = CANVAS.0 - 32.0;
        let gap = if row == selected { 18.0 } else { 0.0 };
        let available = 250.0;
        let shown = value_width.min(available);
        if row == selected {
            painter.text(right - 12.0, y + 11.0, text_size, Weight::Bold, ">", FOCUS);
            painter.text(right - shown - gap - 16.0, y + 11.0, text_size, Weight::Bold, "<", FOCUS);
        }
        painter.text_fit(
            right - shown - gap,
            y + 11.0,
            text_size,
            Weight::Bold,
            &value,
            available,
            if row == selected { TEXT } else { MUTED },
        );
    }
    painter.clear_clip();
    if selected == SIGN_OUT_ROW {
        theme::draw_footer(painter, "Sign out of this GeForce NOW account", &[("A", "Sign out"), ("B", "Back")]);
    } else {
        theme::draw_footer(painter, "Server, bit rate, and picture size apply on the next game", &[("A", "Change"), ("B", "Back")]);
    }
}

fn draw_playtime(painter: &mut Painter, playtime: Option<Playtime>) -> f32 {
    let y = 96.0;
    let bar_y = y + 20.0;
    let width = CANVAS.0 - EDGE * 2.0;
    let (left, right) = match playtime {
        Some(playtime) if playtime.unlimited => ("Unlimited".into(), String::new()),
        Some(playtime) => (format!("{} playtime left", hours_label(playtime.remaining)), hours_label(playtime.total)),
        None => ("Loading playtime…".into(), String::new()),
    };
    painter.text(EDGE, y, 14.0, Weight::Bold, &left, TEXT);
    if !right.is_empty() {
        let right_width = painter.measure(&right, 14.0, Weight::Bold);
        painter.text(EDGE + width - right_width, y, 14.0, Weight::Bold, &right, MUTED);
    }
    painter.fill(EDGE, bar_y, width, 6.0, 3.0, SEPARATOR);
    let fraction = match playtime {
        Some(playtime) if playtime.unlimited => 1.0,
        Some(playtime) if playtime.total > 0.0 => (playtime.remaining / playtime.total).clamp(0.0, 1.0),
        _ => 0.0,
    };
    if fraction > 0.0 {
        painter.fill(EDGE, bar_y, (width * fraction as f32).max(6.0), 6.0, 3.0, FOCUS);
    }
    bar_y + 16.0
}

fn hours_label(hours: f64) -> String {
    let minutes = (hours * 60.0).round().max(0.0) as u64;
    let whole = minutes / 60;
    let rest = minutes % 60;
    if rest == 0 { format!("{whole}h") } else { format!("{whole}h {rest}m") }
}

fn list_start(selected: usize, visible: usize) -> usize {
    if ROWS <= visible {
        return 0;
    }
    selected.saturating_sub(visible / 2).min(ROWS - visible)
}

fn row_label(row: usize) -> &'static str {
    match row {
        0 => "Bit rate",
        1 => "Frame rate",
        2 => "Resolution",
        3 => "Render",
        4 => "VSync",
        5 => "Layout",
        6 => "Server",
        _ => "Sign out",
    }
}

fn config_path() -> PathBuf {
    core::executable_dir().join("stream.json")
}

fn bitrate_json(value: f64) -> Value {
    if (value - value.round()).abs() < 1e-9 { json!(value.round() as i64) } else { json!(value) }
}

fn bitrate_text(value: f64) -> String {
    if (value - value.round()).abs() < 1e-9 { format!("{}", value.round() as i64) } else { format!("{value:.2}") }
}

fn snap_bitrate(value: f64) -> f64 {
    let stepped = (value / 5.0).round() * 5.0;
    stepped.clamp(5.0, 100.0)
}

/// Unique entitled sizes from `account.subscription.get`, smallest first.
pub fn resolutions_from_subscription(value: &Value) -> Vec<String> {
    let mut sizes = Vec::new();
    for entry in value["subscription"]["entitledResolutions"].as_array().into_iter().flatten() {
        let width = entry["width"].as_u64().unwrap_or(0);
        let height = entry["height"].as_u64().unwrap_or(0);
        if width < 640 || height < 480 {
            continue;
        }
        let label = format!("{width}x{height}");
        if !sizes.iter().any(|existing: &(u64, u64, String)| existing.2.eq_ignore_ascii_case(&label)) {
            sizes.push((width, height, label));
        }
    }
    sizes.sort_by_key(|(width, height, _)| (*width * *height, *width));
    sizes.into_iter().map(|(_, _, label)| label).collect()
}

fn pretty_size(value: &str) -> String {
    value.replace('x', " × ")
}

fn text_or(value: &Value, key: &str, fallback: &str) -> String {
    value.get(key).and_then(Value::as_str).filter(|text| !text.is_empty()).unwrap_or(fallback).to_owned()
}

fn resolution_ok(value: &str) -> bool {
    let Some((width, height)) = dimensions(value) else {
        return false;
    };
    (640..=7680).contains(&width) && (480..=4320).contains(&height) && width % 2 == 0 && height % 2 == 0
}

fn render_ok(value: &str) -> bool {
    value.eq_ignore_ascii_case("native") || resolution_ok(value)
}

fn aspect_for(resolution: &str) -> String {
    let Some((width, height)) = dimensions(resolution) else {
        return "16:9".into();
    };
    let ratio = width as f32 / height as f32;
    let choices = [(4.0 / 3.0, "4:3"), (16.0 / 10.0, "16:10"), (21.0 / 9.0, "21:9"), (32.0 / 9.0, "32:9")];
    choices.into_iter().find(|(expected, _)| (ratio - expected).abs() < 0.04).map(|(_, label)| label.to_owned()).unwrap_or_else(|| "16:9".into())
}

fn dimensions(value: &str) -> Option<(u32, u32)> {
    let (width, height) = value.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

fn step_str<'a>(choices: &'a [&'a str], current: &str, delta: i32) -> &'a str {
    let index = choices.iter().position(|item| item.eq_ignore_ascii_case(current));
    choices[step_index(choices.len(), index, delta)]
}

fn step_u64(choices: &[u64], current: u64, delta: i32) -> u64 {
    let index = choices.iter().position(|item| *item == current);
    choices[step_index(choices.len(), index, delta)]
}

fn step_f64(choices: &[f64], current: f64, delta: i32) -> f64 {
    let index = choices.iter().position(|item| (*item - current).abs() < 0.05);
    choices[step_index(choices.len(), index, delta)]
}

fn step_index(len: usize, current: Option<usize>, delta: i32) -> usize {
    match current {
        Some(index) => (index as i32 + delta).rem_euclid(len as i32) as usize,
        None if delta < 0 => len - 1,
        None => 0,
    }
}

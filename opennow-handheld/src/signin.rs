//! Device sign-in: the core's QR code and short code for the NVIDIA account
//! page, a countdown, and the authorization progress.

use crate::gl::Painter;
use crate::text::Weight;
use crate::theme::{self, *};

pub struct Challenge {
    pub attempt_id: String,
    pub user_code: String,
    pub verification_uri: String,
    pub qr: Vec<Vec<bool>>,
    /// Unix milliseconds, as reported by the core.
    pub expires_at: u64,
}

pub enum Phase {
    Starting,
    ChooseProvider,
    Waiting(Challenge),
    Completing,
    Failed(String),
}

const CARD: (f32, f32, f32) = (36.0, 92.0, 228.0);
const QUIET: f32 = 14.0;
const COLUMN: f32 = 290.0;

pub fn hints(phase: &Phase) -> Vec<(&'static str, &'static str)> {
    match phase {
        Phase::ChooseProvider => vec![("A", "Choose")],
        Phase::Failed(_) | Phase::Waiting(_) => vec![("A", "New code")],
        Phase::Starting | Phase::Completing => Vec::new(),
    }
}

pub fn draw(
    painter: &mut Painter,
    phase: &Phase,
    providers: &[&str],
    selected_provider: usize,
    clock: &Clock,
    now_ms: u64,
    seconds: f32,
) {
    theme::draw_header(painter, clock);
    let offset_y = ((canvas().1 - 480.0) / 2.0).max(0.0);
    let (card_x, card_y, card) = (CARD.0, CARD.1 + offset_y, CARD.2);
    let width = canvas().0 - COLUMN - EDGE;
    if !matches!(phase, Phase::ChooseProvider) {
        painter.text(
            COLUMN,
            96.0 + offset_y,
            24.0,
            Weight::Black,
            "Sign in",
            TEXT,
        );
    }
    match phase {
        Phase::ChooseProvider => {
            let heading = "Choose your GeForce NOW provider";
            let heading_width = painter.measure(heading, 23.0, Weight::Black);
            painter.text(
                ((canvas().0 - heading_width) / 2.0).max(EDGE),
                82.0 + offset_y,
                23.0,
                Weight::Black,
                heading,
                TEXT,
            );
            let subtitle = "Select the provider where your membership is active";
            let subtitle_width = painter.measure(subtitle, 14.0, Weight::SemiBold);
            painter.text(
                ((canvas().0 - subtitle_width) / 2.0).max(EDGE),
                113.0 + offset_y,
                14.0,
                Weight::SemiBold,
                subtitle,
                MUTED,
            );
            let warning = "Selecting the wrong one can prevent the session from starting";
            let warning_width = painter.measure(warning, 13.0, Weight::SemiBold);
            painter.text(
                ((canvas().0 - warning_width) / 2.0).max(EDGE),
                132.0 + offset_y,
                13.0,
                Weight::SemiBold,
                warning,
                MUTED,
            );
            const VISIBLE_PROVIDERS: usize = 5;
            const ROW_HEIGHT: f32 = 46.0;
            let visible_count = providers.len().min(VISIBLE_PROVIDERS);
            let first = selected_provider
                .saturating_sub(VISIBLE_PROVIDERS - 1)
                .min(providers.len().saturating_sub(VISIBLE_PROVIDERS));
            let button_width = (canvas().0 - EDGE * 2.0).min(480.0);
            let button_x = (canvas().0 - button_width) / 2.0;
            for row in 0..visible_count {
                let index = first + row;
                let y = 160.0 + offset_y + row as f32 * ROW_HEIGHT;
                let selected = index == selected_provider;
                painter.fill(
                    button_x,
                    y,
                    button_width,
                    ROW_HEIGHT - 3.0,
                    11.0,
                    if selected { ADD_TILE } else { PANEL },
                );
                if selected {
                    painter.stroke(
                        button_x,
                        y,
                        button_width,
                        ROW_HEIGHT - 3.0,
                        11.0,
                        2.5,
                        FOCUS,
                    );
                }
                let label_weight = if selected { Weight::Bold } else { Weight::SemiBold };
                let label_size = 19.0;
                let label_width = painter.measure(providers[index], label_size, label_weight);
                let fitted_size = if label_width > button_width - 88.0 {
                    label_size * (button_width - 88.0) / label_width
                } else {
                    label_size
                };
                let fitted_width = painter.measure(providers[index], fitted_size, label_weight);
                painter.text(
                    button_x + (button_width - fitted_width) / 2.0,
                    y + 9.0,
                    fitted_size,
                    label_weight,
                    providers[index],
                    if selected { FOCUS } else { TEXT },
                );
            }
            if !providers.is_empty() {
                let hint = format!("{} providers available", providers.len());
                let hint_width = painter.measure(&hint, 12.0, Weight::SemiBold);
                painter.text_fit(
                    ((canvas().0 - hint_width) / 2.0).max(EDGE),
                    393.0 + offset_y,
                    12.0,
                    Weight::SemiBold,
                    &hint,
                    canvas().0 - EDGE * 2.0,
                    MUTED,
                );
            }
        }
        Phase::Starting | Phase::Completing => {
            painter.fill(card_x, card_y, card, card, 14.0, PANEL);
            theme::draw_spinner(
                painter,
                card_x + card / 2.0,
                card_y + card / 2.0,
                22.0,
                seconds,
            );
            let message = if matches!(phase, Phase::Starting) {
                "Getting a sign-in code…"
            } else {
                "Signing in…"
            };
            painter.text_fit(
                COLUMN,
                140.0 + offset_y,
                16.0,
                Weight::SemiBold,
                message,
                width,
                MUTED,
            );
        }
        Phase::Failed(message) => {
            painter.fill(card_x, card_y, card, card, 14.0, PANEL);
            painter.text_fit(
                COLUMN,
                140.0 + offset_y,
                16.0,
                Weight::Bold,
                "Sign-in didn't finish",
                width,
                ERROR,
            );
            draw_wrapped(
                painter,
                COLUMN,
                170.0 + offset_y,
                15.0,
                message,
                width,
                MUTED,
            );
            painter.text_fit(
                COLUMN,
                250.0 + offset_y,
                15.0,
                Weight::SemiBold,
                "Press A for a new code.",
                width,
                TEXT,
            );
        }
        Phase::Waiting(challenge) => {
            draw_qr(painter, &challenge.qr, offset_y);
            painter.text_fit(
                COLUMN,
                136.0 + offset_y,
                14.0,
                Weight::SemiBold,
                "Scan the code with your phone, or visit",
                width,
                MUTED,
            );
            painter.text_fit(
                COLUMN,
                158.0 + offset_y,
                17.0,
                Weight::ExtraBold,
                &display_uri(&challenge.verification_uri),
                width,
                TITLE,
            );
            painter.text_fit(
                COLUMN,
                190.0 + offset_y,
                14.0,
                Weight::SemiBold,
                "and enter this code:",
                width,
                MUTED,
            );
            painter.fill(COLUMN, 214.0 + offset_y, width, 56.0, 14.0, PANEL);
            painter.stroke(COLUMN, 214.0 + offset_y, width, 56.0, 14.0, 1.0, SEPARATOR);
            let code = spaced(&challenge.user_code);
            let code_width = painter.measure(&code, 28.0, Weight::Black);
            painter.text(
                COLUMN + (width - code_width) / 2.0,
                224.0 + offset_y,
                28.0,
                Weight::Black,
                &code,
                TEXT,
            );

            theme::draw_spinner(painter, COLUMN + 9.0, 302.0 + offset_y, 8.0, seconds);
            painter.text_fit(
                COLUMN + 26.0,
                293.0 + offset_y,
                15.0,
                Weight::Bold,
                "Waiting for approval…",
                width - 26.0,
                TEXT,
            );
            let remaining = challenge.expires_at.saturating_sub(now_ms) / 1000;
            let expiry = format!("Code expires in {}:{:02}", remaining / 60, remaining % 60);
            painter.text_fit(
                COLUMN + 26.0,
                317.0 + offset_y,
                13.0,
                Weight::SemiBold,
                &expiry,
                width - 26.0,
                MUTED,
            );
        }
    }
}

/// Black modules on a white card. Each row's dark runs are one rectangle, and
/// rows overlap slightly so fractional scales leave no seams.
fn draw_qr(painter: &mut Painter, rows: &[Vec<bool>], offset_y: f32) {
    let (card_x, card_y, card) = (CARD.0, CARD.1 + offset_y, CARD.2);
    painter.fill(card_x, card_y, card, card, 14.0, WHITE);
    let count = rows.len().max(1) as f32;
    let module = (card - QUIET * 2.0) / count;
    let origin = (card_x + QUIET, card_y + QUIET);
    for (row_index, row) in rows.iter().enumerate() {
        let y = origin.1 + row_index as f32 * module;
        let mut column = 0;
        while column < row.len() {
            if !row[column] {
                column += 1;
                continue;
            }
            let start = column;
            while column < row.len() && row[column] {
                column += 1;
            }
            let x = origin.0 + start as f32 * module;
            painter.fill(
                x,
                y,
                (column - start) as f32 * module,
                module + 0.6,
                0.0,
                BLACK,
            );
        }
    }
}

fn display_uri(uri: &str) -> String {
    uri.trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned()
}

/// Separates the code's halves for legibility, e.g. "ABCD-EFGH".
fn spaced(code: &str) -> String {
    if code.contains('-') || code.len() != 8 || !code.is_ascii() {
        return code.to_owned();
    }
    format!("{}-{}", &code[..4], &code[4..])
}

fn draw_wrapped(
    painter: &mut Painter,
    x: f32,
    y: f32,
    size: f32,
    text: &str,
    width: f32,
    color: [f32; 4],
) {
    let mut line = String::new();
    let mut line_y = y;
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_owned()
        } else {
            format!("{line} {word}")
        };
        if !line.is_empty() && painter.measure(&candidate, size, Weight::SemiBold) > width {
            painter.text(x, line_y, size, Weight::SemiBold, &line, color);
            line_y += size * 1.4;
            if line_y > y + size * 1.4 * 3.0 {
                return;
            }
            line = word.to_owned();
        } else {
            line = candidate;
        }
    }
    if !line.is_empty() {
        painter.text(x, line_y, size, Weight::SemiBold, &line, color);
    }
}

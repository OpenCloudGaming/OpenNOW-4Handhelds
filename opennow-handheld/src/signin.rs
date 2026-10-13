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
    Waiting(Challenge),
    Completing,
    Failed(String),
}

const CARD: (f32, f32, f32) = (36.0, 92.0, 228.0);
const QUIET: f32 = 14.0;
const COLUMN: f32 = 290.0;

pub fn hints(phase: &Phase) -> Vec<(&'static str, &'static str)> {
    match phase {
        Phase::Failed(_) | Phase::Waiting(_) => vec![("A", "New code")],
        Phase::Starting | Phase::Completing => Vec::new(),
    }
}

pub fn draw(painter: &mut Painter, phase: &Phase, clock: &Clock, now_ms: u64, seconds: f32) {
    theme::draw_header(painter, clock);
    let (card_x, card_y, card) = CARD;
    let width = CANVAS.0 - COLUMN - EDGE;
    painter.text(COLUMN, 96.0, 24.0, Weight::Black, "Sign in", TEXT);
    match phase {
        Phase::Starting | Phase::Completing => {
            painter.fill(card_x, card_y, card, card, 14.0, PANEL);
            theme::draw_spinner(painter, card_x + card / 2.0, card_y + card / 2.0, 22.0, seconds);
            let message = if matches!(phase, Phase::Starting) { "Getting a sign-in code…" } else { "Signing in…" };
            painter.text_fit(COLUMN, 140.0, 16.0, Weight::SemiBold, message, width, MUTED);
        }
        Phase::Failed(message) => {
            painter.fill(card_x, card_y, card, card, 14.0, PANEL);
            painter.text_fit(COLUMN, 140.0, 16.0, Weight::Bold, "Sign-in didn't finish", width, ERROR);
            draw_wrapped(painter, COLUMN, 170.0, 15.0, message, width, MUTED);
            painter.text_fit(COLUMN, 250.0, 15.0, Weight::SemiBold, "Press A for a new code.", width, TEXT);
        }
        Phase::Waiting(challenge) => {
            draw_qr(painter, &challenge.qr);
            painter.text_fit(COLUMN, 136.0, 14.0, Weight::SemiBold, "Scan the code with your phone, or visit", width, MUTED);
            painter.text_fit(COLUMN, 158.0, 17.0, Weight::ExtraBold, &display_uri(&challenge.verification_uri), width, TITLE);
            painter.text_fit(COLUMN, 190.0, 14.0, Weight::SemiBold, "and enter this code:", width, MUTED);
            painter.fill(COLUMN, 214.0, width, 56.0, 14.0, PANEL);
            painter.stroke(COLUMN, 214.0, width, 56.0, 14.0, 1.0, SEPARATOR);
            let code = spaced(&challenge.user_code);
            let code_width = painter.measure(&code, 28.0, Weight::Black);
            painter.text(COLUMN + (width - code_width) / 2.0, 224.0, 28.0, Weight::Black, &code, TEXT);

            theme::draw_spinner(painter, COLUMN + 9.0, 302.0, 8.0, seconds);
            painter.text_fit(COLUMN + 26.0, 293.0, 15.0, Weight::Bold, "Waiting for approval…", width - 26.0, TEXT);
            let remaining = challenge.expires_at.saturating_sub(now_ms) / 1000;
            let expiry = format!("Code expires in {}:{:02}", remaining / 60, remaining % 60);
            painter.text_fit(COLUMN + 26.0, 317.0, 13.0, Weight::SemiBold, &expiry, width - 26.0, MUTED);
        }
    }
}

/// Black modules on a white card. Each row's dark runs are one rectangle, and
/// rows overlap slightly so fractional scales leave no seams.
fn draw_qr(painter: &mut Painter, rows: &[Vec<bool>]) {
    let (card_x, card_y, card) = CARD;
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
            painter.fill(x, y, (column - start) as f32 * module, module + 0.6, 0.0, BLACK);
        }
    }
}

fn display_uri(uri: &str) -> String {
    uri.trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/').to_owned()
}

/// Separates the code's halves for legibility, e.g. "ABCD-EFGH".
fn spaced(code: &str) -> String {
    if code.contains('-') || code.len() != 8 || !code.is_ascii() {
        return code.to_owned();
    }
    format!("{}-{}", &code[..4], &code[4..])
}

fn draw_wrapped(painter: &mut Painter, x: f32, y: f32, size: f32, text: &str, width: f32, color: [f32; 4]) {
    let mut line = String::new();
    let mut line_y = y;
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() { word.to_owned() } else { format!("{line} {word}") };
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

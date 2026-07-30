//! panini's output palette: a **teal + cyan** theme with a violet accent.
//!
//! Colors are emitted as truecolor ANSI and suppressed automatically when
//! stdout isn't a terminal, when `NO_COLOR` is set, or under `TERM=dumb`, so
//! piped/redirected output stays clean. clap's help/usage/errors are themed to
//! match via [`clap_styles`].

use std::io::IsTerminal;
use std::sync::OnceLock;

pub type Rgb = (u8, u8, u8);

// ---- the theme ----
pub const PRIMARY: Rgb = (0, 232, 198); // teal / mint — primary brand color
pub const SECONDARY: Rgb = (0, 198, 232); // cyan — values, secondary accent
pub const SKY: Rgb = (0, 210, 255); // sky blue — clap help headers / usage
pub const ACCENT: Rgb = (130, 100, 255); // violet — titles / rules
#[allow(dead_code)]
pub const HIGHLIGHT: Rgb = (100, 232, 130); // green — positive highlight
pub const LINK: Rgb = (255, 160, 100); // orange — links / attention
pub const MUTED: Rgb = (200, 210, 220); // light grey — secondary text
pub const ERROR: Rgb = (255, 100, 100); // red — errors

/// Whether to emit color, decided once from the environment.
fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("NO_COLOR").is_none()
            && std::env::var_os("TERM")
                .map(|t| t != "dumb")
                .unwrap_or(true)
            && std::io::stdout().is_terminal()
    })
}

fn fg(c: Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2)
}

/// Foreground-colored text (a no-op when color is disabled).
pub fn paint(c: Rgb, s: &str) -> String {
    if enabled() {
        format!("{}{s}\x1b[0m", fg(c))
    } else {
        s.to_string()
    }
}

/// Bold, foreground-colored text.
pub fn bold(c: Rgb, s: &str) -> String {
    if enabled() {
        format!("\x1b[1m{}{s}\x1b[0m", fg(c))
    } else {
        s.to_string()
    }
}

// ---- semantic shortcuts ----
/// Primary (teal) accent — checkmarks, counts, key output.
pub fn teal(s: &str) -> String {
    paint(PRIMARY, s)
}
/// Secondary (cyan) accent — values.
pub fn cyan(s: &str) -> String {
    paint(SECONDARY, s)
}
/// A bold violet section header / title.
pub fn header(s: &str) -> String {
    bold(ACCENT, s)
}
/// Success text or a ✓ mark.
pub fn ok(s: &str) -> String {
    paint(PRIMARY, s)
}
/// A notice / attention accent.
pub fn warn(s: &str) -> String {
    paint(LINK, s)
}
/// Bold error text.
pub fn error(s: &str) -> String {
    bold(ERROR, s)
}
/// Muted / secondary text.
pub fn muted(s: &str) -> String {
    paint(MUTED, s)
}

/// A left-to-right teal→violet gradient across the characters of `s` (bold).
/// Used for the banner; degrades to plain text when color is disabled.
pub fn gradient(s: &str) -> String {
    if !enabled() {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let last = chars.len().saturating_sub(1).max(1) as f32;
    let mut out = String::from("\x1b[1m");
    for (i, ch) in chars.iter().enumerate() {
        let t = i as f32 / last;
        out.push_str(&fg((
            lerp(PRIMARY.0, ACCENT.0, t),
            lerp(PRIMARY.1, ACCENT.1, t),
            lerp(PRIMARY.2, ACCENT.2, t),
        )));
        out.push(*ch);
    }
    out.push_str("\x1b[0m");
    out
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}

/// clap's help/usage/error styling, themed to match.
pub fn clap_styles() -> clap::builder::Styles {
    use clap::builder::styling::{Color, RgbColor, Style, Styles};
    let rgb = |c: Rgb| Color::Rgb(RgbColor(c.0, c.1, c.2));
    Styles::styled()
        .header(Style::new().bold().fg_color(Some(rgb(SKY))))
        .usage(Style::new().bold().fg_color(Some(rgb(SKY))))
        .literal(Style::new().fg_color(Some(rgb(PRIMARY))))
        .placeholder(Style::new().fg_color(Some(rgb(SECONDARY))))
        .valid(Style::new().fg_color(Some(rgb(PRIMARY))))
        .invalid(Style::new().bold().fg_color(Some(rgb(LINK))))
        .error(Style::new().bold().fg_color(Some(rgb(ERROR))))
}

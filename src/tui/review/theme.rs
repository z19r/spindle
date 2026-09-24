//! Colours that read on both light and dark terminals. Body text uses
//! the terminal's default foreground; only accents are fixed RGB, and
//! those are mid-tones that stay legible on either background.

use ratatui::style::{Color, Modifier, Style};

pub const PURPLE: Color = Color::Rgb(125, 86, 244);
pub const BORDER_PURPLE: Color = Color::Rgb(135, 75, 253);
pub const GREEN: Color = Color::Rgb(4, 150, 100);
/// Text drawn on a purple background.
pub const ON_ACCENT: Color = Color::Rgb(250, 250, 250);
/// Body text: whatever the terminal uses by default.
pub const TEXT: Color = Color::Reset;
/// De-emphasised text; ANSI dark grey is themed by the terminal.
pub const SUBTLE: Color = Color::DarkGray;
pub const RED_SOFT: Color = Color::Rgb(200, 70, 90);
pub const DIM_PURPLE: Color = Color::Rgb(120, 100, 170);
pub const BG_SELECTED: Color = Color::Rgb(49, 42, 76);
pub const BG_HEADER: Color = Color::Rgb(125, 86, 244);
/// Amber for warnings; bright yellow vanishes on light backgrounds.
pub const AMBER: Color = Color::Rgb(190, 120, 0);

// Kept for call sites that still name the old constants.
pub const WHITE: Color = TEXT;
pub const BRIGHT_GREEN: Color = GREEN;
pub const BRIGHT_YELLOW: Color = AMBER;

pub fn title_badge() -> Style {
  Style::default()
    .fg(ON_ACCENT)
    .bg(BG_HEADER)
    .add_modifier(Modifier::BOLD)
}

pub fn selected() -> Style {
  Style::default()
    .fg(ON_ACCENT)
    .bg(BG_SELECTED)
    .add_modifier(Modifier::BOLD)
}

pub fn normal() -> Style {
  Style::default().fg(TEXT)
}

pub fn dim() -> Style {
  Style::default().fg(SUBTLE)
}

pub fn label() -> Style {
  Style::default().fg(PURPLE).add_modifier(Modifier::BOLD)
}

pub fn value() -> Style {
  Style::default().fg(TEXT).add_modifier(Modifier::BOLD)
}

pub fn path() -> Style {
  Style::default().fg(TEXT)
}

pub fn approved() -> Style {
  Style::default().fg(GREEN)
}

pub fn rejected() -> Style {
  Style::default().fg(RED_SOFT)
}

pub fn key_hint() -> Style {
  Style::default().fg(PURPLE).add_modifier(Modifier::BOLD)
}

pub fn input_cursor() -> Style {
  Style::default().fg(ON_ACCENT).bg(PURPLE)
}

pub fn warning() -> Style {
  Style::default().fg(AMBER).add_modifier(Modifier::BOLD)
}

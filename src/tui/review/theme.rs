use ratatui::style::{Color, Modifier, Style};

pub const PURPLE: Color = Color::Rgb(125, 86, 244);
pub const BORDER_PURPLE: Color = Color::Rgb(135, 75, 253);
pub const GREEN: Color = Color::Rgb(4, 181, 117);
pub const WHITE: Color = Color::Rgb(250, 250, 250);
pub const SUBTLE: Color = Color::Rgb(136, 136, 136);
pub const RED_SOFT: Color = Color::Rgb(237, 135, 150);
pub const CREAM: Color = Color::Rgb(202, 211, 245);
pub const DIM_PURPLE: Color = Color::Rgb(110, 90, 160);
pub const BG_SELECTED: Color = Color::Rgb(49, 42, 76);
pub const BG_HEADER: Color = Color::Rgb(125, 86, 244);
pub const YELLOW: Color = Color::Rgb(249, 226, 175);
pub const BRIGHT_GREEN: Color = Color::Rgb(80, 250, 123);
pub const BRIGHT_YELLOW: Color = Color::Rgb(255, 214, 102);

pub fn title_badge() -> Style {
  Style::default()
    .fg(WHITE)
    .bg(BG_HEADER)
    .add_modifier(Modifier::BOLD)
}

pub fn selected() -> Style {
  Style::default()
    .fg(WHITE)
    .bg(BG_SELECTED)
    .add_modifier(Modifier::BOLD)
}

pub fn normal() -> Style {
  Style::default().fg(CREAM)
}

pub fn dim() -> Style {
  Style::default().fg(SUBTLE)
}

pub fn label() -> Style {
  Style::default().fg(PURPLE).add_modifier(Modifier::BOLD)
}

pub fn value() -> Style {
  Style::default().fg(WHITE)
}

pub fn path() -> Style {
  Style::default().fg(CREAM)
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
  Style::default().fg(WHITE).bg(PURPLE)
}

pub fn warning() -> Style {
  Style::default().fg(YELLOW).add_modifier(Modifier::BOLD)
}

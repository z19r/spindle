//! Colours that read on any terminal. Body text is the terminal's own
//! foreground (`Color::Reset`); accents come from a `Palette`. The
//! default palette keeps every accent in a luminance band that clears
//! 3:1 contrast against both pure white and pure black (enforced by a
//! test). On Omarchy the palette is derived from the active desktop
//! theme's alacritty colours instead, so spindle matches the terminal.

use std::path::PathBuf;
use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
  /// Brand and focus: titles, key hints, labels.
  pub accent: Color,
  pub border: Color,
  pub ok: Color,
  pub warn: Color,
  pub error: Color,
  /// De-emphasised text.
  pub subtle: Color,
  pub selected_bg: Color,
  pub selected_fg: Color,
  /// Text drawn on the accent colour (badges, cursor).
  pub on_accent: Color,
}

impl Palette {
  /// Every accent sits in the theme-safe luminance band [0.10, 0.30].
  pub const DEFAULT: Palette = Palette {
    accent: Color::Rgb(150, 100, 225),
    border: Color::Rgb(120, 80, 190),
    ok: Color::Rgb(40, 160, 70),
    warn: Color::Rgb(175, 135, 20),
    error: Color::Rgb(215, 70, 70),
    subtle: Color::Rgb(125, 118, 140),
    selected_bg: Color::Rgb(95, 62, 160),
    selected_fg: Color::Rgb(245, 245, 250),
    on_accent: Color::Rgb(245, 245, 250),
  };

  /// Build a palette from an alacritty colour scheme (the file Omarchy
  /// writes for the active theme). `None` when the primary colours are
  /// missing, so callers fall back to `DEFAULT`.
  pub fn from_alacritty_toml(text: &str) -> Option<Palette> {
    let doc: toml::Value = toml::from_str(text).ok()?;
    let get = |section: &str, key: &str| -> Option<Color> {
      doc
        .get("colors")?
        .get(section)?
        .get(key)?
        .as_str()
        .and_then(parse_hex)
    };
    let background = get("primary", "background")?;
    let foreground = get("primary", "foreground")?;
    let pick =
      |key: &str| get("bright", key).or_else(|| get("normal", key));

    let accent = pick("magenta").or_else(|| pick("blue"))?;
    let border = get("normal", "blue").unwrap_or(accent);
    let ok = pick("green").unwrap_or(Palette::DEFAULT.ok);
    let warn = pick("yellow").unwrap_or(Palette::DEFAULT.warn);
    let error = pick("red").unwrap_or(Palette::DEFAULT.error);
    // Bright black is the theme's own "comment" grey when it differs
    // from the background; otherwise blend fg and bg.
    let subtle = get("bright", "black")
      .filter(|c| *c != background)
      .unwrap_or_else(|| mix(foreground, background, 0.5));
    let selected_bg =
      get("selection", "background").unwrap_or(accent);
    let selected_fg = get("selection", "text").unwrap_or(background);
    // Terminal themes often ship a yellow or grey that vanishes against
    // their own background; pull such colours toward the foreground.
    let legible =
      |c: Color| ensure_contrast(c, background, foreground);
    Some(Palette {
      accent: legible(accent),
      border: legible(border),
      ok: legible(ok),
      warn: legible(warn),
      error: legible(error),
      subtle: legible(subtle),
      selected_bg,
      selected_fg,
      on_accent: background,
    })
  }

  /// The active Omarchy theme, if this machine has one.
  pub fn from_omarchy() -> Option<Palette> {
    omarchy_theme_paths()
      .into_iter()
      .find_map(|p| std::fs::read_to_string(p).ok())
      .and_then(|text| Palette::from_alacritty_toml(&text))
  }
}

fn omarchy_theme_paths() -> Vec<PathBuf> {
  let Some(home) =
    directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
  else {
    return Vec::new();
  };
  vec![
    home.join(".local/state/omarchy/current/theme/alacritty.toml"),
    home.join(".config/omarchy/current/theme/alacritty.toml"),
  ]
}

pub(crate) fn parse_hex(s: &str) -> Option<Color> {
  let hex = s.trim().trim_start_matches('#').trim_start_matches("0x");
  if hex.len() != 6 {
    return None;
  }
  let v = u32::from_str_radix(hex, 16).ok()?;
  Some(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// Minimum contrast for accent text against the terminal background.
const MIN_ACCENT_CONTRAST: f64 = 2.5;

/// Blend `color` toward `toward` in small steps until it reads against
/// `background`. Returns the input unchanged when it already does.
fn ensure_contrast(
  color: Color,
  background: Color,
  toward: Color,
) -> Color {
  let mut c = color;
  for _ in 0..20 {
    if contrast(c, background) >= MIN_ACCENT_CONTRAST {
      return c;
    }
    c = mix(c, toward, 0.15);
  }
  c
}

fn mix(a: Color, b: Color, t: f32) -> Color {
  match (a, b) {
    (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
      let m =
        |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t) as u8;
      Color::Rgb(m(r1, r2), m(g1, g2), m(b1, b2))
    }
    _ => a,
  }
}

static ACTIVE: OnceLock<Palette> = OnceLock::new();

/// Choose the palette for this process. Later calls are ignored.
pub fn init(palette: Palette) {
  let _ = ACTIVE.set(palette);
}

/// Use the terminal's own theme when it can be found, else the
/// contrast-safe default.
pub fn init_from_terminal() {
  init(Palette::from_omarchy().unwrap_or(Palette::DEFAULT));
}

fn active() -> &'static Palette {
  ACTIVE.get_or_init(|| Palette::DEFAULT)
}

pub fn text() -> Color {
  Color::Reset
}
pub fn accent() -> Color {
  active().accent
}
pub fn border() -> Color {
  active().border
}
pub fn ok() -> Color {
  active().ok
}
pub fn warn() -> Color {
  active().warn
}
pub fn error() -> Color {
  active().error
}
pub fn subtle() -> Color {
  active().subtle
}
pub fn selected_bg() -> Color {
  active().selected_bg
}
pub fn on_accent() -> Color {
  active().on_accent
}

pub fn title_badge() -> Style {
  Style::default()
    .fg(active().on_accent)
    .bg(active().accent)
    .add_modifier(Modifier::BOLD)
}

pub fn selected() -> Style {
  Style::default()
    .fg(active().selected_fg)
    .bg(active().selected_bg)
    .add_modifier(Modifier::BOLD)
}

pub fn normal() -> Style {
  Style::default().fg(text())
}

pub fn dim() -> Style {
  Style::default().fg(subtle())
}

pub fn label() -> Style {
  Style::default().fg(accent()).add_modifier(Modifier::BOLD)
}

pub fn value() -> Style {
  Style::default().fg(text()).add_modifier(Modifier::BOLD)
}

pub fn path() -> Style {
  Style::default().fg(text())
}

pub fn approved() -> Style {
  Style::default().fg(ok())
}

pub fn rejected() -> Style {
  Style::default().fg(error())
}

pub fn key_hint() -> Style {
  Style::default().fg(accent()).add_modifier(Modifier::BOLD)
}

pub fn input_cursor() -> Style {
  Style::default().fg(active().on_accent).bg(accent())
}

pub fn warning() -> Style {
  Style::default().fg(warn()).add_modifier(Modifier::BOLD)
}

/// Relative luminance per WCAG, for contrast checks.
pub(crate) fn luminance(color: Color) -> f64 {
  let Color::Rgb(r, g, b) = color else {
    return 0.0;
  };
  let ch = |c: u8| {
    let c = c as f64 / 255.0;
    if c <= 0.03928 {
      c / 12.92
    } else {
      ((c + 0.055) / 1.055).powf(2.4)
    }
  };
  0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b)
}

pub(crate) fn contrast(a: Color, b: Color) -> f64 {
  let (la, lb) = (luminance(a), luminance(b));
  let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
  (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn default_accents_read_on_light_and_dark_terminals() {
    let p = Palette::DEFAULT;
    for (name, c) in [
      ("accent", p.accent),
      ("border", p.border),
      ("ok", p.ok),
      ("warn", p.warn),
      ("error", p.error),
      ("subtle", p.subtle),
    ] {
      let l = luminance(c);
      assert!(
        (0.10..=0.30).contains(&l),
        "{name} luminance {l:.3} outside the theme-safe band"
      );
    }
    assert!(contrast(p.selected_fg, p.selected_bg) >= 4.5);
    assert!(contrast(p.on_accent, p.accent) >= 3.0);
  }

  const AETHER: &str = r##"
[colors.primary]
background = "#dfe4c4"
foreground = "#1c2d28"

[colors.selection]
text = "#1c2d28"
background = "#5e81ac"

[colors.normal]
black = "#dfe4c4"
red = "#b14752"
green = "#556753"
yellow = "#dc8164"
blue = "#4c6c94"
magenta = "#8a5b81"

[colors.bright]
black = "#7d8794"
magenta = "#8a5b81"
"##;

  #[test]
  fn omarchy_palette_maps_the_terminal_colours() {
    let p = Palette::from_alacritty_toml(AETHER).unwrap();
    assert_eq!(p.accent, Color::Rgb(0x8a, 0x5b, 0x81));
    assert_eq!(p.border, Color::Rgb(0x4c, 0x6c, 0x94));
    assert_eq!(p.ok, Color::Rgb(0x55, 0x67, 0x53));
    // The theme's orange is too faint on its background and gets pulled
    // toward the foreground; the rest pass through untouched.
    assert_ne!(p.warn, Color::Rgb(0xdc, 0x81, 0x64));
    assert_eq!(p.error, Color::Rgb(0xb1, 0x47, 0x52));
    assert_eq!(p.subtle, Color::Rgb(0x7d, 0x87, 0x94));
    assert_eq!(p.selected_bg, Color::Rgb(0x5e, 0x81, 0xac));
    assert_eq!(p.selected_fg, Color::Rgb(0x1c, 0x2d, 0x28));
    assert_eq!(p.on_accent, Color::Rgb(0xdf, 0xe4, 0xc4));
    // Everything derived must read against this theme's background.
    let bg = Color::Rgb(0xdf, 0xe4, 0xc4);
    for c in [p.accent, p.border, p.ok, p.warn, p.error, p.subtle] {
      assert!(
        contrast(c, bg) >= MIN_ACCENT_CONTRAST,
        "{c:?} vs background"
      );
    }
  }

  #[test]
  fn omarchy_palette_needs_primary_colours() {
    assert!(Palette::from_alacritty_toml(
      "[colors.normal]\nred = \"#ff0000\""
    )
    .is_none());
    assert!(
      Palette::from_alacritty_toml("not toml at all [").is_none()
    );
    assert_eq!(parse_hex("#0a0b0c"), Some(Color::Rgb(10, 11, 12)));
    assert_eq!(parse_hex("0x0a0b0c"), Some(Color::Rgb(10, 11, 12)));
    assert_eq!(parse_hex("#abc"), None);
  }
}

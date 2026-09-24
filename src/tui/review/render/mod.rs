//! Drawing the review screen. Sub-modules own one panel each.

use super::*;

mod detail;
mod files;
mod footer;
mod groups;
mod modals;

pub(crate) use detail::*;
pub(crate) use files::*;
pub(crate) use footer::*;
pub(crate) use groups::*;
pub(crate) use modals::*;

pub fn render(frame: &mut Frame, state: &mut ReviewState) {
  let outer = Layout::vertical([
    Constraint::Length(3),
    Constraint::Min(6),
    Constraint::Length(3),
  ])
  .split(frame.area());

  render_header(frame, outer[0], state);

  let body = Layout::horizontal([
    Constraint::Percentage(22),
    Constraint::Percentage(38),
    Constraint::Percentage(40),
  ])
  .split(outer[1]);

  render_group_list(frame, body[0], state);
  render_middle_panel(frame, body[1], state);
  render_detail(frame, body[2], state);
  render_footer(frame, outer[2], state);

  match &state.mode {
    Mode::DiffView { compare_idx } => {
      render_diff_modal(frame, state, *compare_idx);
    }
    Mode::Preview => {
      render_preview_modal(frame, state);
    }
    Mode::ConfirmExecute => {
      render_confirm_execute_modal(frame, state);
    }
    Mode::Help => {
      render_help_modal(frame, state);
    }
    _ => {}
  }
}

/// A centered, cleared modal area sized as a fraction of the screen.
pub(crate) fn centered_modal(
  frame: &Frame,
  pct_w: u16,
  pct_h: u16,
) -> Rect {
  let area = frame.area();
  let w = (area.width * pct_w / 100)
    .max(30)
    .min(area.width.saturating_sub(2));
  let h = (area.height * pct_h / 100)
    .max(8)
    .min(area.height.saturating_sub(2));
  let x = (area.width.saturating_sub(w)) / 2;
  let y = (area.height.saturating_sub(h)) / 2;
  Rect::new(x, y, w, h)
}

pub(crate) fn panel_block<'a>(
  title: &'a str,
  focused: bool,
) -> Block<'a> {
  let border_color = if focused {
    theme::BORDER_PURPLE
  } else {
    theme::DIM_PURPLE
  };

  let title_style = if focused {
    Style::default()
      .fg(theme::WHITE)
      .add_modifier(Modifier::BOLD)
  } else {
    Style::default()
      .fg(theme::SUBTLE)
      .add_modifier(Modifier::BOLD)
  };

  Block::bordered()
    .border_type(BorderType::Rounded)
    .title_top(Line::from(Span::styled(
      format!(" {title} "),
      title_style,
    )))
    .border_style(Style::default().fg(border_color))
    .padding(Padding::new(1, 1, 0, 0))
}

pub(crate) fn render_middle_panel(
  frame: &mut Frame,
  area: Rect,
  state: &ReviewState,
) {
  match &state.mode {
    Mode::Normal => render_file_list(frame, area, state),
    Mode::MoveToGroup { cursor } => render_group_picker(
      frame,
      area,
      state,
      *cursor,
      "Move to\u{2026}",
    ),
    Mode::MergeInto { cursor } => render_group_picker(
      frame,
      area,
      state,
      *cursor,
      "Merge into\u{2026}",
    ),
    Mode::NewGroup { input, cursor_pos } => render_new_group_input(
      frame,
      area,
      state,
      input,
      *cursor_pos,
      "New Group",
    ),
    Mode::RenameGroup { input, cursor_pos } => {
      render_new_group_input(
        frame,
        area,
        state,
        input,
        *cursor_pos,
        "Rename Group",
      )
    }
    Mode::ConfirmRemove => render_confirm_remove(frame, area, state),
    Mode::DiffView { .. }
    | Mode::Preview
    | Mode::ConfirmExecute
    | Mode::Help => render_file_list(frame, area, state),
  }
}

pub(crate) fn format_size(bytes: u64) -> String {
  if bytes >= 1_000_000_000 {
    format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
  } else if bytes >= 1_000_000 {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
  } else if bytes >= 1_000 {
    format!("{:.1} KB", bytes as f64 / 1_000.0)
  } else {
    format!("{bytes} B")
  }
}

pub(crate) fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
  let mut lines = Vec::new();
  let mut current = String::new();

  for word in text.split_whitespace() {
    if current.is_empty() {
      current = word.to_string();
    } else if current.len() + 1 + word.len() <= max_width {
      current.push(' ');
      current.push_str(word);
    } else {
      lines.push(current);
      current = word.to_string();
    }
  }
  if !current.is_empty() {
    lines.push(current);
  }
  if lines.is_empty() {
    lines.push(String::new());
  }
  lines
}

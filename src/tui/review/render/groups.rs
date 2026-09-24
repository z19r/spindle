use super::*;

pub(crate) fn render_header(
  frame: &mut Frame,
  area: Rect,
  state: &ReviewState,
) {
  let approved_count = state.approved.iter().filter(|&&a| a).count();
  let total = state.groups.len();

  let counter_style = if approved_count == total {
    Style::default()
      .fg(theme::GREEN)
      .add_modifier(Modifier::BOLD)
  } else {
    Style::default()
      .fg(theme::RED_SOFT)
      .add_modifier(Modifier::BOLD)
  };

  let block = Block::bordered()
    .border_type(BorderType::Rounded)
    .title_top(Line::from(vec![
      Span::styled(" ", Style::default().bg(theme::BG_HEADER)),
      Span::styled(" spindle ", theme::title_badge()),
      Span::styled(" ", Style::default().bg(theme::BG_HEADER)),
    ]))
    .title_top(
      Line::from(vec![Span::styled(
        format!(" {approved_count}/{total} approved "),
        counter_style,
      )])
      .alignment(Alignment::Right),
    )
    .border_style(Style::default().fg(theme::BORDER_PURPLE));
  let block = match state.banner() {
    Some(text) => block.title_bottom(
      Line::from(Span::styled(format!(" {text} "), theme::dim()))
        .alignment(Alignment::Center),
    ),
    None => block,
  };

  frame.render_widget(block, area);
}

pub(crate) fn render_group_list(
  frame: &mut Frame,
  area: Rect,
  state: &ReviewState,
) {
  let focused =
    state.focus == Pane::Groups && state.mode == Mode::Normal;
  let block = panel_block("Groups", focused);

  let items: Vec<ListItem> = state
    .groups
    .iter()
    .enumerate()
    .map(|(i, group)| {
      let approved = state.approved[i];
      let is_cursor = i == state.selected;

      let checkbox = if approved {
        Span::styled(
          " \u{25c9} ",
          Style::default()
            .fg(theme::GREEN)
            .add_modifier(Modifier::BOLD),
        )
      } else {
        Span::styled(" \u{25cb} ", theme::dim())
      };

      let label_style = if is_cursor && focused {
        theme::selected()
      } else if is_cursor {
        Style::default()
          .fg(theme::WHITE)
          .add_modifier(Modifier::BOLD)
      } else if approved {
        theme::normal()
      } else {
        theme::dim()
      };

      let file_count =
        state.group_moves.get(i).map(|m| m.len()).unwrap_or(0);
      let count_span =
        Span::styled(format!(" {file_count}"), theme::dim());

      let mut line_style = Style::default();
      if is_cursor && focused {
        line_style = line_style.bg(theme::BG_SELECTED);
      }

      ListItem::new(Line::from(vec![
        checkbox,
        Span::styled(&group.label, label_style),
        count_span,
      ]))
      .style(line_style)
    })
    .collect();

  let list = List::new(items).block(block);
  frame.render_widget(list, area);
}

pub(crate) fn render_group_picker(
  frame: &mut Frame,
  area: Rect,
  state: &ReviewState,
  cursor: usize,
  title: &str,
) {
  let block = panel_block(title, true);
  let targets = state.move_target_groups();

  if targets.is_empty() {
    let empty = Paragraph::new(Line::from(Span::styled(
      "No other groups",
      theme::dim(),
    )))
    .alignment(Alignment::Center)
    .block(block);
    frame.render_widget(empty, area);
    return;
  }

  let items: Vec<ListItem> = targets
    .iter()
    .enumerate()
    .map(|(i, (_, group))| {
      let is_cursor = i == cursor;
      let file_count = state
        .group_moves
        .get(
          state
            .groups
            .iter()
            .position(|g| g.id == group.id)
            .unwrap_or(0),
        )
        .map(|m| m.len())
        .unwrap_or(0);

      let label_style = if is_cursor {
        theme::selected()
      } else {
        theme::normal()
      };

      let mut line_style = Style::default();
      if is_cursor {
        line_style = line_style.bg(theme::BG_SELECTED);
      }

      ListItem::new(Line::from(vec![
        Span::styled(
          if is_cursor { " \u{25b8} " } else { "   " },
          theme::label(),
        ),
        Span::styled(&group.label, label_style),
        Span::styled(format!(" ({file_count})"), theme::dim()),
      ]))
      .style(line_style)
    })
    .collect();

  let list = List::new(items).block(block);
  frame.render_widget(list, area);
}

pub(crate) fn render_new_group_input(
  frame: &mut Frame,
  area: Rect,
  _state: &ReviewState,
  input: &str,
  cursor_pos: usize,
  title: &str,
) {
  let block = panel_block(title, true);
  let slug = crate::group::sanitize_folder_name(input.trim());
  let slug = if input.trim().is_empty() {
    String::new()
  } else {
    slug
  };

  let mut input_spans: Vec<Span> = Vec::new();
  input_spans.push(Span::styled("  ", Style::default()));
  for (i, ch) in input.chars().enumerate() {
    if i == cursor_pos {
      input_spans
        .push(Span::styled(ch.to_string(), theme::input_cursor()));
    } else {
      input_spans.push(Span::styled(ch.to_string(), theme::value()));
    }
  }
  if cursor_pos >= input.len() {
    input_spans.push(Span::styled(" ", theme::input_cursor()));
  }

  let mut lines = vec![
    Line::from(Span::styled("  GROUP NAME", theme::label())),
    Line::from(input_spans),
    Line::from(""),
  ];

  if !slug.is_empty() {
    lines.push(Line::from(Span::styled(
      "  DESTINATION",
      theme::label(),
    )));
    lines.push(Line::from(vec![
      Span::styled("  ", Style::default()),
      Span::styled(format!("{slug}/"), theme::path()),
    ]));
  }

  let paragraph = Paragraph::new(lines).block(block);
  frame.render_widget(paragraph, area);
}

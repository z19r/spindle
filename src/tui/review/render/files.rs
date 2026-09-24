use super::*;

pub(crate) fn render_file_list(
  frame: &mut Frame,
  area: Rect,
  state: &ReviewState,
) {
  let focused = state.focus == Pane::Files;
  let moves = state.current_group_moves();

  let marked_count = state
    .file_marked
    .get(state.selected)
    .map(|s| s.len())
    .unwrap_or(0);
  let title = if moves.is_empty() {
    "Files".to_string()
  } else if marked_count > 0 {
    format!("Files ({} selected)", marked_count)
  } else {
    format!("Files ({})", moves.len())
  };
  let block = panel_block(&title, focused);

  if moves.is_empty() {
    let empty = Paragraph::new(Line::from(Span::styled(
      "No files",
      theme::dim(),
    )))
    .alignment(Alignment::Center)
    .block(block);
    frame.render_widget(empty, area);
    return;
  }

  let items: Vec<ListItem> = moves
    .iter()
    .enumerate()
    .map(|(i, mv)| {
      let is_cursor = i == state.file_selected;

      let filename: String = mv
        .from
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| mv.from.display().to_string());

      let dest_dir: String = mv
        .to
        .parent()
        .map(|p| {
          p.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| p.display().to_string())
        })
        .unwrap_or_default();

      let name_style = if is_cursor && focused {
        theme::selected()
      } else if is_cursor {
        Style::default()
          .fg(theme::WHITE)
          .add_modifier(Modifier::BOLD)
      } else {
        theme::normal()
      };

      let mut line_style = Style::default();
      if is_cursor && focused {
        line_style = line_style.bg(theme::BG_SELECTED);
      }

      let mut spans = Vec::new();
      if state.review_mode == ReviewMode::Dupes {
        let keep_indicator = if state.is_file_kept(state.selected, i)
        {
          Span::styled(" \u{2713} ", theme::approved())
        } else {
          Span::styled(" \u{2717} ", theme::rejected())
        };
        spans.push(keep_indicator);
      } else if state.is_file_marked(state.selected, i) {
        spans.push(Span::styled(
          " \u{25cf} ",
          Style::default()
            .fg(theme::PURPLE)
            .add_modifier(Modifier::BOLD),
        ));
      } else {
        spans.push(Span::styled(" \u{25cb} ", theme::dim()));
      }
      spans.push(Span::styled(filename, name_style));
      spans.push(Span::styled(
        "  \u{2192}  ",
        Style::default().fg(theme::DIM_PURPLE),
      ));
      spans.push(Span::styled(format!("{dest_dir}/"), theme::dim()));

      ListItem::new(Line::from(spans)).style(line_style)
    })
    .collect();

  let list = List::new(items).block(block);
  frame.render_widget(list, area);
}

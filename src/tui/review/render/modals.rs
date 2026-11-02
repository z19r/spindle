use super::*;

pub(crate) fn render_confirm_execute_modal(
  frame: &mut Frame,
  state: &ReviewState,
) {
  let modal_area = centered_modal(frame, 55, 45);
  frame.render_widget(Clear, modal_area);

  let mut lines: Vec<Line<'static>> = vec![Line::from("")];
  match state.review_mode {
    ReviewMode::Organize => {
      // Counted from what the groups hold now, not from the plan the
      // pipeline proposed: files may have been moved, removed or
      // marked for deletion since.
      let groups = state.approved_group_counts();
      let moving: usize = groups.iter().map(|(_, n)| n).sum();
      lines.push(Line::from(vec![
        Span::styled("  Move ", theme::normal()),
        Span::styled(
          format!("{moving}"),
          Style::default()
            .fg(theme::ok())
            .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" files into ", theme::normal()),
        Span::styled(
          format!("{}", groups.len()),
          Style::default()
            .fg(theme::ok())
            .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" folders:", theme::normal()),
      ]));
      lines.push(Line::from(""));
      for (group, count) in groups.iter().take(6) {
        lines.push(Line::from(vec![
          Span::styled("    ", Style::default()),
          Span::styled(group.label.clone(), theme::value()),
          Span::styled(format!("  ({count} files)"), theme::dim()),
        ]));
      }
      if groups.len() > 6 {
        lines.push(Line::from(Span::styled(
          format!("    … and {} more", groups.len() - 6),
          theme::dim(),
        )));
      }
      let deletions = state.files_to_delete();
      if !deletions.is_empty() {
        let bytes: u64 = deletions
          .iter()
          .filter_map(|p| state.file_metadata.get(p))
          .map(|(_, size)| *size)
          .sum();
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
          Span::styled("  Stage ", theme::normal()),
          Span::styled(
            format!("{}", deletions.len()),
            Style::default()
              .fg(theme::warn())
              .add_modifier(Modifier::BOLD),
          ),
          Span::styled(" files to trash (", theme::normal()),
          Span::styled(format_size(bytes), theme::value()),
          Span::styled("), undoable.", theme::normal()),
        ]));
      }
    }
    ReviewMode::Dupes => {
      let deletions = state.files_to_delete();
      let bytes: u64 = deletions
        .iter()
        .filter_map(|p| state.file_metadata.get(p))
        .map(|(_, size)| *size)
        .sum();
      lines.push(Line::from(vec![
        Span::styled("  Stage ", theme::normal()),
        Span::styled(
          format!("{}", deletions.len()),
          Style::default()
            .fg(theme::warn())
            .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" duplicates to trash (", theme::normal()),
        Span::styled(format_size(bytes), theme::value()),
        Span::styled(")", theme::normal()),
      ]));
      lines.push(Line::from(""));
      lines.push(Line::from(Span::styled(
        "  Nothing is deleted permanently — undo with",
        theme::dim(),
      )));
      lines.push(Line::from(Span::styled(
        "  'spindle --undo', reclaim with 'spindle --purge'.",
        theme::dim(),
      )));
    }
  }
  lines.push(Line::from(""));
  lines.push(Line::from(vec![
    Span::styled("  \u{23ce}/y", theme::key_hint()),
    Span::styled(" go ahead    ", theme::dim()),
    Span::styled("esc/n", theme::key_hint()),
    Span::styled(" back to review", theme::dim()),
  ]));

  let block = Block::bordered()
    .border_type(BorderType::Rounded)
    .title_top(Line::from(Span::styled(
      " Ready to execute? ",
      theme::title_badge(),
    )))
    .border_style(Style::default().fg(theme::border()));
  frame.render_widget(Paragraph::new(lines).block(block), modal_area);
}

pub(crate) fn render_help_modal(
  frame: &mut Frame,
  state: &ReviewState,
) {
  let modal_area = centered_modal(frame, 60, 75);
  frame.render_widget(Clear, modal_area);

  let mut lines: Vec<Line<'static>> = vec![Line::from("")];
  let section = |title: &str,
                 keys: &[(&str, &str)],
                 lines: &mut Vec<Line<'static>>| {
    lines.push(Line::from(Span::styled(
      format!("  {title}"),
      theme::label(),
    )));
    for (key, desc) in keys {
      lines.push(Line::from(vec![
        Span::styled(format!("    {key:<8}"), theme::key_hint()),
        Span::styled((*desc).to_string(), theme::normal()),
      ]));
    }
    lines.push(Line::from(""));
  };

  section("KEYS", &key_table(state.review_mode, None), &mut lines);
  lines.push(Line::from(Span::styled(
    "  Space always toggles the item under the cursor; Enter always opens it.",
    theme::dim(),
  )));
  lines.push(Line::from(Span::styled(
    "  Deleted files are staged to trash and undoable with 'spindle --undo'.",
    theme::dim(),
  )));
  lines.push(Line::from(""));

  let block = Block::bordered()
    .border_type(BorderType::Rounded)
    .title_top(Line::from(Span::styled(
      " Keys ",
      theme::title_badge(),
    )))
    .border_style(Style::default().fg(theme::border()));
  frame.render_widget(Paragraph::new(lines).block(block), modal_area);
}

pub(crate) fn render_preview_modal(
  frame: &mut Frame,
  state: &mut ReviewState,
) {
  let modal_area = centered_modal(frame, 75, 75);
  frame.render_widget(Clear, modal_area);

  let Some(ds) = state.diff_state.as_mut() else {
    return;
  };
  let block = Block::default()
    .borders(ratatui::widgets::Borders::ALL)
    .border_style(Style::default().fg(theme::accent()))
    .title(Span::styled(
      format!(" {} ", file_name(&ds.primary.path)),
      Style::default()
        .fg(theme::accent())
        .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Line::from(vec![
      Span::styled("[/]", theme::key_hint()),
      Span::styled(" scroll  ", theme::dim()),
      Span::styled("esc", theme::key_hint()),
      Span::styled(" close", theme::dim()),
    ]));
  let inner = block.inner(modal_area);
  frame.render_widget(block, modal_area);

  let DiffContent::Text(lines) = &ds.content else {
    render_side_image(
      frame,
      inner,
      &mut ds.primary.preview,
      "No preview available",
    );
    return;
  };
  let visible_h = inner.height as usize;
  ds.scroll = ds.scroll.min(lines.len().saturating_sub(visible_h));
  let shown: Vec<Line> = lines
    .iter()
    .skip(ds.scroll)
    .take(visible_h)
    .map(|line| match line {
      DiffLine::Same(s)
      | DiffLine::Added(s)
      | DiffLine::Removed(s) => {
        Line::from(Span::styled(s.clone(), theme::dim()))
      }
    })
    .collect();
  frame.render_widget(Paragraph::new(shown), inner);
}

/// A file's name for a modal title or header.
fn file_name(path: &Path) -> String {
  path
    .file_name()
    .unwrap_or_default()
    .to_string_lossy()
    .to_string()
}

/// Draw a decoded image into `area`, or a dim line while it is still
/// loading or when there is nothing to draw.
fn render_side_image(
  frame: &mut Frame,
  area: Rect,
  preview: &mut PreviewState,
  none_msg: &str,
) {
  let msg = match preview {
    PreviewState::Ready(protocol) => {
      let image = ratatui_image::StatefulImage::new();
      frame.render_stateful_widget(image, area, protocol.as_mut());
      return;
    }
    PreviewState::Animated(anim) => {
      let image = ratatui_image::StatefulImage::new();
      frame.render_stateful_widget(
        image,
        area,
        anim.protocol.as_mut(),
      );
      return;
    }
    PreviewState::Loading => "Loading\u{2026}",
    PreviewState::None => none_msg,
  };
  frame.render_widget(
    Paragraph::new(Span::styled(msg, theme::dim()))
      .alignment(Alignment::Center),
    area,
  );
}

/// "◉ first  vs  ▸ second", with "(2/5)" when a duplicate set has more
/// copies to cycle through.
fn diff_header_line(
  primary: &str,
  secondary: &str,
  counter: Option<(usize, usize)>,
) -> Line<'static> {
  let bold =
    |color| Style::default().fg(color).add_modifier(Modifier::BOLD);
  let mut spans = vec![
    Span::styled(format!("  \u{25c9} {primary}"), bold(theme::ok())),
    Span::styled("    vs    ", theme::dim()),
    Span::styled(
      format!("\u{25b8} {secondary}"),
      bold(theme::warn()),
    ),
  ];
  if let Some((shown, total)) = counter {
    spans.push(Span::styled(
      format!("  ({shown}/{total})"),
      theme::dim(),
    ));
  }
  Line::from(spans)
}

pub(crate) fn render_diff_modal(
  frame: &mut Frame,
  state: &mut ReviewState,
  compare_idx: usize,
) {
  let area = frame.area();
  let modal_w = (area.width * 75 / 100)
    .max(50)
    .min(area.width.saturating_sub(4));
  let body_top: u16 = 3;
  let body_bottom = area.height.saturating_sub(3);
  let body_h = body_bottom.saturating_sub(body_top);
  let modal_h = (body_h * 90 / 100).max(16).min(body_h);
  let x = (area.width.saturating_sub(modal_w)) / 2;
  let y = body_top + (body_h.saturating_sub(modal_h)) / 2;
  let modal_area = Rect::new(x, y, modal_w, modal_h);

  frame.render_widget(Clear, modal_area);

  let pair = state.diff_state.as_ref().and_then(|ds| {
    let secondary = ds.secondary.as_ref()?;
    Some((ds.primary.path.clone(), secondary.path.clone()))
  });
  let Some((primary, secondary)) = pair else {
    let p = Paragraph::new(Line::from(Span::styled(
      "  Not enough files to diff",
      theme::dim(),
    )))
    .block(diff_modal_block(&[], false));
    frame.render_widget(p, modal_area);
    return;
  };

  // Only a duplicate set cycles; an organize diff is one fixed pair,
  // and its partner may live in another group.
  let others = match state.review_mode {
    ReviewMode::Dupes => {
      state.current_group_moves().len().saturating_sub(1)
    }
    ReviewMode::Organize => 1,
  };
  let counter = (others > 1).then_some((compare_idx + 1, others));
  let header = diff_header_line(
    &file_name(&primary),
    &file_name(&secondary),
    counter,
  );
  let kind = state
    .dupe_info(&secondary)
    .or_else(|| state.dupe_info(&primary))
    .map(|info| info.kind)
    .or_else(|| state.dupe_types.get(state.selected).copied());

  let metas = [&primary, &secondary]
    .map(|path| state.file_metadata.get(path).cloned());

  let Some(ds) = state.diff_state.as_mut() else {
    return;
  };
  let is_text = matches!(ds.content, DiffContent::Text(_));
  let hints: &[(&str, &str)] =
    if is_text { &[("[/]", "scroll")] } else { &[] };
  let block = diff_modal_block(hints, others > 1);
  let inner = block.inner(modal_area);
  frame.render_widget(block, modal_area);

  match &ds.content {
    DiffContent::Images => {
      let split =
        Layout::vertical([Constraint::Length(3), Constraint::Min(0)])
          .split(inner);
      frame.render_widget(
        Paragraph::new(vec![header, Line::from("")]),
        split[0],
      );
      let cols = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
      ])
      .split(split[1]);
      render_framed_side(
        frame,
        cols[0],
        " primary ",
        theme::ok(),
        &mut ds.primary.preview,
      );
      if let Some(side) = ds.secondary.as_mut() {
        render_framed_side(
          frame,
          cols[1],
          " compare ",
          theme::warn(),
          &mut side.preview,
        );
      }
    }
    DiffContent::Text(diff_lines) => {
      let split =
        Layout::vertical([Constraint::Length(2), Constraint::Min(1)])
          .split(inner);
      frame.render_widget(Paragraph::new(header), split[0]);
      let visible_h = split[1].height as usize;
      let scroll =
        ds.scroll.min(diff_lines.len().saturating_sub(visible_h));
      let lines = text_diff_lines(diff_lines, scroll, visible_h);
      frame.render_widget(Paragraph::new(lines), split[1]);
    }
    DiffContent::Binary => {
      render_diff_modal_metadata(frame, inner, header, metas, kind);
    }
  }
}

/// One bordered half of an image diff.
fn render_framed_side(
  frame: &mut Frame,
  area: Rect,
  title: &'static str,
  color: ratatui::style::Color,
  preview: &mut PreviewState,
) {
  let block = Block::bordered()
    .border_type(BorderType::Rounded)
    .title_top(Line::from(Span::styled(
      title,
      Style::default().fg(color),
    )))
    .border_style(Style::default().fg(theme::subtle()));
  let inner = block.inner(area);
  frame.render_widget(block, area);
  render_side_image(frame, inner, preview, "No preview");
}

/// The visible rows of a text diff, numbered, with a banner when the
/// two files have no differing line.
fn text_diff_lines(
  diff_lines: &[DiffLine],
  scroll: usize,
  visible_h: usize,
) -> Vec<Line<'static>> {
  let mut lines = Vec::new();
  if diff_lines.iter().all(|l| matches!(l, DiffLine::Same(_))) {
    lines.push(Line::from(Span::styled(
      "  Files are identical",
      Style::default()
        .fg(theme::ok())
        .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
  }
  for (i, line) in diff_lines.iter().enumerate().skip(scroll) {
    if lines.len() >= visible_h {
      break;
    }
    let (marker, marker_style, text, text_style) = match line {
      DiffLine::Same(text) => {
        ("  ", theme::dim(), text, theme::normal())
      }
      DiffLine::Added(text) => (
        "+ ",
        Style::default()
          .fg(theme::ok())
          .add_modifier(Modifier::BOLD),
        text,
        Style::default().fg(theme::ok()),
      ),
      DiffLine::Removed(text) => (
        "- ",
        Style::default()
          .fg(theme::error())
          .add_modifier(Modifier::BOLD),
        text,
        Style::default().fg(theme::error()),
      ),
    };
    lines.push(Line::from(vec![
      Span::styled(format!("{:>4} ", i + 1), theme::dim()),
      Span::styled(marker, marker_style),
      Span::styled(text.clone(), text_style),
    ]));
  }
  lines
}

pub(crate) fn diff_modal_block<'a>(
  extra_hints: &[(&'a str, &'a str)],
  show_cycle: bool,
) -> Block<'a> {
  let mut bottom_spans = vec![Span::styled(" ", Style::default())];
  for (key, desc) in extra_hints {
    bottom_spans.push(Span::styled(*key, theme::key_hint()));
    bottom_spans
      .push(Span::styled(format!(" {desc}   "), theme::dim()));
  }
  if show_cycle {
    bottom_spans
      .push(Span::styled("\u{2191}\u{2193}", theme::key_hint()));
    bottom_spans.push(Span::styled(" cycle   ", theme::dim()));
  }
  bottom_spans.push(Span::styled("esc", theme::key_hint()));
  bottom_spans.push(Span::styled(" close ", theme::dim()));

  Block::bordered()
    .border_type(BorderType::Rounded)
    .title_top(Line::from(Span::styled(
      " Diff ",
      Style::default()
        .fg(theme::text())
        .add_modifier(Modifier::BOLD),
    )))
    .title_bottom(
      Line::from(bottom_spans).alignment(Alignment::Right),
    )
    .border_style(Style::default().fg(theme::warn()))
    .padding(Padding::new(1, 1, 1, 0))
}

/// Size, hash and verdict for two files that cannot be shown side by
/// side. `metas` holds each file's (hash, size), primary first.
pub(crate) fn render_diff_modal_metadata(
  frame: &mut Frame,
  area: Rect,
  header: Line<'static>,
  metas: [Option<(String, u64)>; 2],
  dupe_type: Option<DuplicateType>,
) {
  let [p_meta, s_meta] = metas;
  let p_meta = p_meta.as_ref();
  let p_size = p_meta.map(|(_, s)| *s).unwrap_or(0);
  let s_size = s_meta.map(|(_, s)| s).unwrap_or(0);

  let mut lines = vec![header, Line::from("")];

  lines.push(Line::from(Span::styled("  SIZE", theme::label())));
  if p_size == s_size {
    lines.push(Line::from(vec![
      Span::styled("  ", Style::default()),
      Span::styled(format_size(p_size), theme::normal()),
      Span::styled("  (identical)", theme::dim()),
    ]));
  } else {
    lines.push(Line::from(vec![
      Span::styled("  ", Style::default()),
      Span::styled(
        format!(
          "{} \u{2192} {}",
          format_size(p_size),
          format_size(s_size)
        ),
        theme::normal(),
      ),
      Span::styled(
        if s_size > p_size {
          format!("  (+{})", format_size(s_size - p_size))
        } else {
          format!("  (-{})", format_size(p_size - s_size))
        },
        theme::dim(),
      ),
    ]));
  }
  lines.push(Line::from(""));

  match dupe_type {
    Some(DuplicateType::Exact) => {
      lines.push(Line::from(Span::styled("  HASH", theme::label())));
      if let Some((hash, _)) = p_meta {
        lines.push(Line::from(vec![
          Span::styled("  ", Style::default()),
          Span::styled(hash.clone(), theme::normal()),
        ]));
        lines.push(Line::from(Span::styled(
          "  (byte-for-byte match)",
          theme::dim(),
        )));
      }
      lines.push(Line::from(""));
      lines
        .push(Line::from(Span::styled("  VERDICT", theme::label())));
      lines.push(Line::from(Span::styled(
        "  Identical. Safe to delete either.",
        theme::normal(),
      )));
    }
    Some(DuplicateType::NearDuplicate { distance }) => {
      lines.push(Line::from(Span::styled(
        "  SIMILARITY",
        theme::label(),
      )));
      lines.push(Line::from(vec![
        Span::styled("  Hamming distance: ", theme::dim()),
        Span::styled(
          format!("{distance}"),
          Style::default()
            .fg(theme::warn())
            .add_modifier(Modifier::BOLD),
        ),
      ]));
      lines.push(Line::from(""));
      lines
        .push(Line::from(Span::styled("  VERDICT", theme::label())));
      lines.push(Line::from(Span::styled(
        "  Perceptually similar. Review before deleting.",
        theme::normal(),
      )));
    }
    Some(other) => push_similarity_verdict(&mut lines, other),
    None => {
      lines
        .push(Line::from(Span::styled("  VERDICT", theme::label())));
      lines.push(Line::from(Span::styled(
        "  Duplicate detected.",
        theme::normal(),
      )));
    }
  }

  let paragraph =
    Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true });
  frame.render_widget(paragraph, area);
}

pub(crate) fn render_confirm_remove(
  frame: &mut Frame,
  area: Rect,
  state: &ReviewState,
) {
  let block = panel_block("Confirm", true);

  let group_label = state
    .groups
    .get(state.selected)
    .map(|g| g.label.as_str())
    .unwrap_or("this group");

  let lines = vec![
    Line::from(""),
    Line::from(Span::styled(
      "  Group is now empty.",
      theme::warning(),
    )),
    Line::from(""),
    Line::from(vec![
      Span::styled("  Delete ", theme::normal()),
      Span::styled(
        group_label,
        Style::default()
          .fg(theme::text())
          .add_modifier(Modifier::BOLD),
      ),
      Span::styled("?", theme::normal()),
    ]),
    Line::from(""),
    Line::from(vec![
      Span::styled("  y", theme::key_hint()),
      Span::styled(" delete   ", theme::dim()),
      Span::styled("n", theme::key_hint()),
      Span::styled(" keep", theme::dim()),
    ]),
  ];

  let paragraph = Paragraph::new(lines).block(block);
  frame.render_widget(paragraph, area);
}

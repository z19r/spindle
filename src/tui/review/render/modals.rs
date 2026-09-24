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
      let groups = state.approved_groups();
      let moves = state.approved_moves();
      lines.push(Line::from(vec![
        Span::styled("  Move ", theme::normal()),
        Span::styled(
          format!("{}", moves.len()),
          Style::default()
            .fg(theme::BRIGHT_GREEN)
            .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" files into ", theme::normal()),
        Span::styled(
          format!("{}", groups.len()),
          Style::default()
            .fg(theme::BRIGHT_GREEN)
            .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" folders:", theme::normal()),
      ]));
      lines.push(Line::from(""));
      for group in groups.iter().take(6) {
        lines.push(Line::from(vec![
          Span::styled("    ", Style::default()),
          Span::styled(group.label.clone(), theme::value()),
          Span::styled(
            format!("  ({} files)", group.members.len()),
            theme::dim(),
          ),
        ]));
      }
      if groups.len() > 6 {
        lines.push(Line::from(Span::styled(
          format!("    … and {} more", groups.len() - 6),
          theme::dim(),
        )));
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
            .fg(theme::BRIGHT_YELLOW)
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
    .border_style(Style::default().fg(theme::BORDER_PURPLE));
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

  section(
    "NAVIGATE",
    &[
      ("j/k \u{2191}\u{2193}", "move up/down"),
      ("tab", "switch pane (groups \u{2194} files)"),
      ("s", "switch organize \u{2194} dupes mode"),
    ],
    &mut lines,
  );
  match state.review_mode {
    ReviewMode::Organize => section(
      "ORGANIZE",
      &[
        ("space", "toggle group approval / preview file"),
        ("enter", "mark file (multi-select)"),
        ("m", "move file(s) to another group"),
        ("n", "move file(s) to a new group"),
        ("d", "remove file(s) from the plan"),
        ("r", "rename the current group"),
        ("M", "merge the current group into another"),
      ],
      &mut lines,
    ),
    ReviewMode::Dupes => section(
      "DUPES",
      &[
        ("space", "toggle keep/delete on a file"),
        ("d", "side-by-side diff of the set"),
      ],
      &mut lines,
    ),
  }
  section(
    "ACT",
    &[
      ("x", "execute (with confirmation)"),
      ("q", "quit without changes"),
      ("?", "this help"),
    ],
    &mut lines,
  );

  let block = Block::bordered()
    .border_type(BorderType::Rounded)
    .title_top(Line::from(Span::styled(
      " Keys ",
      theme::title_badge(),
    )))
    .border_style(Style::default().fg(theme::BORDER_PURPLE));
  frame.render_widget(Paragraph::new(lines).block(block), modal_area);
}

pub(crate) fn render_preview_modal(
  frame: &mut Frame,
  state: &mut ReviewState,
) {
  let area = frame.area();
  let modal_w = (area.width * 75 / 100).max(40).min(area.width - 2);
  let modal_h = (area.height * 75 / 100).max(10).min(area.height - 2);
  let x = (area.width.saturating_sub(modal_w)) / 2;
  let y = (area.height.saturating_sub(modal_h)) / 2;
  let modal_area = Rect::new(x, y, modal_w, modal_h);

  frame.render_widget(Clear, modal_area);

  let filename = state
    .diff_state
    .as_ref()
    .and_then(|ds| ds.primary_path.as_ref())
    .and_then(|p| p.file_name())
    .map(|n| n.to_string_lossy().to_string())
    .unwrap_or_else(|| "Preview".to_string());

  let bottom_spans = vec![
    Span::styled("[/]", theme::key_hint()),
    Span::styled(" scroll  ", theme::dim()),
    Span::styled("\u{2191}\u{2193}", theme::key_hint()),
    Span::styled(" navigate  ", theme::dim()),
    Span::styled("esc", theme::key_hint()),
    Span::styled(" close", theme::dim()),
  ];

  let block = Block::default()
    .borders(ratatui::widgets::Borders::ALL)
    .border_style(Style::default().fg(theme::PURPLE))
    .title(Span::styled(
      format!(" {} ", filename),
      Style::default()
        .fg(theme::PURPLE)
        .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Line::from(bottom_spans));
  let inner = block.inner(modal_area);
  frame.render_widget(block, modal_area);

  if let Some(ref mut ds) = state.diff_state {
    if let Some(rx) = ds.primary_rx.as_ref() {
      if let Ok((path, protocol)) = rx.try_recv() {
        if Some(&path) == ds.primary_path.as_ref() {
          ds.primary_preview =
            PreviewState::Ready(Box::new(protocol));
        }
        ds.primary_rx = None;
      }
    }

    match &mut ds.primary_preview {
      PreviewState::Ready(protocol) => {
        let img = ratatui_image::StatefulImage::new();
        frame.render_stateful_widget(img, inner, protocol.as_mut());
      }
      PreviewState::Loading => {
        let loading = Paragraph::new(Span::styled(
          "Loading preview\u{2026}",
          theme::dim(),
        ))
        .alignment(Alignment::Center);
        frame.render_widget(loading, inner);
      }
      PreviewState::None => match &ds.content {
        DiffContent::Text(lines) => {
          let visible_h = inner.height as usize;
          let total = lines.len();
          let scroll = ds.scroll.min(total.saturating_sub(visible_h));
          ds.scroll = scroll;

          let styled_lines: Vec<Line> = lines
            .iter()
            .skip(scroll)
            .take(visible_h)
            .map(|dl| match dl {
              DiffLine::Same(s) => {
                Line::from(Span::styled(s.clone(), theme::dim()))
              }
              DiffLine::Added(s) => Line::from(Span::styled(
                s.clone(),
                Style::default().fg(ratatui::style::Color::Green),
              )),
              DiffLine::Removed(s) => Line::from(Span::styled(
                s.clone(),
                Style::default().fg(ratatui::style::Color::Red),
              )),
            })
            .collect();

          let text = ratatui::text::Text::from(styled_lines);
          let para = Paragraph::new(text);
          frame.render_widget(para, inner);
        }
        DiffContent::Binary | DiffContent::Images => {
          let msg = Paragraph::new(Span::styled(
            "No preview available",
            theme::dim(),
          ))
          .alignment(Alignment::Center);
          frame.render_widget(msg, inner);
        }
      },
    }
  }
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

  let moves = state.current_group_moves();
  if moves.len() < 2 {
    let block = diff_modal_block(&[], false);
    let p = Paragraph::new(Line::from(Span::styled(
      "  Not enough files to diff",
      theme::dim(),
    )))
    .block(block);
    frame.render_widget(p, modal_area);
    return;
  }

  let primary = &moves[0];
  let secondary_idx = compare_idx + 1;
  let secondary = match moves.get(secondary_idx) {
    Some(m) => m,
    None => return,
  };
  let total_others = moves.len() - 1;

  let p_name = primary
    .from
    .file_name()
    .unwrap_or_default()
    .to_string_lossy()
    .to_string();
  let s_name = secondary
    .from
    .file_name()
    .unwrap_or_default()
    .to_string_lossy()
    .to_string();

  let is_text_diff = state
    .diff_state
    .as_ref()
    .map(|ds| matches!(ds.content, DiffContent::Text(_)))
    .unwrap_or(false);
  let is_image_diff = state
    .diff_state
    .as_ref()
    .map(|ds| matches!(ds.content, DiffContent::Images))
    .unwrap_or(false);

  let mut extra_hints: Vec<(&str, &str)> = Vec::new();
  if is_text_diff {
    extra_hints.push(("[/]", "scroll"));
  }

  let block = diff_modal_block(&extra_hints, total_others > 1);
  let inner = block.inner(modal_area);
  frame.render_widget(block, modal_area);

  if is_image_diff {
    let header_h: u16 = 3;
    let image_h = inner.height.saturating_sub(header_h);

    let split = Layout::vertical([
      Constraint::Length(header_h),
      Constraint::Min(image_h),
    ])
    .split(inner);

    let header_lines = vec![
      Line::from(vec![
        Span::styled(
          format!("  \u{25c9} {p_name}"),
          Style::default()
            .fg(theme::BRIGHT_GREEN)
            .add_modifier(Modifier::BOLD),
        ),
        Span::styled("    vs    ", theme::dim()),
        Span::styled(
          format!("\u{25b8} {s_name}"),
          Style::default()
            .fg(theme::BRIGHT_YELLOW)
            .add_modifier(Modifier::BOLD),
        ),
        if total_others > 1 {
          Span::styled(
            format!("  ({}/{})", compare_idx + 1, total_others),
            theme::dim(),
          )
        } else {
          Span::raw("")
        },
      ]),
      Line::from(""),
    ];
    let header = Paragraph::new(header_lines);
    frame.render_widget(header, split[0]);

    let img_cols = Layout::horizontal([
      Constraint::Percentage(50),
      Constraint::Percentage(50),
    ])
    .split(split[1]);

    let left_block = Block::bordered()
      .border_type(BorderType::Rounded)
      .title_top(Line::from(Span::styled(
        " primary ",
        Style::default().fg(theme::BRIGHT_GREEN),
      )))
      .border_style(Style::default().fg(theme::DIM_PURPLE));

    let right_block = Block::bordered()
      .border_type(BorderType::Rounded)
      .title_top(Line::from(Span::styled(
        " compare ",
        Style::default().fg(theme::BRIGHT_YELLOW),
      )))
      .border_style(Style::default().fg(theme::DIM_PURPLE));

    let left_inner = left_block.inner(img_cols[0]);
    let right_inner = right_block.inner(img_cols[1]);
    frame.render_widget(left_block, img_cols[0]);
    frame.render_widget(right_block, img_cols[1]);

    if let Some(ref mut ds) = state.diff_state {
      match &mut ds.primary_preview {
        PreviewState::Ready(protocol) => {
          let iw = ratatui_image::StatefulImage::new();
          frame.render_stateful_widget(
            iw,
            left_inner,
            protocol.as_mut(),
          );
        }
        PreviewState::Loading => {
          let loading = Paragraph::new(Span::styled(
            "Loading\u{2026}",
            theme::dim(),
          ))
          .alignment(Alignment::Center);
          frame.render_widget(loading, left_inner);
        }
        PreviewState::None => {
          let na =
            Paragraph::new(Span::styled("No preview", theme::dim()))
              .alignment(Alignment::Center);
          frame.render_widget(na, left_inner);
        }
      }
      match &mut ds.secondary_preview {
        PreviewState::Ready(protocol) => {
          let iw = ratatui_image::StatefulImage::new();
          frame.render_stateful_widget(
            iw,
            right_inner,
            protocol.as_mut(),
          );
        }
        PreviewState::Loading => {
          let loading = Paragraph::new(Span::styled(
            "Loading\u{2026}",
            theme::dim(),
          ))
          .alignment(Alignment::Center);
          frame.render_widget(loading, right_inner);
        }
        PreviewState::None => {
          let na =
            Paragraph::new(Span::styled("No preview", theme::dim()))
              .alignment(Alignment::Center);
          frame.render_widget(na, right_inner);
        }
      }
    }
  } else if is_text_diff {
    let header_h: u16 = 2;
    let split = Layout::vertical([
      Constraint::Length(header_h),
      Constraint::Min(1),
    ])
    .split(inner);

    let header_lines = vec![Line::from(vec![
      Span::styled(
        format!("  \u{25c9} {p_name}"),
        Style::default()
          .fg(theme::BRIGHT_GREEN)
          .add_modifier(Modifier::BOLD),
      ),
      Span::styled("    vs    ", theme::dim()),
      Span::styled(
        format!("\u{25b8} {s_name}"),
        Style::default()
          .fg(theme::BRIGHT_YELLOW)
          .add_modifier(Modifier::BOLD),
      ),
      if total_others > 1 {
        Span::styled(
          format!("  ({}/{})", compare_idx + 1, total_others),
          theme::dim(),
        )
      } else {
        Span::raw("")
      },
    ])];
    frame.render_widget(Paragraph::new(header_lines), split[0]);

    if let Some(ref ds) = state.diff_state {
      if let DiffContent::Text(ref diff_lines) = ds.content {
        let visible_h = split[1].height as usize;
        let scroll =
          ds.scroll.min(diff_lines.len().saturating_sub(visible_h));
        let mut lines: Vec<Line> = Vec::new();

        let all_same =
          diff_lines.iter().all(|l| matches!(l, DiffLine::Same(_)));

        if all_same {
          lines.push(Line::from(Span::styled(
            "  Files are identical",
            Style::default()
              .fg(theme::BRIGHT_GREEN)
              .add_modifier(Modifier::BOLD),
          )));
          lines.push(Line::from(""));
        }

        for (i, dl) in diff_lines.iter().enumerate().skip(scroll) {
          if lines.len() >= visible_h {
            break;
          }
          let line_no = format!("{:>4} ", i + 1);
          match dl {
            DiffLine::Same(text) => {
              lines.push(Line::from(vec![
                Span::styled(line_no, theme::dim()),
                Span::styled("  ", theme::dim()),
                Span::styled(text.clone(), theme::normal()),
              ]));
            }
            DiffLine::Added(text) => {
              lines.push(Line::from(vec![
                Span::styled(line_no, theme::dim()),
                Span::styled(
                  "+ ",
                  Style::default()
                    .fg(theme::BRIGHT_GREEN)
                    .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                  text.clone(),
                  Style::default().fg(theme::BRIGHT_GREEN),
                ),
              ]));
            }
            DiffLine::Removed(text) => {
              lines.push(Line::from(vec![
                Span::styled(line_no, theme::dim()),
                Span::styled(
                  "- ",
                  Style::default()
                    .fg(theme::RED_SOFT)
                    .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                  text.clone(),
                  Style::default().fg(theme::RED_SOFT),
                ),
              ]));
            }
          }
        }

        let diff_para = Paragraph::new(lines);
        frame.render_widget(diff_para, split[1]);
      }
    }
  } else {
    render_diff_modal_metadata(
      frame,
      inner,
      state,
      &p_name,
      &s_name,
      compare_idx,
      total_others,
    );
  }
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
        .fg(theme::WHITE)
        .add_modifier(Modifier::BOLD),
    )))
    .title_bottom(
      Line::from(bottom_spans).alignment(Alignment::Right),
    )
    .border_style(Style::default().fg(theme::BRIGHT_YELLOW))
    .padding(Padding::new(1, 1, 1, 0))
}

pub(crate) fn render_diff_modal_metadata(
  frame: &mut Frame,
  area: Rect,
  state: &ReviewState,
  p_name: &str,
  s_name: &str,
  compare_idx: usize,
  total_others: usize,
) {
  let moves = state.current_group_moves();
  let primary = &moves[0];
  let secondary = &moves[compare_idx + 1];
  let dupe_type = state.dupe_types.get(state.selected).copied();
  let p_meta = state.file_metadata.get(&primary.from);
  let s_meta = state.file_metadata.get(&secondary.from);
  let p_size = p_meta.map(|(_, s)| *s).unwrap_or(0);
  let s_size = s_meta.map(|(_, s)| *s).unwrap_or(0);

  let mut lines = Vec::new();

  lines.push(Line::from(vec![
    Span::styled(
      format!("  \u{25c9} {p_name}"),
      Style::default()
        .fg(theme::BRIGHT_GREEN)
        .add_modifier(Modifier::BOLD),
    ),
    Span::styled("    vs    ", theme::dim()),
    Span::styled(
      format!("\u{25b8} {s_name}"),
      Style::default()
        .fg(theme::BRIGHT_YELLOW)
        .add_modifier(Modifier::BOLD),
    ),
    if total_others > 1 {
      Span::styled(
        format!("  ({}/{})", compare_idx + 1, total_others),
        theme::dim(),
      )
    } else {
      Span::raw("")
    },
  ]));
  lines.push(Line::from(""));

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
            .fg(theme::BRIGHT_YELLOW)
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
          .fg(theme::WHITE)
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

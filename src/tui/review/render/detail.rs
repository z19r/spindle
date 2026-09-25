use super::*;

pub(crate) fn render_detail(
  frame: &mut Frame,
  area: Rect,
  state: &mut ReviewState,
) {
  let focused = false;
  let block = panel_block("Detail", focused);

  if state.groups.is_empty() {
    let empty = Paragraph::new(Line::from(Span::styled(
      "No groups to display.",
      theme::dim(),
    )))
    .block(block);
    frame.render_widget(empty, area);
    return;
  }

  let in_files =
    state.mode == Mode::Normal && state.focus == Pane::Files;
  if state.focus == Pane::Files {
    state.ensure_facts_for_current();
  }
  let show_image = state.has_image_preview() && in_files;
  let show_loading = state.is_image_loading() && in_files;

  let lines = match &state.mode {
    Mode::Normal if state.focus == Pane::Files => {
      render_detail_file(state)
    }
    Mode::Normal => render_detail_group(state),
    Mode::MoveToGroup { cursor } | Mode::MergeInto { cursor } => {
      render_detail_move_target(state, *cursor)
    }
    Mode::NewGroup { input, .. }
    | Mode::RenameGroup { input, .. } => {
      render_detail_new_group(state, input)
    }
    Mode::ConfirmRemove | Mode::ConfirmExecute | Mode::Help => {
      render_detail_group(state)
    }
    Mode::DiffView { .. } | Mode::Preview => {
      render_detail_file(state)
    }
  };

  if show_image || show_loading {
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let split = Layout::vertical([
      Constraint::Percentage(60),
      Constraint::Percentage(40),
    ])
    .split(inner);

    if let Some(protocol) = state.image_state_mut() {
      let image_widget = ratatui_image::StatefulImage::new();
      frame.render_stateful_widget(image_widget, split[0], protocol);
    } else if show_loading {
      let loading = Paragraph::new(Line::from(Span::styled(
        "Loading preview\u{2026}",
        theme::dim(),
      )))
      .alignment(ratatui::layout::Alignment::Center);
      frame.render_widget(loading, split[0]);
    }

    let scroll = state.clamp_detail_scroll(&lines, split[1]);
    let detail = Paragraph::new(lines)
      .wrap(ratatui::widgets::Wrap { trim: true })
      .scroll((scroll, 0));
    frame.render_widget(detail, split[1]);
  } else {
    let scroll = state.clamp_detail_scroll(&lines, block.inner(area));
    let detail = Paragraph::new(lines)
      .block(block)
      .wrap(ratatui::widgets::Wrap { trim: true })
      .scroll((scroll, 0));
    frame.render_widget(detail, area);
  }
}

pub(crate) fn render_detail_file(
  state: &ReviewState,
) -> Vec<Line<'static>> {
  let group = &state.groups[state.selected];

  if let Some(mv) = state.current_file_move() {
    let filename: String = mv
      .from
      .file_name()
      .map(|n| n.to_string_lossy().to_string())
      .unwrap_or_else(|| "?".to_string());

    let dest_name: String = mv
      .to
      .file_name()
      .map(|n| n.to_string_lossy().to_string())
      .unwrap_or_else(|| "?".to_string());

    let from_dir = mv
      .from
      .parent()
      .map(|p| p.display().to_string())
      .unwrap_or_default();

    let to_dir = mv
      .to
      .parent()
      .map(|p| p.display().to_string())
      .unwrap_or_default();

    let ext = mv
      .from
      .extension()
      .map(|e| e.to_string_lossy().to_uppercase())
      .unwrap_or_default();

    let mut lines = vec![
      Line::from(Span::styled(
        format!(" {filename} "),
        Style::default()
          .fg(theme::text())
          .add_modifier(Modifier::BOLD),
      )),
      Line::from(""),
    ];

    if let Some(desc) = state.description(&mv.from) {
      lines.push(Line::from(Span::styled(
        "  DESCRIPTION",
        theme::label(),
      )));
      for text in wrap_text(&desc.summary, 60) {
        lines.push(Line::from(vec![
          Span::styled("  ", Style::default()),
          Span::styled(text, theme::value()),
        ]));
      }
      if !desc.tags.is_empty() {
        lines.push(Line::from(vec![
          Span::styled("  ", Style::default()),
          Span::styled(desc.tags.join(", "), theme::dim()),
        ]));
      }
      let (verdict, style) = match desc.source {
        DescriptionSource::Ai => (
          format!(
            "confidence {:.2} (content analysed)",
            desc.confidence
          ),
          if desc.confidence < 0.6 {
            theme::warning()
          } else {
            theme::approved()
          },
        ),
        DescriptionSource::Filename => (
          "filename only — content not read".to_string(),
          theme::warning(),
        ),
        DescriptionSource::Unanalyzed => {
          ("not analysed".to_string(), theme::rejected())
        }
      };
      lines.push(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(verdict, style),
      ]));
      lines.push(Line::from(""));
    }
    if let Some(note) = state.file_note(&mv.from) {
      lines.push(Line::from(Span::styled("  NOTE", theme::label())));
      lines.push(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(note.to_string(), theme::warning()),
      ]));
      lines.push(Line::from(""));
    }
    let facts = state.facts(&mv.from);
    if !facts.is_empty() {
      lines
        .push(Line::from(Span::styled("  METADATA", theme::label())));
      for f in facts {
        lines.push(Line::from(vec![
          Span::styled(format!("  {:<11}", f.key), theme::dim()),
          Span::styled(f.value.clone(), theme::normal()),
        ]));
      }
      lines.push(Line::from(""));
    }
    let alternatives = state.alternatives(&mv.from);
    if !alternatives.is_empty() {
      lines.push(Line::from(Span::styled(
        "  ALSO FITS",
        theme::label(),
      )));
      for (i, alt) in alternatives.iter().enumerate() {
        lines.push(Line::from(vec![
          Span::styled(
            format!("  {} ", i + 1),
            theme::value().add_modifier(Modifier::BOLD),
          ),
          Span::styled(alt.label.clone(), theme::value()),
        ]));
        lines.push(Line::from(vec![
          Span::styled("    shares ", theme::dim()),
          Span::styled(alt.shared_tags.join(", "), theme::normal()),
        ]));
        if !alt.samples.is_empty() {
          lines.push(Line::from(vec![
            Span::styled("    e.g. ", theme::dim()),
            Span::styled(alt.samples.join(", "), theme::path()),
          ]));
        }
      }
      lines.push(Line::from(Span::styled(
        "  press 1-3 to move this file there",
        theme::dim(),
      )));
      lines.push(Line::from(""));
    }
    if let Some(info) = state.dupe_info(&mv.from) {
      lines.push(Line::from(Span::styled(
        "  DUPLICATE",
        theme::label(),
      )));
      let what = match (info.is_canonical, info.kind) {
        (true, _) => "has a copy at".to_string(),
        (false, DuplicateType::Exact) => {
          "byte-identical copy of".to_string()
        }
        (false, DuplicateType::NearDuplicate { distance }) => {
          format!("perceptually similar (distance {distance}) to")
        }
        (false, DuplicateType::SimilarText { distance }) => {
          format!("near-identical text (distance {distance}) to")
        }
        (false, DuplicateType::SimilarAudio { score }) => {
          format!("same recording ({score}% match) as")
        }
        (false, DuplicateType::ArchiveMatch) => {
          "archive already extracted at".to_string()
        }
      };
      lines.push(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(what, theme::warning()),
      ]));
      lines.push(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
          info.partner.display().to_string(),
          theme::path(),
        ),
      ]));
      lines.push(Line::from(Span::styled(
        if state.is_file_kept(state.selected, state.file_selected) {
          "  kept — press space to stage it for deletion"
        } else {
          "  marked for deletion — press space to keep"
        },
        theme::dim(),
      )));
      lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled("  SOURCE", theme::label())));
    lines.push(Line::from(vec![
      Span::styled("  ", Style::default()),
      Span::styled(from_dir, theme::path()),
    ]));
    lines.push(Line::from(vec![
      Span::styled(
        "  \u{2514}\u{2500} ",
        Style::default().fg(theme::subtle()),
      ),
      Span::styled(filename.clone(), theme::value()),
    ]));
    lines.push(Line::from(""));

    lines.push(Line::from(Span::styled(
      "  DESTINATION",
      theme::label(),
    )));
    lines.push(Line::from(vec![
      Span::styled("  ", Style::default()),
      Span::styled(to_dir, theme::path()),
    ]));
    lines.push(Line::from(vec![
      Span::styled(
        "  \u{2514}\u{2500} ",
        Style::default().fg(theme::subtle()),
      ),
      Span::styled(dest_name, theme::value()),
    ]));
    lines.push(Line::from(""));

    if !ext.is_empty() {
      lines.push(Line::from(vec![
        Span::styled("  Type ", theme::dim()),
        Span::styled(ext, theme::normal()),
      ]));
    }
    if let Some((hash, size)) = state.file_metadata.get(&mv.from) {
      lines.push(Line::from(vec![
        Span::styled("  Size ", theme::dim()),
        Span::styled(format_size(*size), theme::normal()),
      ]));

      if state.review_mode == ReviewMode::Organize {
        lines.push(Line::from(vec![
          Span::styled("  Hash ", theme::dim()),
          Span::styled(
            format!("{}...", &hash[..16.min(hash.len())]),
            theme::normal(),
          ),
        ]));
      }
    }
    lines.push(Line::from(vec![
      Span::styled("  Group ", theme::dim()),
      Span::styled(group.label.clone(), theme::normal()),
    ]));

    if state.review_mode == ReviewMode::Dupes {
      lines.push(Line::from(""));
      let dupe_type = state.dupe_types.get(state.selected).copied();
      match dupe_type {
        Some(DuplicateType::Exact) => {
          lines.push(Line::from(Span::styled(
            "  MATCH TYPE",
            theme::label(),
          )));
          lines.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(
              "Exact duplicate",
              Style::default()
                .fg(theme::ok())
                .add_modifier(Modifier::BOLD),
            ),
          ]));
          lines.push(Line::from(""));
          lines
            .push(Line::from(Span::styled("  WHY", theme::label())));
          lines.push(Line::from(Span::styled(
            "  Byte-identical content.",
            theme::normal(),
          )));
          lines.push(Line::from(Span::styled(
            "  BLAKE3 hashes match.",
            theme::normal(),
          )));
          lines.push(Line::from(""));
          if let Some((hash, _)) = state.file_metadata.get(&mv.from) {
            lines.push(Line::from(Span::styled(
              "  HASH",
              theme::label(),
            )));
            lines.push(Line::from(vec![
              Span::styled("  ", Style::default()),
              Span::styled(hash.clone(), theme::normal()),
            ]));
          }
        }
        Some(DuplicateType::NearDuplicate { distance }) => {
          lines.push(Line::from(Span::styled(
            "  MATCH TYPE",
            theme::label(),
          )));
          lines.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(
              "Close match",
              Style::default()
                .fg(theme::warn())
                .add_modifier(Modifier::BOLD),
            ),
          ]));
          lines.push(Line::from(""));
          lines
            .push(Line::from(Span::styled("  WHY", theme::label())));
          lines.push(Line::from(Span::styled(
            "  Perceptual hashes are similar.",
            theme::normal(),
          )));
          lines.push(Line::from(Span::styled(
            format!("  Hamming distance: {distance}"),
            theme::normal(),
          )));
          lines.push(Line::from(Span::styled(
            "  (lower = more alike)",
            theme::dim(),
          )));
          lines.push(Line::from(""));
          lines.push(Line::from(Span::styled(
            "  Likely resized, re-encoded,",
            theme::normal(),
          )));
          lines.push(Line::from(Span::styled(
            "  or lightly edited.",
            theme::normal(),
          )));
        }
        Some(other) => push_similarity_verdict(&mut lines, other),
        None => {}
      }
    }

    lines
  } else {
    vec![Line::from(Span::styled("No file selected.", theme::dim()))]
  }
}

pub(crate) fn render_detail_group(
  state: &ReviewState,
) -> Vec<Line<'static>> {
  let group = &state.groups[state.selected];
  let approved = state.is_approved(state.selected);

  let status_span = if approved {
    Span::styled(" Approved ", theme::approved())
  } else {
    Span::styled(" Excluded ", theme::rejected())
  };

  let file_count = state.current_group_moves().len();

  let mut lines = vec![
    Line::from(vec![
      Span::styled(
        format!(" {} ", group.label),
        Style::default()
          .fg(theme::text())
          .add_modifier(Modifier::BOLD),
      ),
      Span::raw("  "),
      status_span,
    ]),
    Line::from(""),
  ];

  if state.review_mode == ReviewMode::Dupes {
    let dupe_type = state.dupe_types.get(state.selected).copied();
    render_dupe_reason(&mut lines, state, dupe_type);
  } else if !group.rationale.is_empty() {
    lines.push(Line::from(Span::styled("  WHY", theme::label())));
    for chunk in wrap_text(&group.rationale, 36) {
      lines.push(Line::from(Span::styled(
        format!("  {chunk}"),
        theme::normal(),
      )));
    }
    lines.push(Line::from(""));
  }

  lines
    .push(Line::from(Span::styled("  DESTINATION", theme::label())));
  lines.push(Line::from(vec![
    Span::styled("  ", Style::default()),
    Span::styled(
      format!("{}", group.suggested_path.display()),
      theme::path(),
    ),
  ]));
  lines.push(Line::from(""));

  lines.push(Line::from(vec![
    Span::styled("  Files ", theme::dim()),
    Span::styled(format!("{file_count}"), theme::value()),
  ]));

  lines
}

pub(crate) fn render_dupe_reason(
  lines: &mut Vec<Line<'static>>,
  state: &ReviewState,
  dupe_type: Option<DuplicateType>,
) {
  match dupe_type {
    Some(DuplicateType::Exact) => {
      lines.push(Line::from(Span::styled(
        "  MATCH TYPE",
        theme::label(),
      )));
      lines.push(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
          "Exact duplicate",
          Style::default()
            .fg(theme::ok())
            .add_modifier(Modifier::BOLD),
        ),
      ]));
      lines.push(Line::from(""));

      lines.push(Line::from(Span::styled("  WHY", theme::label())));
      lines.push(Line::from(Span::styled(
        "  Files are byte-identical.",
        theme::normal(),
      )));
      lines.push(Line::from(Span::styled(
        "  BLAKE3 hashes match exactly.",
        theme::normal(),
      )));
      lines.push(Line::from(""));

      let moves = state.current_group_moves();
      if let Some(first) = moves.first() {
        if let Some((hash, _)) = state.file_metadata.get(&first.from)
        {
          lines
            .push(Line::from(Span::styled("  HASH", theme::label())));
          lines.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(hash.clone(), theme::normal()),
          ]));
          lines.push(Line::from(""));
        }
      }
    }
    Some(DuplicateType::NearDuplicate { distance }) => {
      lines.push(Line::from(Span::styled(
        "  MATCH TYPE",
        theme::label(),
      )));
      lines.push(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
          "Close match",
          Style::default()
            .fg(theme::warn())
            .add_modifier(Modifier::BOLD),
        ),
      ]));
      lines.push(Line::from(""));

      lines.push(Line::from(Span::styled("  WHY", theme::label())));
      lines.push(Line::from(Span::styled(
        "  Perceptual hashes are similar.",
        theme::normal(),
      )));
      lines.push(Line::from(Span::styled(
        format!("  Hamming distance: {} (lower = more", distance),
        theme::normal(),
      )));
      lines.push(Line::from(Span::styled(
        "  alike). These images look nearly",
        theme::normal(),
      )));
      lines.push(Line::from(Span::styled(
        "  identical to the human eye —",
        theme::normal(),
      )));
      lines.push(Line::from(Span::styled(
        "  likely resized, re-encoded, or",
        theme::normal(),
      )));
      lines.push(Line::from(Span::styled(
        "  lightly edited versions.",
        theme::normal(),
      )));
      lines.push(Line::from(""));
    }
    Some(other) => push_similarity_verdict(lines, other),
    None => {
      lines.push(Line::from(Span::styled("  WHY", theme::label())));
      lines.push(Line::from(Span::styled(
        "  Duplicate detected",
        theme::normal(),
      )));
      lines.push(Line::from(""));
    }
  }
}

/// MATCH TYPE + WHY lines for the non-perceptual similarity kinds
/// (archive≡folder, near-identical text, acoustic audio match).
pub(crate) fn push_similarity_verdict(
  lines: &mut Vec<Line<'static>>,
  dupe_type: DuplicateType,
) {
  let (label, why): (&str, Vec<String>) = match dupe_type {
    DuplicateType::ArchiveMatch => (
      "Archive ≡ extracted folder",
      vec![
        "  Every archive entry exists on".to_string(),
        "  disk with identical content.".to_string(),
        "  The archive is redundant.".to_string(),
      ],
    ),
    DuplicateType::SimilarText { distance } => (
      "Near-identical text",
      vec![
        format!("  Simhash distance: {distance}"),
        "  Same document, lightly edited.".to_string(),
      ],
    ),
    DuplicateType::SimilarAudio { score } => (
      "Same recording",
      vec![
        format!("  Acoustic match: {score}%"),
        "  Same audio, different encode.".to_string(),
      ],
    ),
    DuplicateType::Exact | DuplicateType::NearDuplicate { .. } => {
      return
    }
  };

  lines
    .push(Line::from(Span::styled("  MATCH TYPE", theme::label())));
  lines.push(Line::from(vec![
    Span::styled("  ", Style::default()),
    Span::styled(
      label.to_string(),
      Style::default()
        .fg(theme::warn())
        .add_modifier(Modifier::BOLD),
    ),
  ]));
  lines.push(Line::from(""));
  lines.push(Line::from(Span::styled("  WHY", theme::label())));
  for line in why {
    lines.push(Line::from(Span::styled(line, theme::normal())));
  }
  lines.push(Line::from(""));
}

pub(crate) fn render_detail_move_target(
  state: &ReviewState,
  cursor: usize,
) -> Vec<Line<'static>> {
  let targets = state.move_target_groups();
  let file_mv = state.current_file_move();

  let mut lines = Vec::new();

  if let Some(mv) = file_mv {
    let filename: String = mv
      .from
      .file_name()
      .map(|n| n.to_string_lossy().to_string())
      .unwrap_or_else(|| "?".to_string());
    lines.push(Line::from(Span::styled(
      format!(" Moving: {filename} "),
      Style::default()
        .fg(theme::text())
        .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
  }

  if let Some((_, group)) = targets.get(cursor) {
    lines.push(Line::from(Span::styled(
      "  TARGET GROUP",
      theme::label(),
    )));
    lines.push(Line::from(vec![
      Span::styled("  ", Style::default()),
      Span::styled(
        group.label.clone(),
        Style::default()
          .fg(theme::text())
          .add_modifier(Modifier::BOLD),
      ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
      "  DESTINATION",
      theme::label(),
    )));
    lines.push(Line::from(vec![
      Span::styled("  ", Style::default()),
      Span::styled(
        format!("{}", group.suggested_path.display()),
        theme::path(),
      ),
    ]));
  }

  lines
}

pub(crate) fn render_detail_new_group(
  state: &ReviewState,
  input: &str,
) -> Vec<Line<'static>> {
  let file_mv = state.current_file_move();
  let slug = input.trim().to_lowercase().replace(' ', "_");

  let mut lines = Vec::new();

  if let Some(mv) = file_mv {
    let filename: String = mv
      .from
      .file_name()
      .map(|n| n.to_string_lossy().to_string())
      .unwrap_or_else(|| "?".to_string());
    lines.push(Line::from(Span::styled(
      format!(" Moving: {filename} "),
      Style::default()
        .fg(theme::text())
        .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
  }

  if !slug.is_empty() {
    lines.push(Line::from(Span::styled("  PREVIEW", theme::label())));
    lines.push(Line::from(vec![
      Span::styled("  Group: ", theme::dim()),
      Span::styled(input.trim().to_string(), theme::value()),
    ]));
    lines.push(Line::from(vec![
      Span::styled("  Path:  ", theme::dim()),
      Span::styled(format!("{slug}/"), theme::path()),
    ]));
  } else {
    lines.push(Line::from(Span::styled(
      "  Type a group name\u{2026}",
      theme::dim(),
    )));
  }

  lines
}

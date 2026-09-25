use super::*;

pub(crate) fn render_footer(
  frame: &mut Frame,
  area: Rect,
  state: &ReviewState,
) {
  let mode_label = match state.review_mode {
    ReviewMode::Organize => "Organize",
    ReviewMode::Dupes => "Dupes",
  };

  let keys: Vec<(&str, &str)> = match &state.mode {
    Mode::Normal => key_table(state.review_mode, Some(state.focus)),
    Mode::MoveToGroup { .. } => vec![
      ("j/k", "navigate"),
      ("\u{23ce}", "confirm"),
      ("esc", "cancel"),
    ],
    Mode::NewGroup { .. } | Mode::RenameGroup { .. } => {
      vec![
        ("type", "name"),
        ("\u{23ce}", "confirm"),
        ("esc", "cancel"),
      ]
    }
    Mode::MergeInto { .. } => vec![
      ("j/k", "navigate"),
      ("\u{23ce}", "merge"),
      ("esc", "cancel"),
    ],
    Mode::ConfirmRemove => {
      vec![("y", "delete group"), ("n", "keep"), ("esc", "cancel")]
    }
    Mode::ConfirmExecute => {
      vec![("\u{23ce}/y", "confirm"), ("esc/n", "cancel")]
    }
    Mode::Help => vec![("any key", "close")],
    Mode::DiffView { .. } => {
      let mut k = vec![("j/k", "cycle files")];
      let is_text = state
        .diff_state
        .as_ref()
        .map(|ds| matches!(ds.content, DiffContent::Text(_)))
        .unwrap_or(false);
      if is_text {
        k.push(("[/]", "scroll"));
      }
      k.push(("esc", "close"));
      k
    }
    Mode::Preview => {
      let mut k: Vec<(&str, &str)> = Vec::new();
      let is_text = state
        .diff_state
        .as_ref()
        .map(|ds| matches!(ds.content, DiffContent::Text(_)))
        .unwrap_or(false);
      if is_text {
        k.push(("[/]", "scroll"));
      }
      k.push(("esc", "close"));
      k
    }
  };

  let mut spans = vec![
    Span::styled(
      format!(" {mode_label} "),
      Style::default()
        .fg(theme::text())
        .bg(theme::accent())
        .add_modifier(Modifier::BOLD),
    ),
    Span::styled("  ", theme::dim()),
  ];
  for (i, (key, desc)) in keys.iter().enumerate() {
    if i > 0 {
      spans.push(Span::styled("   ", theme::dim()));
    }
    spans.push(Span::styled(*key, theme::key_hint()));
    spans.push(Span::styled(format!(" {desc}"), theme::dim()));
  }

  let footer = Paragraph::new(Line::from(spans))
    .alignment(Alignment::Center)
    .block(
      Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::subtle()))
        .padding(Padding::new(0, 0, 0, 0)),
    );

  frame.render_widget(footer, area);
}

/// The one list of normal-mode keys. The footer shows the subset for
/// the focused pane; the help modal shows everything (`pane: None`).
pub(crate) fn key_table(
  review_mode: ReviewMode,
  pane: Option<Pane>,
) -> Vec<(&'static str, &'static str)> {
  let groups = pane != Some(Pane::Files);
  let files = pane != Some(Pane::Groups);
  let organize = review_mode == ReviewMode::Organize;

  let mut k: Vec<(&str, &str)> = vec![("j/k", "navigate")];
  match pane {
    Some(Pane::Groups) => k.push(("\u{23ce}", "open files")),
    Some(Pane::Files) => k.push(("\u{23ce}", "preview")),
    None => k.push(("\u{23ce}", "open (files / preview)")),
  }
  match pane {
    Some(Pane::Groups) => k.push(("\u{2423}", "approve")),
    Some(Pane::Files) => k.push(("\u{2423}", "keep/delete")),
    None => {
      k.push(("\u{2423}", "toggle (approve group / keep file)"))
    }
  }
  if organize && files {
    k.extend([
      ("v", "mark"),
      ("m", "move"),
      ("1-3", "move to also-fits"),
      ("n", "new group"),
      ("d", "remove"),
      ("D", "diff with copy"),
    ]);
  }
  if organize && groups {
    k.extend([("r", "rename"), ("M", "merge")]);
  }
  if !organize {
    k.push(("D", "diff"));
  }
  k.extend([
    ("tab", "pane"),
    ("x", "execute"),
    ("?", "help"),
    ("q", "quit"),
  ]);
  // Last: the footer truncates on the right, and this is the hint the
  // narrowest terminals can most afford to lose.
  k.push(("[/]", "scroll detail"));
  k
}

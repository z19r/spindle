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

  let can_swap = state.can_swap_mode();
  let keys: Vec<(&str, &str)> = match &state.mode {
    Mode::Normal => match (state.focus, state.review_mode) {
      (Pane::Groups, _) => {
        let mut k = vec![("j/k", "navigate"), ("\u{2423}", "toggle")];
        if state.review_mode == ReviewMode::Organize {
          k.push(("r", "rename"));
          k.push(("M", "merge"));
        }
        if can_swap {
          k.push(("s", "mode"));
        }
        if state.review_mode == ReviewMode::Dupes {
          k.push(("d", "diff"));
        }
        k.extend([
          ("tab", "pane"),
          ("x", "execute"),
          ("?", "help"),
          ("q", "quit"),
        ]);
        k
      }
      (Pane::Files, ReviewMode::Organize) => {
        let mut k = vec![
          ("j/k", "navigate"),
          ("\u{23ce}", "select"),
          ("\u{2423}", "preview"),
          ("d", "remove"),
          ("m", "move"),
          ("n", "new group"),
        ];
        if can_swap {
          k.push(("s", "mode"));
        }
        k.extend([("tab", "pane"), ("x", "execute")]);
        k
      }
      (Pane::Files, ReviewMode::Dupes) => {
        let mut k =
          vec![("j/k", "navigate"), ("\u{2423}", "keep/delete")];
        if can_swap {
          k.push(("s", "mode"));
        }
        k.extend([("d", "diff"), ("tab", "pane"), ("x", "execute")]);
        k
      }
    },
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
        .fg(theme::WHITE)
        .bg(theme::PURPLE)
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
        .border_style(Style::default().fg(theme::DIM_PURPLE))
        .padding(Padding::new(0, 0, 0, 0)),
    );

  frame.render_widget(footer, area);
}

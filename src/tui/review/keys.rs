//! Key handling and every mutation the review screen can make.

use super::*;

/// Rows PageUp/PageDown move the detail column by.
const DETAIL_PAGE: i16 = 10;

impl ReviewState {
  /// Move focus, remembering which list pane the detail column is
  /// describing. Tabbing into [`Pane::Detail`] must not change what it
  /// shows, so the renderer keys off `detail_of` rather than `focus`.
  pub(crate) fn set_focus(&mut self, pane: Pane) {
    if pane != Pane::Detail {
      self.detail_of = pane;
    }
    self.focus = pane;
  }

  pub fn handle_key(&mut self, code: KeyCode) {
    // The detail pane has no cursor of its own; it shows whatever the
    // group and file cursors point at. Any key that moves them starts
    // the pane back at the top instead of inheriting an offset that
    // meant something for the previous item.
    let cursor_before =
      (self.selected, self.file_selected, self.focus);
    match &self.mode {
      Mode::Normal => self.handle_normal_key(code),
      Mode::MoveToGroup { .. } => self.handle_move_to_group_key(code),
      Mode::MergeInto { .. } => self.handle_merge_key(code),
      Mode::NewGroup { .. } | Mode::RenameGroup { .. } => {
        self.handle_text_input_key(code)
      }
      Mode::ConfirmRemove => self.handle_confirm_remove_key(code),
      Mode::ConfirmExecute => self.handle_confirm_execute_key(code),
      Mode::DiffView { .. } => self.handle_diff_view_key(code),
      Mode::Preview => self.handle_preview_key(code),
      Mode::Help => self.mode = Mode::Normal,
    }
    if (self.selected, self.file_selected, self.focus)
      != cursor_before
    {
      self.detail_scroll = 0;
    }
    self.update_image_preview();
  }

  pub(crate) fn handle_confirm_execute_key(&mut self, code: KeyCode) {
    match code {
      KeyCode::Enter | KeyCode::Char('y') => {
        self.action = Some(ReviewAction::Execute);
        self.mode = Mode::Normal;
      }
      KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => {
        self.mode = Mode::Normal;
      }
      _ => {}
    }
  }

  pub(crate) fn handle_normal_key(&mut self, code: KeyCode) {
    match code {
      KeyCode::Tab => self.set_focus(match self.focus {
        Pane::Groups => Pane::Files,
        Pane::Files => Pane::Detail,
        Pane::Detail => Pane::Groups,
      }),
      KeyCode::BackTab => self.set_focus(match self.focus {
        Pane::Groups => Pane::Detail,
        Pane::Files => Pane::Groups,
        Pane::Detail => Pane::Files,
      }),

      // The detail column has no cursor, so while it holds focus the
      // movement keys drive its scroll offset instead. A page is
      // deliberately coarse: the offset is clamped to the rendered
      // content, so overshooting lands on the last screen.
      KeyCode::Char('j') | KeyCode::Down
        if self.focus == Pane::Detail =>
      {
        self.scroll_detail(1)
      }
      KeyCode::Char('k') | KeyCode::Up
        if self.focus == Pane::Detail =>
      {
        self.scroll_detail(-1)
      }
      KeyCode::PageDown if self.focus == Pane::Detail => {
        self.scroll_detail(DETAIL_PAGE)
      }
      KeyCode::PageUp if self.focus == Pane::Detail => {
        self.scroll_detail(-DETAIL_PAGE)
      }
      KeyCode::Home if self.focus == Pane::Detail => {
        self.detail_scroll = 0
      }
      KeyCode::End if self.focus == Pane::Detail => {
        self.detail_scroll = u16::MAX
      }

      KeyCode::Char('j') | KeyCode::Down => match self.focus {
        Pane::Groups if !self.groups.is_empty() => {
          self.clear_marks();
          self.selected = (self.selected + 1) % self.groups.len();
          self.file_selected = 0;
        }
        Pane::Files => {
          let count = self.current_group_moves().len();
          if count > 0 {
            self.file_selected = (self.file_selected + 1) % count;
          }
        }
        _ => {}
      },

      KeyCode::Char('k') | KeyCode::Up => match self.focus {
        Pane::Groups if !self.groups.is_empty() => {
          self.clear_marks();
          self.selected = if self.selected == 0 {
            self.groups.len() - 1
          } else {
            self.selected - 1
          };
          self.file_selected = 0;
        }
        Pane::Files => {
          let count = self.current_group_moves().len();
          if count > 0 {
            self.file_selected = if self.file_selected == 0 {
              count - 1
            } else {
              self.file_selected - 1
            };
          }
        }
        _ => {}
      },

      // Space always toggles the item under the cursor.
      KeyCode::Char(' ') => match self.focus {
        Pane::Groups => {
          if let Some(val) = self.approved.get_mut(self.selected) {
            *val = !*val;
          }
        }
        Pane::Files => self.toggle_file_keep(),
        Pane::Detail => {}
      },

      // Enter always opens: a group opens its files, a file its preview.
      KeyCode::Enter => match self.focus {
        Pane::Groups if !self.current_group_moves().is_empty() => {
          self.set_focus(Pane::Files);
        }
        Pane::Files => self.enter_preview(),
        _ => {}
      },

      KeyCode::Char('v')
        if self.focus == Pane::Files
          && self.review_mode == ReviewMode::Organize =>
      {
        self.toggle_file_mark();
      }

      // The detail pane has no cursor of its own, so it scrolls with
      // the same keys the preview and diff modals use.
      KeyCode::Char('[') => self.scroll_detail(-3),
      KeyCode::Char(']') => self.scroll_detail(3),

      KeyCode::Char('x') => {
        self.mode = Mode::ConfirmExecute;
      }

      KeyCode::Char('?') => {
        self.mode = Mode::Help;
      }

      KeyCode::Char('d')
        if self.focus == Pane::Files
          && self.review_mode == ReviewMode::Organize =>
      {
        self.remove_marked_files();
      }

      KeyCode::Char('D')
        if self.review_mode == ReviewMode::Dupes
          && self
            .group_moves
            .get(self.selected)
            .map(|m| m.len())
            .unwrap_or(0)
            > 1 =>
      {
        self.mode = Mode::DiffView { compare_idx: 0 };
        self.enter_diff_view(0);
      }

      KeyCode::Char('D')
        if self.focus == Pane::Files
          && self.review_mode == ReviewMode::Organize =>
      {
        self.enter_dupe_diff();
      }

      KeyCode::Char('m')
        if self.focus == Pane::Files
          && self.review_mode == ReviewMode::Organize =>
      {
        self.enter_move_to_group();
      }

      KeyCode::Char(c @ '1'..='3')
        if self.focus == Pane::Files
          && self.review_mode == ReviewMode::Organize =>
      {
        let n = c.to_digit(10).unwrap_or(0) as usize;
        self.move_current_file_to_alternative(n);
      }

      KeyCode::Char('n')
        if self.focus == Pane::Files
          && self.review_mode == ReviewMode::Organize =>
      {
        self.enter_new_group();
      }

      KeyCode::Char('r')
        if self.review_mode == ReviewMode::Organize =>
      {
        self.enter_rename();
      }
      KeyCode::Char('M')
        if self.review_mode == ReviewMode::Organize =>
      {
        self.enter_merge();
      }
      _ => {}
    }
  }

  pub(crate) fn handle_move_to_group_key(&mut self, code: KeyCode) {
    let cursor = match &self.mode {
      Mode::MoveToGroup { cursor } => *cursor,
      _ => return,
    };
    let target_count = self.move_target_groups().len();
    if target_count == 0 {
      self.mode = Mode::Normal;
      return;
    }

    match code {
      KeyCode::Char('j') | KeyCode::Down => {
        self.mode = Mode::MoveToGroup {
          cursor: (cursor + 1) % target_count,
        };
      }
      KeyCode::Char('k') | KeyCode::Up => {
        self.mode = Mode::MoveToGroup {
          cursor: if cursor == 0 {
            target_count - 1
          } else {
            cursor - 1
          },
        };
      }
      KeyCode::Enter => {
        self.confirm_move_to_group(cursor);
      }
      KeyCode::Esc => {
        self.mode = Mode::Normal;
      }
      _ => {}
    }
  }

  /// Shared line editor for the NewGroup and RenameGroup prompts.
  pub(crate) fn handle_text_input_key(&mut self, code: KeyCode) {
    let (input, cursor_pos, rename) = match &self.mode {
      Mode::NewGroup { input, cursor_pos } => {
        (input.clone(), *cursor_pos, false)
      }
      Mode::RenameGroup { input, cursor_pos } => {
        (input.clone(), *cursor_pos, true)
      }
      _ => return,
    };
    let rebuild = |input: String, cursor_pos: usize| {
      if rename {
        Mode::RenameGroup { input, cursor_pos }
      } else {
        Mode::NewGroup { input, cursor_pos }
      }
    };

    match code {
      KeyCode::Char(c) => {
        let mut new_input = input;
        new_input.insert(cursor_pos, c);
        self.mode = rebuild(new_input, cursor_pos + 1);
      }
      KeyCode::Backspace if cursor_pos > 0 => {
        let mut new_input = input;
        new_input.remove(cursor_pos - 1);
        self.mode = rebuild(new_input, cursor_pos - 1);
      }
      KeyCode::Left if cursor_pos > 0 => {
        self.mode = rebuild(input, cursor_pos - 1);
      }
      KeyCode::Right => {
        let max = input.len();
        if cursor_pos < max {
          self.mode = rebuild(input, cursor_pos + 1);
        }
      }
      KeyCode::Enter => {
        if rename {
          self.confirm_rename(input);
        } else {
          self.confirm_new_group(input);
        }
      }
      KeyCode::Esc => {
        self.mode = Mode::Normal;
      }
      _ => {}
    }
  }

  pub(crate) fn handle_merge_key(&mut self, code: KeyCode) {
    let cursor = match &self.mode {
      Mode::MergeInto { cursor } => *cursor,
      _ => return,
    };
    let target_count = self.move_target_groups().len();
    if target_count == 0 {
      self.mode = Mode::Normal;
      return;
    }
    match code {
      KeyCode::Char('j') | KeyCode::Down => {
        self.mode = Mode::MergeInto {
          cursor: (cursor + 1) % target_count,
        };
      }
      KeyCode::Char('k') | KeyCode::Up => {
        self.mode = Mode::MergeInto {
          cursor: if cursor == 0 {
            target_count - 1
          } else {
            cursor - 1
          },
        };
      }
      KeyCode::Enter => self.merge_current_into(cursor),
      KeyCode::Esc => self.mode = Mode::Normal,
      _ => {}
    }
  }

  pub(crate) fn handle_confirm_remove_key(&mut self, code: KeyCode) {
    match code {
      KeyCode::Char('y') => {
        self.delete_current_group();
        self.mode = Mode::Normal;
      }
      KeyCode::Char('n') | KeyCode::Esc => {
        self.mode = Mode::Normal;
      }
      _ => {}
    }
  }

  pub(crate) fn handle_diff_view_key(&mut self, code: KeyCode) {
    let file_count = self
      .group_moves
      .get(self.selected)
      .map(|m| m.len())
      .unwrap_or(0);
    let max_compare = file_count.saturating_sub(1);

    match code {
      KeyCode::Char('j') | KeyCode::Down => {
        if let Mode::DiffView {
          ref mut compare_idx,
        } = self.mode
        {
          if *compare_idx < max_compare.saturating_sub(1) {
            *compare_idx += 1;
          }
        }
        if let Mode::DiffView { compare_idx } = self.mode {
          self.enter_diff_view(compare_idx);
        }
      }
      KeyCode::Char('k') | KeyCode::Up => {
        if let Mode::DiffView {
          ref mut compare_idx,
        } = self.mode
        {
          if *compare_idx > 0 {
            *compare_idx -= 1;
          }
        }
        if let Mode::DiffView { compare_idx } = self.mode {
          self.enter_diff_view(compare_idx);
        }
      }
      KeyCode::Char('[') => {
        if let Some(ref mut ds) = self.diff_state {
          ds.scroll = ds.scroll.saturating_sub(3);
        }
      }
      KeyCode::Char(']') => {
        if let Some(ref mut ds) = self.diff_state {
          ds.scroll = ds.scroll.saturating_add(3);
        }
      }
      KeyCode::Esc
      | KeyCode::Char('d')
      | KeyCode::Char('D')
      | KeyCode::Char('q') => {
        self.mode = Mode::Normal;
        self.exit_diff_view();
      }
      _ => {}
    }
  }

  pub(crate) fn handle_preview_key(&mut self, code: KeyCode) {
    match code {
      KeyCode::Char('[') => {
        if let Some(ref mut ds) = self.diff_state {
          ds.scroll = ds.scroll.saturating_sub(3);
        }
      }
      KeyCode::Char(']') => {
        if let Some(ref mut ds) = self.diff_state {
          ds.scroll = ds.scroll.saturating_add(3);
        }
      }
      KeyCode::Esc | KeyCode::Char(' ') | KeyCode::Char('q') => {
        self.mode = Mode::Normal;
        self.exit_diff_view();
      }
      _ => {}
    }
  }

  pub(crate) fn remove_marked_files(&mut self) {
    if self.selected >= self.group_moves.len() {
      return;
    }
    if self.group_moves[self.selected].is_empty() {
      return;
    }

    let indices = self.marked_file_indices();
    for &i in indices.iter().rev() {
      if i < self.group_moves[self.selected].len() {
        self.group_moves[self.selected].remove(i);
        self.file_keep[self.selected].remove(i);
      }
    }
    self.clear_marks();

    let count = self.group_moves[self.selected].len();
    if count == 0 {
      self.file_selected = 0;
      self.mode = Mode::ConfirmRemove;
    } else {
      self.file_selected = self.file_selected.min(count - 1);
    }
  }

  pub(crate) fn delete_current_group(&mut self) {
    if self.selected >= self.groups.len() {
      return;
    }
    self.groups.remove(self.selected);
    self.approved.remove(self.selected);
    self.group_moves.remove(self.selected);
    self.file_keep.remove(self.selected);
    self.file_marked.remove(self.selected);

    if self.groups.is_empty() {
      self.selected = 0;
    } else {
      self.selected = self.selected.min(self.groups.len() - 1);
    }
    self.file_selected = 0;
  }

  pub(crate) fn enter_move_to_group(&mut self) {
    if self.current_group_moves().is_empty() {
      return;
    }
    if self.groups.len() < 2 {
      return;
    }
    self.mode = Mode::MoveToGroup { cursor: 0 };
  }

  pub(crate) fn confirm_move_to_group(&mut self, cursor: usize) {
    let targets = self.move_target_groups();
    if cursor >= targets.len() {
      self.mode = Mode::Normal;
      return;
    }
    let dest_idx = targets[cursor].0;
    self.move_marked_to_group(dest_idx);
  }

  /// Move the marked files (or the one under the cursor) of the
  /// selected group into `dest_idx`, then leave move mode.
  pub(crate) fn move_marked_to_group(&mut self, dest_idx: usize) {
    if dest_idx >= self.groups.len() || dest_idx == self.selected {
      self.mode = Mode::Normal;
      return;
    }
    let dest_group_id = self.groups[dest_idx].id;
    let dest_suggested = self.groups[dest_idx].suggested_path.clone();

    let indices = self.marked_file_indices();
    for &i in indices.iter().rev() {
      if i >= self.group_moves[self.selected].len() {
        continue;
      }
      let mut file_move = self.group_moves[self.selected].remove(i);
      let was_kept = self.file_keep[self.selected].remove(i);

      let filename = file_move
        .from
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

      file_move.group_id = dest_group_id;
      file_move.to =
        self.output_dir.join(&dest_suggested).join(&filename);

      self.group_moves[dest_idx].push(file_move);
      self.file_keep[dest_idx].push(was_kept);
    }
    self.clear_marks();

    let src_count = self.group_moves[self.selected].len();
    if src_count == 0 {
      self.file_selected = 0;
      self.mode = Mode::ConfirmRemove;
    } else {
      self.file_selected = self.file_selected.min(src_count - 1);
      self.mode = Mode::Normal;
    }
  }

  /// Absolute destination directory of a group. Pipeline groups carry
  /// an absolute `suggested_path`; TUI-created ones a relative slug.
  pub(crate) fn group_dir(&self, idx: usize) -> PathBuf {
    let p = &self.groups[idx].suggested_path;
    if p.is_absolute() {
      p.clone()
    } else {
      self.output_dir.join(p)
    }
  }

  pub(crate) fn enter_rename(&mut self) {
    if self.selected >= self.groups.len() {
      return;
    }
    let input = self.groups[self.selected].label.clone();
    let cursor_pos = input.len();
    self.mode = Mode::RenameGroup { input, cursor_pos };
  }

  /// Relabel the current group and point every one of its moves at the
  /// new folder, keeping any sub-path under the old one.
  pub(crate) fn confirm_rename(&mut self, input: String) {
    let name = input.trim().to_string();
    if name.is_empty() {
      return;
    }
    let old_dir = self.group_dir(self.selected);
    let new_dir = self
      .output_dir
      .join(crate::group::sanitize_folder_name(&name));
    for mv in &mut self.group_moves[self.selected] {
      let rel: PathBuf = mv
        .to
        .strip_prefix(&old_dir)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| {
          PathBuf::from(mv.from.file_name().unwrap_or_default())
        });
      mv.to = new_dir.join(rel);
    }
    let group = &mut self.groups[self.selected];
    group.label = name;
    group.suggested_path = new_dir;
    self.mode = Mode::Normal;
  }

  pub(crate) fn enter_merge(&mut self) {
    if self.groups.len() < 2 {
      return;
    }
    self.mode = Mode::MergeInto { cursor: 0 };
  }

  /// Move every file of the current group into the picked one and drop
  /// the now-empty source group.
  pub(crate) fn merge_current_into(&mut self, cursor: usize) {
    let targets = self.move_target_groups();
    let Some(&(dest_idx, _)) = targets.get(cursor) else {
      self.mode = Mode::Normal;
      return;
    };
    let dest_group_id = self.groups[dest_idx].id;
    let dest_dir = self.group_dir(dest_idx);

    let moves = std::mem::take(&mut self.group_moves[self.selected]);
    let keeps = std::mem::take(&mut self.file_keep[self.selected]);
    for (mut mv, kept) in moves.into_iter().zip(keeps) {
      let filename =
        PathBuf::from(mv.from.file_name().unwrap_or_default());
      mv.group_id = dest_group_id;
      mv.to = dest_dir.join(filename);
      self.group_moves[dest_idx].push(mv);
      self.file_keep[dest_idx].push(kept);
    }
    self.clear_marks();
    self.delete_current_group();
    self.mode = Mode::Normal;
  }

  pub(crate) fn enter_new_group(&mut self) {
    if self.current_group_moves().is_empty() {
      return;
    }
    self.mode = Mode::NewGroup {
      input: String::new(),
      cursor_pos: 0,
    };
  }

  pub(crate) fn confirm_new_group(&mut self, input: String) {
    let name = input.trim().to_string();
    if name.is_empty() {
      return;
    }

    let slug = name.to_lowercase().replace(' ', "_");
    let new_id = self.next_group_id;
    self.next_group_id += 1;

    let new_group = FileGroup {
      id: new_id,
      label: name,
      rationale: String::new(),
      members: vec![],
      member_destinations: vec![],
      suggested_path: PathBuf::from(&slug),
      member_notes: vec![],
    };

    self.groups.push(new_group);
    self.approved.push(true);
    self.group_moves.push(Vec::new());
    self.file_keep.push(Vec::new());
    self.file_marked.push(HashSet::new());

    let indices = self.marked_file_indices();
    let new_idx = self.groups.len() - 1;
    for &i in indices.iter().rev() {
      if i >= self.group_moves[self.selected].len() {
        continue;
      }
      let mut file_move = self.group_moves[self.selected].remove(i);
      let was_kept = self.file_keep[self.selected].remove(i);

      let filename = file_move
        .from
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

      file_move.group_id = new_id;
      file_move.to = self.output_dir.join(&slug).join(&filename);

      self.group_moves[new_idx].push(file_move);
      self.file_keep[new_idx].push(was_kept);
    }
    self.clear_marks();

    let src_count = self.group_moves[self.selected].len();
    if src_count == 0 {
      self.file_selected = 0;
      self.mode = Mode::ConfirmRemove;
    } else {
      self.file_selected = self.file_selected.min(src_count - 1);
      self.mode = Mode::Normal;
    }
  }

  #[cfg(test)]
  pub(crate) fn resolve_move_target_index(
    &self,
    cursor: usize,
  ) -> Option<usize> {
    self.move_target_groups().get(cursor).map(|(i, _)| *i)
  }
}

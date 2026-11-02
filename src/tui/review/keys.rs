//! Key handling and every mutation the review screen can make.

use std::ffi::OsString;

use super::*;

/// Rows PageUp/PageDown move the detail column by.
const DETAIL_PAGE: i16 = 10;

/// Rows `[` and `]` scroll the detail column and the modals by.
const SCROLL_STEP: usize = 3;

/// The next (or previous) index in a list of `len`, wrapping at both
/// ends. `len` must be non-zero.
fn cycle(index: usize, len: usize, forward: bool) -> usize {
  if forward {
    (index + 1) % len
  } else {
    index.checked_sub(1).unwrap_or(len - 1)
  }
}

/// Byte offset of the `chars`th character of `s`, or its length.
///
/// The name prompts keep their cursor in characters: a byte index
/// that lands inside "é" panics on the next insert or delete.
fn char_to_byte(s: &str, chars: usize) -> usize {
  s.char_indices()
    .nth(chars)
    .map_or(s.len(), |(byte, _)| byte)
}

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
      Mode::MoveToGroup { .. } | Mode::MergeInto { .. } => {
        self.handle_picker_key(code)
      }
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

  fn handle_confirm_execute_key(&mut self, code: KeyCode) {
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

  fn handle_normal_key(&mut self, code: KeyCode) {
    let organize = self.review_mode == ReviewMode::Organize;
    let on_files = self.focus == Pane::Files;
    let on_detail = self.focus == Pane::Detail;

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
      KeyCode::Down if on_detail => self.scroll_detail(1),
      KeyCode::Up if on_detail => self.scroll_detail(-1),
      KeyCode::PageDown if on_detail => {
        self.scroll_detail(DETAIL_PAGE)
      }
      KeyCode::PageUp if on_detail => {
        self.scroll_detail(-DETAIL_PAGE)
      }
      KeyCode::Home if on_detail => self.detail_scroll = 0,
      KeyCode::End if on_detail => self.detail_scroll = u16::MAX,

      KeyCode::Down => self.move_cursor(true),
      KeyCode::Up => self.move_cursor(false),

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

      // The detail pane has no cursor of its own, so it scrolls with
      // the same keys the preview and diff modals use.
      KeyCode::Char('[') => self.scroll_detail(-(SCROLL_STEP as i16)),
      KeyCode::Char(']') => self.scroll_detail(SCROLL_STEP as i16),

      KeyCode::Char('x') => self.mode = Mode::ConfirmExecute,
      KeyCode::Char('?') => self.mode = Mode::Help,

      KeyCode::Char('D')
        if !organize && self.current_group_moves().len() > 1 =>
      {
        self.mode = Mode::DiffView { compare_idx: 0 };
        self.enter_diff_view(0);
      }

      KeyCode::Char('r') if organize => self.enter_rename(),
      KeyCode::Char('M') if organize => self.enter_merge(),

      // Everything below acts on files of an organize plan.
      _ if !(organize && on_files) => {}
      KeyCode::Char('v') => self.toggle_file_mark(),
      KeyCode::Char('d') => self.remove_marked_files(),
      KeyCode::Char('D') => self.enter_dupe_diff(),
      KeyCode::Char('m') => self.enter_move_to_group(),
      KeyCode::Char('n') => self.enter_new_group(),
      KeyCode::Char(c @ '1'..='3') => {
        let n = c.to_digit(10).unwrap_or(0) as usize;
        self.move_current_file_to_alternative(n);
      }
      _ => {}
    }
  }

  /// Up/Down in a list pane: step the cursor, wrapping at both ends.
  fn move_cursor(&mut self, forward: bool) {
    match self.focus {
      Pane::Groups if !self.groups.is_empty() => {
        self.clear_marks();
        self.selected =
          cycle(self.selected, self.groups.len(), forward);
        self.file_selected = 0;
      }
      Pane::Files => {
        let count = self.current_group_moves().len();
        if count > 0 {
          self.file_selected =
            cycle(self.file_selected, count, forward);
        }
      }
      _ => {}
    }
  }

  /// The "Move to…" and "Merge into…" pickers: same list, same keys,
  /// different action on Enter.
  fn handle_picker_key(&mut self, code: KeyCode) {
    let (cursor, merge) = match self.mode {
      Mode::MoveToGroup { cursor } => (cursor, false),
      Mode::MergeInto { cursor } => (cursor, true),
      _ => return,
    };
    let target_count = self.move_target_groups().len();
    if target_count == 0 {
      self.mode = Mode::Normal;
      return;
    }

    let cursor = match code {
      KeyCode::Down => cycle(cursor, target_count, true),
      KeyCode::Up => cycle(cursor, target_count, false),
      KeyCode::Enter if merge => {
        return self.merge_current_into(cursor)
      }
      KeyCode::Enter => return self.confirm_move_to_group(cursor),
      KeyCode::Esc => {
        self.mode = Mode::Normal;
        return;
      }
      _ => return,
    };
    self.mode = if merge {
      Mode::MergeInto { cursor }
    } else {
      Mode::MoveToGroup { cursor }
    };
  }

  /// Shared line editor for the NewGroup and RenameGroup prompts.
  /// `cursor_pos` counts characters, not bytes.
  fn handle_text_input_key(&mut self, code: KeyCode) {
    let (mut input, mut cursor, rename) = match &self.mode {
      Mode::NewGroup { input, cursor_pos } => {
        (input.clone(), *cursor_pos, false)
      }
      Mode::RenameGroup { input, cursor_pos } => {
        (input.clone(), *cursor_pos, true)
      }
      _ => return,
    };

    match code {
      KeyCode::Char(c) => {
        input.insert(char_to_byte(&input, cursor), c);
        cursor += 1;
      }
      KeyCode::Backspace if cursor > 0 => {
        cursor -= 1;
        input.remove(char_to_byte(&input, cursor));
      }
      KeyCode::Left => cursor = cursor.saturating_sub(1),
      KeyCode::Right => {
        cursor = (cursor + 1).min(input.chars().count())
      }
      KeyCode::Enter if rename => return self.confirm_rename(input),
      KeyCode::Enter => return self.confirm_new_group(input),
      KeyCode::Esc => {
        self.mode = Mode::Normal;
        return;
      }
      _ => return,
    }
    self.mode = if rename {
      Mode::RenameGroup {
        input,
        cursor_pos: cursor,
      }
    } else {
      Mode::NewGroup {
        input,
        cursor_pos: cursor,
      }
    };
  }

  fn handle_confirm_remove_key(&mut self, code: KeyCode) {
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

  /// Scroll the open preview or diff one step down or up.
  fn scroll_modal(&mut self, forward: bool) {
    if let Some(ds) = &mut self.diff_state {
      ds.scroll = if forward {
        ds.scroll.saturating_add(SCROLL_STEP)
      } else {
        ds.scroll.saturating_sub(SCROLL_STEP)
      };
    }
  }

  fn handle_diff_view_key(&mut self, code: KeyCode) {
    let Mode::DiffView { compare_idx } = self.mode else {
      return;
    };
    // Only a duplicate set can be cycled: its files are copies of one
    // another. An organize diff pairs one file with its partner, which
    // may sit in another group; stepping through the current group
    // would compare it with files it has nothing to do with.
    let cycles = self.review_mode == ReviewMode::Dupes;
    // `compare_idx` counts the copies after the first file, so the
    // last one is two below the file count.
    let last = self.current_group_moves().len().saturating_sub(2);

    match code {
      KeyCode::Down if cycles => {
        self.show_diff((compare_idx + 1).min(last))
      }
      KeyCode::Up if cycles => {
        self.show_diff(compare_idx.saturating_sub(1))
      }
      KeyCode::Char('[') => self.scroll_modal(false),
      KeyCode::Char(']') => self.scroll_modal(true),
      KeyCode::Esc
      | KeyCode::Char('d')
      | KeyCode::Char('D')
      | KeyCode::Char('q') => self.close_modal(),
      _ => {}
    }
  }

  fn show_diff(&mut self, compare_idx: usize) {
    self.mode = Mode::DiffView { compare_idx };
    self.enter_diff_view(compare_idx);
  }

  fn handle_preview_key(&mut self, code: KeyCode) {
    match code {
      KeyCode::Char('[') => self.scroll_modal(false),
      KeyCode::Char(']') => self.scroll_modal(true),
      KeyCode::Esc | KeyCode::Char(' ') | KeyCode::Char('q') => {
        self.close_modal()
      }
      _ => {}
    }
  }

  fn close_modal(&mut self) {
    self.mode = Mode::Normal;
    self.exit_diff_view();
  }

  /// Drop the marked files (or the one under the cursor) from the
  /// plan. They stay where they are.
  pub(crate) fn remove_marked_files(&mut self) {
    if self.current_group_moves().is_empty() {
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
    self.settle_after_files_left();
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

    self.selected =
      self.selected.min(self.groups.len().saturating_sub(1));
    self.file_selected = 0;
  }

  fn enter_move_to_group(&mut self) {
    if !self.current_group_moves().is_empty()
      && self.groups.len() >= 2
    {
      self.mode = Mode::MoveToGroup { cursor: 0 };
    }
  }

  fn confirm_move_to_group(&mut self, cursor: usize) {
    match self.move_target_groups().get(cursor) {
      Some(&(dest_idx, _)) => self.move_marked_to_group(dest_idx),
      None => self.mode = Mode::Normal,
    }
  }

  /// Move the marked files (or the one under the cursor) of the
  /// selected group into `dest_idx`, then leave move mode.
  pub(crate) fn move_marked_to_group(&mut self, dest_idx: usize) {
    if dest_idx >= self.groups.len() || dest_idx == self.selected {
      self.mode = Mode::Normal;
      return;
    }
    let indices = self.marked_file_indices();
    self.transfer_files(&indices, dest_idx);
    self.settle_after_files_left();
  }

  /// Move files `indices` of the selected group into group `dest_idx`,
  /// pointed at that group's folder under their own file names. Each
  /// file's keep/delete choice travels with it, and the files keep
  /// their order.
  fn transfer_files(&mut self, indices: &[usize], dest_idx: usize) {
    let dest_id = self.groups[dest_idx].id;
    let dest_dir = self.group_dir(dest_idx);
    let src = self.selected;

    let mut moved = Vec::with_capacity(indices.len());
    for &i in indices.iter().rev() {
      if i < self.group_moves[src].len() {
        moved.push((
          self.group_moves[src].remove(i),
          self.file_keep[src].remove(i),
        ));
      }
    }
    for (mut mv, kept) in moved.into_iter().rev() {
      // The OS name as-is: a lossy conversion would rename files
      // whose names are not valid UTF-8.
      let name = mv
        .from
        .file_name()
        .map_or_else(|| OsString::from("unknown"), OsString::from);
      mv.to = dest_dir.join(name);
      mv.group_id = dest_id;
      self.group_moves[dest_idx].push(mv);
      self.file_keep[dest_idx].push(kept);
    }
    self.clear_marks();
  }

  /// After files leave the selected group: keep the file cursor in
  /// range, and offer to drop the group once nothing is left in it.
  fn settle_after_files_left(&mut self) {
    match self.current_group_moves().len() {
      0 => {
        self.file_selected = 0;
        self.mode = Mode::ConfirmRemove;
      }
      count => {
        self.file_selected = self.file_selected.min(count - 1);
        self.mode = Mode::Normal;
      }
    }
  }

  /// Absolute destination directory of a group. Pipeline and review
  /// groups carry an absolute `suggested_path`; a relative one is
  /// taken as relative to the output directory.
  pub(crate) fn group_dir(&self, idx: usize) -> PathBuf {
    self.output_dir.join(&self.groups[idx].suggested_path)
  }

  /// The folder a typed group name becomes. The name prompt previews
  /// this same path, so what the user sees is what gets created.
  pub(crate) fn dir_for_name(&self, name: &str) -> PathBuf {
    self
      .output_dir
      .join(crate::group::sanitize_folder_name(name))
  }

  fn enter_rename(&mut self) {
    let Some(group) = self.groups.get(self.selected) else {
      return;
    };
    let input = group.label.clone();
    let cursor_pos = input.chars().count();
    self.mode = Mode::RenameGroup { input, cursor_pos };
  }

  /// Relabel the current group and point every one of its moves at the
  /// new folder, keeping any sub-path under the old one.
  fn confirm_rename(&mut self, input: String) {
    let name = input.trim().to_string();
    if name.is_empty() {
      return;
    }
    let old_dir = self.group_dir(self.selected);
    let new_dir = self.dir_for_name(&name);
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

  fn enter_merge(&mut self) {
    if self.groups.len() >= 2 {
      self.mode = Mode::MergeInto { cursor: 0 };
    }
  }

  /// Move every file of the current group into the picked one and drop
  /// the now-empty source group.
  fn merge_current_into(&mut self, cursor: usize) {
    let Some(&(dest_idx, _)) = self.move_target_groups().get(cursor)
    else {
      self.mode = Mode::Normal;
      return;
    };
    let all: Vec<usize> =
      (0..self.current_group_moves().len()).collect();
    self.transfer_files(&all, dest_idx);
    self.delete_current_group();
    self.mode = Mode::Normal;
  }

  fn enter_new_group(&mut self) {
    if !self.current_group_moves().is_empty() {
      self.mode = Mode::NewGroup {
        input: String::new(),
        cursor_pos: 0,
      };
    }
  }

  /// Create a group from the typed name and move the marked files (or
  /// the one under the cursor) into it.
  fn confirm_new_group(&mut self, input: String) {
    let name = input.trim().to_string();
    if name.is_empty() {
      return;
    }
    let id = self.next_group_id;
    self.next_group_id += 1;

    self.groups.push(FileGroup {
      id,
      suggested_path: self.dir_for_name(&name),
      label: name,
      rationale: String::new(),
      members: vec![],
      member_destinations: vec![],
      member_notes: vec![],
    });
    self.approved.push(true);
    self.group_moves.push(Vec::new());
    self.file_keep.push(Vec::new());
    self.file_marked.push(HashSet::new());

    let indices = self.marked_file_indices();
    self.transfer_files(&indices, self.groups.len() - 1);
    self.settle_after_files_left();
  }

  #[cfg(test)]
  pub(crate) fn resolve_move_target_index(
    &self,
    cursor: usize,
  ) -> Option<usize> {
    self.move_target_groups().get(cursor).map(|(i, _)| *i)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn cycle_wraps_both_ways() {
    assert_eq!(cycle(0, 3, true), 1);
    assert_eq!(cycle(2, 3, true), 0);
    assert_eq!(cycle(0, 3, false), 2);
    assert_eq!(cycle(1, 3, false), 0);
    assert_eq!(cycle(0, 1, false), 0);
  }

  #[test]
  fn char_to_byte_counts_characters() {
    assert_eq!(char_to_byte("Café", 3), 3);
    assert_eq!(char_to_byte("Café", 4), 5);
    assert_eq!(char_to_byte("éa", 1), 2);
    assert_eq!(char_to_byte("", 0), 0);
  }
}

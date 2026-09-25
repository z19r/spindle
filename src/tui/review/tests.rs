use super::render::*;
use super::*;
use std::path::PathBuf;

fn make_groups() -> Vec<FileGroup> {
  vec![
    FileGroup {
      id: 0,
      label: "Beach".to_string(),
      rationale: "Beach photos".to_string(),
      members: vec![0, 1],
      member_destinations: vec![],
      suggested_path: PathBuf::from("beach"),
      member_notes: vec![],
    },
    FileGroup {
      id: 1,
      label: "Cats".to_string(),
      rationale: "Cat photos".to_string(),
      members: vec![2, 3, 4],
      member_destinations: vec![],
      suggested_path: PathBuf::from("cats"),
      member_notes: vec![],
    },
  ]
}

fn make_moves() -> Vec<FileMove> {
  vec![
    FileMove {
      from: PathBuf::from("/dl/beach1.jpg"),
      to: PathBuf::from("/out/beach/beach1.jpg"),
      group_id: 0,
    },
    FileMove {
      from: PathBuf::from("/dl/beach2.jpg"),
      to: PathBuf::from("/out/beach/beach2.jpg"),
      group_id: 0,
    },
    FileMove {
      from: PathBuf::from("/dl/cat1.jpg"),
      to: PathBuf::from("/out/cats/cat1.jpg"),
      group_id: 1,
    },
    FileMove {
      from: PathBuf::from("/dl/cat2.jpg"),
      to: PathBuf::from("/out/cats/cat2.jpg"),
      group_id: 1,
    },
    FileMove {
      from: PathBuf::from("/dl/cat3.jpg"),
      to: PathBuf::from("/out/cats/cat3.jpg"),
      group_id: 1,
    },
  ]
}

fn make_state_with_mode(mode: ReviewMode) -> ReviewState {
  ReviewState::new(
    make_groups(),
    make_moves(),
    PathBuf::from("/out"),
    None,
    mode,
  )
}

fn make_state() -> ReviewState {
  make_state_with_mode(ReviewMode::Organize)
}

fn make_empty_review_state() -> ReviewState {
  ReviewState::new(
    vec![],
    vec![],
    PathBuf::from("/out"),
    None,
    ReviewMode::Organize,
  )
}

fn make_dupes_state() -> ReviewState {
  make_state_with_mode(ReviewMode::Dupes)
}

fn type_text(state: &mut ReviewState, text: &str) {
  for c in text.chars() {
    state.handle_key(KeyCode::Char(c));
  }
}

#[test]
fn r_opens_rename_prefilled_with_the_label() {
  let mut state = make_state();
  state.handle_key(KeyCode::Char('r'));
  assert_eq!(
    state.mode,
    Mode::RenameGroup {
      input: "Beach".to_string(),
      cursor_pos: 5
    }
  );
}

#[test]
fn rename_updates_label_path_and_every_move() {
  let mut state = make_state();
  state.handle_key(KeyCode::Char('r'));
  for _ in 0..5 {
    state.handle_key(KeyCode::Backspace);
  }
  type_text(&mut state, "Sea Days");
  state.handle_key(KeyCode::Enter);

  assert_eq!(state.mode, Mode::Normal);
  assert_eq!(state.groups[0].label, "Sea Days");
  assert_eq!(
    state.groups[0].suggested_path,
    PathBuf::from("/out/sea_days")
  );
  assert_eq!(state.group_moves[0].len(), 2);
  for mv in &state.group_moves[0] {
    assert_eq!(mv.to.parent().unwrap(), Path::new("/out/sea_days"));
    assert_eq!(mv.to.file_name(), mv.from.file_name());
    assert_eq!(mv.group_id, 0);
  }
  // Other groups untouched.
  assert_eq!(state.groups[1].label, "Cats");
  assert!(state.group_moves[1]
    .iter()
    .all(|m| m.to.starts_with("/out/cats")));
}

#[test]
fn rename_keeps_nested_destinations() {
  let mut state = make_state();
  state.group_moves[0][1].to =
    PathBuf::from("/out/beach/raw/beach2.jpg");
  state.handle_key(KeyCode::Char('r'));
  type_text(&mut state, " Days");
  state.handle_key(KeyCode::Enter);
  assert_eq!(
    state.group_moves[0][1].to,
    PathBuf::from("/out/beach_days/raw/beach2.jpg")
  );
}

#[test]
fn rename_esc_cancels_and_empty_name_is_ignored() {
  let mut state = make_state();
  state.handle_key(KeyCode::Char('r'));
  for _ in 0..5 {
    state.handle_key(KeyCode::Backspace);
  }
  state.handle_key(KeyCode::Enter);
  assert!(matches!(state.mode, Mode::RenameGroup { .. }));
  state.handle_key(KeyCode::Esc);
  assert_eq!(state.mode, Mode::Normal);
  assert_eq!(state.groups[0].label, "Beach");
}

#[test]
fn rename_and_merge_are_disabled_in_dupes_mode() {
  let mut state = make_dupes_state();
  state.handle_key(KeyCode::Char('r'));
  assert_eq!(state.mode, Mode::Normal);
  state.handle_key(KeyCode::Char('M'));
  assert_eq!(state.mode, Mode::Normal);
}

#[test]
fn shift_m_merges_the_current_group_into_the_picked_one() {
  let mut state = make_state();
  state.handle_key(KeyCode::Char('M'));
  assert_eq!(state.mode, Mode::MergeInto { cursor: 0 });
  state.handle_key(KeyCode::Enter);

  assert_eq!(state.mode, Mode::Normal);
  assert_eq!(state.groups.len(), 1);
  assert_eq!(state.groups[0].label, "Cats");
  assert_eq!(state.group_moves[0].len(), 5);
  assert_eq!(state.file_keep[0].len(), 5);
  assert_eq!(state.selected, 0);
  let beach = state.group_moves[0]
    .iter()
    .find(|m| m.from.ends_with("beach1.jpg"))
    .unwrap();
  assert_eq!(beach.to, PathBuf::from("/out/cats/beach1.jpg"));
  assert_eq!(beach.group_id, 1);
}

#[test]
fn merge_picker_navigates_and_esc_cancels() {
  let mut state = make_state();
  state.groups.push(FileGroup {
    id: 2,
    label: "Dogs".to_string(),
    rationale: String::new(),
    members: vec![],
    member_destinations: vec![],
    suggested_path: PathBuf::from("dogs"),
    member_notes: vec![],
  });
  state.approved.push(true);
  state.group_moves.push(vec![]);
  state.file_keep.push(vec![]);
  state.file_marked.push(HashSet::new());

  state.handle_key(KeyCode::Char('M'));
  state.handle_key(KeyCode::Char('j'));
  assert_eq!(state.mode, Mode::MergeInto { cursor: 1 });
  state.handle_key(KeyCode::Esc);
  assert_eq!(state.mode, Mode::Normal);
  assert_eq!(state.groups.len(), 3);
}

#[test]
fn merge_with_a_single_group_is_a_noop() {
  let mut state = ReviewState::new(
    make_groups().into_iter().take(1).collect(),
    make_moves().into_iter().take(2).collect(),
    PathBuf::from("/out"),
    None,
    ReviewMode::Organize,
  );
  state.handle_key(KeyCode::Char('M'));
  assert_eq!(state.mode, Mode::Normal);
}

#[test]
fn system_groups_come_first_and_start_unapproved() {
  let mut groups = make_groups();
  groups.push(FileGroup {
    id: 2,
    label: crate::pipeline::UNSORTED_LABEL.to_string(),
    rationale: "leftovers".to_string(),
    members: vec![5],
    member_destinations: vec![],
    suggested_path: PathBuf::from("unsorted"),
    member_notes: vec![],
  });
  let mut moves = make_moves();
  moves.push(FileMove {
    from: PathBuf::from("/dl/odd.bin"),
    to: PathBuf::from("/out/unsorted/odd.bin"),
    group_id: 2,
  });
  let state = ReviewState::new(
    groups,
    moves,
    PathBuf::from("/out"),
    None,
    ReviewMode::Organize,
  );
  let labels: Vec<&str> =
    state.groups.iter().map(|g| g.label.as_str()).collect();
  assert_eq!(labels, vec!["Unsorted", "Beach", "Cats"]);
  assert_eq!(state.approved, vec![false, true, true]);
  assert_eq!(state.group_moves[0].len(), 1);
  assert!(state.group_moves[0][0].from.ends_with("odd.bin"));
  assert_eq!(state.group_moves[1].len(), 2);
}

#[test]
fn file_notes_are_looked_up_by_path_and_rendered() {
  let mut groups = make_groups();
  groups[0].member_notes = vec![crate::model::MemberNote {
    index: 1,
    note: "not placed by grouping".to_string(),
  }];
  let mut state = ReviewState::new(
    groups,
    make_moves(),
    PathBuf::from("/out"),
    None,
    ReviewMode::Organize,
  );
  assert_eq!(state.file_note(Path::new("/dl/beach1.jpg")), None);
  assert_eq!(
    state.file_note(Path::new("/dl/beach2.jpg")),
    Some("not placed by grouping")
  );

  state.focus = Pane::Files;
  state.file_selected = 1;
  let text: String = render_detail_file(&state)
    .iter()
    .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
    .collect();
  assert!(text.contains("not placed by grouping"), "{text}");
}

#[test]
fn new_builds_group_moves_correctly() {
  let state = make_state();
  assert_eq!(state.group_moves.len(), 2);
  assert_eq!(state.group_moves[0].len(), 2);
  assert_eq!(state.group_moves[1].len(), 3);
}

#[test]
fn tab_switches_focus() {
  let mut state = make_state();
  assert_eq!(state.focus(), Pane::Groups);
  state.handle_key(KeyCode::Tab);
  assert_eq!(state.focus(), Pane::Files);
  state.handle_key(KeyCode::Tab);
  assert_eq!(state.focus(), Pane::Groups);
}

#[test]
fn group_nav_j_k() {
  let mut state = make_state();
  assert_eq!(state.selected_index(), 0);
  state.handle_key(KeyCode::Char('j'));
  assert_eq!(state.selected_index(), 1);
  state.handle_key(KeyCode::Char('j'));
  assert_eq!(state.selected_index(), 0);
}

#[test]
fn file_nav_j_k_in_files_pane() {
  let mut state = make_state();
  state.handle_key(KeyCode::Char('j'));
  state.handle_key(KeyCode::Tab);
  assert_eq!(state.file_selected(), 0);
  state.handle_key(KeyCode::Char('j'));
  assert_eq!(state.file_selected(), 1);
  state.handle_key(KeyCode::Char('j'));
  assert_eq!(state.file_selected(), 2);
  state.handle_key(KeyCode::Char('j'));
  assert_eq!(state.file_selected(), 0);
}

#[test]
fn group_change_resets_file_cursor() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('j'));
  assert_eq!(state.file_selected(), 1);
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('j'));
  assert_eq!(state.file_selected(), 0);
}

#[test]
fn space_toggles_group_from_groups_pane() {
  let mut state = make_state();
  assert!(state.is_approved(0));
  state.handle_key(KeyCode::Char(' '));
  assert!(!state.is_approved(0));
}

#[test]
fn space_toggles_file_keep_from_files_pane() {
  let mut state = make_dupes_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('j'));
  assert!(!state.is_file_kept(0, 1));
  state.handle_key(KeyCode::Char(' '));
  assert!(state.is_file_kept(0, 1));
}

#[test]
fn current_group_moves_returns_correct_slice() {
  let state = make_state();
  assert_eq!(state.current_group_moves().len(), 2);
  assert_eq!(
    state.current_group_moves()[0].from,
    PathBuf::from("/dl/beach1.jpg")
  );
}

#[test]
fn current_file_move_returns_selected() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('j'));
  let mv = state.current_file_move().unwrap();
  assert_eq!(mv.from, PathBuf::from("/dl/beach2.jpg"));
}

#[test]
fn file_nav_wraps_up() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('k'));
  assert_eq!(state.file_selected(), 1);
}

#[test]
fn x_opens_confirmation_then_enter_executes() {
  let mut state = make_state();
  state.handle_key(KeyCode::Char('x'));
  assert_eq!(*state.mode(), Mode::ConfirmExecute);
  assert_eq!(state.pending_action(), None);

  state.handle_key(KeyCode::Enter);
  assert_eq!(state.pending_action(), Some(ReviewAction::Execute));
  assert_eq!(*state.mode(), Mode::Normal);
}

#[test]
fn confirm_execute_can_be_cancelled() {
  let mut state = make_state();
  state.handle_key(KeyCode::Char('x'));
  state.handle_key(KeyCode::Esc);
  assert_eq!(state.pending_action(), None);
  assert_eq!(*state.mode(), Mode::Normal);
}

#[test]
fn question_mark_opens_help_any_key_closes() {
  let mut state = make_state();
  state.handle_key(KeyCode::Char('?'));
  assert_eq!(*state.mode(), Mode::Help);

  state.handle_key(KeyCode::Char('j'));
  assert_eq!(*state.mode(), Mode::Normal);
}

#[test]
fn is_approved_out_of_bounds() {
  let state = make_state();
  assert!(!state.is_approved(999));
}

#[test]
fn approved_groups_filters_correctly() {
  let mut state = make_state();
  state.handle_key(KeyCode::Char(' '));
  let approved = state.approved_groups();
  assert_eq!(approved.len(), 1);
  assert_eq!(approved[0].label, "Cats");
}

#[test]
fn empty_groups_handles_gracefully() {
  let state = make_empty_review_state();
  assert_eq!(state.current_group_moves().len(), 0);
  assert!(state.current_file_move().is_none());
  assert!(state.approved_groups().is_empty());
}

#[test]
fn wrap_text_splits_long_lines() {
  let result = wrap_text("hello world this is a test", 12);
  assert_eq!(result, vec!["hello world", "this is a", "test"]);
}

#[test]
fn wrap_text_handles_empty() {
  let result = wrap_text("", 20);
  assert_eq!(result, vec![""]);
}

#[test]
fn wrap_text_single_word() {
  let result = wrap_text("hello", 3);
  assert_eq!(result, vec!["hello"]);
}

// --- Per-file operation tests ---

#[test]
fn remove_file_decrements_count() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  assert_eq!(state.current_group_moves().len(), 2);
  state.handle_key(KeyCode::Char('d'));
  assert_eq!(state.current_group_moves().len(), 1);
}

#[test]
fn remove_file_clamps_cursor() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('j'));
  assert_eq!(state.file_selected(), 1);
  state.handle_key(KeyCode::Char('d'));
  assert_eq!(state.file_selected(), 0);
}

#[test]
fn remove_last_file_enters_confirm_mode() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('d'));
  state.handle_key(KeyCode::Char('d'));
  assert_eq!(state.mode, Mode::ConfirmRemove);
}

#[test]
fn confirm_y_deletes_group() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('d'));
  state.handle_key(KeyCode::Char('d'));
  assert_eq!(state.groups.len(), 2);
  state.handle_key(KeyCode::Char('y'));
  assert_eq!(state.groups.len(), 1);
  assert_eq!(state.groups[0].label, "Cats");
  assert_eq!(state.mode, Mode::Normal);
}

#[test]
fn confirm_n_keeps_group() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('d'));
  state.handle_key(KeyCode::Char('d'));
  state.handle_key(KeyCode::Char('n'));
  assert_eq!(state.groups.len(), 2);
  assert_eq!(state.mode, Mode::Normal);
}

#[test]
fn move_to_group_transfers_file() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  assert_eq!(state.group_moves[0].len(), 2);
  assert_eq!(state.group_moves[1].len(), 3);

  state.handle_key(KeyCode::Char('m'));
  assert!(matches!(state.mode, Mode::MoveToGroup { cursor: 0 }));

  state.handle_key(KeyCode::Enter);
  assert_eq!(state.group_moves[0].len(), 1);
  assert_eq!(state.group_moves[1].len(), 4);
  assert_eq!(state.mode, Mode::Normal);
}

#[test]
fn move_updates_path_and_group_id() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('m'));
  state.handle_key(KeyCode::Enter);

  let moved = state.group_moves[1].last().unwrap();
  assert_eq!(moved.group_id, 1);
  assert_eq!(moved.to, PathBuf::from("/out/cats/beach1.jpg"));
}

#[test]
fn move_to_group_noop_single_group() {
  let mut state = ReviewState::new(
    vec![FileGroup {
      id: 0,
      label: "Only".to_string(),
      rationale: String::new(),
      members: vec![0],
      member_destinations: vec![],
      suggested_path: PathBuf::from("only"),
      member_notes: vec![],
    }],
    vec![FileMove {
      from: PathBuf::from("/dl/a.jpg"),
      to: PathBuf::from("/out/only/a.jpg"),
      group_id: 0,
    }],
    PathBuf::from("/out"),
    None,
    ReviewMode::Organize,
  );
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('m'));
  assert_eq!(state.mode, Mode::Normal);
}

#[test]
fn move_skips_current_in_picker() {
  let state = make_state();
  let targets = state.move_target_groups();
  assert_eq!(targets.len(), 1);
  assert_eq!(targets[0].1.label, "Cats");
}

#[test]
fn new_group_creates_with_correct_id() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('n'));

  for c in "Vacation".chars() {
    state.handle_key(KeyCode::Char(c));
  }
  state.handle_key(KeyCode::Enter);

  assert_eq!(state.groups.len(), 3);
  assert_eq!(state.groups[2].id, 2);
  assert_eq!(state.groups[2].label, "Vacation");
  assert_eq!(
    state.groups[2].suggested_path,
    PathBuf::from("vacation")
  );
}

#[test]
fn new_group_approved_by_default() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('n'));
  for c in "New".chars() {
    state.handle_key(KeyCode::Char(c));
  }
  state.handle_key(KeyCode::Enter);

  assert!(state.is_approved(2));
}

#[test]
fn new_group_moves_file_correctly() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('n'));
  for c in "Trip".chars() {
    state.handle_key(KeyCode::Char(c));
  }
  state.handle_key(KeyCode::Enter);

  assert_eq!(state.group_moves[0].len(), 1);
  assert_eq!(state.group_moves[2].len(), 1);
  let moved = &state.group_moves[2][0];
  assert_eq!(moved.from, PathBuf::from("/dl/beach1.jpg"));
  assert_eq!(moved.to, PathBuf::from("/out/trip/beach1.jpg"));
  assert_eq!(moved.group_id, 2);
}

#[test]
fn new_group_empty_name_noop() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('n'));
  state.handle_key(KeyCode::Enter);
  assert_eq!(state.groups.len(), 2);
  assert!(matches!(state.mode, Mode::NewGroup { .. }));
}

#[test]
fn text_input_handles_backspace() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('n'));
  for c in "abc".chars() {
    state.handle_key(KeyCode::Char(c));
  }
  state.handle_key(KeyCode::Backspace);
  if let Mode::NewGroup { input, cursor_pos } = &state.mode {
    assert_eq!(input, "ab");
    assert_eq!(*cursor_pos, 2);
  } else {
    panic!("Expected NewGroup mode");
  }
}

#[test]
fn esc_cancels_move_to_group() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('m'));
  assert!(matches!(state.mode, Mode::MoveToGroup { .. }));
  state.handle_key(KeyCode::Esc);
  assert_eq!(state.mode, Mode::Normal);
}

#[test]
fn esc_cancels_new_group() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('n'));
  state.handle_key(KeyCode::Esc);
  assert_eq!(state.mode, Mode::Normal);
}

#[test]
fn esc_cancels_confirm_remove() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('d'));
  state.handle_key(KeyCode::Char('d'));
  assert_eq!(state.mode, Mode::ConfirmRemove);
  state.handle_key(KeyCode::Esc);
  assert_eq!(state.mode, Mode::Normal);
  assert_eq!(state.groups.len(), 2);
}

#[test]
fn d_noop_in_groups_pane() {
  let mut state = make_state();
  assert_eq!(state.focus(), Pane::Groups);
  state.handle_key(KeyCode::Char('d'));
  assert_eq!(state.group_moves[0].len(), 2);
}

#[test]
fn d_noop_with_empty_file_list() {
  let mut state = ReviewState::new(
    vec![FileGroup {
      id: 0,
      label: "Empty".to_string(),
      rationale: String::new(),
      members: vec![],
      member_destinations: vec![],
      suggested_path: PathBuf::from("empty"),
      member_notes: vec![],
    }],
    vec![],
    PathBuf::from("/out"),
    None,
    ReviewMode::Organize,
  );
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('d'));
  assert_eq!(state.mode, Mode::Normal);
}

#[test]
fn approved_moves_reflects_mutations() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('d'));

  let moves = state.approved_moves();
  assert_eq!(moves.len(), 4);
}

#[test]
fn approved_moves_excludes_unapproved_groups() {
  let mut state = make_state();
  state.handle_key(KeyCode::Char(' '));

  let moves = state.approved_moves();
  assert_eq!(moves.len(), 3);
  assert!(moves.iter().all(|m| m.group_id == 1));
}

#[test]
fn mode_starts_as_normal() {
  let state = make_state();
  assert_eq!(*state.mode(), Mode::Normal);
}

#[test]
fn next_group_id_initializes_correctly() {
  let state = make_state();
  assert_eq!(state.next_group_id, 2);
}

#[test]
fn move_to_group_picker_navigates() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('m'));

  if let Mode::MoveToGroup { cursor } = state.mode {
    assert_eq!(cursor, 0);
  }

  state.handle_key(KeyCode::Char('j'));
  if let Mode::MoveToGroup { cursor } = state.mode {
    assert_eq!(cursor, 0);
  }
}

#[test]
fn text_input_cursor_movement() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('n'));
  for c in "abc".chars() {
    state.handle_key(KeyCode::Char(c));
  }
  state.handle_key(KeyCode::Left);
  if let Mode::NewGroup { cursor_pos, .. } = &state.mode {
    assert_eq!(*cursor_pos, 2);
  }
  state.handle_key(KeyCode::Right);
  if let Mode::NewGroup { cursor_pos, .. } = &state.mode {
    assert_eq!(*cursor_pos, 3);
  }
}

#[test]
fn resolve_move_target_index_works() {
  let state = make_state();
  assert_eq!(state.resolve_move_target_index(0), Some(1));
  assert_eq!(state.resolve_move_target_index(1), None);
}

#[test]
fn preview_none_when_no_files() {
  let state = ReviewState::new(
    vec![],
    vec![],
    PathBuf::from("/out"),
    None,
    ReviewMode::Organize,
  );

  assert!(!state.has_image_preview());
  assert!(state.preview_path.is_none());
}

#[test]
fn preview_none_for_video_extension() {
  let state = ReviewState::new(
    vec![FileGroup {
      id: 0,
      label: "Videos".to_string(),
      rationale: "".to_string(),
      members: vec![0],
      member_destinations: vec![],
      suggested_path: PathBuf::from("videos"),
      member_notes: vec![],
    }],
    vec![FileMove {
      from: PathBuf::from("/dl/clip.mp4"),
      to: PathBuf::from("/out/videos/clip.mp4"),
      group_id: 0,
    }],
    PathBuf::from("/out"),
    None,
    ReviewMode::Organize,
  );

  assert!(!state.has_image_preview());
  assert_eq!(state.preview_path, Some(PathBuf::from("/dl/clip.mp4")));
}

#[test]
fn preview_path_caches_avoids_reload() {
  let mut state = make_state();
  state.focus = Pane::Files;

  let path_before = state.preview_path.clone();
  state.handle_key(KeyCode::Char('j'));
  let path_after_move = state.preview_path.clone();
  state.handle_key(KeyCode::Char('k'));
  let path_after_return = state.preview_path.clone();

  assert_ne!(path_before, path_after_move);
  assert_eq!(path_before, path_after_return);
}

#[test]
fn preview_path_updates_on_group_navigation() {
  let mut state = make_state();

  let first_path = state.preview_path.clone();
  state.handle_key(KeyCode::Char('j'));
  let second_path = state.preview_path.clone();

  assert_eq!(first_path, Some(PathBuf::from("/dl/beach1.jpg")));
  assert_eq!(second_path, Some(PathBuf::from("/dl/cat1.jpg")));
}

#[test]
fn preview_without_picker_never_loads_image() {
  let state = make_state();

  assert!(state.picker.is_none());
  assert!(!state.has_image_preview());
  assert!(state.preview_path.is_some());
}

// --- Per-file keep/delete tests ---

#[test]
fn file_keep_defaults_first_kept_in_dupes() {
  let state = make_dupes_state();
  assert!(state.is_file_kept(0, 0));
  assert!(!state.is_file_kept(0, 1));

  assert!(state.is_file_kept(1, 0));
  assert!(!state.is_file_kept(1, 1));
  assert!(!state.is_file_kept(1, 2));
}

#[test]
fn file_keep_defaults_all_kept_in_organize() {
  let state = make_state();
  assert!(state.is_file_kept(0, 0));
  assert!(state.is_file_kept(0, 1));
  assert!(state.is_file_kept(1, 0));
  assert!(state.is_file_kept(1, 1));
  assert!(state.is_file_kept(1, 2));
}

#[test]
fn toggle_file_keep_in_dupes_mode() {
  let mut state = make_dupes_state();
  state.handle_key(KeyCode::Tab);
  assert!(state.is_file_kept(0, 0));

  state.handle_key(KeyCode::Char('j'));
  assert!(!state.is_file_kept(0, 1));
  state.handle_key(KeyCode::Char(' '));
  assert!(state.is_file_kept(0, 1));

  state.handle_key(KeyCode::Char(' '));
  assert!(!state.is_file_kept(0, 1));
}

#[test]
fn marks_clear_on_group_navigation() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('v'));
  assert!(state.is_file_marked(0, 0));
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('j'));
  assert!(!state.is_file_marked(0, 0));
}

#[test]
fn remove_marked_files_removes_multiple() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('v'));
  state.handle_key(KeyCode::Char('j'));
  state.handle_key(KeyCode::Char('v'));
  assert_eq!(state.current_group_moves().len(), 2);
  state.handle_key(KeyCode::Char('d'));
  assert_eq!(state.current_group_moves().len(), 0);
}

#[test]
fn move_marked_files_to_group() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('v'));
  state.handle_key(KeyCode::Char('j'));
  state.handle_key(KeyCode::Char('v'));
  assert_eq!(state.current_group_moves().len(), 2);
  state.handle_key(KeyCode::Char('m'));
  state.handle_key(KeyCode::Enter);
  assert_eq!(state.current_group_moves().len(), 0);
}

#[test]
fn cannot_unkeep_last_kept_file() {
  let mut state = make_dupes_state();
  state.handle_key(KeyCode::Tab);

  assert!(state.is_file_kept(0, 0));
  assert!(!state.is_file_kept(0, 1));
  state.handle_key(KeyCode::Char(' '));
  assert!(state.is_file_kept(0, 0));
}

#[test]
fn files_to_delete_returns_non_kept() {
  let mut state = make_dupes_state();

  let dels = state.files_to_delete();
  assert_eq!(dels.len(), 3);
  assert!(dels.contains(&PathBuf::from("/dl/beach2.jpg")));
  assert!(dels.contains(&PathBuf::from("/dl/cat2.jpg")));
  assert!(dels.contains(&PathBuf::from("/dl/cat3.jpg")));

  state.handle_key(KeyCode::Char(' '));
  let dels = state.files_to_delete();
  assert_eq!(dels.len(), 2);
  assert!(!dels.iter().any(|p| p.starts_with("/dl/beach")));
}

// --- Async image loading tests ---

#[test]
fn preview_state_defaults_none() {
  let state = make_state();
  assert!(matches!(state.preview, PreviewState::None));
  assert!(!state.has_image_preview());
  assert!(!state.is_image_loading());
}

#[test]
fn poll_image_decode_clears_on_disconnect() {
  let mut state = make_state();
  let (tx, rx) = mpsc::channel();
  state.preview_path = Some(PathBuf::from("/dl/beach1.jpg"));
  state.preview = PreviewState::Loading;
  state.image_rx = Some(rx);

  drop(tx);
  state.poll_image_decode();

  assert!(matches!(state.preview, PreviewState::None));
  assert!(state.image_rx.is_none());
}

#[test]
fn poll_image_decode_noop_without_receiver() {
  let mut state = make_state();
  state.preview = PreviewState::Loading;

  state.poll_image_decode();
  assert!(matches!(state.preview, PreviewState::Loading));
}

#[test]
fn poll_image_decode_empty_channel_stays_loading() {
  let mut state = make_state();
  let (_tx, rx) = mpsc::channel();
  state.preview = PreviewState::Loading;
  state.image_rx = Some(rx);

  state.poll_image_decode();
  assert!(matches!(state.preview, PreviewState::Loading));
  assert!(state.image_rx.is_some());
}

// --- ReviewMode tests ---

#[test]
fn mode_defaults_to_constructor_arg() {
  let org = make_state();
  assert_eq!(org.review_mode(), ReviewMode::Organize);

  let dupes = make_dupes_state();
  assert_eq!(dupes.review_mode(), ReviewMode::Dupes);
}

#[test]
fn d_opens_diff_view_in_dupes_mode() {
  let mut state = make_dupes_state();
  state.handle_key(KeyCode::Char('D'));
  assert_eq!(state.mode, Mode::DiffView { compare_idx: 0 });

  state.handle_key(KeyCode::Esc);
  assert_eq!(state.mode, Mode::Normal);
}

#[test]
fn diff_view_jk_cycles_files() {
  let mut state = make_dupes_state();
  state.handle_key(KeyCode::Char('D'));
  assert_eq!(state.mode, Mode::DiffView { compare_idx: 0 });

  state.handle_key(KeyCode::Char('j'));
  let max = state.group_moves[0].len().saturating_sub(2);
  let expected = 1.min(max);
  assert_eq!(
    state.mode,
    Mode::DiffView {
      compare_idx: expected
    }
  );

  state.handle_key(KeyCode::Char('k'));
  assert_eq!(state.mode, Mode::DiffView { compare_idx: 0 });
}

#[test]
fn diff_view_k_does_not_underflow() {
  let mut state = make_dupes_state();
  state.handle_key(KeyCode::Char('D'));
  state.handle_key(KeyCode::Char('k'));
  assert_eq!(state.mode, Mode::DiffView { compare_idx: 0 });
}

#[test]
fn diff_view_d_toggles_back() {
  let mut state = make_dupes_state();
  state.handle_key(KeyCode::Char('D'));
  assert_eq!(state.mode, Mode::DiffView { compare_idx: 0 });
  state.handle_key(KeyCode::Char('D'));
  assert_eq!(state.mode, Mode::Normal);
}

#[test]
fn set_dupe_types_stores_types() {
  let mut state = make_dupes_state();
  state.set_dupe_types(vec![
    DuplicateType::Exact,
    DuplicateType::NearDuplicate { distance: 5 },
  ]);
  assert_eq!(state.dupe_types.len(), 2);
  assert_eq!(state.dupe_types[0], DuplicateType::Exact);
  assert_eq!(
    state.dupe_types[1],
    DuplicateType::NearDuplicate { distance: 5 }
  );
}

#[test]
fn d_noop_in_organize_files_pane() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  let moves_before = state.group_moves[0].len();
  state.handle_key(KeyCode::Char('d'));
  let moves_after = state.group_moves[0].len();
  assert_eq!(moves_before - 1, moves_after);
}

fn fp(path: &str) -> FingerprintedFile {
  FingerprintedFile {
    scanned: crate::model::ScannedFile {
      path: PathBuf::from(path),
      scan_root: PathBuf::from("/dl"),
      size: 10,
      modified: std::time::SystemTime::UNIX_EPOCH,
      file_type: FileType::Image(crate::model::ImageFormat::Jpg),
    },
    blake3_hash: [0u8; 32],
    perceptual_hash: None,
  }
}

fn files() -> Vec<FingerprintedFile> {
  make_moves()
    .iter()
    .map(|m| fp(m.from.to_str().unwrap()))
    .collect()
}

fn state_with_dupes(sets: Vec<DuplicateSet>) -> ReviewState {
  make_state().with_duplicates(&sets, &files())
}

#[test]
fn space_toggles_keep_in_organize_files_pane() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  assert!(state.is_file_kept(0, 0));
  state.handle_key(KeyCode::Char(' '));
  assert!(!state.is_file_kept(0, 0));
  assert_eq!(
    state.files_to_delete(),
    vec![PathBuf::from("/dl/beach1.jpg")]
  );
  // Deleted files never move.
  assert!(state
    .approved_moves()
    .iter()
    .all(|m| m.from != Path::new("/dl/beach1.jpg")));
  state.handle_key(KeyCode::Char(' '));
  assert!(state.is_file_kept(0, 0));
}

#[test]
fn enter_opens_files_from_groups_and_preview_from_files() {
  let mut state = make_state();
  state.handle_key(KeyCode::Enter);
  assert_eq!(state.focus, Pane::Files);
  state.handle_key(KeyCode::Enter);
  assert_eq!(state.mode, Mode::Preview);
}

#[test]
fn v_marks_files_in_organize_only() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('v'));
  assert!(state.is_file_marked(0, 0));
  state.handle_key(KeyCode::Char('v'));
  assert!(!state.is_file_marked(0, 0));

  let mut dupes = make_dupes_state();
  dupes.handle_key(KeyCode::Tab);
  dupes.handle_key(KeyCode::Char('v'));
  assert!(!dupes.is_file_marked(0, 0));
}

#[test]
fn exact_duplicates_start_marked_for_deletion_and_badge_both_files() {
  let state = state_with_dupes(vec![DuplicateSet {
    canonical: 0,
    duplicates: vec![1],
    duplicate_type: DuplicateType::Exact,
  }]);
  assert!(state.is_file_kept(0, 0));
  assert!(!state.is_file_kept(0, 1));
  assert_eq!(
    state.files_to_delete(),
    vec![PathBuf::from("/dl/beach2.jpg")]
  );
  assert_eq!(state.approved_moves().len(), 4);

  let canon = state.dupe_info(Path::new("/dl/beach1.jpg")).unwrap();
  assert!(canon.is_canonical);
  assert_eq!(canon.partner, PathBuf::from("/dl/beach2.jpg"));
  let dup = state.dupe_info(Path::new("/dl/beach2.jpg")).unwrap();
  assert!(!dup.is_canonical);
  assert_eq!(dup.partner, PathBuf::from("/dl/beach1.jpg"));
  assert_eq!(dup.kind, DuplicateType::Exact);
}

#[test]
fn near_duplicates_stay_kept_until_the_user_decides() {
  let state = state_with_dupes(vec![DuplicateSet {
    canonical: 2,
    duplicates: vec![3],
    duplicate_type: DuplicateType::NearDuplicate { distance: 3 },
  }]);
  assert!(state.is_file_kept(1, 1));
  assert!(state.files_to_delete().is_empty());
  assert!(state.dupe_info(Path::new("/dl/cat2.jpg")).is_some());
}

#[test]
fn shift_d_diffs_a_file_with_its_partner_in_another_group() {
  let mut state = state_with_dupes(vec![DuplicateSet {
    canonical: 0,
    duplicates: vec![2],
    duplicate_type: DuplicateType::NearDuplicate { distance: 2 },
  }]);
  // Cursor on cat1.jpg (group 1, file 0), whose partner is beach1.jpg
  // in group 0.
  state.handle_key(KeyCode::Char('j'));
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('D'));
  assert!(matches!(state.mode, Mode::DiffView { .. }));
  let ds = state.diff_state.as_ref().unwrap();
  assert_eq!(ds.primary_path, Some(PathBuf::from("/dl/beach1.jpg")));
  assert_eq!(ds.secondary_path, Some(PathBuf::from("/dl/cat1.jpg")));
}

#[test]
fn shift_d_without_a_partner_does_nothing() {
  let mut state = make_state();
  state.handle_key(KeyCode::Tab);
  state.handle_key(KeyCode::Char('D'));
  assert_eq!(state.mode, Mode::Normal);
}

#[test]
fn key_table_matches_pane_and_mode() {
  let keys = |m, p| {
    key_table(m, p)
      .into_iter()
      .map(|(k, _)| k)
      .collect::<Vec<_>>()
  };
  let groups = keys(ReviewMode::Organize, Some(Pane::Groups));
  assert!(groups.contains(&"r") && groups.contains(&"M"));
  assert!(!groups.contains(&"v") && !groups.contains(&"m"));
  let files = keys(ReviewMode::Organize, Some(Pane::Files));
  assert!(
    files.contains(&"v")
      && files.contains(&"D")
      && files.contains(&"d")
  );
  assert!(!files.contains(&"r"));
  let all = keys(ReviewMode::Organize, None);
  assert!(all.contains(&"r") && all.contains(&"v"));
  let dupes = keys(ReviewMode::Dupes, Some(Pane::Files));
  assert!(dupes.contains(&"D") && !dupes.contains(&"m"));
  for k in [&groups, &files, &all, &dupes] {
    assert!(k.contains(&"x") && k.contains(&"?") && k.contains(&"q"));
    assert!(k.contains(&"\u{2423}") && k.contains(&"\u{23ce}"));
  }
}

#[test]
fn banner_is_stored_for_the_header() {
  let state = make_state();
  assert_eq!(state.banner(), None);
  let state = state.with_banner("2 groups · 5 moves");
  assert_eq!(state.banner(), Some("2 groups · 5 moves"));
}

#[test]
fn descriptions_are_looked_up_by_path_and_rendered() {
  let desc = ContentDescription {
    summary: "Lease agreement for 418 Maple St".to_string(),
    tags: vec!["lease".to_string(), "housing".to_string()],
    suggested_category: "legal".to_string(),
    confidence: 0.55,
    source: DescriptionSource::Ai,
  };
  let state = make_state()
    .with_descriptions(&HashMap::from([(1usize, desc)]), &files());
  assert!(state.description(Path::new("/dl/beach1.jpg")).is_none());
  assert_eq!(
    state
      .description(Path::new("/dl/beach2.jpg"))
      .map(|d| d.confidence),
    Some(0.55)
  );

  let mut state = state;
  state.focus = Pane::Files;
  state.file_selected = 1;
  let text: String = render_detail_file(&state)
    .iter()
    .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
    .collect();
  assert!(
    text.contains("Lease agreement for 418 Maple St"),
    "{text}"
  );
  assert!(text.contains("lease, housing"), "{text}");
  assert!(text.contains("confidence 0.55"), "{text}");
}

#[test]
fn filename_only_descriptions_say_so() {
  let desc = ContentDescription {
    summary: "software installer: x.dmg".to_string(),
    tags: vec![],
    suggested_category: "software".to_string(),
    confidence: 0.5,
    source: DescriptionSource::Filename,
  };
  let mut state = make_state()
    .with_descriptions(&HashMap::from([(0usize, desc)]), &files());
  state.focus = Pane::Files;
  let text: String = render_detail_file(&state)
    .iter()
    .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
    .collect();
  assert!(text.contains("filename only"), "{text}");
}

#[test]
fn compact_label_keeps_the_tail_of_long_labels() {
  assert_eq!(compact_label("Work", 20), "Work");
  assert_eq!(
    compact_label("Work/Acme Corp/Website Redesign", 40),
    "Work/Acme Corp/Website Redesign"
  );
  assert_eq!(
    compact_label("Work/Acme Corp/Website Redesign", 28),
    "\u{2026}/Acme Corp/Website Redesign"
  );
  assert_eq!(
    compact_label("Work/Acme Corp/Website Redesign", 18),
    "\u{2026}/Website Redesign"
  );
  assert_eq!(
    compact_label("Work/Acme Corp/Website Redesign", 10),
    "\u{2026} Redesign"
  );
}

fn tagged(tags: &[&str], category: &str) -> ContentDescription {
  ContentDescription {
    summary: format!("about {}", tags.join(" ")),
    tags: tags.iter().map(|t| t.to_string()).collect(),
    suggested_category: category.to_string(),
    confidence: 0.9,
    source: DescriptionSource::Ai,
  }
}

#[test]
fn alternatives_rank_other_groups_by_shared_tags() {
  let descriptions = HashMap::from([
    (1usize, tagged(&["cat", "beach", "pet"], "pets")),
    (2usize, tagged(&["cat", "kitten"], "pets")),
    (3usize, tagged(&["sofa"], "pets")),
    (0usize, tagged(&["beach", "sunset"], "travel")),
  ]);
  let state = make_state().with_descriptions(&descriptions, &files());

  let alts = state.alternatives(Path::new("/dl/beach2.jpg"));
  assert_eq!(alts.len(), 1);
  assert_eq!(alts[0].label, "Cats");
  assert_eq!(alts[0].group_idx, 1);
  assert_eq!(alts[0].shared_tags, vec!["cat".to_string()]);
  assert_eq!(alts[0].samples, vec!["cat1.jpg", "cat2.jpg"]);

  // A file whose tags overlap nothing else has no alternatives; a file
  // without a description has none either.
  assert!(state.alternatives(Path::new("/dl/cat2.jpg")).is_empty());
  assert!(state.alternatives(Path::new("/dl/nope.jpg")).is_empty());
}

#[test]
fn number_key_moves_the_file_to_that_alternative() {
  let descriptions = HashMap::from([
    (1usize, tagged(&["cat", "beach"], "pets")),
    (2usize, tagged(&["cat"], "pets")),
  ]);
  let mut state =
    make_state().with_descriptions(&descriptions, &files());
  state.focus = Pane::Files;
  state.file_selected = 1;

  state.handle_key(KeyCode::Char('1'));

  assert_eq!(state.group_moves[0].len(), 1);
  assert_eq!(state.group_moves[1].len(), 4);
  let moved = &state.group_moves[1][3];
  assert_eq!(moved.from, PathBuf::from("/dl/beach2.jpg"));
  assert_eq!(moved.to, PathBuf::from("/out/cats/beach2.jpg"));
  assert_eq!(moved.group_id, 1);
  assert_eq!(state.mode, Mode::Normal);

  // No second alternative: the key is a no-op.
  state.file_selected = 0;
  state.handle_key(KeyCode::Char('2'));
  assert_eq!(state.group_moves[0].len(), 1);
}

#[test]
fn detail_pane_lists_metadata_and_alternatives() {
  let dir = tempfile::tempdir().unwrap();
  let png = dir.path().join("beach2.png");
  image::RgbImage::new(4, 3).save(&png).unwrap();
  let mut moves = make_moves();
  moves[1].from = png.clone();
  let descriptions = HashMap::from([
    (1usize, tagged(&["cat", "beach"], "pets")),
    (2usize, tagged(&["cat"], "pets")),
  ]);
  let mut fps = files();
  fps[1].scanned.path = png.clone();
  let mut state = ReviewState::new(
    make_groups(),
    moves,
    PathBuf::from("/out"),
    None,
    ReviewMode::Organize,
  )
  .with_descriptions(&descriptions, &fps);
  state.focus = Pane::Files;
  state.file_selected = 1;
  state.ensure_facts_for_current();

  let text: String = render_detail_file(&state)
    .iter()
    .map(|l| {
      l.spans
        .iter()
        .map(|s| s.content.to_string())
        .collect::<String>()
    })
    .collect::<Vec<_>>()
    .join("\n");
  assert!(text.contains("METADATA"), "{text}");
  assert!(text.contains("4 × 3"), "{text}");
  assert!(text.contains("ALSO FITS"), "{text}");
  assert!(text.contains("Cats"), "{text}");
  assert!(text.contains("shares cat"), "{text}");
}

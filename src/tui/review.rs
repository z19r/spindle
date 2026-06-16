use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use crossterm::event::KeyCode;
use ratatui::{
  layout::{Alignment, Constraint, Layout, Rect},
  style::{Modifier, Style},
  text::{Line, Span},
  widgets::{
    Block, BorderType, Clear, List, ListItem, Padding, Paragraph,
  },
  Frame,
};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use crate::model::{
  DuplicateType, FileGroup, FileMove, FileType, FingerprintedFile,
};

fn decode_image(path: &Path) -> Option<image::DynamicImage> {
  match image::ImageReader::open(path)
    .and_then(|r| r.with_guessed_format())
  {
    Ok(reader) => match reader.decode() {
      Ok(img) => return Some(img),
      Err(e) => tracing::debug!(?path, %e, "image crate failed, trying magick"),
    },
    Err(e) => tracing::debug!(?path, %e, "image crate failed, trying magick"),
  }

  let tmp = std::env::temp_dir().join(format!(
    "spindle-preview-{}.png",
    std::process::id()
  ));
  let ok = std::process::Command::new("magick")
    .arg("convert")
    .arg(path)
    .arg("-resize")
    .arg("1200x1200>")
    .arg(&tmp)
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status()
    .map(|s| s.success())
    .unwrap_or(false);
  if !ok {
    tracing::warn!(?path, "magick convert also failed");
    return None;
  }
  let result = image::open(&tmp).ok();
  let _ = std::fs::remove_file(&tmp);
  result
}

mod theme {
  use ratatui::style::{Color, Modifier, Style};

  pub const PURPLE: Color = Color::Rgb(125, 86, 244);
  pub const BORDER_PURPLE: Color = Color::Rgb(135, 75, 253);
  pub const GREEN: Color = Color::Rgb(4, 181, 117);
  pub const WHITE: Color = Color::Rgb(250, 250, 250);
  pub const SUBTLE: Color = Color::Rgb(136, 136, 136);
  pub const RED_SOFT: Color = Color::Rgb(237, 135, 150);
  pub const CREAM: Color = Color::Rgb(202, 211, 245);
  pub const DIM_PURPLE: Color = Color::Rgb(110, 90, 160);
  pub const BG_SELECTED: Color = Color::Rgb(49, 42, 76);
  pub const BG_HEADER: Color = Color::Rgb(125, 86, 244);
  pub const YELLOW: Color = Color::Rgb(249, 226, 175);
  pub const BRIGHT_GREEN: Color = Color::Rgb(80, 250, 123);
  pub const BRIGHT_YELLOW: Color = Color::Rgb(255, 214, 102);

  pub fn title_badge() -> Style {
    Style::default()
      .fg(WHITE)
      .bg(BG_HEADER)
      .add_modifier(Modifier::BOLD)
  }

  pub fn selected() -> Style {
    Style::default()
      .fg(WHITE)
      .bg(BG_SELECTED)
      .add_modifier(Modifier::BOLD)
  }

  pub fn normal() -> Style {
    Style::default().fg(CREAM)
  }

  pub fn dim() -> Style {
    Style::default().fg(SUBTLE)
  }

  pub fn label() -> Style {
    Style::default().fg(PURPLE).add_modifier(Modifier::BOLD)
  }

  pub fn value() -> Style {
    Style::default().fg(WHITE)
  }

  pub fn path() -> Style {
    Style::default().fg(CREAM)
  }

  pub fn approved() -> Style {
    Style::default().fg(GREEN)
  }

  pub fn rejected() -> Style {
    Style::default().fg(RED_SOFT)
  }

  pub fn key_hint() -> Style {
    Style::default().fg(PURPLE).add_modifier(Modifier::BOLD)
  }

  pub fn input_cursor() -> Style {
    Style::default().fg(WHITE).bg(PURPLE)
  }

  pub fn warning() -> Style {
    Style::default().fg(YELLOW).add_modifier(Modifier::BOLD)
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewAction {
  Execute,
  Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
  Groups,
  Files,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
  Normal,
  MoveToGroup { cursor: usize },
  NewGroup { input: String, cursor_pos: usize },
  ConfirmRemove,
  DiffView { compare_idx: usize },
  Preview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewMode {
  Organize,
  Dupes,
}

pub enum PreviewState {
  None,
  Loading,
  Ready(StatefulProtocol),
}

enum DiffLine {
  Same(String),
  Added(String),
  Removed(String),
}

enum DiffContent {
  Images,
  Text(Vec<DiffLine>),
  Binary,
}

struct DiffState {
  primary_preview: PreviewState,
  secondary_preview: PreviewState,
  primary_rx: Option<Receiver<(PathBuf, StatefulProtocol)>>,
  secondary_rx: Option<Receiver<(PathBuf, StatefulProtocol)>>,
  primary_path: Option<PathBuf>,
  secondary_path: Option<PathBuf>,
  content: DiffContent,
  scroll: usize,
}

struct ModeData {
  groups: Vec<FileGroup>,
  group_moves: Vec<Vec<FileMove>>,
  approved: Vec<bool>,
  file_keep: Vec<Vec<bool>>,
  file_marked: Vec<HashSet<usize>>,
  dupe_types: Vec<DuplicateType>,
}

pub struct ReviewState {
  groups: Vec<FileGroup>,
  approved: Vec<bool>,
  selected: usize,
  file_selected: usize,
  focus: Pane,
  action: Option<ReviewAction>,
  group_moves: Vec<Vec<FileMove>>,
  mode: Mode,
  output_dir: PathBuf,
  next_group_id: usize,
  picker: Option<Picker>,
  preview: PreviewState,
  preview_path: Option<PathBuf>,
  image_rx: Option<Receiver<(PathBuf, StatefulProtocol)>>,
  file_keep: Vec<Vec<bool>>,
  file_marked: Vec<HashSet<usize>>,
  review_mode: ReviewMode,
  other_mode_data: Option<ModeData>,
  file_metadata: HashMap<PathBuf, (String, u64)>,
  dupe_types: Vec<DuplicateType>,
  diff_state: Option<DiffState>,
}

impl ReviewState {
  #[allow(clippy::type_complexity)]
  pub fn new(
    groups: Vec<FileGroup>,
    moves: Vec<FileMove>,
    output_dir: PathBuf,
    picker: Option<Picker>,
    mode: ReviewMode,
    other_mode: Option<(
      Vec<FileGroup>,
      Vec<FileMove>,
      ReviewMode,
      Vec<DuplicateType>,
    )>,
  ) -> Self {
    let len = groups.len();
    let next_group_id = groups
      .iter()
      .map(|g| g.id)
      .max()
      .map(|m| m + 1)
      .unwrap_or(0);
    let group_moves: Vec<Vec<FileMove>> = groups
      .iter()
      .map(|g| {
        moves
          .iter()
          .filter(|m| m.group_id == g.id)
          .cloned()
          .collect()
      })
      .collect();

    let file_keep: Vec<Vec<bool>> =
      Self::init_file_keep(&group_moves, mode);

    let other_mode_data =
      other_mode.map(|(og, om, omode, odupe_types)| {
        let other_group_moves: Vec<Vec<FileMove>> = og
          .iter()
          .map(|g| {
            om.iter()
              .filter(|m| m.group_id == g.id)
              .cloned()
              .collect()
          })
          .collect();
        let other_approved = vec![true; og.len()];
        let other_file_keep =
          Self::init_file_keep(&other_group_moves, omode);
        let other_file_marked =
          vec![HashSet::new(); og.len()];
        ModeData {
          groups: og,
          group_moves: other_group_moves,
          approved: other_approved,
          file_keep: other_file_keep,
          file_marked: other_file_marked,
          dupe_types: odupe_types,
        }
      });

    let mut state = Self {
      groups,
      approved: vec![true; len],
      selected: 0,
      file_selected: 0,
      focus: Pane::Groups,
      action: None,
      group_moves,
      mode: Mode::Normal,
      output_dir,
      next_group_id,
      picker,
      preview: PreviewState::None,
      preview_path: None,
      image_rx: None,
      file_keep,
      file_marked: vec![HashSet::new(); len],
      review_mode: mode,
      other_mode_data,
      file_metadata: HashMap::new(),
      dupe_types: Vec::new(),
      diff_state: None,
    };
    state.update_image_preview();
    state
  }

  pub fn set_dupe_types(&mut self, types: Vec<DuplicateType>) {
    self.dupe_types = types;
  }

  pub fn with_file_metadata(
    mut self,
    files: &[FingerprintedFile],
  ) -> Self {
    self.file_metadata = files
      .iter()
      .map(|f| {
        let hex = hex::encode(f.blake3_hash);
        (f.scanned.path.clone(), (hex, f.scanned.size))
      })
      .collect();
    self
  }

  fn init_file_keep(
    group_moves: &[Vec<FileMove>],
    mode: ReviewMode,
  ) -> Vec<Vec<bool>> {
    group_moves
      .iter()
      .map(|moves| match mode {
        ReviewMode::Organize => vec![true; moves.len()],
        ReviewMode::Dupes => {
          moves.iter().enumerate().map(|(i, _)| i == 0).collect()
        }
      })
      .collect()
  }

  pub fn can_swap_mode(&self) -> bool {
    self.other_mode_data.is_some()
  }

  pub fn selected_index(&self) -> usize {
    self.selected
  }

  pub fn file_selected(&self) -> usize {
    self.file_selected
  }

  pub fn focus(&self) -> Pane {
    self.focus
  }

  pub fn mode(&self) -> &Mode {
    &self.mode
  }

  pub fn review_mode(&self) -> ReviewMode {
    self.review_mode
  }

  pub fn is_approved(&self, index: usize) -> bool {
    self.approved.get(index).copied().unwrap_or(false)
  }

  pub fn pending_action(&self) -> Option<ReviewAction> {
    self.action
  }

  pub fn approved_groups(&self) -> Vec<&FileGroup> {
    self
      .groups
      .iter()
      .enumerate()
      .filter(|(i, _)| self.approved[*i])
      .map(|(_, g)| g)
      .collect()
  }

  pub fn approved_moves(&self) -> Vec<FileMove> {
    self
      .groups
      .iter()
      .enumerate()
      .filter(|(i, _)| self.approved[*i])
      .flat_map(|(i, _)| self.group_moves[i].iter().cloned())
      .collect()
  }

  pub fn current_group_moves(&self) -> &[FileMove] {
    if self.selected < self.group_moves.len() {
      &self.group_moves[self.selected]
    } else {
      &[]
    }
  }

  pub fn current_file_move(&self) -> Option<&FileMove> {
    self.current_group_moves().get(self.file_selected)
  }

  pub fn image_state_mut(&mut self) -> Option<&mut StatefulProtocol> {
    match &mut self.preview {
      PreviewState::Ready(protocol) => Some(protocol),
      _ => None,
    }
  }

  pub fn has_image_preview(&self) -> bool {
    matches!(self.preview, PreviewState::Ready(_))
  }

  pub fn is_image_loading(&self) -> bool {
    matches!(self.preview, PreviewState::Loading)
  }

  pub fn is_file_kept(
    &self,
    group_idx: usize,
    file_idx: usize,
  ) -> bool {
    self
      .file_keep
      .get(group_idx)
      .and_then(|g| g.get(file_idx))
      .copied()
      .unwrap_or(true)
  }

  pub fn files_to_delete(&self) -> Vec<PathBuf> {
    let mut deletions = Vec::new();
    for (gi, _group) in self.groups.iter().enumerate() {
      if !self.approved[gi] {
        continue;
      }
      for (fi, mv) in self.group_moves[gi].iter().enumerate() {
        if !self.is_file_kept(gi, fi) {
          deletions.push(mv.from.clone());
        }
      }
    }
    deletions
  }

  pub fn is_file_marked(&self, group_idx: usize, file_idx: usize) -> bool {
    self
      .file_marked
      .get(group_idx)
      .map(|s| s.contains(&file_idx))
      .unwrap_or(false)
  }

  fn toggle_file_mark(&mut self) {
    let gi = self.selected;
    let fi = self.file_selected;
    if let Some(set) = self.file_marked.get_mut(gi) {
      if !set.remove(&fi) {
        set.insert(fi);
      }
    }
  }

  fn marked_file_indices(&self) -> Vec<usize> {
    self
      .file_marked
      .get(self.selected)
      .filter(|s| !s.is_empty())
      .map(|s| {
        let mut v: Vec<usize> = s.iter().copied().collect();
        v.sort_unstable();
        v
      })
      .unwrap_or_else(|| vec![self.file_selected])
  }

  fn clear_marks(&mut self) {
    if let Some(set) = self.file_marked.get_mut(self.selected) {
      set.clear();
    }
  }

  fn toggle_file_keep(&mut self) {
    let gi = self.selected;
    let fi = self.file_selected;
    let Some(keeps) = self.file_keep.get_mut(gi) else {
      return;
    };
    let Some(&current) = keeps.get(fi) else {
      return;
    };
    if current {
      let kept_count = keeps.iter().filter(|&&k| k).count();
      if kept_count > 1 {
        keeps[fi] = false;
      }
    } else {
      keeps[fi] = true;
    }
  }

  fn enter_diff_view(&mut self, compare_idx: usize) {
    let moves = self.current_group_moves();
    if moves.len() < 2 {
      return;
    }
    let primary_path = moves[0].from.clone();
    let secondary_idx = compare_idx + 1;
    let secondary_path = match moves.get(secondary_idx) {
      Some(m) => m.from.clone(),
      None => return,
    };

    let primary_ext = primary_path
      .extension()
      .and_then(|e| e.to_str())
      .map(FileType::from_extension);

    let is_image = primary_ext
      .as_ref()
      .map(|ft| ft.is_image())
      .unwrap_or(false);

    let is_text =
      primary_ext.as_ref().map(|ft| ft.is_text()).unwrap_or(false);

    if is_image {
      let mut ds = DiffState {
        primary_preview: PreviewState::Loading,
        secondary_preview: PreviewState::Loading,
        primary_rx: None,
        secondary_rx: None,
        primary_path: Some(primary_path.clone()),
        secondary_path: Some(secondary_path.clone()),
        content: DiffContent::Images,
        scroll: 0,
      };

      if let Some(picker) = &self.picker {
        let pk1 = picker.clone();
        let (tx1, rx1) = mpsc::channel();
        let p1 = primary_path.clone();
        thread::spawn(move || {
          if let Some(img) = decode_image(&p1) {
            let protocol = pk1.new_resize_protocol(img);
            let _ = tx1.send((p1, protocol));
          }
        });
        ds.primary_rx = Some(rx1);

        let pk2 = picker.clone();
        let (tx2, rx2) = mpsc::channel();
        let p2 = secondary_path.clone();
        thread::spawn(move || {
          if let Some(img) = decode_image(&p2) {
            let protocol = pk2.new_resize_protocol(img);
            let _ = tx2.send((p2, protocol));
          }
        });
        ds.secondary_rx = Some(rx2);
      }

      self.diff_state = Some(ds);
    } else if is_text {
      let primary_text =
        std::fs::read_to_string(&primary_path).unwrap_or_default();
      let secondary_text =
        std::fs::read_to_string(&secondary_path).unwrap_or_default();

      let diff =
        similar::TextDiff::from_lines(&primary_text, &secondary_text);
      let lines: Vec<DiffLine> = diff
        .iter_all_changes()
        .map(|change| {
          let text =
            change.value().trim_end_matches('\n').to_string();
          match change.tag() {
            similar::ChangeTag::Equal => DiffLine::Same(text),
            similar::ChangeTag::Insert => DiffLine::Added(text),
            similar::ChangeTag::Delete => DiffLine::Removed(text),
          }
        })
        .collect();

      self.diff_state = Some(DiffState {
        primary_preview: PreviewState::None,
        secondary_preview: PreviewState::None,
        primary_rx: None,
        secondary_rx: None,
        primary_path: Some(primary_path),
        secondary_path: Some(secondary_path),
        content: DiffContent::Text(lines),
        scroll: 0,
      });
    } else {
      self.diff_state = Some(DiffState {
        primary_preview: PreviewState::None,
        secondary_preview: PreviewState::None,
        primary_rx: None,
        secondary_rx: None,
        primary_path: Some(primary_path),
        secondary_path: Some(secondary_path),
        content: DiffContent::Binary,
        scroll: 0,
      });
    }
  }

  fn exit_diff_view(&mut self) {
    self.diff_state = None;
  }

  fn enter_preview(&mut self) {
    let mv = match self.current_file_move() {
      Some(m) => m,
      None => return,
    };
    let path = mv.from.clone();

    let ft = path
      .extension()
      .and_then(|e| e.to_str())
      .map(FileType::from_extension);

    let is_image = ft.as_ref().map(|f| f.is_image()).unwrap_or(false);
    let is_text = ft.as_ref().map(|f| f.is_text()).unwrap_or(false);

    if is_image {
      let mut ds = DiffState {
        primary_preview: PreviewState::Loading,
        secondary_preview: PreviewState::None,
        primary_rx: None,
        secondary_rx: None,
        primary_path: Some(path.clone()),
        secondary_path: None,
        content: DiffContent::Images,
        scroll: 0,
      };

      if let Some(picker) = &self.picker {
        let pk = picker.clone();
        let (tx, rx) = mpsc::channel();
        let p = path;
        thread::spawn(move || {
          if let Some(img) = decode_image(&p) {
            let protocol = pk.new_resize_protocol(img);
            let _ = tx.send((p, protocol));
          }
        });
        ds.primary_rx = Some(rx);
      } else {
        ds.primary_preview = PreviewState::None;
      }

      self.diff_state = Some(ds);
    } else if is_text {
      let text = std::fs::read_to_string(&path).unwrap_or_default();
      let lines: Vec<DiffLine> =
        text.lines().map(|l| DiffLine::Same(l.to_string())).collect();
      self.diff_state = Some(DiffState {
        primary_preview: PreviewState::None,
        secondary_preview: PreviewState::None,
        primary_rx: None,
        secondary_rx: None,
        primary_path: Some(path),
        secondary_path: None,
        content: DiffContent::Text(lines),
        scroll: 0,
      });
    } else {
      self.diff_state = Some(DiffState {
        primary_preview: PreviewState::None,
        secondary_preview: PreviewState::None,
        primary_rx: None,
        secondary_rx: None,
        primary_path: Some(path),
        secondary_path: None,
        content: DiffContent::Binary,
        scroll: 0,
      });
    }

    self.mode = Mode::Preview;
  }

  fn update_image_preview(&mut self) {
    let path = self.current_file_move().map(|mv| mv.from.clone());
    if path == self.preview_path {
      return;
    }
    self.preview_path = path.clone();
    self.preview = PreviewState::None;
    self.image_rx = None;

    let Some(picker) = &self.picker else { return };
    let Some(path) = path else { return };

    let is_image = path
      .extension()
      .and_then(|e| e.to_str())
      .map(FileType::from_extension)
      .map(|ft| ft.is_image())
      .unwrap_or(false);

    if !is_image {
      return;
    }

    self.preview = PreviewState::Loading;

    let picker_clone = picker.clone();
    let (tx, rx) = mpsc::channel();
    self.image_rx = Some(rx);

    thread::spawn(move || {
      if let Some(img) = decode_image(&path) {
        let protocol = picker_clone.new_resize_protocol(img);
        let _ = tx.send((path, protocol));
      }
    });
  }

  pub fn poll_image_decode(&mut self) {
    if let Some(rx) = &self.image_rx {
      match rx.try_recv() {
        Ok((path, protocol)) => {
          if Some(&path) == self.preview_path.as_ref() {
            self.preview = PreviewState::Ready(protocol);
          }
          self.image_rx = None;
        }
        Err(mpsc::TryRecvError::Disconnected) => {
          if matches!(self.preview, PreviewState::Loading) {
            self.preview = PreviewState::None;
          }
          self.image_rx = None;
        }
        Err(mpsc::TryRecvError::Empty) => {}
      }
    }

    if let Some(ref mut ds) = self.diff_state {
      if let Some(rx) = &ds.primary_rx {
        match rx.try_recv() {
          Ok((path, protocol)) => {
            if Some(&path) == ds.primary_path.as_ref() {
              ds.primary_preview = PreviewState::Ready(protocol);
            }
            ds.primary_rx = None;
          }
          Err(mpsc::TryRecvError::Disconnected) => {
            if matches!(ds.primary_preview, PreviewState::Loading) {
              ds.primary_preview = PreviewState::None;
            }
            ds.primary_rx = None;
          }
          Err(mpsc::TryRecvError::Empty) => {}
        }
      }
      if let Some(rx) = &ds.secondary_rx {
        match rx.try_recv() {
          Ok((path, protocol)) => {
            if Some(&path) == ds.secondary_path.as_ref() {
              ds.secondary_preview = PreviewState::Ready(protocol);
            }
            ds.secondary_rx = None;
          }
          Err(mpsc::TryRecvError::Disconnected) => {
            if matches!(ds.secondary_preview, PreviewState::Loading) {
              ds.secondary_preview = PreviewState::None;
            }
            ds.secondary_rx = None;
          }
          Err(mpsc::TryRecvError::Empty) => {}
        }
      }
    }
  }

  pub fn move_target_groups(&self) -> Vec<(usize, &FileGroup)> {
    self
      .groups
      .iter()
      .enumerate()
      .filter(|(i, _)| *i != self.selected)
      .collect()
  }

  pub fn handle_key(&mut self, code: KeyCode) {
    match &self.mode {
      Mode::Normal => self.handle_normal_key(code),
      Mode::MoveToGroup { .. } => self.handle_move_to_group_key(code),
      Mode::NewGroup { .. } => self.handle_new_group_key(code),
      Mode::ConfirmRemove => self.handle_confirm_remove_key(code),
      Mode::DiffView { .. } => self.handle_diff_view_key(code),
      Mode::Preview => self.handle_preview_key(code),
    }
    self.update_image_preview();
  }

  fn handle_normal_key(&mut self, code: KeyCode) {
    match code {
      KeyCode::Tab => {
        self.focus = match self.focus {
          Pane::Groups => Pane::Files,
          Pane::Files => Pane::Groups,
        };
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

      KeyCode::Char(' ') => match self.focus {
        Pane::Groups => {
          if let Some(val) = self.approved.get_mut(self.selected) {
            *val = !*val;
          }
        }
        Pane::Files if self.review_mode == ReviewMode::Dupes => {
          self.toggle_file_keep();
        }
        Pane::Files => {
          self.enter_preview();
        }
      },

      KeyCode::Enter
        if self.focus == Pane::Files
          && self.review_mode == ReviewMode::Organize =>
      {
        self.toggle_file_mark();
      }

      KeyCode::Char('x') => {
        self.action = Some(ReviewAction::Execute);
      }

      KeyCode::Char('d')
        if self.focus == Pane::Files
          && self.review_mode == ReviewMode::Organize =>
      {
        self.remove_marked_files();
      }

      KeyCode::Char('d')
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

      KeyCode::Char('m')
        if self.focus == Pane::Files
          && self.review_mode == ReviewMode::Organize =>
      {
        self.enter_move_to_group();
      }

      KeyCode::Char('n')
        if self.focus == Pane::Files
          && self.review_mode == ReviewMode::Organize =>
      {
        self.enter_new_group();
      }

      KeyCode::Char('s') if self.other_mode_data.is_some() => {
        let cached = self.other_mode_data.take().unwrap();
        let current = ModeData {
          groups: std::mem::replace(&mut self.groups, cached.groups),
          group_moves: std::mem::replace(
            &mut self.group_moves,
            cached.group_moves,
          ),
          approved: std::mem::replace(
            &mut self.approved,
            cached.approved,
          ),
          file_keep: std::mem::replace(
            &mut self.file_keep,
            cached.file_keep,
          ),
          file_marked: std::mem::replace(
            &mut self.file_marked,
            cached.file_marked,
          ),
          dupe_types: std::mem::replace(
            &mut self.dupe_types,
            cached.dupe_types,
          ),
        };
        self.other_mode_data = Some(current);
        self.review_mode = match self.review_mode {
          ReviewMode::Organize => ReviewMode::Dupes,
          ReviewMode::Dupes => ReviewMode::Organize,
        };
        self.selected = 0;
        self.file_selected = 0;
        self.focus = Pane::Groups;
        self.mode = Mode::Normal;
        self.next_group_id = self
          .groups
          .iter()
          .map(|g| g.id)
          .max()
          .map(|m| m + 1)
          .unwrap_or(0);
        self.update_image_preview();
      }

      _ => {}
    }
  }

  fn handle_move_to_group_key(&mut self, code: KeyCode) {
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

  fn handle_new_group_key(&mut self, code: KeyCode) {
    let (input, cursor_pos) = match &self.mode {
      Mode::NewGroup { input, cursor_pos } => {
        (input.clone(), *cursor_pos)
      }
      _ => return,
    };

    match code {
      KeyCode::Char(c) => {
        let mut new_input = input;
        new_input.insert(cursor_pos, c);
        self.mode = Mode::NewGroup {
          input: new_input,
          cursor_pos: cursor_pos + 1,
        };
      }
      KeyCode::Backspace if cursor_pos > 0 => {
        let mut new_input = input;
        new_input.remove(cursor_pos - 1);
        self.mode = Mode::NewGroup {
          input: new_input,
          cursor_pos: cursor_pos - 1,
        };
      }
      KeyCode::Left if cursor_pos > 0 => {
        self.mode = Mode::NewGroup {
          input,
          cursor_pos: cursor_pos - 1,
        };
      }
      KeyCode::Right => {
        let max = input.len();
        if cursor_pos < max {
          self.mode = Mode::NewGroup {
            input,
            cursor_pos: cursor_pos + 1,
          };
        }
      }
      KeyCode::Enter => {
        self.confirm_new_group(input);
      }
      KeyCode::Esc => {
        self.mode = Mode::Normal;
      }
      _ => {}
    }
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

  fn handle_diff_view_key(&mut self, code: KeyCode) {
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
      KeyCode::Esc | KeyCode::Char('d') | KeyCode::Char('q') => {
        self.mode = Mode::Normal;
        self.exit_diff_view();
      }
      _ => {}
    }
  }

  fn handle_preview_key(&mut self, code: KeyCode) {
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
      KeyCode::Esc
      | KeyCode::Char(' ')
      | KeyCode::Char('q') => {
        self.mode = Mode::Normal;
        self.exit_diff_view();
      }
      _ => {}
    }
  }

  fn remove_marked_files(&mut self) {
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

  fn delete_current_group(&mut self) {
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

  fn enter_move_to_group(&mut self) {
    if self.current_group_moves().is_empty() {
      return;
    }
    if self.groups.len() < 2 {
      return;
    }
    self.mode = Mode::MoveToGroup { cursor: 0 };
  }

  fn confirm_move_to_group(&mut self, cursor: usize) {
    let targets = self.move_target_groups();
    if cursor >= targets.len() {
      self.mode = Mode::Normal;
      return;
    }
    let dest_idx = targets[cursor].0;
    let dest_group_id = self.groups[dest_idx].id;
    let dest_suggested = self.groups[dest_idx].suggested_path.clone();

    let indices = self.marked_file_indices();
    for &i in indices.iter().rev() {
      if i >= self.group_moves[self.selected].len() {
        continue;
      }
      let mut file_move =
        self.group_moves[self.selected].remove(i);
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

  fn enter_new_group(&mut self) {
    if self.current_group_moves().is_empty() {
      return;
    }
    self.mode = Mode::NewGroup {
      input: String::new(),
      cursor_pos: 0,
    };
  }

  fn confirm_new_group(&mut self, input: String) {
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
      let mut file_move =
        self.group_moves[self.selected].remove(i);
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
  fn resolve_move_target_index(
    &self,
    cursor: usize,
  ) -> Option<usize> {
    self.move_target_groups().get(cursor).map(|(i, _)| *i)
  }
}

pub fn render(frame: &mut Frame, state: &mut ReviewState) {
  let outer = Layout::vertical([
    Constraint::Length(3),
    Constraint::Min(6),
    Constraint::Length(3),
  ])
  .split(frame.area());

  render_header(frame, outer[0], state);

  let body = Layout::horizontal([
    Constraint::Percentage(22),
    Constraint::Percentage(38),
    Constraint::Percentage(40),
  ])
  .split(outer[1]);

  render_group_list(frame, body[0], state);
  render_middle_panel(frame, body[1], state);
  render_detail(frame, body[2], state);
  render_footer(frame, outer[2], state);

  match &state.mode {
    Mode::DiffView { compare_idx } => {
      render_diff_modal(frame, state, *compare_idx);
    }
    Mode::Preview => {
      render_preview_modal(frame, state);
    }
    _ => {}
  }
}

fn panel_block<'a>(title: &'a str, focused: bool) -> Block<'a> {
  let border_color = if focused {
    theme::BORDER_PURPLE
  } else {
    theme::DIM_PURPLE
  };

  let title_style = if focused {
    Style::default()
      .fg(theme::WHITE)
      .add_modifier(Modifier::BOLD)
  } else {
    Style::default()
      .fg(theme::SUBTLE)
      .add_modifier(Modifier::BOLD)
  };

  Block::bordered()
    .border_type(BorderType::Rounded)
    .title_top(Line::from(Span::styled(
      format!(" {title} "),
      title_style,
    )))
    .border_style(Style::default().fg(border_color))
    .padding(Padding::new(1, 1, 0, 0))
}

fn render_header(frame: &mut Frame, area: Rect, state: &ReviewState) {
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

  frame.render_widget(block, area);
}

fn render_group_list(
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

fn render_middle_panel(
  frame: &mut Frame,
  area: Rect,
  state: &ReviewState,
) {
  match &state.mode {
    Mode::Normal => render_file_list(frame, area, state),
    Mode::MoveToGroup { cursor } => {
      render_group_picker(frame, area, state, *cursor)
    }
    Mode::NewGroup { input, cursor_pos } => {
      render_new_group_input(frame, area, state, input, *cursor_pos)
    }
    Mode::ConfirmRemove => render_confirm_remove(frame, area, state),
    Mode::DiffView { .. } | Mode::Preview => {
      render_file_list(frame, area, state)
    }
  }
}

fn render_preview_modal(
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
          ds.primary_preview = PreviewState::Ready(protocol);
        }
        ds.primary_rx = None;
      }
    }

    match &mut ds.primary_preview {
      PreviewState::Ready(protocol) => {
        let img = ratatui_image::StatefulImage::new();
        frame.render_stateful_widget(img, inner, protocol);
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

fn render_diff_modal(
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
          frame.render_stateful_widget(iw, left_inner, protocol);
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
          frame.render_stateful_widget(iw, right_inner, protocol);
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

fn diff_modal_block<'a>(
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

fn render_diff_modal_metadata(
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

fn render_file_list(
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

fn render_group_picker(
  frame: &mut Frame,
  area: Rect,
  state: &ReviewState,
  cursor: usize,
) {
  let block = panel_block("Move to\u{2026}", true);
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

fn render_new_group_input(
  frame: &mut Frame,
  area: Rect,
  _state: &ReviewState,
  input: &str,
  cursor_pos: usize,
) {
  let block = panel_block("New Group", true);
  let slug = input.trim().to_lowercase().replace(' ', "_");

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

fn render_confirm_remove(
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

fn render_detail(
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
  let show_image = state.has_image_preview() && in_files;
  let show_loading = state.is_image_loading() && in_files;

  let lines = match &state.mode {
    Mode::Normal if state.focus == Pane::Files => {
      render_detail_file(state)
    }
    Mode::Normal => render_detail_group(state),
    Mode::MoveToGroup { cursor } => {
      render_detail_move_target(state, *cursor)
    }
    Mode::NewGroup { input, .. } => {
      render_detail_new_group(state, input)
    }
    Mode::ConfirmRemove => render_detail_group(state),
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

    let detail = Paragraph::new(lines)
      .wrap(ratatui::widgets::Wrap { trim: true });
    frame.render_widget(detail, split[1]);
  } else {
    let detail = Paragraph::new(lines)
      .block(block)
      .wrap(ratatui::widgets::Wrap { trim: true });
    frame.render_widget(detail, area);
  }
}

fn render_detail_file(state: &ReviewState) -> Vec<Line<'static>> {
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
          .fg(theme::WHITE)
          .add_modifier(Modifier::BOLD),
      )),
      Line::from(""),
    ];

    lines.push(Line::from(Span::styled("  SOURCE", theme::label())));
    lines.push(Line::from(vec![
      Span::styled("  ", Style::default()),
      Span::styled(from_dir, theme::path()),
    ]));
    lines.push(Line::from(vec![
      Span::styled(
        "  \u{2514}\u{2500} ",
        Style::default().fg(theme::DIM_PURPLE),
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
        Style::default().fg(theme::DIM_PURPLE),
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
                .fg(theme::BRIGHT_GREEN)
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
                .fg(theme::BRIGHT_YELLOW)
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
        None => {}
      }
    }

    lines
  } else {
    vec![Line::from(Span::styled("No file selected.", theme::dim()))]
  }
}

fn render_detail_group(state: &ReviewState) -> Vec<Line<'static>> {
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
        format!(" {} ", &group.label),
        Style::default()
          .fg(theme::WHITE)
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

fn render_dupe_reason(
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
            .fg(theme::BRIGHT_GREEN)
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
            .fg(theme::BRIGHT_YELLOW)
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

fn render_detail_move_target(
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
        .fg(theme::WHITE)
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
          .fg(theme::WHITE)
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

fn render_detail_new_group(
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
        .fg(theme::WHITE)
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

fn format_size(bytes: u64) -> String {
  if bytes >= 1_000_000_000 {
    format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
  } else if bytes >= 1_000_000 {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
  } else if bytes >= 1_000 {
    format!("{:.1} KB", bytes as f64 / 1_000.0)
  } else {
    format!("{bytes} B")
  }
}

fn render_footer(frame: &mut Frame, area: Rect, state: &ReviewState) {
  let mode_label = match state.review_mode {
    ReviewMode::Organize => "Organize",
    ReviewMode::Dupes => "Dupes",
  };

  let can_swap = state.can_swap_mode();
  let keys: Vec<(&str, &str)> = match &state.mode {
    Mode::Normal => match (state.focus, state.review_mode) {
      (Pane::Groups, _) => {
        let mut k = vec![("j/k", "navigate"), ("\u{2423}", "toggle")];
        if can_swap {
          k.push(("s", "mode"));
        }
        if state.review_mode == ReviewMode::Dupes {
          k.push(("d", "diff"));
        }
        k.extend([("tab", "pane"), ("x", "execute"), ("q", "quit")]);
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
    Mode::NewGroup { .. } => {
      vec![
        ("type", "name"),
        ("\u{23ce}", "confirm"),
        ("esc", "cancel"),
      ]
    }
    Mode::ConfirmRemove => {
      vec![("y", "delete group"), ("n", "keep"), ("esc", "cancel")]
    }
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

fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
  let mut lines = Vec::new();
  let mut current = String::new();

  for word in text.split_whitespace() {
    if current.is_empty() {
      current = word.to_string();
    } else if current.len() + 1 + word.len() <= max_width {
      current.push(' ');
      current.push_str(word);
    } else {
      lines.push(current);
      current = word.to_string();
    }
  }
  if !current.is_empty() {
    lines.push(current);
  }
  if lines.is_empty() {
    lines.push(String::new());
  }
  lines
}

#[cfg(test)]
mod tests {
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
      },
      FileGroup {
        id: 1,
        label: "Cats".to_string(),
        rationale: "Cat photos".to_string(),
        members: vec![2, 3, 4],
        member_destinations: vec![],
        suggested_path: PathBuf::from("cats"),
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
      None,
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
      None,
    )
  }

  fn make_dupes_state() -> ReviewState {
    make_state_with_mode(ReviewMode::Dupes)
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
  fn x_triggers_execute() {
    let mut state = make_state();
    state.handle_key(KeyCode::Char('x'));
    assert_eq!(state.pending_action(), Some(ReviewAction::Execute));
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
      }],
      vec![FileMove {
        from: PathBuf::from("/dl/a.jpg"),
        to: PathBuf::from("/out/only/a.jpg"),
        group_id: 0,
      }],
      PathBuf::from("/out"),
      None,
      ReviewMode::Organize,
      None,
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
      }],
      vec![],
      PathBuf::from("/out"),
      None,
      ReviewMode::Organize,
      None,
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
      None,
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
      }],
      vec![FileMove {
        from: PathBuf::from("/dl/clip.mp4"),
        to: PathBuf::from("/out/videos/clip.mp4"),
        group_id: 0,
      }],
      PathBuf::from("/out"),
      None,
      ReviewMode::Organize,
      None,
    );

    assert!(!state.has_image_preview());
    assert_eq!(
      state.preview_path,
      Some(PathBuf::from("/dl/clip.mp4"))
    );
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
  fn space_in_files_does_not_toggle_keep_in_organize() {
    let mut state = make_state();
    state.handle_key(KeyCode::Tab);
    state.handle_key(KeyCode::Char('j'));
    assert!(state.is_file_kept(0, 1));
    state.handle_key(KeyCode::Char(' '));
    assert!(state.is_file_kept(0, 1));
  }

  #[test]
  fn enter_toggles_file_mark_in_organize() {
    let mut state = make_state();
    state.handle_key(KeyCode::Tab);
    assert!(!state.is_file_marked(0, 0));
    state.handle_key(KeyCode::Enter);
    assert!(state.is_file_marked(0, 0));
    state.handle_key(KeyCode::Enter);
    assert!(!state.is_file_marked(0, 0));
  }

  #[test]
  fn marks_clear_on_group_navigation() {
    let mut state = make_state();
    state.handle_key(KeyCode::Tab);
    state.handle_key(KeyCode::Enter);
    assert!(state.is_file_marked(0, 0));
    state.handle_key(KeyCode::Tab);
    state.handle_key(KeyCode::Char('j'));
    assert!(!state.is_file_marked(0, 0));
  }

  #[test]
  fn remove_marked_files_removes_multiple() {
    let mut state = make_state();
    state.handle_key(KeyCode::Tab);
    state.handle_key(KeyCode::Enter);
    state.handle_key(KeyCode::Char('j'));
    state.handle_key(KeyCode::Enter);
    assert_eq!(state.current_group_moves().len(), 2);
    state.handle_key(KeyCode::Char('d'));
    assert_eq!(state.current_group_moves().len(), 0);
  }

  #[test]
  fn move_marked_files_to_group() {
    let mut state = make_state();
    state.handle_key(KeyCode::Tab);
    state.handle_key(KeyCode::Enter);
    state.handle_key(KeyCode::Char('j'));
    state.handle_key(KeyCode::Enter);
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
  fn s_key_toggles_mode() {
    let mut state = ReviewState::new(
      make_groups(),
      make_moves(),
      PathBuf::from("/out"),
      None,
      ReviewMode::Organize,
      Some((
        make_groups(),
        make_moves(),
        ReviewMode::Dupes,
        vec![DuplicateType::Exact],
      )),
    );
    assert_eq!(state.review_mode(), ReviewMode::Organize);
    assert!(state.is_file_kept(0, 0));
    assert!(state.is_file_kept(0, 1));

    state.handle_key(KeyCode::Char('s'));
    assert_eq!(state.review_mode(), ReviewMode::Dupes);
    assert!(state.is_file_kept(0, 0));
    assert!(!state.is_file_kept(0, 1));

    state.handle_key(KeyCode::Char('s'));
    assert_eq!(state.review_mode(), ReviewMode::Organize);
    assert!(state.is_file_kept(0, 0));
    assert!(state.is_file_kept(0, 1));
  }

  #[test]
  fn s_key_does_nothing_without_other_mode() {
    let mut state = make_state();
    assert_eq!(state.review_mode(), ReviewMode::Organize);

    state.handle_key(KeyCode::Char('s'));
    assert_eq!(state.review_mode(), ReviewMode::Organize);
  }

  #[test]
  fn organize_keys_disabled_in_dupes_mode() {
    let mut state = make_dupes_state();
    state.handle_key(KeyCode::Tab);
    assert_eq!(state.focus(), Pane::Files);

    state.handle_key(KeyCode::Char('m'));
    assert_eq!(state.mode, Mode::Normal);

    state.handle_key(KeyCode::Char('n'));
    assert_eq!(state.mode, Mode::Normal);
  }

  #[test]
  fn d_opens_diff_view_in_dupes_mode() {
    let mut state = make_dupes_state();
    state.handle_key(KeyCode::Char('d'));
    assert_eq!(state.mode, Mode::DiffView { compare_idx: 0 });

    state.handle_key(KeyCode::Esc);
    assert_eq!(state.mode, Mode::Normal);
  }

  #[test]
  fn diff_view_jk_cycles_files() {
    let mut state = make_dupes_state();
    state.handle_key(KeyCode::Char('d'));
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
    state.handle_key(KeyCode::Char('d'));
    state.handle_key(KeyCode::Char('k'));
    assert_eq!(state.mode, Mode::DiffView { compare_idx: 0 });
  }

  #[test]
  fn diff_view_d_toggles_back() {
    let mut state = make_dupes_state();
    state.handle_key(KeyCode::Char('d'));
    assert_eq!(state.mode, Mode::DiffView { compare_idx: 0 });
    state.handle_key(KeyCode::Char('d'));
    assert_eq!(state.mode, Mode::Normal);
  }

  #[test]
  fn dupe_types_preserved_across_mode_swap() {
    let mut state = ReviewState::new(
      make_groups(),
      make_moves(),
      PathBuf::from("/out"),
      None,
      ReviewMode::Organize,
      Some((
        make_groups(),
        make_moves(),
        ReviewMode::Dupes,
        vec![DuplicateType::Exact],
      )),
    );
    assert!(state.dupe_types.is_empty());

    state.handle_key(KeyCode::Char('s'));
    assert_eq!(state.review_mode(), ReviewMode::Dupes);
    assert_eq!(state.dupe_types.len(), 1);
    assert_eq!(state.dupe_types[0], DuplicateType::Exact);

    state.handle_key(KeyCode::Char('s'));
    assert_eq!(state.review_mode(), ReviewMode::Organize);
    assert!(state.dupe_types.is_empty());
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
}

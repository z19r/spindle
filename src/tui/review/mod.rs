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
  ContentDescription, DescriptionSource, DuplicateSet, DuplicateType,
  FileGroup, FileMove, FileType, FingerprintedFile,
};

mod keys;
mod render;
mod state;
#[cfg(test)]
mod tests;
pub mod theme;

pub use render::render;

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
  RenameGroup { input: String, cursor_pos: usize },
  MergeInto { cursor: usize },
  ConfirmRemove,
  ConfirmExecute,
  DiffView { compare_idx: usize },
  Preview,
  Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewMode {
  Organize,
  Dupes,
}

pub enum PreviewState {
  None,
  Loading,
  Ready(Box<StatefulProtocol>),
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

/// Why a file is a duplicate and of what, so the organize screen can
/// badge it and diff it against its partner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DupeInfo {
  pub kind: DuplicateType,
  /// The other file: the canonical copy for a duplicate, the first
  /// duplicate for a canonical.
  pub partner: PathBuf,
  pub is_canonical: bool,
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
  file_metadata: HashMap<PathBuf, (String, u64)>,
  /// Duplicate relationships by source path (organize mode).
  dupe_info: HashMap<PathBuf, DupeInfo>,
  /// One-line run summary shown in the header.
  banner: Option<String>,
  /// Model descriptions by source path (organize mode).
  descriptions: HashMap<PathBuf, ContentDescription>,
  dupe_types: Vec<DuplicateType>,
  diff_state: Option<DiffState>,
  /// Per-file explanation (from `FileGroup::member_notes`), keyed by
  /// source path so it survives moves between groups.
  file_notes: HashMap<PathBuf, String>,
}

/// Groups the pipeline creates for files it could not place. Shown
/// first and left unapproved so nothing surprising moves.
fn is_system_group(group: &FileGroup) -> bool {
  group.label == crate::pipeline::UNSORTED_LABEL
    || group.label == "Needs Review"
}

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::KeyCode;
use ratatui::{
  layout::{Alignment, Constraint, Layout, Rect},
  style::{Modifier, Style},
  text::{Line, Span},
  widgets::{
    Block, BorderType, Clear, List, ListItem, ListState, Padding,
    Paragraph,
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
  /// The detail column. It has no cursor of its own — it shows
  /// whatever the group or file cursor points at — so focusing it
  /// only redirects the movement keys to its scroll offset.
  Detail,
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
  /// An animated GIF, playing. Boxed because the frames make this
  /// variant far larger than the others.
  Animated(Box<Animation>),
}

/// What the preview thread hands back: one image, or a GIF's frames.
///
/// The still arrives as a finished protocol because the worker has a
/// picker clone and nothing else to do with it. The animation arrives
/// as frames, because its first protocol is built at the same moment
/// as every later one and there is no reason to special-case it.
pub enum Decoded {
  Still(Box<StatefulProtocol>),
  Animation(Vec<AnimationFrame>),
}

/// One decoded frame and how long it stays on screen.
pub struct AnimationFrame {
  pub image: image::DynamicImage,
  pub delay: Duration,
}

/// A GIF being played in the preview pane.
///
/// The frames are kept as images and the protocol for the visible one
/// is rebuilt on each advance, rather than building every frame's
/// protocol up front. Measured on this machine (see
/// `examples/measure_gif_protocol.rs`), a rebuild costs 0.3ms under
/// kitty and 5.1ms under sixel — the worst case is 6.8% of the
/// review screen's 80ms tick — while holding sixty ready-made
/// protocols costs their encoded payload for as long as the file
/// stays selected. Time is the cheap resource here and memory is not.
pub struct Animation {
  pub frames: Vec<AnimationFrame>,
  /// Index of the frame on screen.
  pub current: usize,
  /// The protocol for that frame.
  pub protocol: Box<StatefulProtocol>,
  /// When it went up, so the next advance knows if it is due.
  pub shown_at: Instant,
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
  /// The list pane whose cursor the detail column is describing:
  /// [`Pane::Groups`] or [`Pane::Files`], never [`Pane::Detail`].
  ///
  /// Focusing the detail column must not change what it shows, so the
  /// renderer reads this rather than `focus`.
  detail_of: Pane,
  action: Option<ReviewAction>,
  group_moves: Vec<Vec<FileMove>>,
  mode: Mode,
  output_dir: PathBuf,
  next_group_id: usize,
  picker: Option<Picker>,
  preview: PreviewState,
  preview_path: Option<PathBuf>,
  image_rx: Option<Receiver<(PathBuf, Decoded)>>,
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
  /// Header-only metadata per source path, computed the first time a
  /// file is shown in the detail pane.
  facts: HashMap<PathBuf, Vec<crate::facts::Fact>>,
  /// Viewport offsets for the list panes. The cursor lives in
  /// `selected` / `file_selected` / the mode's cursor; these only carry
  /// the scroll offset ratatui computes to keep that cursor on screen.
  group_list: ListState,
  file_list: ListState,
  picker_list: ListState,
  /// Rows the detail pane is scrolled down by. Reset whenever the
  /// cursor moves, clamped to the content at render time.
  detail_scroll: u16,
}

/// Another group the selected file could plausibly join, ranked by the
/// tags it shares with that group's members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alternative {
  pub group_idx: usize,
  pub label: String,
  pub shared_tags: Vec<String>,
  /// A few member filenames, so the user can judge without navigating.
  pub samples: Vec<String>,
}

/// Groups the pipeline creates for files it could not place. Shown
/// first and left unapproved so nothing surprising moves.
fn is_system_group(group: &FileGroup) -> bool {
  group.label == crate::pipeline::UNSORTED_LABEL
    || group.label == "Needs Review"
}

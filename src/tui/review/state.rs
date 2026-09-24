//! Construction, queries, and preview/diff plumbing for `ReviewState`.

use super::*;

pub(crate) fn decode_image(
  path: &Path,
) -> Option<image::DynamicImage> {
  match image::ImageReader::open(path)
    .and_then(|r| r.with_guessed_format())
  {
    Ok(reader) => match reader.decode() {
      Ok(img) => return Some(img),
      Err(e) => {
        tracing::debug!(?path, %e, "image crate failed, trying magick")
      }
    },
    Err(e) => {
      tracing::debug!(?path, %e, "image crate failed, trying magick")
    }
  }

  let tmp = std::env::temp_dir()
    .join(format!("spindle-preview-{}.png", std::process::id()));
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

impl ReviewState {
  pub fn new(
    groups: Vec<FileGroup>,
    moves: Vec<FileMove>,
    output_dir: PathBuf,
    picker: Option<Picker>,
    mode: ReviewMode,
  ) -> Self {
    // Notes are keyed by fingerprinted index; moves line up with
    // `members` in order, so pair them positionally once, by path.
    let mut file_notes: HashMap<PathBuf, String> = HashMap::new();
    for g in &groups {
      let group_paths: Vec<&PathBuf> = moves
        .iter()
        .filter(|m| m.group_id == g.id)
        .map(|m| &m.from)
        .collect();
      for note in &g.member_notes {
        if let Some(pos) =
          g.members.iter().position(|&i| i == note.index)
        {
          if let Some(path) = group_paths.get(pos) {
            file_notes.insert((*path).clone(), note.note.clone());
          }
        }
      }
    }

    // System groups (Unsorted, Needs Review) first so the user sees
    // what the plan could not place before approving anything.
    let (mut groups, rest): (Vec<FileGroup>, Vec<FileGroup>) =
      if mode == ReviewMode::Organize {
        groups.into_iter().partition(is_system_group)
      } else {
        (Vec::new(), groups)
      };
    let system_count = groups.len();
    groups.extend(rest);

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
    let approved: Vec<bool> =
      (0..len).map(|i| i >= system_count).collect();

    let file_keep: Vec<Vec<bool>> =
      Self::init_file_keep(&group_moves, mode);

    let mut state = Self {
      groups,
      approved,
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
      file_metadata: HashMap::new(),
      dupe_info: HashMap::new(),
      dupe_types: Vec::new(),
      diff_state: None,
      file_notes,
    };
    state.update_image_preview();
    state
  }

  /// Why the pipeline put this file where it did, if it said.
  pub fn file_note(&self, path: &Path) -> Option<&str> {
    self.file_notes.get(path).map(String::as_str)
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

  pub(crate) fn init_file_keep(
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

  /// Attach duplicate relationships so the organize screen can badge
  /// files and diff them. Byte-identical copies start marked for
  /// deletion; near/similar matches stay kept until the user decides.
  pub fn with_duplicates(
    mut self,
    dupes: &[DuplicateSet],
    files: &[FingerprintedFile],
  ) -> Self {
    for set in dupes {
      let Some(canonical) = files.get(set.canonical) else {
        continue;
      };
      let canonical_path = canonical.scanned.path.clone();
      let mut first_dup: Option<PathBuf> = None;
      for &dup_idx in &set.duplicates {
        let Some(dup) = files.get(dup_idx) else {
          continue;
        };
        let path = dup.scanned.path.clone();
        first_dup.get_or_insert_with(|| path.clone());
        self.dupe_info.insert(
          path.clone(),
          DupeInfo {
            kind: set.duplicate_type,
            partner: canonical_path.clone(),
            is_canonical: false,
          },
        );
        if set.duplicate_type == DuplicateType::Exact {
          self.set_keep_by_path(&path, false);
        }
      }
      if let Some(partner) = first_dup {
        self.dupe_info.insert(
          canonical_path,
          DupeInfo {
            kind: set.duplicate_type,
            partner,
            is_canonical: true,
          },
        );
      }
    }
    self
  }

  pub fn dupe_info(&self, path: &Path) -> Option<&DupeInfo> {
    self.dupe_info.get(path)
  }

  fn set_keep_by_path(&mut self, path: &Path, keep: bool) {
    for (gi, moves) in self.group_moves.iter().enumerate() {
      if let Some(fi) = moves.iter().position(|m| m.from == path) {
        if let Some(k) = self.file_keep[gi].get_mut(fi) {
          *k = keep;
        }
      }
    }
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

  /// Moves that will run: approved groups, files not marked for
  /// deletion.
  pub fn approved_moves(&self) -> Vec<FileMove> {
    self
      .groups
      .iter()
      .enumerate()
      .filter(|(i, _)| self.approved[*i])
      .flat_map(|(gi, _)| {
        self.group_moves[gi]
          .iter()
          .enumerate()
          .filter(move |(fi, _)| self.is_file_kept(gi, *fi))
          .map(|(_, m)| m.clone())
      })
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
      PreviewState::Ready(protocol) => Some(protocol.as_mut()),
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

  pub fn is_file_marked(
    &self,
    group_idx: usize,
    file_idx: usize,
  ) -> bool {
    self
      .file_marked
      .get(group_idx)
      .map(|s| s.contains(&file_idx))
      .unwrap_or(false)
  }

  pub(crate) fn toggle_file_mark(&mut self) {
    let gi = self.selected;
    let fi = self.file_selected;
    if let Some(set) = self.file_marked.get_mut(gi) {
      if !set.remove(&fi) {
        set.insert(fi);
      }
    }
  }

  pub(crate) fn marked_file_indices(&self) -> Vec<usize> {
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

  pub(crate) fn clear_marks(&mut self) {
    if let Some(set) = self.file_marked.get_mut(self.selected) {
      set.clear();
    }
  }

  pub(crate) fn toggle_file_keep(&mut self) {
    let gi = self.selected;
    let fi = self.file_selected;
    let Some(keeps) = self.file_keep.get_mut(gi) else {
      return;
    };
    let Some(&current) = keeps.get(fi) else {
      return;
    };
    if current {
      // A duplicate set must keep at least one copy; an organize
      // group may delete anything.
      let kept_count = keeps.iter().filter(|&&k| k).count();
      if self.review_mode == ReviewMode::Organize || kept_count > 1 {
        keeps[fi] = false;
      }
    } else {
      keeps[fi] = true;
    }
  }

  pub(crate) fn enter_diff_view(&mut self, compare_idx: usize) {
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
    self.enter_diff_paths(primary_path, secondary_path);
  }

  /// Organize mode: diff the current file against its duplicate
  /// partner, wherever that partner sits.
  pub(crate) fn enter_dupe_diff(&mut self) {
    let Some(mv) = self.current_file_move() else {
      return;
    };
    let path = mv.from.clone();
    let Some(info) = self.dupe_info.get(&path).cloned() else {
      return;
    };
    let (primary, secondary) = if info.is_canonical {
      (path, info.partner)
    } else {
      (info.partner, path)
    };
    self.mode = Mode::DiffView { compare_idx: 0 };
    self.enter_diff_paths(primary, secondary);
  }

  pub(crate) fn enter_diff_paths(
    &mut self,
    primary_path: PathBuf,
    secondary_path: PathBuf,
  ) {
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

  pub(crate) fn exit_diff_view(&mut self) {
    self.diff_state = None;
  }

  pub(crate) fn enter_preview(&mut self) {
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
      let lines: Vec<DiffLine> = text
        .lines()
        .map(|l| DiffLine::Same(l.to_string()))
        .collect();
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

  pub(crate) fn update_image_preview(&mut self) {
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
            self.preview = PreviewState::Ready(Box::new(protocol));
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
              ds.primary_preview =
                PreviewState::Ready(Box::new(protocol));
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
              ds.secondary_preview =
                PreviewState::Ready(Box::new(protocol));
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
}

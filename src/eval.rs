//! Grouping-quality scorer. Compares a produced plan against an
//! `expected.toml` fixture and reports pairwise co-assignment F1,
//! top-level area accuracy, placement coverage, and label hygiene.
//! Pure functions — the real-API runner lives in `tests/`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::group::alternatives;
use crate::model::{
  ContentDescription, FingerprintedFile, ReorgPlan,
};

/// Folder-name segments that describe a file *type* rather than a
/// subject. Shared with the validator so the eval judges the same rule
/// the pipeline enforces.
pub use crate::group::validate::TYPE_WORDS;

/// Smallest folder worth making. Shared with the validator's size
/// floor so the eval judges the shape the pipeline aims for.
pub use crate::group::validate::MIN_GROUP_SIZE;

/// Groups the pipeline itself creates; excluded from hygiene checks.
pub const SYSTEM_LABELS: &[&str] = &["Needs Review", "Unsorted"];

/// Deepest label the hygiene check accepts (segments).
pub const MAX_LABEL_DEPTH: usize = 3;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ExpectedFile {
  /// Path relative to the fixture root, `/`-separated.
  pub path: String,
  /// Expected group label (may be nested with `/`).
  pub group: String,
  /// Expected top-level area (first label segment).
  pub area: String,
  /// Other top-level areas a reasonable organizer might pick.
  #[serde(default)]
  pub alt_areas: Vec<String>,
  /// The expected folder already exists from a previous run, so the
  /// produced label should match it exactly (label reuse).
  #[serde(default)]
  pub existing: bool,
}

impl ExpectedFile {
  fn accepts_area(&self, normalized_first_segment: &str) -> bool {
    std::iter::once(&self.area)
      .chain(self.alt_areas.iter())
      .any(|a| normalize_segment(a) == normalized_first_segment)
  }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExpectedSet {
  #[serde(default, rename = "file")]
  pub files: Vec<ExpectedFile>,
}

/// Where the pipeline actually put a file.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
  pub path: String,
  pub label: String,
}

/// How many files each already-organised folder holds, before this
/// run adds anything to it.
///
/// The shape metrics ask what a reviewer finds on opening a folder,
/// and that is everything in the folder — not the share this run
/// happened to contribute. A second run that drops two photos into a
/// `Personal/Pets/Biscuit` that already holds four has produced a
/// six-file folder, and [`Granularity`] calling it thin is the metric
/// misreading its own input. The validator already agrees: since its
/// size floor learned about existing folders it declines to fold
/// exactly the groups scored as thin here, so without this the eval
/// penalises the behaviour the pipeline was fixed to have.
///
/// Empty is a first run, and is [`Default`].
#[derive(Debug, Clone, Default)]
pub struct ExistingSizes(HashMap<String, usize>);

impl ExistingSizes {
  /// Count the files under every directory of an already-organised
  /// tree, keyed by the label that directory stands for.
  ///
  /// A `root` that does not exist gives an empty set, which scores as
  /// a first run. Files loose at the root belong to no folder and are
  /// skipped.
  pub fn from_tree(root: &Path) -> Self {
    let mut sizes: HashMap<String, usize> = HashMap::new();
    for entry in walkdir::WalkDir::new(root)
      .into_iter()
      .filter_map(std::result::Result::ok)
      .filter(|e| e.file_type().is_file())
    {
      let Ok(rel) = entry.path().strip_prefix(root) else {
        continue;
      };
      let Some(parent) = rel.parent() else { continue };
      let label = parent.to_string_lossy();
      if label.is_empty() {
        continue;
      }
      *sizes.entry(normalize_label(&label)).or_default() += 1;
    }
    Self(sizes)
  }

  /// Record a folder that already holds `files` files.
  pub fn insert(&mut self, label: &str, files: usize) {
    self.0.insert(normalize_label(label), files);
  }

  /// Files already filed under `label`; zero if this run invented it.
  ///
  /// Keyed like every other label comparison here, so a case or
  /// punctuation variant of the folder on disk still counts as it.
  pub fn get(&self, label: &str) -> usize {
    self.0.get(&normalize_label(label)).copied().unwrap_or(0)
  }

  /// The labels this set knows about.
  pub fn len(&self) -> usize {
    self.0.len()
  }

  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hygiene {
  pub total_groups: usize,
  pub type_word_groups: usize,
  pub singleton_groups: usize,
  pub too_deep_groups: usize,
}

impl Hygiene {
  /// 1.0 when no group violates any rule; a group counts once even
  /// if it breaks several.
  pub fn score(&self) -> f64 {
    if self.total_groups == 0 {
      return 1.0;
    }
    let bad = (self.type_word_groups
      + self.singleton_groups
      + self.too_deep_groups)
      .min(self.total_groups);
    1.0 - bad as f64 / self.total_groups as f64
  }
}

/// How the produced folders are *shaped*, as distinct from whether
/// the right files are in them.
///
/// [`Hygiene`] counts the groups that are outright malformed: named
/// for a file type, deeper than the cap, holding exactly one file.
/// None of that notices what a real run of 1226 files actually did —
/// 58% of its folders held two or three files and 72% of its labels
/// sat at the depth cap. Every one of those folders is defensible on
/// its own. It is the distribution that is wrong, and a per-group
/// pass/fail cannot see a distribution.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Granularity {
  /// Non-system groups.
  pub total_groups: usize,
  /// Groups holding fewer than [`MIN_GROUP_SIZE`] files.
  pub thin_groups: usize,
  /// Files sitting in one of those.
  pub files_in_thin_groups: usize,
  /// Files in any non-system group; the denominator for the score.
  pub placed_files: usize,
  /// Groups whose label is exactly [`MAX_LABEL_DEPTH`] deep. Legal,
  /// but a tree that is nearly all of them is reaching for the cap
  /// rather than finding a shape.
  pub deepest_groups: usize,
  /// Middle group size — the upper of the two middles when the count
  /// is even, so the number is always a size some folder really has.
  pub median_group_size: usize,
}

impl Granularity {
  /// Share of placed files that landed somewhere worth opening.
  ///
  /// Counted by file, not by group, on purpose: by group, a run that
  /// files two hundred documents well and strands six pairs scores
  /// the same as one that puts half of everything in pairs. What goes
  /// wrong when folders are too thin is the reviewer's click through
  /// a near-empty folder, and that happens once per file.
  ///
  /// Depth is deliberately not scored. `Work/Acme Corp/Website
  /// Redesign` holding eight files is exactly right, and a depth term
  /// would push the prompt towards a flat tree that is no better.
  /// Depth at the cap is a symptom of thinness rather than a fault of
  /// its own — in the measured run, depth-3 labels fell from 134 to 76
  /// purely because the size floor merged the thin ones. So
  /// [`Self::deepest_groups`] is reported and left out of the number.
  pub fn score(&self) -> f64 {
    if self.placed_files == 0 {
      return 1.0;
    }
    1.0 - self.files_in_thin_groups as f64 / self.placed_files as f64
  }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EvalReport {
  pub expected_files: usize,
  pub placed_files: usize,
  /// Distinct expected groups.
  pub expected_groups: usize,
  /// Distinct produced groups, system groups excluded. Compared with
  /// `expected_groups` it shows over- or under-splitting at a glance.
  pub produced_groups: usize,
  pub pairwise_precision: f64,
  pub pairwise_recall: f64,
  pub pairwise_f1: f64,
  pub top_level_accuracy: f64,
  pub hygiene: Hygiene,
  pub granularity: Granularity,
  /// Files whose expected folder already existed before the run.
  pub reuse_expected: usize,
  /// Of those, how many landed under exactly that label.
  pub reuse_matched: usize,
}

impl EvalReport {
  pub fn placed_fraction(&self) -> f64 {
    if self.expected_files == 0 {
      return 1.0;
    }
    self.placed_files as f64 / self.expected_files as f64
  }

  /// Share of files with a pre-existing folder that were filed under
  /// exactly that label. `None` when the fixture has no such files.
  pub fn reuse_rate(&self) -> Option<f64> {
    (self.reuse_expected > 0)
      .then(|| self.reuse_matched as f64 / self.reuse_expected as f64)
  }

  /// Single number to compare runs: grouping agreement dominates,
  /// then top-level routing, then coverage, label hygiene and the
  /// shape of the folders.
  ///
  /// Granularity is weighted like hygiene rather than like agreement.
  /// A run that splits one right group into two thin ones is still a
  /// run that understood the files, and the pairwise F1 already docks
  /// it for the split; this term is what makes the same mistake
  /// visible when the fixture is small enough that F1 barely moves.
  ///
  /// Adding a fifth term rescales the whole number, so composites
  /// recorded before it are not comparable with ones recorded after.
  pub fn composite(&self) -> f64 {
    0.45 * self.pairwise_f1
      + 0.25 * self.top_level_accuracy
      + 0.1 * self.placed_fraction()
      + 0.1 * self.hygiene.score()
      + 0.1 * self.granularity.score()
  }
}

impl std::fmt::Display for EvalReport {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    writeln!(f, "metric                 value")?;
    writeln!(f, "---------------------  -----")?;
    writeln!(
      f,
      "placed                 {}/{} ({:.2})",
      self.placed_files,
      self.expected_files,
      self.placed_fraction()
    )?;
    writeln!(
      f,
      "groups                 {} produced / {} expected",
      self.produced_groups, self.expected_groups
    )?;
    writeln!(
      f,
      "pairwise precision     {:.3}",
      self.pairwise_precision
    )?;
    writeln!(
      f,
      "pairwise recall        {:.3}",
      self.pairwise_recall
    )?;
    writeln!(f, "pairwise f1            {:.3}", self.pairwise_f1)?;
    writeln!(
      f,
      "top-level accuracy     {:.3}",
      self.top_level_accuracy
    )?;
    writeln!(
      f,
      "hygiene                {:.3} (groups {}, type-word {}, singleton {}, too-deep {})",
      self.hygiene.score(),
      self.hygiene.total_groups,
      self.hygiene.type_word_groups,
      self.hygiene.singleton_groups,
      self.hygiene.too_deep_groups
    )?;
    writeln!(
      f,
      "granularity            {:.3} (thin {}/{} groups, {} files; median {}; at depth {} {})",
      self.granularity.score(),
      self.granularity.thin_groups,
      self.granularity.total_groups,
      self.granularity.files_in_thin_groups,
      self.granularity.median_group_size,
      MAX_LABEL_DEPTH,
      self.granularity.deepest_groups
    )?;
    if let Some(rate) = self.reuse_rate() {
      writeln!(
        f,
        "label reuse            {}/{} ({:.3})",
        self.reuse_matched, self.reuse_expected, rate
      )?;
    }
    writeln!(f, "composite              {:.3}", self.composite())
  }
}

pub fn load_expected(path: &Path) -> Result<ExpectedSet> {
  let text = std::fs::read_to_string(path).with_context(|| {
    format!("Failed to read expected set: {}", path.display())
  })?;
  toml::from_str(&text).with_context(|| {
    format!("Failed to parse expected set: {}", path.display())
  })
}

/// Flatten a plan into (relative path, label) pairs. Paths are made
/// relative to `root` and `/`-separated so they match `expected.toml`.
pub fn placements_from_plan(
  plan: &ReorgPlan,
  files: &[FingerprintedFile],
  root: &Path,
) -> Vec<Placement> {
  let mut out = Vec::new();
  for group in &plan.groups {
    for &idx in &group.members {
      let Some(file) = files.get(idx) else { continue };
      out.push(Placement {
        path: relative_path(&file.scanned.path, root),
        label: group.label.clone(),
      });
    }
  }
  out
}

/// A path as `expected.toml` writes it: relative to the fixture root
/// and `/`-separated.
fn relative_path(path: &Path, root: &Path) -> String {
  path
    .strip_prefix(root)
    .unwrap_or(path)
    .to_string_lossy()
    .replace('\\', "/")
}

/// Descriptions keyed by the same relative path [`Placement`] uses,
/// so the alternatives metric can look a file's tags up by path. The
/// pipeline keys them by fingerprinted index, which nothing outside
/// one run can interpret.
pub fn descriptions_by_path(
  files: &[FingerprintedFile],
  descriptions: &HashMap<usize, ContentDescription>,
  root: &Path,
) -> HashMap<String, ContentDescription> {
  descriptions
    .iter()
    .filter_map(|(&idx, d)| {
      let file = files.get(idx)?;
      Some((relative_path(&file.scanned.path, root), d.clone()))
    })
    .collect()
}

/// How often the review pane's ALSO FITS list offers the other home
/// the answer key allows.
///
/// Reported, never scored. The list is a convenience — a reviewer who
/// is offered nothing retypes a label and moves on — so a bad number
/// here is an argument for #127, not a failing run. Folding it into
/// the composite would also move every fixture's ceiling and every
/// floor derived from one, for a number nobody has a baseline for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AltCoverage {
  /// Described files whose answer key names an `alt_areas` home the
  /// run did not use. The denominator.
  pub files: usize,
  /// Of those, the ones offered a group in an acceptable other area.
  pub offered: usize,
  /// Of those, the ones offered no alternatives whatsoever. A subset
  /// of the misses, and a different complaint: the ranking found
  /// nothing rather than finding the wrong thing.
  pub empty: usize,
}

impl AltCoverage {
  /// `None` when no fixture file poses the question.
  pub fn rate(&self) -> Option<f64> {
    (self.files > 0).then(|| self.offered as f64 / self.files as f64)
  }
}

/// Ask, for every file whose answer key names more than one
/// acceptable area, whether [`alternatives::rank`] offers a folder in
/// one of the areas the run did not already use.
///
/// The question needs no new ground truth: `alt_areas` is already
/// there, recording where else a reasonable organizer might have put
/// the file, which is exactly what ALSO FITS is for.
pub fn alternatives_coverage(
  expected: &ExpectedSet,
  placements: &[Placement],
  descriptions: &HashMap<String, ContentDescription>,
) -> AltCoverage {
  /// What the review pane shows.
  const MAX: usize = 3;

  let mut members: HashMap<&str, Vec<&str>> = HashMap::new();
  for p in placements {
    if is_system_label(&p.label) {
      continue;
    }
    members.entry(&p.label).or_default().push(&p.path);
  }
  let mut labels: Vec<&str> = members.keys().copied().collect();
  labels.sort_unstable();

  let placed: HashMap<&str, &str> =
    placements.iter().map(|p| (&*p.path, &*p.label)).collect();

  let mut out = AltCoverage::default();
  for file in &expected.files {
    if file.alt_areas.is_empty() {
      continue;
    }
    let Some(desc) = descriptions.get(&file.path) else {
      continue;
    };
    let own = placed.get(file.path.as_str()).copied();

    // Every area the key allows except the one the run used: the
    // alternative to a file already filed under `alt_areas[0]` is its
    // primary area, not the folder it is sitting in.
    let used = own.map(area_of).unwrap_or_default();
    let wanted: HashSet<String> = std::iter::once(&file.area)
      .chain(file.alt_areas.iter())
      .map(|a| normalize_segment(a))
      .filter(|a| *a != used)
      .collect();
    if wanted.is_empty() {
      continue;
    }
    out.files += 1;

    let candidates: Vec<alternatives::Candidate> = labels
      .iter()
      .filter(|l| Some(**l) != own)
      .map(|l| alternatives::Candidate {
        label: l,
        members: members[l]
          .iter()
          .filter_map(|p| descriptions.get(*p))
          .collect(),
      })
      .collect();

    let ranked = alternatives::rank(desc, &candidates, MAX);
    if ranked.is_empty() {
      out.empty += 1;
      continue;
    }
    if ranked
      .iter()
      .any(|r| wanted.contains(&area_of(candidates[r.index].label)))
    {
      out.offered += 1;
    }
  }
  out
}

/// A label's top-level area, normalized. Empty for an empty label.
fn area_of(label: &str) -> String {
  segments(label).into_iter().next().unwrap_or_default()
}

/// Lowercase alphanumerics only, so "Acme Corp" == "acme_corp".
pub fn normalize_segment(segment: &str) -> String {
  segment
    .chars()
    .filter(|c| c.is_alphanumeric())
    .flat_map(char::to_lowercase)
    .collect()
}

fn segments(label: &str) -> Vec<String> {
  label
    .replace('\\', "/")
    .split('/')
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(normalize_segment)
    .collect()
}

fn normalize_label(label: &str) -> String {
  segments(label).join("/")
}

fn is_system_label(label: &str) -> bool {
  SYSTEM_LABELS
    .iter()
    .any(|s| s.eq_ignore_ascii_case(label.trim()))
}

/// Score a run against its answer key.
///
/// `existing` is what the output tree already held before this run;
/// pass `&ExistingSizes::default()` for a first run.
pub fn score(
  expected: &ExpectedSet,
  actual: &[Placement],
  existing: &ExistingSizes,
) -> EvalReport {
  let actual_by_path: HashMap<&str, &str> = actual
    .iter()
    .map(|p| (p.path.as_str(), p.label.as_str()))
    .collect();

  // Each expected file gets an actual label; unplaced files get a
  // unique sentinel so they never pair with anything.
  let assigned: Vec<(&ExpectedFile, String)> = expected
    .files
    .iter()
    .map(|e| {
      let label = actual_by_path
        .get(e.path.as_str())
        .map(|l| normalize_label(l))
        .unwrap_or_else(|| format!("__unplaced__/{}", e.path));
      (e, label)
    })
    .collect();

  let placed_files = assigned
    .iter()
    .filter(|(e, _)| actual_by_path.contains_key(e.path.as_str()))
    .count();

  let (mut tp, mut fp, mut fnn) = (0usize, 0usize, 0usize);
  for i in 0..assigned.len() {
    for j in (i + 1)..assigned.len() {
      let (ea, la) = &assigned[i];
      let (eb, lb) = &assigned[j];
      let exp_same =
        normalize_label(&ea.group) == normalize_label(&eb.group);
      let act_same = la == lb;
      match (exp_same, act_same) {
        (true, true) => tp += 1,
        (false, true) => fp += 1,
        (true, false) => fnn += 1,
        (false, false) => {}
      }
    }
  }
  let precision = ratio(tp, tp + fp);
  let recall = ratio(tp, tp + fnn);
  let f1 = if precision + recall == 0.0 {
    0.0
  } else {
    2.0 * precision * recall / (precision + recall)
  };

  let top_hits = assigned
    .iter()
    .filter(|(e, label)| {
      label
        .split('/')
        .next()
        .map(|first| e.accepts_area(first))
        .unwrap_or(false)
    })
    .count();
  let top_level_accuracy = ratio(top_hits, assigned.len());

  let reuse_expected =
    assigned.iter().filter(|(e, _)| e.existing).count();
  let reuse_matched = assigned
    .iter()
    .filter(|(e, label)| {
      e.existing && *label == normalize_label(&e.group)
    })
    .count();

  EvalReport {
    expected_files: expected.files.len(),
    placed_files,
    pairwise_precision: precision,
    pairwise_recall: recall,
    pairwise_f1: f1,
    top_level_accuracy,
    expected_groups: expected
      .files
      .iter()
      .map(|e| normalize_label(&e.group))
      .collect::<HashSet<_>>()
      .len(),
    produced_groups: hygiene(actual, existing).total_groups,
    hygiene: hygiene(actual, existing),
    granularity: granularity(actual, existing),
    reuse_expected,
    reuse_matched,
  }
}

fn ratio(num: usize, den: usize) -> f64 {
  if den == 0 {
    // No pairs to judge: perfect by vacuity, mirrors precision/recall
    // conventions for empty sets.
    1.0
  } else {
    num as f64 / den as f64
  }
}

/// Label hygiene over the actual groups (system groups excluded).
///
/// `existing` lets a folder that already has files in it out of the
/// singleton count: one more file into a folder of ten is not a
/// one-file folder, and the validator no longer collapses it either.
pub fn hygiene(
  actual: &[Placement],
  existing: &ExistingSizes,
) -> Hygiene {
  let mut members: HashMap<&str, usize> = HashMap::new();
  for p in actual {
    if is_system_label(&p.label) {
      continue;
    }
    *members.entry(p.label.as_str()).or_default() += 1;
  }

  let type_words: HashSet<&str> =
    TYPE_WORDS.iter().copied().collect();
  let mut h = Hygiene {
    total_groups: members.len(),
    ..Default::default()
  };
  for (label, count) in members {
    let segs = segments(label);
    let has_type_word =
      segs.iter().any(|s| type_words.contains(s.as_str()));
    let too_deep = segs.len() > MAX_LABEL_DEPTH;
    let singleton = count + existing.get(label) == 1;
    if has_type_word {
      h.type_word_groups += 1;
    } else if too_deep {
      h.too_deep_groups += 1;
    } else if singleton {
      h.singleton_groups += 1;
    }
  }
  h
}

/// Group-shape statistics over the actual groups.
///
/// System groups are excluded, as in [`hygiene`]: `Unsorted` is a
/// holding pen rather than a proposal, and counting it as one enormous
/// group would hide exactly the thinness this measures.
///
/// Sizes are what the folder holds once this run is applied, so a
/// group is measured against [`ExistingSizes`] as well as its own
/// members. See that type for why.
pub fn granularity(
  actual: &[Placement],
  existing: &ExistingSizes,
) -> Granularity {
  let mut members: HashMap<&str, usize> = HashMap::new();
  for p in actual {
    if is_system_label(&p.label) {
      continue;
    }
    *members.entry(p.label.as_str()).or_default() += 1;
  }

  let mut sizes: Vec<usize> = Vec::with_capacity(members.len());
  let mut g = Granularity {
    total_groups: members.len(),
    ..Default::default()
  };
  for (label, count) in &members {
    // What the reviewer opens, not what this run contributed.
    let on_disk = count + existing.get(label);
    sizes.push(on_disk);
    g.placed_files += count;
    if on_disk < MIN_GROUP_SIZE {
      g.thin_groups += 1;
      // Still this run's files: `placed_files` is the denominator,
      // and a run is not charged for what a previous one placed.
      g.files_in_thin_groups += count;
    }
    if segments(label).len() == MAX_LABEL_DEPTH {
      g.deepest_groups += 1;
    }
  }
  sizes.sort_unstable();
  g.median_group_size =
    sizes.get(sizes.len() / 2).copied().unwrap_or(0);
  g
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Score as a first run: nothing was organised before, so no group
  /// is measured against an existing folder.
  fn scored(
    expected: &ExpectedSet,
    actual: &[Placement],
  ) -> EvalReport {
    score(expected, actual, &ExistingSizes::default())
  }

  fn gran(actual: &[Placement]) -> Granularity {
    granularity(actual, &ExistingSizes::default())
  }

  fn hyg(actual: &[Placement]) -> Hygiene {
    hygiene(actual, &ExistingSizes::default())
  }

  fn existing(path: &str, group: &str) -> ExpectedFile {
    ExpectedFile {
      existing: true,
      ..exp(path, group)
    }
  }

  fn pl(path: &str, label: &str) -> Placement {
    Placement {
      path: path.to_string(),
      label: label.to_string(),
    }
  }

  /// A folder with `files` already in it.
  fn already(label: &str, files: usize) -> ExistingSizes {
    let mut e = ExistingSizes::default();
    e.insert(label, files);
    e
  }

  /// Two more photos into a folder that already holds two is a
  /// four-file folder, and a reviewer opening it finds four.
  #[test]
  fn a_folder_that_already_has_files_is_not_thin() {
    let run = group_of(2, "Personal/Pets/Biscuit");
    let g = granularity(&run, &already("Personal/Pets/Biscuit", 2));
    assert_eq!(g.thin_groups, 0);
    assert_eq!(g.files_in_thin_groups, 0);
    assert_eq!(g.score(), 1.0);
  }

  /// The same two files, in a folder this run invented, still are.
  #[test]
  fn the_same_two_files_are_thin_in_a_new_folder() {
    let g = gran(&group_of(2, "Personal/Pets/Mochi"));
    assert_eq!(g.thin_groups, 1);
    assert_eq!(g.files_in_thin_groups, 2);
    assert_eq!(g.score(), 0.0);
  }

  /// A run is credited with what it placed, not with what it found.
  /// `placed_files` is the denominator of the score, so counting the
  /// previous run's files there would dilute this run's mistakes.
  #[test]
  fn existing_files_are_not_counted_as_this_runs() {
    let run = group_of(2, "Personal/Recipes");
    let g = granularity(&run, &already("Personal/Recipes", 9));
    assert_eq!(g.placed_files, 2);
    assert_eq!(g.total_groups, 1);
  }

  /// The median reports the size a folder really has once the run is
  /// applied, which is the only size anyone can go and look at.
  #[test]
  fn the_median_counts_what_the_folder_will_hold() {
    let run = group_of(1, "Work/Acme Corp/Website Redesign");
    let g = granularity(
      &run,
      &already("Work/Acme Corp/Website Redesign", 7),
    );
    assert_eq!(g.median_group_size, 8);
  }

  /// Labels are compared the way they are everywhere else here, so a
  /// folder on disk is recognised through case and punctuation.
  #[test]
  fn an_existing_folder_is_matched_loosely() {
    let run = group_of(2, "Legal/Smith v. Jones");
    let g = granularity(&run, &already("legal/smith v jones", 2));
    assert_eq!(g.thin_groups, 0);
  }

  /// One file into a folder of ten is not a one-file folder, and the
  /// validator stopped collapsing it — hygiene has to agree.
  #[test]
  fn one_file_into_an_existing_folder_is_not_a_singleton() {
    let run = group_of(1, "Reference/Manuals");
    let h = hygiene(&run, &already("Reference/Manuals", 10));
    assert_eq!(h.singleton_groups, 0);
    assert_eq!(h.score(), 1.0);
    // The same lone file in a folder nobody has is still a singleton.
    assert_eq!(hyg(&run).singleton_groups, 1);
  }

  /// A folder nobody has yet contributes nothing.
  #[test]
  fn an_unknown_label_has_no_existing_files() {
    assert_eq!(already("Work/Acme Corp", 4).get("Work/Initech"), 0);
  }

  #[test]
  fn from_tree_counts_the_files_under_each_folder() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    for (rel, n) in
      [("Finance/Taxes/2023", 3), ("Personal/Recipes", 1)]
    {
      std::fs::create_dir_all(root.join(rel)).unwrap();
      for i in 0..n {
        std::fs::write(root.join(rel).join(format!("{i}.txt")), "x")
          .unwrap();
      }
    }
    // A file loose at the root is in no folder at all.
    std::fs::write(root.join("stray.txt"), "x").unwrap();

    let e = ExistingSizes::from_tree(root);
    assert_eq!(e.get("Finance/Taxes/2023"), 3);
    assert_eq!(e.get("Personal/Recipes"), 1);
    assert_eq!(e.len(), 2);
  }

  /// A first run has no tree to read, and that is not an error.
  #[test]
  fn from_tree_on_a_missing_root_is_empty() {
    let dir = tempfile::TempDir::new().unwrap();
    let e = ExistingSizes::from_tree(&dir.path().join("Organized"));
    assert!(e.is_empty());
    assert_eq!(e.get("Anything"), 0);
  }

  /// `n` files, all filed under one label.
  fn group_of(n: usize, label: &str) -> Vec<Placement> {
    (0..n).map(|i| pl(&format!("{label}/{i}"), label)).collect()
  }

  #[test]
  fn a_full_group_is_not_thin() {
    let g = gran(&group_of(MIN_GROUP_SIZE, "Work/Acme Corp"));
    assert_eq!(g.thin_groups, 0);
    assert_eq!(g.files_in_thin_groups, 0);
    assert_eq!(g.score(), 1.0);
  }

  /// One short of the floor is thin, even though [`Hygiene`] calls it
  /// clean — a three-file folder is neither a singleton nor too deep.
  #[test]
  fn a_group_below_the_floor_is_thin_where_hygiene_sees_nothing() {
    let files = group_of(MIN_GROUP_SIZE - 1, "Work/Acme Corp");
    assert_eq!(hyg(&files).score(), 1.0);
    let g = gran(&files);
    assert_eq!(g.thin_groups, 1);
    assert_eq!(g.files_in_thin_groups, 3);
    assert_eq!(g.score(), 0.0);
  }

  /// The score counts files, not groups: one stray pair beside a large
  /// well-filed run barely moves it.
  #[test]
  fn thinness_is_weighted_by_files_not_by_groups() {
    let mut files = group_of(18, "Work/Acme Corp");
    files.extend(group_of(2, "Work/Odds and Ends"));
    let g = gran(&files);
    assert_eq!((g.total_groups, g.thin_groups), (2, 1));
    // Half the groups are thin; a tenth of the files are.
    assert!((g.score() - 0.9).abs() < 1e-9, "got {}", g.score());
  }

  /// The same two groups, sized the other way round, score far worse —
  /// which a per-group ratio could not tell apart.
  #[test]
  fn many_thin_groups_score_worse_than_one() {
    let mut split: Vec<Placement> = Vec::new();
    for i in 0..10 {
      split.extend(group_of(2, &format!("Work/Thing {i}")));
    }
    let whole = group_of(20, "Work/Acme Corp");
    assert_eq!(gran(&split).score(), 0.0);
    assert_eq!(gran(&whole).score(), 1.0);
  }

  /// A legal label at the depth cap is reported but not penalised: a
  /// full third-level folder is the shape the prompt asks for.
  #[test]
  fn depth_at_the_cap_is_counted_but_not_scored() {
    let g = gran(&group_of(8, "Work/Acme Corp/Website Redesign"));
    assert_eq!(g.deepest_groups, 1);
    assert_eq!(g.score(), 1.0);
  }

  #[test]
  fn the_holding_pen_is_not_a_group() {
    let mut files = group_of(6, "Work/Acme Corp");
    files.extend(group_of(40, "Unsorted"));
    let g = gran(&files);
    assert_eq!((g.total_groups, g.placed_files), (1, 6));
    assert_eq!(g.score(), 1.0);
  }

  #[test]
  fn the_median_is_a_size_some_folder_has() {
    let mut files = group_of(1, "Work/A");
    files.extend(group_of(5, "Work/B"));
    files.extend(group_of(9, "Work/C"));
    assert_eq!(gran(&files).median_group_size, 5);
    // Even count: the upper middle, so the answer is never a fraction.
    files.extend(group_of(11, "Work/D"));
    assert_eq!(gran(&files).median_group_size, 9);
  }

  #[test]
  fn no_groups_at_all_is_vacuously_full() {
    assert_eq!(gran(&[]).score(), 1.0);
  }

  /// The whole point of the term: a run that shreds correct groups
  /// into pairs must score below one that keeps them whole, even
  /// though both put every file in the right area.
  #[test]
  fn over_splitting_shows_up_in_the_composite() {
    let expected: Vec<ExpectedFile> = (0..8)
      .map(|i| exp(&format!("Work/Acme Corp/{i}"), "Work/Acme Corp"))
      .collect();
    let expected = ExpectedSet { files: expected };

    let whole: Vec<Placement> = expected
      .files
      .iter()
      .map(|e| pl(&e.path, "Work/Acme Corp"))
      .collect();
    let shredded: Vec<Placement> = expected
      .files
      .iter()
      .enumerate()
      .map(|(i, e)| pl(&e.path, &format!("Work/Acme Corp {}", i / 2)))
      .collect();

    let whole = scored(&expected, &whole);
    let shredded = scored(&expected, &shredded);
    assert_eq!(whole.granularity.score(), 1.0);
    assert_eq!(shredded.granularity.score(), 0.0);
    assert!(
      shredded.composite() < whole.composite() - 0.1,
      "shredding scored {:.3} against {:.3}",
      shredded.composite(),
      whole.composite()
    );
  }

  #[test]
  fn reuse_rate_counts_exact_label_matches_for_existing_folders() {
    let expected = ExpectedSet {
      files: vec![
        existing("a.txt", "Finance/Bills/Utilities"),
        existing("b.txt", "Finance/Bills/Utilities"),
        existing("c.txt", "Personal/Recipes"),
        exp("d.txt", "Work/Initech/Onboarding"),
      ],
    };
    let actual = vec![
      pl("a.txt", "Finance/Bills/Utilities"),
      pl("b.txt", "finance/bills/utilities"),
      pl("c.txt", "Personal/Recipes/Baking"),
      pl("d.txt", "Work/Initech/Onboarding"),
    ];
    let report = scored(&expected, &actual);
    assert_eq!(report.reuse_expected, 3);
    assert_eq!(report.reuse_matched, 2);
    assert!((report.reuse_rate().unwrap() - 2.0 / 3.0).abs() < 1e-9);
    assert!(report
      .to_string()
      .contains("label reuse            2/3"));
  }

  #[test]
  fn reuse_rate_is_absent_without_existing_folders() {
    let expected = ExpectedSet {
      files: vec![exp("a.txt", "Work/Acme")],
    };
    let report = scored(&expected, &[pl("a.txt", "Work/Acme")]);
    assert_eq!(report.reuse_rate(), None);
    assert!(!report.to_string().contains("label reuse"));
  }

  // --- alternatives coverage -------------------------------------

  fn described(tags: &[&str], category: &str) -> ContentDescription {
    ContentDescription {
      summary: String::new(),
      tags: tags.iter().map(|t| t.to_string()).collect(),
      suggested_category: category.to_string(),
      confidence: 0.9,
      source: crate::model::DescriptionSource::Ai,
    }
  }

  fn descs(
    entries: &[(&str, &[&str])],
  ) -> HashMap<String, ContentDescription> {
    entries
      .iter()
      .map(|(path, tags)| (path.to_string(), described(tags, "misc")))
      .collect()
  }

  fn with_alts(
    path: &str,
    group: &str,
    alts: &[&str],
  ) -> ExpectedFile {
    ExpectedFile {
      alt_areas: alts.iter().map(|a| a.to_string()).collect(),
      ..exp(path, group)
    }
  }

  /// A receipt filed under Finance, whose key also allows Work, is
  /// offered the Work folder full of receipts.
  #[test]
  fn the_allowed_other_area_is_offered() {
    let expected = ExpectedSet {
      files: vec![with_alts("a.pdf", "Finance/Receipts", &["Work"])],
    };
    let placements = vec![
      pl("a.pdf", "Finance/Receipts"),
      pl("b.pdf", "Work/Acme Corp/Expenses"),
    ];
    let d = descs(&[
      ("a.pdf", &["receipt", "acme"]),
      ("b.pdf", &["receipt", "acme"]),
    ]);
    let got = alternatives_coverage(&expected, &placements, &d);
    assert_eq!(got.files, 1);
    assert_eq!(got.offered, 1);
    assert_eq!(got.rate(), Some(1.0));
  }

  /// Offering something is not offering the right thing.
  #[test]
  fn an_unrelated_area_does_not_count_as_offered() {
    let expected = ExpectedSet {
      files: vec![with_alts("a.pdf", "Finance/Receipts", &["Work"])],
    };
    let placements = vec![
      pl("a.pdf", "Finance/Receipts"),
      pl("b.pdf", "Personal/Pets/Biscuit"),
    ];
    let d = descs(&[
      ("a.pdf", &["receipt"]),
      ("b.pdf", &["receipt", "dog"]),
    ]);
    let got = alternatives_coverage(&expected, &placements, &d);
    assert_eq!((got.files, got.offered, got.empty), (1, 0, 0));
  }

  /// Nothing shares a tag, so the pane shows an empty list. That is a
  /// miss, and a distinguishable one.
  #[test]
  fn an_empty_list_is_counted_separately() {
    let expected = ExpectedSet {
      files: vec![with_alts("a.pdf", "Finance/Receipts", &["Work"])],
    };
    let placements = vec![
      pl("a.pdf", "Finance/Receipts"),
      pl("b.pdf", "Work/Acme Corp/Expenses"),
    ];
    let d =
      descs(&[("a.pdf", &["receipt"]), ("b.pdf", &["onboarding"])]);
    let got = alternatives_coverage(&expected, &placements, &d);
    assert_eq!((got.files, got.offered, got.empty), (1, 0, 1));
  }

  /// A file already filed under one of its `alt_areas` still poses
  /// the question — the alternative it wants is its primary area.
  #[test]
  fn the_area_the_run_used_is_not_the_alternative() {
    let expected = ExpectedSet {
      files: vec![with_alts("a.pdf", "Finance/Receipts", &["Work"])],
    };
    let placements = vec![
      pl("a.pdf", "Work/Acme Corp/Expenses"),
      pl("b.pdf", "Work/Acme Corp/Invoices"),
      pl("c.pdf", "Finance/Receipts"),
    ];
    let d = descs(&[
      ("a.pdf", &["receipt"]),
      ("b.pdf", &["receipt"]),
      ("c.pdf", &["receipt"]),
    ]);
    let got = alternatives_coverage(&expected, &placements, &d);
    // Another Work folder is on offer, but Work is where it already
    // is; only the Finance folder counts.
    assert_eq!((got.files, got.offered), (1, 1));
  }

  /// Files the key gives only one area for are not asked about, and a
  /// fixture with none of them reports nothing at all.
  #[test]
  fn a_file_with_no_alt_areas_is_not_counted() {
    let expected = ExpectedSet {
      files: vec![exp("a.pdf", "Finance/Receipts")],
    };
    let placements = vec![
      pl("a.pdf", "Finance/Receipts"),
      pl("b.pdf", "Work/Acme Corp/Expenses"),
    ];
    let d =
      descs(&[("a.pdf", &["receipt"]), ("b.pdf", &["receipt"])]);
    let got = alternatives_coverage(&expected, &placements, &d);
    assert_eq!(got.files, 0);
    assert_eq!(got.rate(), None);
  }

  /// Unsorted is not somewhere a reviewer is invited to move a file.
  #[test]
  fn the_system_groups_are_never_offered() {
    let expected = ExpectedSet {
      files: vec![with_alts("a.pdf", "Finance/Receipts", &["Work"])],
    };
    let placements =
      vec![pl("a.pdf", "Finance/Receipts"), pl("b.pdf", "Unsorted")];
    let d =
      descs(&[("a.pdf", &["receipt"]), ("b.pdf", &["receipt"])]);
    let got = alternatives_coverage(&expected, &placements, &d);
    assert_eq!((got.files, got.offered, got.empty), (1, 0, 1));
  }

  /// A file the run never described has no tags to rank with, so it
  /// is not evidence about the ranking either way.
  #[test]
  fn an_undescribed_file_is_skipped() {
    let expected = ExpectedSet {
      files: vec![with_alts("a.pdf", "Finance/Receipts", &["Work"])],
    };
    let placements = vec![pl("b.pdf", "Work/Acme Corp/Expenses")];
    let d = descs(&[("b.pdf", &["receipt"])]);
    let got = alternatives_coverage(&expected, &placements, &d);
    assert_eq!(got.files, 0);
  }

  /// Only the three the pane shows count; a fourth-placed match is a
  /// match the reviewer never sees.
  #[test]
  fn only_the_offered_three_count() {
    let expected = ExpectedSet {
      files: vec![with_alts("a.pdf", "Finance/Receipts", &["Work"])],
    };
    let mut placements = vec![pl("a.pdf", "Finance/Receipts")];
    let mut d =
      vec![("a.pdf".to_string(), described(&["x", "y"], "misc"))];
    // Three perfect-overlap decoys sort above a partial Work match.
    for (i, label) in ["Personal/A", "Personal/B", "Personal/C"]
      .iter()
      .enumerate()
    {
      let p = format!("d{i}.pdf");
      placements.push(pl(&p, label));
      d.push((p, described(&["x", "y"], "misc")));
    }
    placements.push(pl("w.pdf", "Work/Acme Corp/Expenses"));
    d.push((
      "w.pdf".to_string(),
      described(&["x", "z", "q"], "misc"),
    ));
    let d: HashMap<String, ContentDescription> =
      d.into_iter().collect();
    let got = alternatives_coverage(&expected, &placements, &d);
    assert_eq!((got.files, got.offered), (1, 0));
  }

  fn exp(path: &str, group: &str) -> ExpectedFile {
    let area = group.split('/').next().unwrap().to_string();
    ExpectedFile {
      path: path.to_string(),
      group: group.to_string(),
      area,
      alt_areas: vec![],
      existing: false,
    }
  }

  fn put(path: &str, label: &str) -> Placement {
    Placement {
      path: path.to_string(),
      label: label.to_string(),
    }
  }

  fn expected() -> ExpectedSet {
    ExpectedSet {
      files: vec![
        exp("a.txt", "Finance/Taxes"),
        exp("b.txt", "Finance/Taxes"),
        exp("c.txt", "Legal/Lease"),
        exp("d.txt", "Legal/Lease"),
      ],
    }
  }

  /// Every file in exactly the right folder. The agreement metrics
  /// are perfect; the composite is not, because this fixture's folders
  /// hold two files each and [`Granularity`] says so. Matching the
  /// ground truth is all the scorer can ask of the model — it cannot
  /// make a four-file folder out of a two-file fixture — so the shape
  /// term docks the *fixture*, which is the honest reading.
  #[test]
  fn perfect_match_scores_one_on_every_agreement_metric() {
    let actual = vec![
      put("a.txt", "Finance/Taxes"),
      put("b.txt", "Finance/Taxes"),
      put("c.txt", "Legal/Lease"),
      put("d.txt", "Legal/Lease"),
    ];
    let r = scored(&expected(), &actual);
    assert_eq!(r.placed_files, 4);
    assert!((r.pairwise_f1 - 1.0).abs() < 1e-9);
    assert!((r.top_level_accuracy - 1.0).abs() < 1e-9);
    assert!((r.hygiene.score() - 1.0).abs() < 1e-9);
    assert!((r.placed_fraction() - 1.0).abs() < 1e-9);
    assert_eq!(r.granularity.thin_groups, 2);
  }

  /// The same perfect match on folders that are full scores one
  /// outright, which is what pins the composite's weights together.
  #[test]
  fn a_perfect_match_on_full_folders_scores_one() {
    let mut files = Vec::new();
    let mut actual = Vec::new();
    for (label, n) in [("Finance/Taxes", 4), ("Legal/Lease", 5)] {
      for i in 0..n {
        let path = format!("{label}/{i}");
        files.push(exp(&path, label));
        actual.push(put(&path, label));
      }
    }
    let r = scored(&ExpectedSet { files }, &actual);
    assert!(
      (r.composite() - 1.0).abs() < 1e-9,
      "composite was {:.6}",
      r.composite()
    );
  }

  #[test]
  fn label_spelling_differences_are_ignored() {
    let actual = vec![
      put("a.txt", "finance / taxes"),
      put("b.txt", "Finance/Taxes"),
      put("c.txt", "Legal/Lease"),
      put("d.txt", "legal/lease"),
    ];
    let r = scored(&expected(), &actual);
    assert!((r.pairwise_f1 - 1.0).abs() < 1e-9);
    assert!((r.top_level_accuracy - 1.0).abs() < 1e-9);
  }

  #[test]
  fn one_broad_bucket_has_perfect_recall_and_low_precision() {
    let actual = vec![
      put("a.txt", "Personal"),
      put("b.txt", "Personal"),
      put("c.txt", "Personal"),
      put("d.txt", "Personal"),
    ];
    let r = scored(&expected(), &actual);
    // 2 true pairs of 6 total pairs.
    assert!((r.pairwise_recall - 1.0).abs() < 1e-9);
    assert!((r.pairwise_precision - 2.0 / 6.0).abs() < 1e-9);
    assert!((r.top_level_accuracy - 0.0).abs() < 1e-9);
  }

  #[test]
  fn unplaced_files_lower_coverage_and_recall() {
    let actual = vec![
      put("a.txt", "Finance/Taxes"),
      put("c.txt", "Legal/Lease"),
      put("d.txt", "Legal/Lease"),
    ];
    let r = scored(&expected(), &actual);
    assert_eq!(r.placed_files, 3);
    assert!((r.placed_fraction() - 0.75).abs() < 1e-9);
    // Pair (a,b) is a false negative: recall 1/2.
    assert!((r.pairwise_recall - 0.5).abs() < 1e-9);
    assert!((r.pairwise_precision - 1.0).abs() < 1e-9);
    // b has no top-level match.
    assert!((r.top_level_accuracy - 0.75).abs() < 1e-9);
  }

  #[test]
  fn top_level_accuracy_checks_only_first_segment() {
    let actual = vec![
      put("a.txt", "Finance/Receipts"),
      put("b.txt", "Finance/Receipts"),
      put("c.txt", "Personal/Lease"),
      put("d.txt", "Personal/Lease"),
    ];
    let r = scored(&expected(), &actual);
    assert!((r.top_level_accuracy - 0.5).abs() < 1e-9);
    // Grouping itself is still perfect.
    assert!((r.pairwise_f1 - 1.0).abs() < 1e-9);
  }

  #[test]
  fn alt_areas_count_as_top_level_hits() {
    let mut set = expected();
    set.files[2].alt_areas = vec!["Housing".to_string()];
    let actual = vec![
      put("a.txt", "Finance/Taxes"),
      put("b.txt", "Finance/Taxes"),
      put("c.txt", "Housing/Lease"),
      put("d.txt", "Housing/Lease"),
    ];
    let r = scored(&set, &actual);
    // c accepts Housing, d does not.
    assert!((r.top_level_accuracy - 0.75).abs() < 1e-9);
  }

  #[test]
  fn hygiene_flags_type_words_singletons_and_depth() {
    let actual = vec![
      put("a", "Work/PDFs"),
      put("b", "Work/PDFs"),
      put("c", "Legal/Lease"),
      put("d", "A/B/C/D"),
      put("e", "A/B/C/D"),
      put("f", "Needs Review"),
      put("g", "Good/Group"),
      put("h", "Good/Group"),
    ];
    let h = hyg(&actual);
    assert_eq!(h.total_groups, 4);
    assert_eq!(h.type_word_groups, 1);
    assert_eq!(h.singleton_groups, 1);
    assert_eq!(h.too_deep_groups, 1);
    assert!((h.score() - 0.25).abs() < 1e-9);
  }

  #[test]
  fn hygiene_counts_each_group_once() {
    // Type word AND singleton AND too deep: one violation.
    let actual = vec![put("a", "A/B/C/Files")];
    let h = hyg(&actual);
    assert_eq!(h.total_groups, 1);
    assert_eq!(h.type_word_groups, 1);
    assert_eq!(h.singleton_groups, 0);
    assert_eq!(h.too_deep_groups, 0);
    assert!((h.score() - 0.0).abs() < 1e-9);
  }

  #[test]
  fn empty_expected_set_is_vacuously_perfect() {
    let r = scored(&ExpectedSet::default(), &[]);
    assert!((r.composite() - 1.0).abs() < 1e-9);
  }

  #[test]
  fn load_expected_parses_toml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("expected.toml");
    std::fs::write(
      &path,
      r#"
[[file]]
path = "Documents/lease.txt"
group = "Legal/Apartment Lease"
area = "Legal"

[[file]]
path = "Downloads/w2.txt"
group = "Finance/Taxes/2023"
area = "Finance"
"#,
    )
    .unwrap();
    let set = load_expected(&path).unwrap();
    assert_eq!(set.files.len(), 2);
    assert!(set.files[0].alt_areas.is_empty());
    assert_eq!(set.files[1].area, "Finance");
    assert_eq!(set.files[0].group, "Legal/Apartment Lease");
  }

  #[test]
  fn placements_from_plan_relativizes_paths() {
    use crate::model::{FileGroup, FileType, PlanStats, ScannedFile};
    let root = Path::new("/fixture");
    let mk = |p: &str| FingerprintedFile {
      scanned: ScannedFile {
        path: root.join(p),
        scan_root: root.to_path_buf(),
        size: 1,
        modified: std::time::SystemTime::UNIX_EPOCH,
        file_type: FileType::Other,
      },
      blake3_hash: [0u8; 32],
      perceptual_hash: None,
    };
    let files = vec![mk("Documents/a.txt"), mk("Downloads/b.txt")];
    let plan = ReorgPlan {
      groups: vec![FileGroup {
        id: 0,
        label: "Legal/Lease".into(),
        rationale: String::new(),
        members: vec![1, 0, 7],
        member_destinations: vec![],
        suggested_path: root.join("out"),
        member_notes: vec![],
      }],
      duplicates: vec![],
      moves: vec![],
      stats: PlanStats {
        total_files: 2,
        groups_created: 1,
        duplicates_found: 0,
        space_to_reclaim: 0,
      },
    };
    let got = placements_from_plan(&plan, &files, root);
    assert_eq!(
      got,
      vec![
        put("Downloads/b.txt", "Legal/Lease"),
        put("Documents/a.txt", "Legal/Lease"),
      ]
    );
  }

  #[test]
  fn report_counts_expected_and_produced_groups() {
    let actual = vec![
      put("a.txt", "Finance/Taxes"),
      put("b.txt", "Finance/Taxes/2023"),
      put("c.txt", "Legal/Lease"),
      put("d.txt", "Unsorted"),
    ];
    let r = scored(&expected(), &actual);
    assert_eq!(r.expected_groups, 2);
    assert_eq!(r.produced_groups, 3);
    assert!(r.to_string().contains("3 produced / 2 expected"));
  }

  #[test]
  fn report_display_lists_every_metric() {
    let r = scored(&expected(), &[]);
    let text = r.to_string();
    for key in [
      "placed",
      "pairwise f1",
      "top-level accuracy",
      "hygiene",
      "composite",
    ] {
      assert!(text.contains(key), "missing {key} in:\n{text}");
    }
  }
}

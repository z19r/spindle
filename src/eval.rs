//! Grouping-quality scorer. Compares a produced plan against an
//! `expected.toml` fixture and reports pairwise co-assignment F1,
//! top-level area accuracy, placement coverage, and label hygiene.
//! Pure functions — the real-API runner lives in `tests/`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::model::{FingerprintedFile, ReorgPlan};

/// Folder-name segments that describe a file *type* rather than a
/// subject. Shared with the validator so the eval judges the same rule
/// the pipeline enforces.
pub use crate::group::validate::TYPE_WORDS;

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
  /// then top-level routing, then coverage and label hygiene.
  pub fn composite(&self) -> f64 {
    0.5 * self.pairwise_f1
      + 0.3 * self.top_level_accuracy
      + 0.1 * self.placed_fraction()
      + 0.1 * self.hygiene.score()
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
      let rel = file
        .scanned
        .path
        .strip_prefix(root)
        .unwrap_or(&file.scanned.path);
      out.push(Placement {
        path: rel.to_string_lossy().replace('\\', "/"),
        label: group.label.clone(),
      });
    }
  }
  out
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

pub fn score(
  expected: &ExpectedSet,
  actual: &[Placement],
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
    produced_groups: hygiene(actual).total_groups,
    hygiene: hygiene(actual),
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
pub fn hygiene(actual: &[Placement]) -> Hygiene {
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
    let singleton = count == 1;
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

#[cfg(test)]
mod tests {
  use super::*;

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
    let report = score(&expected, &actual);
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
    let report = score(&expected, &[pl("a.txt", "Work/Acme")]);
    assert_eq!(report.reuse_rate(), None);
    assert!(!report.to_string().contains("label reuse"));
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

  #[test]
  fn perfect_match_scores_one() {
    let actual = vec![
      put("a.txt", "Finance/Taxes"),
      put("b.txt", "Finance/Taxes"),
      put("c.txt", "Legal/Lease"),
      put("d.txt", "Legal/Lease"),
    ];
    let r = score(&expected(), &actual);
    assert_eq!(r.placed_files, 4);
    assert!((r.pairwise_f1 - 1.0).abs() < 1e-9);
    assert!((r.top_level_accuracy - 1.0).abs() < 1e-9);
    assert!((r.hygiene.score() - 1.0).abs() < 1e-9);
    assert!((r.composite() - 1.0).abs() < 1e-9);
  }

  #[test]
  fn label_spelling_differences_are_ignored() {
    let actual = vec![
      put("a.txt", "finance / taxes"),
      put("b.txt", "Finance/Taxes"),
      put("c.txt", "Legal/Lease"),
      put("d.txt", "legal/lease"),
    ];
    let r = score(&expected(), &actual);
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
    let r = score(&expected(), &actual);
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
    let r = score(&expected(), &actual);
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
    let r = score(&expected(), &actual);
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
    let r = score(&set, &actual);
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
    let h = hygiene(&actual);
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
    let h = hygiene(&actual);
    assert_eq!(h.total_groups, 1);
    assert_eq!(h.type_word_groups, 1);
    assert_eq!(h.singleton_groups, 0);
    assert_eq!(h.too_deep_groups, 0);
    assert!((h.score() - 0.0).abs() < 1e-9);
  }

  #[test]
  fn empty_expected_set_is_vacuously_perfect() {
    let r = score(&ExpectedSet::default(), &[]);
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
    let r = score(&expected(), &actual);
    assert_eq!(r.expected_groups, 2);
    assert_eq!(r.produced_groups, 3);
    assert!(r.to_string().contains("3 produced / 2 expected"));
  }

  #[test]
  fn report_display_lists_every_metric() {
    let r = score(&expected(), &[]);
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

//! Deterministic clean-up of model-proposed group labels. The prompt
//! asks for subject-based, 1–3 level labels, but the model still emits
//! type-word folders ("PDFs"), case/punctuation variants of the same
//! label, lone-file leaf folders, and over-deep trees. Fixing those in
//! Rust is cheaper and more reliable than pleading in the prompt.

use std::collections::HashMap;

use crate::model::ProposedGroup;

/// Folder segments that describe a file *type* rather than a subject.
/// "Photos", "Videos" and "Screenshots" are deliberately absent: they
/// double as real top-level areas people actually keep.
pub const TYPE_WORDS: &[&str] = &[
  "pdf",
  "pdfs",
  "image",
  "images",
  "file",
  "files",
  "doc",
  "docs",
  "document",
  "documents",
  "misc",
  "miscellaneous",
  "other",
  "others",
  "spreadsheet",
  "spreadsheets",
  "text",
  "texts",
];

/// Labels the pipeline owns; never touched.
pub const SYSTEM_LABELS: &[&str] = &["Unsorted", "Needs Review"];

/// Deepest label kept (segments).
pub const MAX_DEPTH: usize = 3;

/// What validation changed, for the progress display.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Normalisation {
  /// Groups folded into another because their labels matched after
  /// normalisation.
  pub merged: usize,
  /// Lone-file leaf groups folded into their parent label.
  pub collapsed: usize,
  /// Labels rewritten (type words dropped, depth capped, whitespace).
  pub rewritten: usize,
}

/// Note attached to members of a group whose label was nothing but type
/// words: it carried no subject, so the files go to `Unsorted`.
pub const TYPE_ONLY_NOTE: &str = "grouped only by file type";

pub fn validate_groups(
  groups: Vec<ProposedGroup>,
) -> (Vec<ProposedGroup>, Normalisation) {
  let mut n = Normalisation::default();
  let mut system = Vec::new();
  let mut work = Vec::new();
  for g in groups {
    if g.member_indices.is_empty() {
      continue;
    }
    if is_system_label(&g.label) {
      system.push(g);
    } else {
      work.push(g);
    }
  }

  for g in &mut work {
    match tidy_label(&g.label) {
      Some(label) => {
        if label != g.label {
          n.rewritten += 1;
          g.label = label;
        }
      }
      None => {
        n.rewritten += 1;
        g.label = SYSTEM_LABELS[0].to_string();
        g.member_notes = g
          .member_indices
          .iter()
          .map(|&index| crate::model::MemberNote {
            index,
            note: TYPE_ONLY_NOTE.to_string(),
          })
          .collect();
      }
    }
  }

  let mut out = merge_by_key(work, &mut n.merged);

  let mut collapsed_any = false;
  for g in &mut out {
    if g.member_indices.len() == 1
      && !is_system_label(&g.label)
      && depth(&g.label) >= 2
    {
      g.label = parent(&g.label);
      n.collapsed += 1;
      collapsed_any = true;
    }
  }
  if collapsed_any {
    out = merge_by_key(out, &mut n.merged);
  }

  out.extend(system);
  (out, n)
}

fn is_system_label(label: &str) -> bool {
  SYSTEM_LABELS
    .iter()
    .any(|s| s.eq_ignore_ascii_case(label.trim()))
}

fn normalize_segment(segment: &str) -> String {
  segment
    .chars()
    .filter(|c| c.is_alphanumeric())
    .flat_map(char::to_lowercase)
    .collect()
}

/// Trim segments, drop empty and type-word ones, cap the depth. `None`
/// when nothing meaningful is left.
fn tidy_label(label: &str) -> Option<String> {
  let normalised = label.replace('\\', "/");
  let kept: Vec<&str> = normalised
    .split('/')
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .filter(|s| !TYPE_WORDS.contains(&normalize_segment(s).as_str()))
    .take(MAX_DEPTH)
    .collect();
  if kept.is_empty() {
    None
  } else {
    Some(kept.join("/"))
  }
}

fn depth(label: &str) -> usize {
  label.split('/').filter(|s| !s.trim().is_empty()).count()
}

fn parent(label: &str) -> String {
  let mut segs: Vec<&str> = label
    .split('/')
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .collect();
  segs.pop();
  segs.join("/")
}

fn key(label: &str) -> String {
  label
    .split('/')
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(normalize_segment)
    .collect::<Vec<_>>()
    .join("/")
}

/// Fold groups whose labels agree after normalisation into the first
/// occurrence, preserving order. System groups are never merged.
fn merge_by_key(
  groups: Vec<ProposedGroup>,
  merged: &mut usize,
) -> Vec<ProposedGroup> {
  let mut out: Vec<ProposedGroup> = Vec::with_capacity(groups.len());
  let mut seen: HashMap<String, usize> = HashMap::new();
  for g in groups {
    if is_system_label(&g.label) {
      out.push(g);
      continue;
    }
    match seen.get(&key(&g.label)) {
      Some(&pos) => {
        let target = &mut out[pos];
        target.member_indices.extend(g.member_indices);
        target.member_destinations.extend(g.member_destinations);
        target.member_notes.extend(g.member_notes);
        *merged += 1;
      }
      None => {
        seen.insert(key(&g.label), out.len());
        out.push(g);
      }
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{MemberDestination, MemberNote};

  fn g(label: &str, members: &[usize]) -> ProposedGroup {
    ProposedGroup {
      label: label.to_string(),
      rationale: format!("r:{label}"),
      member_indices: members.to_vec(),
      member_destinations: vec![],
      member_notes: vec![],
    }
  }

  fn labels(groups: &[ProposedGroup]) -> Vec<&str> {
    groups.iter().map(|g| g.label.as_str()).collect()
  }

  #[test]
  fn clean_labels_pass_through_unchanged() {
    let input = vec![
      g("Work/Acme Corp", &[0, 1]),
      g("Finance/Taxes/2023", &[2, 3]),
    ];
    let (out, n) = validate_groups(input.clone());
    assert_eq!(labels(&out), labels(&input));
    assert_eq!(out[0].member_indices, vec![0, 1]);
    assert_eq!(n, Normalisation::default());
  }

  #[test]
  fn type_word_segments_are_dropped() {
    let (out, n) = validate_groups(vec![
      g("Work/Acme Corp/PDFs", &[0, 1]),
      g("Photos/Hawaii Trip", &[2, 3]),
    ]);
    assert_eq!(
      labels(&out),
      vec!["Work/Acme Corp", "Photos/Hawaii Trip"]
    );
    assert_eq!(n.rewritten, 1);
  }

  #[test]
  fn leading_type_word_is_dropped_too() {
    let (out, _) =
      validate_groups(vec![g("Documents/Smith v Jones", &[0, 1])]);
    assert_eq!(labels(&out), vec!["Smith v Jones"]);
  }

  #[test]
  fn a_label_made_only_of_type_words_becomes_unsorted() {
    let (out, _) = validate_groups(vec![g("Misc/Files", &[0, 1])]);
    assert_eq!(labels(&out), vec!["Unsorted"]);
  }

  #[test]
  fn labels_equal_after_normalisation_merge_into_the_first() {
    let mut a = g("Work/Acme Corp", &[0, 1]);
    a.member_destinations = vec![MemberDestination {
      index: 0,
      dest_name: "a.pdf".into(),
    }];
    let mut b = g("work / acme-corp", &[2]);
    b.member_notes = vec![MemberNote {
      index: 2,
      note: "n".into(),
    }];
    let (out, n) = validate_groups(vec![a, b, g("Legal", &[3, 4])]);
    assert_eq!(labels(&out), vec!["Work/Acme Corp", "Legal"]);
    assert_eq!(out[0].member_indices, vec![0, 1, 2]);
    assert_eq!(out[0].member_destinations.len(), 1);
    assert_eq!(out[0].member_notes.len(), 1);
    assert_eq!(n.merged, 1);
  }

  #[test]
  fn lone_file_leaf_folders_collapse_into_the_parent() {
    let (out, n) = validate_groups(vec![
      g("Photos/Pets/Biscuit", &[0, 1]),
      g("Photos/Pets/Stray Cat", &[2]),
    ]);
    assert_eq!(
      labels(&out),
      vec!["Photos/Pets/Biscuit", "Photos/Pets"]
    );
    assert_eq!(out[1].member_indices, vec![2]);
    assert_eq!(n.collapsed, 1);
  }

  #[test]
  fn collapsed_singleton_merges_into_an_existing_parent_group() {
    let (out, n) = validate_groups(vec![
      g("Finance/Bills", &[0, 1]),
      g("Finance/Bills/Comcast", &[2]),
    ]);
    assert_eq!(labels(&out), vec!["Finance/Bills"]);
    assert_eq!(out[0].member_indices, vec![0, 1, 2]);
    assert_eq!(n.collapsed, 1);
    assert_eq!(n.merged, 1);
  }

  #[test]
  fn top_level_singletons_are_left_alone() {
    let (out, n) = validate_groups(vec![g("Recipes", &[0])]);
    assert_eq!(labels(&out), vec!["Recipes"]);
    assert_eq!(n.collapsed, 0);
  }

  #[test]
  fn depth_is_capped_at_three_segments() {
    let (out, n) = validate_groups(vec![g(
      "Housing/418 Maple St/Utility Bills/2024-08",
      &[0, 1],
    )]);
    assert_eq!(
      labels(&out),
      vec!["Housing/418 Maple St/Utility Bills"]
    );
    assert_eq!(n.rewritten, 1);
  }

  #[test]
  fn whitespace_and_empty_segments_are_tidied() {
    let (out, _) =
      validate_groups(vec![g("  Work //  Acme Corp  ", &[0, 1])]);
    assert_eq!(labels(&out), vec!["Work/Acme Corp"]);
  }

  #[test]
  fn system_groups_are_never_touched() {
    let (out, n) = validate_groups(vec![
      g("Needs Review", &[0]),
      g("Unsorted", &[1]),
      g("Unsorted", &[2]),
    ]);
    assert_eq!(
      labels(&out),
      vec!["Needs Review", "Unsorted", "Unsorted"]
    );
    assert_eq!(n, Normalisation::default());
  }

  #[test]
  fn empty_groups_are_dropped() {
    let (out, _) =
      validate_groups(vec![g("Work", &[]), g("Legal", &[0, 1])]);
    assert_eq!(labels(&out), vec!["Legal"]);
  }
}

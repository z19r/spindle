//! Deterministic clean-up of model-proposed group labels. The prompt
//! asks for subject-based, 1–3 level labels, but the model still emits
//! type-word folders ("PDFs"), case/punctuation variants of the same
//! label, lone-file leaf folders, and over-deep trees. Fixing those in
//! Rust is cheaper and more reliable than pleading in the prompt.

use std::collections::{HashMap, HashSet};

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

/// Sibling groups that differ only by a trailing token are merged unless
/// the result would hold more files than this.
pub const MAX_SIBLING_MERGE: usize = 40;

/// Trailing words that mark a revision of the same subject rather than a
/// different subject.
const STATUS_WORDS: &[&str] = &[
  "draft", "final", "revised", "revision", "latest", "old", "new",
];

const MONTHS: &[&str] = &[
  "jan",
  "january",
  "feb",
  "february",
  "mar",
  "march",
  "apr",
  "april",
  "may",
  "jun",
  "june",
  "jul",
  "july",
  "aug",
  "august",
  "sep",
  "sept",
  "september",
  "oct",
  "october",
  "nov",
  "november",
  "dec",
  "december",
];

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
  out = merge_siblings(out, &mut n.merged);
  out = fold_children_into_singleton_parent(out, &mut n.merged);

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

/// A lone file left at `Health/Fitness` beside `Health/Fitness/Marathon
/// Training` means the model could not narrow it, so the child's extra
/// level is not shared. Fold every group whose label extends a
/// single-member label of depth two or more into that parent, under the
/// parent's label. Depth-one parents are left alone: folding
/// `Reference/Programming/Python` into a bare `Reference` would be
/// worse. Skipped when the result would exceed [`MAX_SIBLING_MERGE`].
fn fold_children_into_singleton_parent(
  groups: Vec<ProposedGroup>,
  merged: &mut usize,
) -> Vec<ProposedGroup> {
  let keys: Vec<String> =
    groups.iter().map(|g| key(&g.label)).collect();
  // parent position → child positions
  let mut into: HashMap<usize, Vec<usize>> = HashMap::new();
  let mut taken: HashSet<usize> = HashSet::new();
  for (pos, g) in groups.iter().enumerate() {
    if g.member_indices.len() != 1
      || is_system_label(&g.label)
      || depth(&g.label) < 2
      || taken.contains(&pos)
    {
      continue;
    }
    let prefix = format!("{}/", keys[pos]);
    let children: Vec<usize> = (0..groups.len())
      .filter(|&c| {
        c != pos
          && !taken.contains(&c)
          && keys[c].starts_with(&prefix)
      })
      .collect();
    if children.is_empty() {
      continue;
    }
    let total: usize = 1
      + children
        .iter()
        .map(|&c| groups[c].member_indices.len())
        .sum::<usize>();
    if total > MAX_SIBLING_MERGE {
      continue;
    }
    taken.insert(pos);
    taken.extend(children.iter().copied());
    into.insert(pos, children);
  }

  let absorbed: HashSet<usize> =
    into.values().flatten().copied().collect();
  let mut slots: Vec<Option<ProposedGroup>> =
    groups.into_iter().map(Some).collect();
  let mut out = Vec::with_capacity(slots.len());
  for pos in 0..slots.len() {
    if absorbed.contains(&pos) {
      continue;
    }
    let Some(mut g) = slots[pos].take() else {
      continue;
    };
    if let Some(children) = into.remove(&pos) {
      for c in children {
        if let Some(child) = slots[c].take() {
          g.member_indices.extend(child.member_indices);
          g.member_destinations.extend(child.member_destinations);
          g.member_notes.extend(child.member_notes);
          *merged += 1;
        }
      }
    }
    out.push(g);
  }
  out
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

/// What a label's last segment ends with, once recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
  /// A year, year-month, year-month-day, or month + year.
  Date,
  /// v2, v1.3, rev 3, version 2.
  Version,
  /// draft, final, revised …
  Status,
}

fn is_year(w: &str) -> bool {
  w.len() == 4
    && w.chars().all(|c| c.is_ascii_digit())
    && (w.starts_with("19") || w.starts_with("20"))
}

/// YYYY, YYYY-MM, YYYY-MM-DD (also with `_`, `.` or `/` separators).
fn is_date_word(w: &str) -> bool {
  let parts: Vec<&str> = w.split(['-', '_', '.', '/']).collect();
  match parts.as_slice() {
    [y] => is_year(y),
    [y, m] => is_year(y) && is_two_digits(m),
    [y, m, d] => is_year(y) && is_two_digits(m) && is_two_digits(d),
    _ => false,
  }
}

fn is_two_digits(w: &str) -> bool {
  (1..=2).contains(&w.len()) && w.chars().all(|c| c.is_ascii_digit())
}

fn is_version_word(w: &str) -> bool {
  let rest = match w.strip_prefix('v').or_else(|| w.strip_prefix('V'))
  {
    Some(r) => r,
    None => return false,
  };
  !rest.is_empty()
    && rest
      .split('.')
      .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

fn is_number(w: &str) -> bool {
  !w.is_empty() && w.chars().all(|c| c.is_ascii_digit())
}

fn trim_punct(w: &str) -> &str {
  w.trim_matches(|c: char| {
    matches!(
      c,
      '(' | ')' | '[' | ']' | ',' | ':' | '-' | '_' | '–' | '—'
    )
  })
}

/// Split a segment into the subject and the trailing token that
/// qualifies it: `"Utilities 2024-06"` → `("Utilities", Date)`,
/// `"Report (Draft)"` → `("Report", Status)`. `None` when the segment
/// ends in an ordinary word. The subject may be empty when the segment
/// is nothing but the token.
fn strip_trailing_token(segment: &str) -> Option<(String, Token)> {
  let words: Vec<&str> = segment.split_whitespace().collect();
  let (last, before) = words.split_last()?;
  let last = trim_punct(last);
  let lower = last.to_ascii_lowercase();
  let prev =
    before.last().map(|w| trim_punct(w).to_ascii_lowercase());

  let (drop, token) = if is_date_word(&lower) {
    let month_before = prev.as_deref().is_some_and(|p| {
      MONTHS.contains(&p)
        || (p.len() == 2
          && p.starts_with('q')
          && p[1..].chars().all(|c| c.is_ascii_digit()))
        || p == "fy"
    });
    (if month_before { 2 } else { 1 }, Token::Date)
  } else if is_version_word(&lower) {
    (1, Token::Version)
  } else if is_number(&lower)
    && prev.as_deref().is_some_and(|p| {
      matches!(p, "rev" | "revision" | "version" | "ver")
    })
  {
    (2, Token::Version)
  } else if STATUS_WORDS.contains(&lower.as_str()) {
    (1, Token::Status)
  } else {
    return None;
  };

  let base = words[..words.len() - drop]
    .join(" ")
    .trim_end_matches(|c: char| {
      c.is_whitespace()
        || matches!(c, '-' | '_' | ':' | '(' | '[' | ',' | '–' | '—')
    })
    .to_string();
  Some((base, token))
}

/// The label a group would share with its siblings once the trailing
/// token of its last segment is removed. `None` for groups that must not
/// take part: a last segment that is only a date or version is a
/// deliberate sub-folder (`Finance/Taxes/2023`).
fn sibling_target(label: &str) -> Option<String> {
  let segs: Vec<&str> = label
    .split('/')
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .collect();
  let last = segs.last()?;
  match strip_trailing_token(last) {
    None => Some(label.to_string()),
    Some((base, token)) if base.is_empty() => match token {
      Token::Status if segs.len() >= 2 => Some(parent(label)),
      _ => None,
    },
    Some((base, _)) => {
      let mut head: Vec<&str> = segs[..segs.len() - 1].to_vec();
      head.push(&base);
      Some(head.join("/"))
    }
  }
}

/// Merge sibling groups whose labels differ only by a trailing date,
/// version or status token, under the label with the token removed.
/// Groups that would end up larger than [`MAX_SIBLING_MERGE`] are left
/// as they are.
fn merge_siblings(
  groups: Vec<ProposedGroup>,
  merged: &mut usize,
) -> Vec<ProposedGroup> {
  // key → (target label, positions in `groups`)
  let mut buckets: Vec<(String, Vec<usize>)> = Vec::new();
  let mut by_key: HashMap<String, usize> = HashMap::new();
  for (pos, g) in groups.iter().enumerate() {
    if is_system_label(&g.label) {
      continue;
    }
    let Some(target) = sibling_target(&g.label) else {
      continue;
    };
    let k = key(&target);
    match by_key.get(&k) {
      Some(&b) => buckets[b].1.push(pos),
      None => {
        by_key.insert(k, buckets.len());
        buckets.push((target, vec![pos]));
      }
    }
  }

  // position → (target label, positions absorbed into it)
  let mut absorb: HashMap<usize, (String, Vec<usize>)> =
    HashMap::new();
  let mut absorbed: HashMap<usize, usize> = HashMap::new();
  for (target, positions) in buckets {
    if positions.len() < 2 {
      continue;
    }
    let total: usize = positions
      .iter()
      .map(|&p| groups[p].member_indices.len())
      .sum();
    if total > MAX_SIBLING_MERGE {
      continue;
    }
    let first = positions[0];
    for &p in &positions[1..] {
      absorbed.insert(p, first);
    }
    absorb.insert(first, (target, positions[1..].to_vec()));
  }

  let mut slots: Vec<Option<ProposedGroup>> =
    groups.into_iter().map(Some).collect();
  let mut out = Vec::with_capacity(slots.len());
  for pos in 0..slots.len() {
    if absorbed.contains_key(&pos) {
      continue;
    }
    let Some(mut g) = slots[pos].take() else {
      continue;
    };
    if let Some((target, others)) = absorb.remove(&pos) {
      g.label = target;
      for o in others {
        if let Some(other) = slots[o].take() {
          g.member_indices.extend(other.member_indices);
          g.member_destinations.extend(other.member_destinations);
          g.member_notes.extend(other.member_notes);
          *merged += 1;
        }
      }
    }
    out.push(g);
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
  fn strip_trailing_token_recognises_dates_versions_and_status() {
    let cases: &[(&str, Option<(&str, Token)>)] = &[
      ("Utilities 2024-06", Some(("Utilities", Token::Date))),
      ("Utilities June 2024", Some(("Utilities", Token::Date))),
      ("Utilities Q3 2024", Some(("Utilities", Token::Date))),
      ("Returns 2023", Some(("Returns", Token::Date))),
      ("2023", Some(("", Token::Date))),
      ("Logo v2", Some(("Logo", Token::Version))),
      ("Logo v1.3", Some(("Logo", Token::Version))),
      ("Logo rev 3", Some(("Logo", Token::Version))),
      ("Report (Draft)", Some(("Report", Token::Status))),
      ("Report - Final", Some(("Report", Token::Status))),
      ("Draft", Some(("", Token::Status))),
      ("Acme Corp", None),
      ("Room 101", None),
      ("iPhone 15", None),
    ];
    for (input, want) in cases {
      let got = strip_trailing_token(input);
      let want = want.map(|(b, t)| (b.to_string(), t));
      assert_eq!(got, want, "{input}");
    }
  }

  #[test]
  fn months_of_one_subject_merge_under_the_bare_label() {
    let input = vec![
      g("Finance/Bills/Utilities 2024-06", &[0, 1]),
      g("Finance/Bills/Utilities 2024-07", &[2, 3]),
      g("Finance/Bills/Utilities 2024-08", &[4]),
    ];
    let (out, n) = validate_groups(input);
    assert_eq!(labels(&out), vec!["Finance/Bills/Utilities"]);
    assert_eq!(out[0].member_indices, vec![0, 1, 2, 3, 4]);
    assert_eq!(n.merged, 2);
  }

  #[test]
  fn tax_year_folders_stay_separate() {
    let input = vec![
      g("Finance/Taxes/2022", &[0, 1]),
      g("Finance/Taxes/2023", &[2, 3]),
      g("Design/Logo/v1", &[4, 5]),
      g("Design/Logo/v2", &[6, 7]),
    ];
    let (out, n) = validate_groups(input.clone());
    assert_eq!(labels(&out), labels(&input));
    assert_eq!(n.merged, 0);
  }

  #[test]
  fn draft_and_final_siblings_merge_into_the_subject() {
    let input = vec![
      g("Work/Acme Q1 Report/Draft", &[0, 1]),
      g("Work/Acme Q1 Report/Final", &[2, 3]),
      g("Design/Logo v1", &[4, 5]),
      g("Design/Logo v2", &[6]),
    ];
    let (out, _) = validate_groups(input);
    assert_eq!(
      labels(&out),
      vec!["Work/Acme Q1 Report", "Design/Logo"]
    );
    assert_eq!(out[0].member_indices, vec![0, 1, 2, 3]);
    assert_eq!(out[1].member_indices, vec![4, 5, 6]);
  }

  #[test]
  fn a_tokened_sibling_joins_the_existing_bare_group() {
    let input = vec![
      g("Finance/Bills/Utilities", &[0, 1]),
      g("Finance/Bills/Utilities 2024-08", &[2, 3]),
    ];
    let (out, n) = validate_groups(input);
    assert_eq!(labels(&out), vec!["Finance/Bills/Utilities"]);
    assert_eq!(out[0].member_indices, vec![0, 1, 2, 3]);
    assert_eq!(n.merged, 1);
  }

  #[test]
  fn a_lone_tokened_label_is_not_renamed() {
    let input = vec![g("Design/Logo v2", &[0, 1])];
    let (out, n) = validate_groups(input);
    assert_eq!(labels(&out), vec!["Design/Logo v2"]);
    assert_eq!(n.merged, 0);
  }

  #[test]
  fn sibling_merge_respects_the_size_cap() {
    let big: Vec<usize> = (0..30).collect();
    let more: Vec<usize> = (30..45).collect();
    let input = vec![
      g("Finance/Bills/Utilities 2024-06", &big),
      g("Finance/Bills/Utilities 2024-07", &more),
    ];
    let (out, n) = validate_groups(input.clone());
    assert_eq!(labels(&out), labels(&input));
    assert_eq!(n.merged, 0);
  }

  #[test]
  fn a_child_folds_into_its_singleton_parent() {
    let input = vec![
      g("Health/Fitness", &[0]),
      g("Health/Fitness/Marathon Training 2024", &[1, 2]),
      g("Health/Medical Records", &[3, 4]),
    ];
    let (out, n) = validate_groups(input);
    assert_eq!(
      labels(&out),
      vec!["Health/Fitness", "Health/Medical Records"]
    );
    assert_eq!(out[0].member_indices, vec![0, 1, 2]);
    assert_eq!(n.merged, 1);
    assert_eq!(n.collapsed, 0);
  }

  #[test]
  fn a_bare_area_singleton_does_not_swallow_its_children() {
    let input = vec![
      g("Reference", &[0]),
      g("Reference/Programming/Python", &[1, 2]),
      g("Reference/Programming/Rust", &[3, 4]),
    ];
    let (out, n) = validate_groups(input.clone());
    assert_eq!(labels(&out), labels(&input));
    assert_eq!(n.merged, 0);
  }

  #[test]
  fn a_parent_with_two_files_keeps_its_children_separate() {
    let input = vec![
      g("Health/Fitness", &[0, 5]),
      g("Health/Fitness/Marathon Training 2024", &[1, 2]),
    ];
    let (out, n) = validate_groups(input.clone());
    assert_eq!(labels(&out), labels(&input));
    assert_eq!(n.merged, 0);
  }

  #[test]
  fn folding_into_a_singleton_parent_respects_the_size_cap() {
    let many: Vec<usize> = (1..=40).collect();
    let input = vec![
      g("Health/Fitness", &[0]),
      g("Health/Fitness/Marathon Training 2024", &many),
    ];
    let (out, n) = validate_groups(input);
    // Not folded; the lone parent then collapses a level as usual.
    assert_eq!(
      labels(&out),
      vec!["Health", "Health/Fitness/Marathon Training 2024"]
    );
    assert_eq!(n.merged, 0);
    assert_eq!(n.collapsed, 1);
  }

  #[test]
  fn empty_groups_are_dropped() {
    let (out, _) =
      validate_groups(vec![g("Work", &[]), g("Legal", &[0, 1])]);
    assert_eq!(labels(&out), vec!["Legal"]);
  }
}

//! Ranking the other folders a file could plausibly have gone into.
//!
//! The review pane shows this as ALSO FITS, and pressing a number key
//! moves the file there. The list is derived here from the tags the
//! analysis stage already produced, rather than asked of the model
//! (#127).
//!
//! It lives beside the grouping code rather than inside the TUI's
//! `ReviewState` because whether the tag-derived list is good enough
//! is a question the eval has to answer, and the eval has no review
//! state to ask. The inputs below are what the ranking actually needs:
//! a file's description, and per candidate folder its label and its
//! members' descriptions.

use std::collections::{HashMap, HashSet};

use crate::model::ContentDescription;

/// Bonus for a folder whose members mostly carry the file's own
/// suggested category. Tag overlap alone treats `Finance/Taxes/2023`
/// and `Work/Acme Corp/Expenses` as equally good homes for a receipt;
/// the category is the cheap tiebreak that separates them.
const CATEGORY_BONUS: f64 = 0.25;

/// One folder the file could be moved to, as the ranking sees it.
pub struct Candidate<'a> {
  pub label: &'a str,
  /// Descriptions of the files already in the folder. A folder whose
  /// members were never described shares no tags and drops out.
  pub members: Vec<&'a ContentDescription>,
}

/// A candidate that qualified, best first.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
  /// Index into the `candidates` slice handed to [`rank`]. Callers
  /// keep their own mapping back to whatever a folder is to them.
  pub index: usize,
  pub score: f64,
  /// Tags the file and the folder's members have in common, sorted.
  /// This is what the pane shows as the reason for the suggestion.
  pub shared_tags: Vec<String>,
}

/// Up to `max` folders the file could join, best first.
///
/// The score is the Jaccard overlap of the file's tags with the union
/// of its members' tags, plus [`CATEGORY_BONUS`] when the folder's
/// dominant suggested category is the file's own.
///
/// A folder that shares no tag at all is not a candidate. The list is
/// meant to be short and plausible — padding it out with the
/// least-bad remaining folder costs the reviewer more attention than
/// showing nothing does.
///
/// Ties break on label, so the same run always offers the same list.
pub fn rank(
  file: &ContentDescription,
  candidates: &[Candidate<'_>],
  max: usize,
) -> Vec<Ranked> {
  let file_tags: HashSet<String> =
    file.tags.iter().map(|t| t.to_lowercase()).collect();
  if file_tags.is_empty() {
    return Vec::new();
  }

  let mut scored: Vec<Ranked> = Vec::new();
  for (index, candidate) in candidates.iter().enumerate() {
    if candidate.members.is_empty() {
      continue;
    }
    let mut group_tags: HashSet<String> = HashSet::new();
    let mut categories: HashMap<&str, usize> = HashMap::new();
    for d in &candidate.members {
      group_tags.extend(d.tags.iter().map(|t| t.to_lowercase()));
      *categories
        .entry(d.suggested_category.as_str())
        .or_default() += 1;
    }

    let mut shared_tags: Vec<String> =
      file_tags.intersection(&group_tags).cloned().collect();
    if shared_tags.is_empty() {
      continue;
    }
    shared_tags.sort();

    let union = file_tags.union(&group_tags).count() as f64;
    let mut score = shared_tags.len() as f64 / union;
    let dominant =
      categories.iter().max_by_key(|(_, n)| **n).map(|(c, _)| *c);
    if dominant == Some(file.suggested_category.as_str()) {
      score += CATEGORY_BONUS;
    }
    scored.push(Ranked {
      index,
      score,
      shared_tags,
    });
  }

  scored.sort_by(|a, b| {
    b.score
      .partial_cmp(&a.score)
      .unwrap_or(std::cmp::Ordering::Equal)
      .then_with(|| {
        candidates[a.index].label.cmp(candidates[b.index].label)
      })
  });
  scored.truncate(max);
  scored
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::DescriptionSource;

  fn desc(category: &str, tags: &[&str]) -> ContentDescription {
    ContentDescription {
      summary: String::new(),
      tags: tags.iter().map(|t| t.to_string()).collect(),
      suggested_category: category.to_string(),
      confidence: 0.9,
      source: DescriptionSource::Ai,
    }
  }

  fn candidate<'a>(
    label: &'a str,
    members: &'a [ContentDescription],
  ) -> Candidate<'a> {
    Candidate {
      label,
      members: members.iter().collect(),
    }
  }

  #[test]
  fn a_folder_sharing_no_tag_is_not_offered() {
    let file = desc("finance", &["invoice", "acme"]);
    let pets = [desc("personal", &["dog", "vet"])];
    let got = rank(&file, &[candidate("Personal/Pets", &pets)], 3);
    assert!(got.is_empty());
  }

  #[test]
  fn shared_tags_are_reported_sorted() {
    let file = desc("finance", &["Invoice", "ACME", "2024"]);
    let bills = [desc("finance", &["acme", "invoice", "paid"])];
    let got = rank(&file, &[candidate("Finance/Bills", &bills)], 3);
    assert_eq!(got[0].shared_tags, vec!["acme", "invoice"]);
  }

  #[test]
  fn the_better_overlap_ranks_first() {
    let file = desc("finance", &["invoice", "acme", "2024"]);
    let close = [desc("finance", &["invoice", "acme", "2024"])];
    let far = [desc("finance", &["invoice", "utilities", "gas"])];
    let got = rank(
      &file,
      &[candidate("Far", &far), candidate("Close", &close)],
      3,
    );
    assert_eq!(got[0].index, 1, "the exact tag match should win");
    assert_eq!(got[1].index, 0);
  }

  #[test]
  fn a_matching_category_breaks_an_equal_overlap() {
    let file = desc("finance", &["invoice"]);
    let money = [desc("finance", &["invoice"])];
    let other = [desc("personal", &["invoice"])];
    let got = rank(
      &file,
      &[
        candidate("Personal/Other", &other),
        candidate("Money", &money),
      ],
      3,
    );
    assert_eq!(got[0].index, 1);
    assert!(
      (got[0].score - got[1].score - CATEGORY_BONUS).abs() < 1e-9
    );
  }

  #[test]
  fn equal_scores_break_on_label_not_input_order() {
    let file = desc("finance", &["invoice"]);
    let a = [desc("finance", &["invoice"])];
    let b = [desc("finance", &["invoice"])];
    let got = rank(
      &file,
      &[candidate("Zebra", &a), candidate("Alpha", &b)],
      3,
    );
    assert_eq!(got[0].index, 1, "Alpha sorts before Zebra");
  }

  #[test]
  fn at_most_max_are_returned() {
    let file = desc("finance", &["invoice"]);
    let members = [desc("finance", &["invoice"])];
    let labels = ["A", "B", "C", "D", "E"];
    let candidates: Vec<Candidate> =
      labels.iter().map(|l| candidate(l, &members)).collect();
    assert_eq!(rank(&file, &candidates, 3).len(), 3);
    assert_eq!(rank(&file, &candidates, 0).len(), 0);
  }

  #[test]
  fn an_untagged_file_has_no_alternatives() {
    let file = desc("finance", &[]);
    let members = [desc("finance", &["invoice"])];
    assert!(
      rank(&file, &[candidate("Money", &members)], 3).is_empty()
    );
  }

  #[test]
  fn an_empty_folder_is_not_offered() {
    let file = desc("finance", &["invoice"]);
    let got = rank(
      &file,
      &[Candidate {
        label: "Empty",
        members: vec![],
      }],
      3,
    );
    assert!(got.is_empty());
  }

  #[test]
  fn tag_case_does_not_matter() {
    let file = desc("finance", &["Invoice"]);
    let members = [desc("finance", &["INVOICE"])];
    let got = rank(&file, &[candidate("Money", &members)], 3);
    assert_eq!(got[0].shared_tags, vec!["invoice"]);
  }
}

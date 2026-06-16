use crate::model::{FileGroup, ProposedGroup};

pub fn build_groups(
  proposed: &[ProposedGroup],
  output_dir: &std::path::Path,
) -> Vec<FileGroup> {
  proposed
    .iter()
    .enumerate()
    .map(|(id, p)| {
      let folder_name = sanitize_folder_name(&p.label);
      FileGroup {
        id,
        label: p.label.clone(),
        rationale: p.rationale.clone(),
        members: p.member_indices.clone(),
        member_destinations: p.member_destinations.clone(),
        suggested_path: output_dir.join(&folder_name),
      }
    })
    .collect()
}

/// Turn a (possibly hierarchical) group label into a relative folder path.
/// `/` and `\` delimit nested directories: each segment is sanitized
/// independently and rejoined with `/`, so "Work/Acme Corp/Website Redesign"
/// becomes "work/acme_corp/website_redesign" — a real directory tree.
/// Empty segments and traversal (".", "..") are dropped, so the result can
/// never escape the output directory.
fn sanitize_folder_name(label: &str) -> String {
  let segments: Vec<String> = label
    .replace('\\', "/")
    .split('/')
    .map(sanitize_segment)
    .filter(|s| !s.is_empty())
    .collect();

  if segments.is_empty() {
    "ungrouped".to_string()
  } else {
    segments.join("/")
  }
}

/// Sanitize a single path segment: lowercase alphanumerics, keep hyphens,
/// collapse everything else (including `.`) to single underscores. Returns
/// an empty string for segments with no usable characters (e.g. "." or "..").
fn sanitize_segment(segment: &str) -> String {
  let mut result = String::with_capacity(segment.len());
  let mut last_was_separator = true;

  for c in segment.chars() {
    if c.is_alphanumeric() {
      result.push(c.to_ascii_lowercase());
      last_was_separator = false;
    } else if c == '-' {
      result.push('-');
      last_was_separator = false;
    } else if !last_was_separator {
      result.push('_');
      last_was_separator = true;
    }
  }

  result.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::{Path, PathBuf};

  fn make_proposed(
    label: &str,
    members: Vec<usize>,
  ) -> ProposedGroup {
    ProposedGroup {
      label: label.to_string(),
      rationale: format!("Test rationale for {label}"),
      member_indices: members,
      member_destinations: vec![],
    }
  }

  #[test]
  fn builds_groups_from_proposals() {
    let proposed = vec![
      make_proposed("Beach Photos", vec![0, 1, 2]),
      make_proposed("Screenshots", vec![3, 4]),
    ];
    let output = Path::new("/organized");

    let groups = build_groups(&proposed, output);

    assert_eq!(groups.len(), 2);
  }

  #[test]
  fn assigns_sequential_ids() {
    let proposed = vec![
      make_proposed("A", vec![0]),
      make_proposed("B", vec![1]),
      make_proposed("C", vec![2]),
    ];
    let output = Path::new("/out");

    let groups = build_groups(&proposed, output);

    assert_eq!(groups[0].id, 0);
    assert_eq!(groups[1].id, 1);
    assert_eq!(groups[2].id, 2);
  }

  #[test]
  fn sanitizes_folder_name_to_lowercase() {
    let proposed = vec![make_proposed("Beach Photos", vec![0])];
    let output = Path::new("/out");

    let groups = build_groups(&proposed, output);

    assert_eq!(
      groups[0].suggested_path,
      PathBuf::from("/out/beach_photos")
    );
  }

  #[test]
  fn replaces_special_chars_in_folder_name() {
    let proposed = vec![make_proposed("My Photos! (2024)", vec![0])];
    let output = Path::new("/out");

    let groups = build_groups(&proposed, output);

    assert_eq!(
      groups[0].suggested_path,
      PathBuf::from("/out/my_photos_2024")
    );
  }

  #[test]
  fn empty_label_becomes_ungrouped() {
    let proposed = vec![make_proposed("   ", vec![0])];
    let output = Path::new("/out");

    let groups = build_groups(&proposed, output);

    assert_eq!(
      groups[0].suggested_path,
      PathBuf::from("/out/ungrouped")
    );
  }

  #[test]
  fn preserves_member_indices() {
    let proposed = vec![make_proposed("Test", vec![5, 10, 15])];
    let output = Path::new("/out");

    let groups = build_groups(&proposed, output);

    assert_eq!(groups[0].members, vec![5, 10, 15]);
  }

  #[test]
  fn preserves_label_and_rationale() {
    let proposed = vec![ProposedGroup {
      label: "Beach Vacation".to_string(),
      rationale: "Similar beach scenes".to_string(),
      member_indices: vec![0],
      member_destinations: vec![],
    }];
    let output = Path::new("/out");

    let groups = build_groups(&proposed, output);

    assert_eq!(groups[0].label, "Beach Vacation");
    assert_eq!(groups[0].rationale, "Similar beach scenes");
  }

  #[test]
  fn empty_proposals_produce_empty_groups() {
    let groups = build_groups(&[], Path::new("/out"));

    assert!(groups.is_empty());
  }

  #[test]
  fn sanitize_strips_leading_trailing_underscores() {
    let result = sanitize_folder_name("__test__");

    assert_eq!(result, "test");
  }

  #[test]
  fn sanitize_preserves_hyphens() {
    let result = sanitize_folder_name("my-photos");

    assert_eq!(result, "my-photos");
  }

  #[test]
  fn sanitize_builds_nested_path_from_hierarchical_label() {
    let result =
      sanitize_folder_name("Work/Acme Corp/Website Redesign");

    assert_eq!(result, "work/acme_corp/website_redesign");
  }

  #[test]
  fn build_groups_creates_nested_suggested_path() {
    let proposed =
      vec![make_proposed("Photos/2023/Hawaii Trip", vec![0, 1])];
    let output = Path::new("/organized");

    let groups = build_groups(&proposed, output);

    assert_eq!(
      groups[0].suggested_path,
      PathBuf::from("/organized/photos/2023/hawaii_trip")
    );
  }

  #[test]
  fn sanitize_drops_empty_segments_and_traversal() {
    // Leading/duplicate slashes and "." / ".." must not escape or
    // produce empty directory levels.
    assert_eq!(sanitize_folder_name("/Work//Acme/"), "work/acme");
    assert_eq!(
      sanitize_folder_name("../../etc/passwd"),
      "etc/passwd"
    );
    assert_eq!(sanitize_folder_name("a\\b\\c"), "a/b/c");
  }

  #[test]
  fn sanitize_all_traversal_becomes_ungrouped() {
    assert_eq!(sanitize_folder_name("../.."), "ungrouped");
  }
}

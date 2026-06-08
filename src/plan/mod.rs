use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::model::{
  DuplicateSet, FileGroup, FileMove, FingerprintedFile, PlanStats,
  ReorgPlan,
};

pub fn propose_plan(
  output_dir: &Path,
  groups: &[FileGroup],
  duplicates: &[DuplicateSet],
  files: &[FingerprintedFile],
) -> ReorgPlan {
  let mut moves = Vec::new();

  for group in groups {
    let dest_dir = output_dir.join(&group.suggested_path);
    let mut used_names: HashSet<String> = HashSet::new();

    let dest_lookup: HashMap<usize, &str> = group
      .member_destinations
      .iter()
      .map(|m| (m.index, m.dest_name.as_str()))
      .collect();

    for &member_idx in &group.members {
      if let Some(file) = files.get(member_idx) {
        let raw_dest =
          if let Some(ai_dest) = dest_lookup.get(&member_idx) {
            sanitize_dest_path(ai_dest)
          } else {
            file
              .scanned
              .path
              .file_name()
              .unwrap_or_default()
              .to_string_lossy()
              .to_string()
          };

        let dest_name = deduplicate_name(&raw_dest, &used_names);
        used_names.insert(dest_name.clone());

        moves.push(FileMove {
          from: file.scanned.path.clone(),
          to: dest_dir.join(&dest_name),
          group_id: group.id,
        });
      }
    }
  }

  let space_to_reclaim = compute_space_to_reclaim(duplicates, files);

  let stats = PlanStats {
    total_files: files.len(),
    groups_created: groups.len(),
    duplicates_found: duplicates
      .iter()
      .map(|d| d.duplicates.len())
      .sum(),
    space_to_reclaim,
  };

  ReorgPlan {
    groups: groups.to_vec(),
    duplicates: duplicates.to_vec(),
    moves,
    stats,
  }
}

fn sanitize_dest_path(dest: &str) -> String {
  dest
    .replace('\\', "/")
    .split('/')
    .filter(|seg| !seg.is_empty() && *seg != "." && *seg != "..")
    .collect::<Vec<_>>()
    .join("/")
}

fn deduplicate_name(name: &str, used: &HashSet<String>) -> String {
  if !used.contains(name) {
    return name.to_string();
  }

  let (prefix, filename) = match name.rfind('/') {
    Some(pos) => (&name[..=pos], &name[pos + 1..]),
    None => ("", name),
  };

  let (stem, ext) = match filename.rfind('.') {
    Some(pos) if pos > 0 => {
      (&filename[..pos], Some(&filename[pos..]))
    }
    _ => (filename, None),
  };

  let suffix = ext.unwrap_or("");
  for n in 1.. {
    let candidate = format!("{prefix}{stem}_{n}{suffix}");
    if !used.contains(&candidate) {
      return candidate;
    }
  }

  unreachable!()
}

pub fn compute_space_to_reclaim(
  duplicates: &[DuplicateSet],
  files: &[FingerprintedFile],
) -> u64 {
  duplicates
    .iter()
    .flat_map(|set| &set.duplicates)
    .filter_map(|&idx| files.get(idx))
    .map(|f| f.scanned.size)
    .sum()
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;
  use std::time::SystemTime;

  use crate::model::{
    DuplicateType, FileType, ImageFormat, MemberDestination,
    ScannedFile,
  };

  fn make_fingerprinted(name: &str, size: u64) -> FingerprintedFile {
    FingerprintedFile {
      scanned: ScannedFile {
        path: PathBuf::from(format!("/downloads/{name}")),
        scan_root: PathBuf::from("/downloads"),
        size,
        modified: SystemTime::now(),
        file_type: FileType::Image(ImageFormat::Jpg),
      },
      blake3_hash: [0u8; 32],
      perceptual_hash: None,
    }
  }

  fn make_group(
    id: usize,
    label: &str,
    members: Vec<usize>,
  ) -> FileGroup {
    FileGroup {
      id,
      label: label.to_string(),
      rationale: format!("test group {id}"),
      members,
      member_destinations: vec![],
      suggested_path: PathBuf::from(
        label.to_lowercase().replace(' ', "_"),
      ),
    }
  }

  #[test]
  fn creates_moves_for_grouped_files() {
    let files = vec![
      make_fingerprinted("beach1.jpg", 100),
      make_fingerprinted("beach2.jpg", 200),
      make_fingerprinted("cat.jpg", 150),
    ];
    let groups = vec![make_group(0, "Beach Photos", vec![0, 1])];
    let output = Path::new("/organized");

    let plan = propose_plan(output, &groups, &[], &files);

    assert_eq!(plan.moves.len(), 2);
  }

  #[test]
  fn preserves_original_filename_in_destination() {
    let files = vec![make_fingerprinted("sunset.jpg", 100)];
    let groups = vec![make_group(0, "Sunsets", vec![0])];
    let output = Path::new("/organized");

    let plan = propose_plan(output, &groups, &[], &files);

    assert_eq!(
      plan.moves[0].to,
      PathBuf::from("/organized/sunsets/sunset.jpg")
    );
  }

  #[test]
  fn sets_correct_from_path() {
    let files = vec![make_fingerprinted("photo.jpg", 100)];
    let groups = vec![make_group(0, "Photos", vec![0])];
    let output = Path::new("/organized");

    let plan = propose_plan(output, &groups, &[], &files);

    assert_eq!(
      plan.moves[0].from,
      PathBuf::from("/downloads/photo.jpg")
    );
  }

  #[test]
  fn includes_stats_in_plan() {
    let files = vec![
      make_fingerprinted("a.jpg", 100),
      make_fingerprinted("b.jpg", 200),
      make_fingerprinted("c.jpg", 300),
    ];
    let groups = vec![make_group(0, "All", vec![0, 1, 2])];
    let dupes = vec![DuplicateSet {
      canonical: 0,
      duplicates: vec![1],
      duplicate_type: DuplicateType::Exact,
    }];
    let output = Path::new("/organized");

    let plan = propose_plan(output, &groups, &dupes, &files);

    assert_eq!(plan.stats.total_files, 3);
    assert_eq!(plan.stats.groups_created, 1);
    assert_eq!(plan.stats.duplicates_found, 1);
  }

  #[test]
  fn assigns_group_id_to_moves() {
    let files = vec![
      make_fingerprinted("a.jpg", 100),
      make_fingerprinted("b.jpg", 200),
    ];
    let groups = vec![
      make_group(0, "Group A", vec![0]),
      make_group(1, "Group B", vec![1]),
    ];
    let output = Path::new("/organized");

    let plan = propose_plan(output, &groups, &[], &files);

    assert_eq!(plan.moves[0].group_id, 0);
    assert_eq!(plan.moves[1].group_id, 1);
  }

  #[test]
  fn empty_groups_produce_no_moves() {
    let files = vec![make_fingerprinted("a.jpg", 100)];
    let groups: Vec<FileGroup> = vec![];
    let output = Path::new("/organized");

    let plan = propose_plan(output, &groups, &[], &files);

    assert!(plan.moves.is_empty());
    assert_eq!(plan.stats.groups_created, 0);
  }

  #[test]
  fn space_reclaim_sums_duplicate_file_sizes() {
    let files = vec![
      make_fingerprinted("original.jpg", 1000),
      make_fingerprinted("copy1.jpg", 1000),
      make_fingerprinted("copy2.jpg", 1000),
    ];
    let dupes = vec![DuplicateSet {
      canonical: 0,
      duplicates: vec![1, 2],
      duplicate_type: DuplicateType::Exact,
    }];

    let space = compute_space_to_reclaim(&dupes, &files);

    assert_eq!(space, 2000);
  }

  #[test]
  fn zero_space_when_no_duplicates() {
    let files = vec![make_fingerprinted("unique.jpg", 500)];

    let space = compute_space_to_reclaim(&[], &files);

    assert_eq!(space, 0);
  }

  #[test]
  fn skips_out_of_bounds_member_indices() {
    let files = vec![make_fingerprinted("a.jpg", 100)];
    let groups = vec![make_group(0, "Bad", vec![0, 5, 99])];
    let output = Path::new("/organized");

    let plan = propose_plan(output, &groups, &[], &files);

    assert_eq!(plan.moves.len(), 1);
  }

  #[test]
  fn deduplicates_colliding_filenames() {
    let files = vec![
      make_fingerprinted("image.jpg", 100),
      make_fingerprinted("image.jpg", 200),
      make_fingerprinted("image.jpg", 300),
    ];
    let groups = vec![make_group(0, "Cats", vec![0, 1, 2])];
    let output = Path::new("/organized");

    let plan = propose_plan(output, &groups, &[], &files);

    assert_eq!(
      plan.moves[0].to,
      PathBuf::from("/organized/cats/image.jpg")
    );
    assert_eq!(
      plan.moves[1].to,
      PathBuf::from("/organized/cats/image_1.jpg")
    );
    assert_eq!(
      plan.moves[2].to,
      PathBuf::from("/organized/cats/image_2.jpg")
    );
  }

  #[test]
  fn deduplicates_extensionless_files() {
    let files = vec![
      make_fingerprinted("Makefile", 50),
      make_fingerprinted("Makefile", 60),
    ];
    let groups = vec![make_group(0, "Build", vec![0, 1])];
    let output = Path::new("/organized");

    let plan = propose_plan(output, &groups, &[], &files);

    assert_eq!(
      plan.moves[0].to,
      PathBuf::from("/organized/build/Makefile")
    );
    assert_eq!(
      plan.moves[1].to,
      PathBuf::from("/organized/build/Makefile_1")
    );
  }

  #[test]
  fn no_collision_when_names_differ() {
    let files = vec![
      make_fingerprinted("cat.jpg", 100),
      make_fingerprinted("dog.jpg", 200),
    ];
    let groups = vec![make_group(0, "Pets", vec![0, 1])];
    let output = Path::new("/organized");

    let plan = propose_plan(output, &groups, &[], &files);

    assert_eq!(
      plan.moves[0].to,
      PathBuf::from("/organized/pets/cat.jpg")
    );
    assert_eq!(
      plan.moves[1].to,
      PathBuf::from("/organized/pets/dog.jpg")
    );
  }

  #[test]
  fn deduplicate_name_unit() {
    let mut used = HashSet::new();

    assert_eq!(deduplicate_name("photo.jpg", &used), "photo.jpg");
    used.insert("photo.jpg".to_string());

    assert_eq!(deduplicate_name("photo.jpg", &used), "photo_1.jpg");
    used.insert("photo_1.jpg".to_string());

    assert_eq!(deduplicate_name("photo.jpg", &used), "photo_2.jpg");
  }

  #[test]
  fn deduplicate_name_no_extension() {
    let mut used = HashSet::new();
    used.insert("README".to_string());

    assert_eq!(deduplicate_name("README", &used), "README_1");
  }

  #[test]
  fn collisions_scoped_per_group() {
    let files = vec![
      make_fingerprinted("pic.jpg", 100),
      make_fingerprinted("pic.jpg", 200),
    ];
    let groups = vec![
      make_group(0, "Group A", vec![0]),
      make_group(1, "Group B", vec![1]),
    ];
    let output = Path::new("/out");

    let plan = propose_plan(output, &groups, &[], &files);

    assert_eq!(
      plan.moves[0].to,
      PathBuf::from("/out/group_a/pic.jpg")
    );
    assert_eq!(
      plan.moves[1].to,
      PathBuf::from("/out/group_b/pic.jpg")
    );
  }

  #[test]
  fn multiple_groups_produce_correct_destinations() {
    let files = vec![
      make_fingerprinted("beach.jpg", 100),
      make_fingerprinted("mountain.jpg", 200),
    ];
    let groups = vec![
      make_group(0, "Beach", vec![0]),
      make_group(1, "Mountains", vec![1]),
    ];
    let output = Path::new("/out");

    let plan = propose_plan(output, &groups, &[], &files);

    assert_eq!(
      plan.moves[0].to,
      PathBuf::from("/out/beach/beach.jpg")
    );
    assert_eq!(
      plan.moves[1].to,
      PathBuf::from("/out/mountains/mountain.jpg")
    );
  }

  fn make_group_with_dests(
    id: usize,
    label: &str,
    members: Vec<usize>,
    dests: Vec<MemberDestination>,
  ) -> FileGroup {
    FileGroup {
      id,
      label: label.to_string(),
      rationale: format!("test group {id}"),
      members,
      member_destinations: dests,
      suggested_path: PathBuf::from(
        label.to_lowercase().replace(' ', "_"),
      ),
    }
  }

  #[test]
  fn uses_ai_dest_name_when_provided() {
    let files = vec![make_fingerprinted("image3.jpg", 100)];
    let dests = vec![MemberDestination {
      index: 0,
      dest_name: "cute_cat.jpg".to_string(),
    }];
    let groups =
      vec![make_group_with_dests(0, "Cats", vec![0], dests)];
    let output = Path::new("/organized");

    let plan = propose_plan(output, &groups, &[], &files);

    assert_eq!(
      plan.moves[0].to,
      PathBuf::from("/organized/cats/cute_cat.jpg")
    );
  }

  #[test]
  fn preserves_subdirs_in_ai_dest_name() {
    let files = vec![make_fingerprinted("nude.jpg", 100)];
    let dests = vec![MemberDestination {
      index: 0,
      dest_name: "porn/nude.jpg".to_string(),
    }];
    let groups =
      vec![make_group_with_dests(0, "Nudes", vec![0], dests)];
    let output = Path::new("/organized");

    let plan = propose_plan(output, &groups, &[], &files);

    assert_eq!(
      plan.moves[0].to,
      PathBuf::from("/organized/nudes/porn/nude.jpg")
    );
  }

  #[test]
  fn falls_back_to_bare_filename_without_ai_dest() {
    let files = vec![make_fingerprinted("photo.jpg", 100)];
    let groups = vec![make_group(0, "Photos", vec![0])];
    let output = Path::new("/organized");

    let plan = propose_plan(output, &groups, &[], &files);

    assert_eq!(
      plan.moves[0].to,
      PathBuf::from("/organized/photos/photo.jpg")
    );
  }

  #[test]
  fn deduplicates_within_subdirs() {
    let files = vec![
      make_fingerprinted("nude.jpg", 100),
      make_fingerprinted("nude.jpg", 200),
    ];
    let dests = vec![
      MemberDestination {
        index: 0,
        dest_name: "porn/nude.jpg".to_string(),
      },
      MemberDestination {
        index: 1,
        dest_name: "porn/nude.jpg".to_string(),
      },
    ];
    let groups =
      vec![make_group_with_dests(0, "Nudes", vec![0, 1], dests)];
    let output = Path::new("/organized");

    let plan = propose_plan(output, &groups, &[], &files);

    assert_eq!(
      plan.moves[0].to,
      PathBuf::from("/organized/nudes/porn/nude.jpg")
    );
    assert_eq!(
      plan.moves[1].to,
      PathBuf::from("/organized/nudes/porn/nude_1.jpg")
    );
  }

  #[test]
  fn sanitize_strips_traversal() {
    assert_eq!(
      sanitize_dest_path("../../../etc/passwd"),
      "etc/passwd"
    );
    assert_eq!(sanitize_dest_path("./foo/./bar.jpg"), "foo/bar.jpg");
    assert_eq!(
      sanitize_dest_path("normal/path.jpg"),
      "normal/path.jpg"
    );
    assert_eq!(sanitize_dest_path("file.jpg"), "file.jpg");
    assert_eq!(sanitize_dest_path("a\\b\\c.jpg"), "a/b/c.jpg");
  }

  #[test]
  fn sanitize_handles_empty_segments() {
    assert_eq!(sanitize_dest_path("a//b///c.jpg"), "a/b/c.jpg");
  }

  #[test]
  fn deduplicate_name_with_subpath() {
    let mut used = HashSet::new();
    used.insert("porn/nude.jpg".to_string());

    assert_eq!(
      deduplicate_name("porn/nude.jpg", &used),
      "porn/nude_1.jpg"
    );
  }

  #[test]
  fn deduplicate_name_preserves_prefix_on_collision() {
    let mut used = HashSet::new();
    used.insert("deep/nested/file.txt".to_string());
    used.insert("deep/nested/file_1.txt".to_string());

    assert_eq!(
      deduplicate_name("deep/nested/file.txt", &used),
      "deep/nested/file_2.txt"
    );
  }
}

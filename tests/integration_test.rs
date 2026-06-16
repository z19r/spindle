use std::fs;
use std::path::Path;

use anyhow::Result;
use tempfile::TempDir;

use spindle::ai::{AiProvider, DescribeContext};
use spindle::analyze::{analyze_batch, analyze_file, AnalyzeOptions};
use spindle::executor::{execute_plan, undo};
use spindle::fingerprint::{
  compute_blake3, find_exact_duplicates, find_near_duplicates,
  fingerprint_files,
};
use spindle::group::build_groups;
use spindle::model::*;
use spindle::plan::propose_plan;
use spindle::scanner::{scan_directories, scan_directory};

fn create_test_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
  use image::{ImageBuffer, RgbaImage};
  let img: RgbaImage =
    ImageBuffer::from_raw(width, height, rgba.to_vec()).unwrap();
  let mut buf = Vec::new();
  let mut cursor = std::io::Cursor::new(&mut buf);
  img.write_to(&mut cursor, image::ImageFormat::Png).unwrap();
  buf
}

fn populate_test_dir(dir: &Path) {
  let red_pixel = create_test_png(1, 1, &[255, 0, 0, 255]);
  let green_pixel = create_test_png(1, 1, &[0, 255, 0, 255]);
  let blue_pixel = create_test_png(1, 1, &[0, 0, 255, 255]);

  fs::write(dir.join("beach1.png"), &red_pixel).unwrap();
  fs::write(dir.join("beach2.png"), &green_pixel).unwrap();
  fs::write(dir.join("sunset.png"), &blue_pixel).unwrap();
  fs::write(dir.join("duplicate.png"), &red_pixel).unwrap();
  fs::write(dir.join("readme.txt"), b"not an image").unwrap();
}

// --- Scan → Fingerprint pipeline ---

#[test]
fn scan_then_fingerprint_pipeline() {
  let dir = TempDir::new().unwrap();
  populate_test_dir(dir.path());

  let scanned = scan_directory(dir.path()).unwrap();
  assert_eq!(scanned.len(), 5);

  let fingerprinted = fingerprint_files(scanned).unwrap();
  assert_eq!(fingerprinted.len(), 5);

  for f in &fingerprinted {
    assert_ne!(f.blake3_hash, [0u8; 32]);
    if f.scanned.file_type.is_image() {
      assert!(f.perceptual_hash.is_some());
    } else {
      assert!(f.perceptual_hash.is_none());
    }
  }
}

#[test]
fn scan_multiple_dirs_then_dedup() {
  let dir1 = TempDir::new().unwrap();
  let dir2 = TempDir::new().unwrap();

  let png = create_test_png(1, 1, &[128, 128, 128, 255]);
  fs::write(dir1.path().join("a.png"), &png).unwrap();
  fs::write(dir2.path().join("b.png"), &png).unwrap();
  fs::write(dir2.path().join("c.jpg"), b"different").unwrap();

  let scanned =
    scan_directories(&[dir1.path(), dir2.path()]).unwrap();
  assert_eq!(scanned.len(), 3);

  let fingerprinted = fingerprint_files(scanned).unwrap();
  let exact_dupes = find_exact_duplicates(&fingerprinted);

  assert_eq!(exact_dupes.len(), 1);
  assert_eq!(exact_dupes[0].duplicates.len(), 1);
  assert_eq!(exact_dupes[0].duplicate_type, DuplicateType::Exact);
}

#[test]
fn near_duplicate_detection_in_pipeline() {
  let dir = TempDir::new().unwrap();

  let img1 = create_test_png(8, 8, &vec![200u8; 8 * 8 * 4]);
  let mut rgba2 = vec![200u8; 8 * 8 * 4];
  rgba2[0] = 201;
  let img2 = create_test_png(8, 8, &rgba2);
  let img3 = create_test_png(8, 8, &vec![50u8; 8 * 8 * 4]);

  fs::write(dir.path().join("similar1.png"), &img1).unwrap();
  fs::write(dir.path().join("similar2.png"), &img2).unwrap();
  fs::write(dir.path().join("different.png"), &img3).unwrap();

  let scanned = scan_directory(dir.path()).unwrap();
  let fingerprinted = fingerprint_files(scanned).unwrap();
  let near_dupes = find_near_duplicates(&fingerprinted, 8);

  assert!(near_dupes.len() <= 1);
  for set in &near_dupes {
    match set.duplicate_type {
      DuplicateType::NearDuplicate { distance } => {
        assert!(distance <= 8)
      }
      _ => panic!("Expected NearDuplicate type"),
    }
  }
}

// --- Group → Plan pipeline ---

#[test]
fn group_and_plan_from_proposals() {
  let dir = TempDir::new().unwrap();
  populate_test_dir(dir.path());

  let scanned = scan_directory(dir.path()).unwrap();
  let fingerprinted = fingerprint_files(scanned).unwrap();
  let dupes = find_exact_duplicates(&fingerprinted);

  let proposed = vec![
    ProposedGroup {
      label: "Beach Photos".to_string(),
      rationale: "Similar beach scenes".to_string(),
      member_indices: vec![0, 1],
      member_destinations: vec![],
    },
    ProposedGroup {
      label: "Sunsets".to_string(),
      rationale: "Sunset imagery".to_string(),
      member_indices: vec![2],
      member_destinations: vec![],
    },
  ];

  let output_dir = dir.path().join("organized");
  let groups = build_groups(&proposed, &output_dir);
  assert_eq!(groups.len(), 2);
  assert_eq!(
    groups[0].suggested_path,
    output_dir.join("beach_photos")
  );
  assert_eq!(groups[1].suggested_path, output_dir.join("sunsets"));

  let plan =
    propose_plan(&output_dir, &groups, &dupes, &fingerprinted);
  assert_eq!(plan.moves.len(), 3);
  assert_eq!(plan.stats.total_files, 5);
  assert_eq!(plan.stats.groups_created, 2);
}

// --- Execute → Undo pipeline ---

#[test]
fn execute_and_undo_full_cycle() {
  let src_dir = TempDir::new().unwrap();
  let dest_dir = TempDir::new().unwrap();

  fs::write(src_dir.path().join("photo1.jpg"), b"image1").unwrap();
  fs::write(src_dir.path().join("photo2.jpg"), b"image2").unwrap();

  let undo_path = dest_dir.path().join("undo.json");

  let plan = ApprovedPlan {
    moves: vec![
      FileMove {
        from: src_dir.path().join("photo1.jpg"),
        to: dest_dir.path().join("beach/photo1.jpg"),
        group_id: 0,
      },
      FileMove {
        from: src_dir.path().join("photo2.jpg"),
        to: dest_dir.path().join("sunset/photo2.jpg"),
        group_id: 1,
      },
    ],
    deletions: vec![],
    skipped_files: vec![],
  };

  let report = execute_plan(&plan, &undo_path, dest_dir.path());

  assert_eq!(report.moves_completed.len(), 2);
  assert!(report.moves_failed.is_empty());
  assert!(dest_dir.path().join("beach/photo1.jpg").exists());
  assert!(dest_dir.path().join("sunset/photo2.jpg").exists());
  assert!(!src_dir.path().join("photo1.jpg").exists());
  assert!(!src_dir.path().join("photo2.jpg").exists());

  let restored = undo(&undo_path).unwrap();

  assert_eq!(restored.len(), 2);
  assert!(src_dir.path().join("photo1.jpg").exists());
  assert!(src_dir.path().join("photo2.jpg").exists());
  assert!(!dest_dir.path().join("beach/photo1.jpg").exists());
  assert!(!dest_dir.path().join("sunset/photo2.jpg").exists());
}

#[test]
fn execute_with_deletions() {
  let dir = TempDir::new().unwrap();
  let dupe = dir.path().join("dupe.jpg");
  fs::write(&dupe, b"duplicate content").unwrap();
  let undo_path = dir.path().join("undo.json");

  let plan = ApprovedPlan {
    moves: vec![],
    deletions: vec![dupe.clone()],
    skipped_files: vec![],
  };

  let report = execute_plan(&plan, &undo_path, dir.path());

  assert_eq!(report.deletions_completed.len(), 1);
  assert!(!dupe.exists());
}

// --- Analyze with cache ---

struct FakeAiProvider;

impl AiProvider for FakeAiProvider {
  async fn describe_image(
    &self,
    _image_data: &[u8],
    _mime_type: &str,
    context: &DescribeContext,
  ) -> Result<ContentDescription> {
    Ok(ContentDescription {
      summary: format!("Description of {}", context.filename),
      tags: vec!["test".to_string()],
      suggested_category: "photo".to_string(),
      confidence: 0.85,
    })
  }

  async fn propose_groups(
    &self,
    _files: &[FileSummary],
  ) -> Result<Vec<ProposedGroup>> {
    Ok(vec![ProposedGroup {
      label: "Test Group".to_string(),
      rationale: "All test files".to_string(),
      member_indices: vec![0],
      member_destinations: vec![],
    }])
  }
}

#[tokio::test]
async fn analyze_caches_and_reuses_results() {
  let file_dir = TempDir::new().unwrap();
  let cache_dir = TempDir::new().unwrap();

  let png = create_test_png(1, 1, &[255, 0, 0, 255]);
  let path = file_dir.path().join("test.png");
  fs::write(&path, &png).unwrap();

  let file = FingerprintedFile {
    scanned: ScannedFile {
      path: path.clone(),
      scan_root: file_dir.path().to_path_buf(),
      size: png.len() as u64,
      modified: std::time::SystemTime::now(),
      file_type: FileType::Image(ImageFormat::Png),
    },
    blake3_hash: compute_blake3(&path).unwrap(),
    perceptual_hash: None,
  };

  let opts = AnalyzeOptions {
    cache_dir: cache_dir.path().to_path_buf(),
    max_concurrent: 2,
    use_batch_api: false,
    ..Default::default()
  };

  let result1 =
    analyze_file(&FakeAiProvider, &file, &opts).await.unwrap();
  assert_eq!(result1.summary, "Description of test.png");
  assert_eq!(result1.confidence, 0.85);

  let cache_file = cache_dir
    .path()
    .join(format!("{}.v2.json", hex::encode(file.blake3_hash)));
  assert!(cache_file.exists());

  let result2 =
    analyze_file(&FakeAiProvider, &file, &opts).await.unwrap();
  assert_eq!(result1.summary, result2.summary);
}

#[tokio::test]
async fn analyze_batch_processes_multiple_files() {
  let file_dir = TempDir::new().unwrap();
  let cache_dir = TempDir::new().unwrap();

  let files: Vec<FingerprintedFile> = (0..3)
    .map(|i| {
      let rgba = vec![(i * 50) as u8; 4];
      let png = create_test_png(1, 1, &rgba);
      let path = file_dir.path().join(format!("img{i}.png"));
      fs::write(&path, &png).unwrap();
      FingerprintedFile {
        scanned: ScannedFile {
          path: path.clone(),
          scan_root: file_dir.path().to_path_buf(),
          size: png.len() as u64,
          modified: std::time::SystemTime::now(),
          file_type: FileType::Image(ImageFormat::Png),
        },
        blake3_hash: compute_blake3(&path).unwrap(),
        perceptual_hash: None,
      }
    })
    .collect();

  let opts = AnalyzeOptions {
    cache_dir: cache_dir.path().to_path_buf(),
    max_concurrent: 2,
    use_batch_api: false,
    ..Default::default()
  };

  let results = analyze_batch(&FakeAiProvider, &files, &opts).await;

  assert_eq!(results.len(), 3);
  for result in &results {
    assert!(result.is_ok());
  }
}

// --- Full pipeline: scan → fingerprint → analyze → group → plan → execute ---

#[tokio::test]
async fn full_pipeline_end_to_end() {
  let source_dir = TempDir::new().unwrap();
  let output_dir = TempDir::new().unwrap();
  let cache_dir = TempDir::new().unwrap();

  let red = create_test_png(1, 1, &[255, 0, 0, 255]);
  let green = create_test_png(1, 1, &[0, 255, 0, 255]);
  let blue = create_test_png(1, 1, &[0, 0, 255, 255]);

  fs::write(source_dir.path().join("red.png"), &red).unwrap();
  fs::write(source_dir.path().join("green.png"), &green).unwrap();
  fs::write(source_dir.path().join("blue.png"), &blue).unwrap();
  fs::write(source_dir.path().join("red_copy.png"), &red).unwrap();

  let scanned = scan_directory(source_dir.path()).unwrap();
  assert_eq!(scanned.len(), 4);

  let fingerprinted = fingerprint_files(scanned).unwrap();
  assert_eq!(fingerprinted.len(), 4);

  let dupes = find_exact_duplicates(&fingerprinted);
  assert_eq!(dupes.len(), 1);

  let opts = AnalyzeOptions {
    cache_dir: cache_dir.path().to_path_buf(),
    max_concurrent: 2,
    use_batch_api: false,
    ..Default::default()
  };
  let descriptions =
    analyze_batch(&FakeAiProvider, &fingerprinted, &opts).await;
  assert!(descriptions.iter().all(|r| r.is_ok()));

  let proposed = vec![
    ProposedGroup {
      label: "Warm Colors".to_string(),
      rationale: "Red-ish images".to_string(),
      member_indices: vec![0, 3],
      member_destinations: vec![],
    },
    ProposedGroup {
      label: "Cool Colors".to_string(),
      rationale: "Green and blue".to_string(),
      member_indices: vec![1, 2],
      member_destinations: vec![],
    },
  ];

  let groups = build_groups(&proposed, output_dir.path());
  let plan =
    propose_plan(output_dir.path(), &groups, &dupes, &fingerprinted);

  assert_eq!(plan.stats.total_files, 4);
  assert_eq!(plan.stats.groups_created, 2);
  assert_eq!(plan.stats.duplicates_found, 1);
  assert_eq!(plan.moves.len(), 4);

  let approved = ApprovedPlan {
    moves: plan.moves.clone(),
    deletions: vec![],
    skipped_files: vec![],
  };

  let undo_path = output_dir.path().join("undo.json");
  let report = execute_plan(&approved, &undo_path, output_dir.path());

  assert_eq!(report.moves_completed.len(), 4);
  assert!(report.moves_failed.is_empty());

  let restored = undo(&undo_path).unwrap();
  assert_eq!(restored.len(), 4);
  for path in &restored {
    assert!(path.exists());
  }
}

// --- Edge cases ---

#[test]
fn empty_directory_produces_empty_plan() {
  let dir = TempDir::new().unwrap();

  let scanned = scan_directory(dir.path()).unwrap();
  assert!(scanned.is_empty());

  let fingerprinted = fingerprint_files(scanned).unwrap();
  assert!(fingerprinted.is_empty());

  let groups = build_groups(&[], dir.path());
  let plan = propose_plan(dir.path(), &groups, &[], &fingerprinted);

  assert!(plan.moves.is_empty());
  assert_eq!(plan.stats.total_files, 0);
}

#[test]
fn deterministic_canonical_selection() {
  let dir = TempDir::new().unwrap();
  let content = b"identical content for all";

  for name in &["z.png", "a.png", "m.png"] {
    fs::write(dir.path().join(name), content).unwrap();
  }

  let scanned = scan_directory(dir.path()).unwrap();
  let fingerprinted = fingerprint_files(scanned).unwrap();
  let dupes = find_exact_duplicates(&fingerprinted);

  assert_eq!(dupes.len(), 1);
  let canonical_idx = dupes[0].canonical;
  assert!(canonical_idx < dupes[0].duplicates[0]);
  assert!(canonical_idx < dupes[0].duplicates[1]);
}

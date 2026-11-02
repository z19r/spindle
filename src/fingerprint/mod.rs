pub mod archive;
pub mod audio;
pub mod text;
mod video_frame;

use std::path::Path;

use anyhow::Result;

use crate::model::{
  DuplicateSet, DuplicateType, FingerprintedFile, ScannedFile,
};

pub fn compute_blake3(path: &Path) -> Result<[u8; 32]> {
  use anyhow::Context;
  let mut file = std::fs::File::open(path).with_context(|| {
    format!("Failed to open for hashing: {}", path.display())
  })?;
  let mut hasher = blake3::Hasher::new();
  std::io::copy(&mut file, &mut hasher).with_context(|| {
    format!("Failed to read for hashing: {}", path.display())
  })?;
  Ok(*hasher.finalize().as_bytes())
}

/// Hash every file. A file that cannot be read (deleted mid-scan, no
/// permission) is logged and left out rather than failing the run.
pub fn fingerprint_files(
  files: Vec<ScannedFile>,
) -> Vec<FingerprintedFile> {
  use rayon::prelude::*;

  files
    .into_par_iter()
    .filter_map(|scanned| {
      let blake3_hash = match compute_blake3(&scanned.path) {
        Ok(hash) => hash,
        Err(err) => {
          tracing::warn!(error = %format!("{err:#}"), "Skipping file");
          return None;
        }
      };
      let perceptual_hash = if scanned.file_type.is_image() {
        compute_perceptual_hash(&scanned.path).ok()
      } else if scanned.file_type.is_video() {
        // Keyframe phash (when ffmpeg is available) lets videos join
        // the same near-duplicate pool as images.
        video_frame::keyframe_phash(&scanned.path)
      } else {
        None
      };
      Some(FingerprintedFile {
        scanned,
        blake3_hash,
        perceptual_hash,
      })
    })
    .collect()
}

pub fn find_exact_duplicates(
  files: &[FingerprintedFile],
) -> Vec<DuplicateSet> {
  use std::collections::HashMap;

  let mut groups: HashMap<[u8; 32], Vec<usize>> = HashMap::new();
  for (idx, file) in files.iter().enumerate() {
    groups.entry(file.blake3_hash).or_default().push(idx);
  }

  groups
    .into_values()
    .filter(|indices| indices.len() > 1)
    .map(|mut indices| {
      indices.sort_unstable();
      DuplicateSet {
        canonical: indices[0],
        duplicates: indices[1..].to_vec(),
        duplicate_type: DuplicateType::Exact,
      }
    })
    .collect()
}

pub fn find_near_duplicates(
  files: &[FingerprintedFile],
  threshold: u32,
) -> Vec<DuplicateSet> {
  let mut results = Vec::new();
  let mut paired = vec![false; files.len()];

  for i in 0..files.len() {
    if paired[i] {
      continue;
    }
    let Some(ref hash_a) = files[i].perceptual_hash else {
      continue;
    };

    let mut group = Vec::new();
    let mut min_distance = u32::MAX;

    // shortcut: compares every pair, O(n²) on 8-byte hashes. Cheap
    // for tens of thousands of images; index the hashes past that.
    for j in i + 1..files.len() {
      if paired[j] {
        continue;
      }
      let Some(ref hash_b) = files[j].perceptual_hash else {
        continue;
      };
      // Length mismatch (different hash kinds) is "not comparable",
      // never "distance 0".
      let Some(distance) = hamming_distance(hash_a, hash_b) else {
        continue;
      };
      if distance <= threshold {
        group.push(j);
        min_distance = min_distance.min(distance);
        paired[j] = true;
      }
    }

    if !group.is_empty() {
      paired[i] = true;
      results.push(DuplicateSet {
        canonical: i,
        duplicates: group,
        duplicate_type: DuplicateType::NearDuplicate {
          distance: min_distance,
        },
      });
    }
  }

  results
}

fn compute_perceptual_hash(path: &Path) -> Result<Vec<u8>> {
  let img =
    image::open(path).or_else(|_| decode_via_magick(path, 512))?;
  let hasher = image_hasher::HasherConfig::new().to_hasher();
  let hash = hasher.hash_image(&img);
  Ok(hash.as_bytes().to_vec())
}

/// Decode with ImageMagick for formats the `image` crate can't read
/// (HEIC, AVIF, …), shrinking so the long edge is at most `max_edge`.
pub(crate) fn decode_via_magick(
  path: &Path,
  max_edge: u32,
) -> Result<image::DynamicImage> {
  static COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
  let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
  let tmp = std::env::temp_dir().join(format!(
    "spindle_conv_{}_{}.png",
    std::process::id(),
    n
  ));
  let status = std::process::Command::new("magick")
    .arg("convert")
    .arg(path)
    .arg("-strip")
    .arg("-resize")
    .arg(format!("{max_edge}x{max_edge}>"))
    .arg(tmp.as_os_str())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status();

  let ok = match status {
    Ok(s) => s.success(),
    Err(_) => false,
  };

  if !ok {
    anyhow::bail!(
      "image crate and magick both failed to decode {}",
      path.display()
    );
  }

  // Remove the temp file whether or not it decodes.
  let img = image::open(&tmp);
  let _ = std::fs::remove_file(&tmp);
  Ok(img?)
}

/// `None` when the hashes have different lengths — they come from
/// different hashers and comparing prefixes would fabricate distances.
pub(crate) fn hamming_distance(a: &[u8], b: &[u8]) -> Option<u32> {
  if a.len() != b.len() {
    return None;
  }
  Some(
    a.iter()
      .zip(b.iter())
      .map(|(x, y)| (x ^ y).count_ones())
      .sum(),
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;
  use std::path::PathBuf;
  use std::time::SystemTime;
  use tempfile::TempDir;

  use crate::model::{FileType, ImageFormat};

  fn make_scanned(
    dir: &Path,
    name: &str,
    content: &[u8],
  ) -> ScannedFile {
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    ScannedFile {
      path,
      scan_root: dir.to_path_buf(),
      size: content.len() as u64,
      modified: SystemTime::now(),
      file_type: FileType::Image(ImageFormat::Jpg),
    }
  }

  // --- blake3 hashing tests ---

  #[test]
  fn blake3_produces_deterministic_hash() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jpg");
    fs::write(&path, b"hello world").unwrap();

    let hash1 = compute_blake3(&path).unwrap();
    let hash2 = compute_blake3(&path).unwrap();

    assert_eq!(hash1, hash2);
  }

  #[test]
  fn blake3_different_content_different_hash() {
    let dir = TempDir::new().unwrap();
    let path1 = dir.path().join("a.jpg");
    let path2 = dir.path().join("b.jpg");
    fs::write(&path1, b"content A").unwrap();
    fs::write(&path2, b"content B").unwrap();

    let hash1 = compute_blake3(&path1).unwrap();
    let hash2 = compute_blake3(&path2).unwrap();

    assert_ne!(hash1, hash2);
  }

  #[test]
  fn blake3_same_content_same_hash() {
    let dir = TempDir::new().unwrap();
    let path1 = dir.path().join("copy1.jpg");
    let path2 = dir.path().join("copy2.jpg");
    fs::write(&path1, b"identical bytes").unwrap();
    fs::write(&path2, b"identical bytes").unwrap();

    let hash1 = compute_blake3(&path1).unwrap();
    let hash2 = compute_blake3(&path2).unwrap();

    assert_eq!(hash1, hash2);
  }

  #[test]
  fn blake3_errors_on_missing_file() {
    let result = compute_blake3(Path::new("/nonexistent/file.jpg"));

    assert!(result.is_err());
  }

  // --- fingerprint_files tests ---

  #[test]
  fn fingerprint_files_computes_hash_for_each() {
    let dir = TempDir::new().unwrap();
    let files = vec![
      make_scanned(dir.path(), "a.jpg", b"aaa"),
      make_scanned(dir.path(), "b.jpg", b"bbb"),
    ];

    let results = fingerprint_files(files);

    assert_eq!(results.len(), 2);
    assert_ne!(results[0].blake3_hash, results[1].blake3_hash);
  }

  #[test]
  fn fingerprint_files_skips_unreadable_files() {
    let dir = TempDir::new().unwrap();
    let files = vec![
      make_scanned(dir.path(), "a.jpg", b"aaa"),
      make_scanned(dir.path(), "gone.jpg", b"bbb"),
    ];
    std::fs::remove_file(dir.path().join("gone.jpg")).unwrap();

    let results = fingerprint_files(files);

    assert_eq!(results.len(), 1);
    assert!(results[0].scanned.path.ends_with("a.jpg"));
  }

  #[test]
  fn fingerprint_files_sets_perceptual_hash_for_images() {
    let dir = TempDir::new().unwrap();

    let png_data = create_test_png(1, 1, &[255, 0, 0, 255]);
    let mut files =
      vec![make_scanned(dir.path(), "img.png", &png_data)];
    files[0].file_type = FileType::Image(ImageFormat::Png);

    let results = fingerprint_files(files);

    assert!(results[0].perceptual_hash.is_some());
  }

  #[test]
  fn fingerprint_files_no_perceptual_hash_for_video() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("clip.mp4");
    fs::write(&path, b"not really a video").unwrap();
    let files = vec![ScannedFile {
      path,
      scan_root: dir.path().to_path_buf(),
      size: 18,
      modified: SystemTime::now(),
      file_type: FileType::Video(crate::model::VideoFormat::Mp4),
    }];

    let results = fingerprint_files(files);

    assert!(results[0].perceptual_hash.is_none());
  }

  // --- exact duplicate detection ---

  #[test]
  fn finds_exact_duplicates_by_hash() {
    let shared_hash = [1u8; 32];
    let unique_hash = [2u8; 32];

    let files = vec![
      make_fingerprinted("a.jpg", shared_hash, None),
      make_fingerprinted("b.jpg", shared_hash, None),
      make_fingerprinted("c.jpg", unique_hash, None),
    ];

    let dupes = find_exact_duplicates(&files);

    assert_eq!(dupes.len(), 1);
    assert_eq!(dupes[0].duplicate_type, DuplicateType::Exact);
    assert_eq!(dupes[0].duplicates.len(), 1);
  }

  #[test]
  fn no_duplicates_when_all_unique() {
    let files = vec![
      make_fingerprinted("a.jpg", [1u8; 32], None),
      make_fingerprinted("b.jpg", [2u8; 32], None),
      make_fingerprinted("c.jpg", [3u8; 32], None),
    ];

    let dupes = find_exact_duplicates(&files);

    assert!(dupes.is_empty());
  }

  #[test]
  fn multiple_duplicate_groups() {
    let files = vec![
      make_fingerprinted("a.jpg", [1u8; 32], None),
      make_fingerprinted("b.jpg", [1u8; 32], None),
      make_fingerprinted("c.jpg", [2u8; 32], None),
      make_fingerprinted("d.jpg", [2u8; 32], None),
      make_fingerprinted("e.jpg", [3u8; 32], None),
    ];

    let dupes = find_exact_duplicates(&files);

    assert_eq!(dupes.len(), 2);
  }

  #[test]
  fn three_way_duplicate() {
    let files = vec![
      make_fingerprinted("a.jpg", [1u8; 32], None),
      make_fingerprinted("b.jpg", [1u8; 32], None),
      make_fingerprinted("c.jpg", [1u8; 32], None),
    ];

    let dupes = find_exact_duplicates(&files);

    assert_eq!(dupes.len(), 1);
    assert_eq!(dupes[0].duplicates.len(), 2);
  }

  // --- near-duplicate detection ---

  #[test]
  fn finds_near_duplicates_within_threshold() {
    let hash_a = vec![0b0000_0000u8];
    let hash_b = vec![0b0000_0011u8];

    let files = vec![
      make_fingerprinted("a.jpg", [1u8; 32], Some(hash_a)),
      make_fingerprinted("b.jpg", [2u8; 32], Some(hash_b)),
    ];

    let dupes = find_near_duplicates(&files, 5);

    assert_eq!(dupes.len(), 1);
    match dupes[0].duplicate_type {
      DuplicateType::NearDuplicate { distance } => {
        assert_eq!(distance, 2)
      }
      _ => panic!("Expected NearDuplicate"),
    }
  }

  #[test]
  fn no_near_duplicates_above_threshold() {
    let hash_a = vec![0b0000_0000u8];
    let hash_b = vec![0b1111_1111u8];

    let files = vec![
      make_fingerprinted("a.jpg", [1u8; 32], Some(hash_a)),
      make_fingerprinted("b.jpg", [2u8; 32], Some(hash_b)),
    ];

    let dupes = find_near_duplicates(&files, 5);

    assert!(dupes.is_empty());
  }

  #[test]
  fn mismatched_hash_lengths_never_compare() {
    // An 8-byte image phash vs. a 1-byte hash must be "not
    // comparable", not "distance over the shared prefix".
    let files = vec![
      make_fingerprinted("a.jpg", [1u8; 32], Some(vec![0u8; 8])),
      make_fingerprinted("b.jpg", [2u8; 32], Some(vec![0u8; 1])),
    ];

    let dupes = find_near_duplicates(&files, 64);

    assert!(dupes.is_empty());
  }

  #[test]
  fn a_threshold_over_the_hash_size_matches_everything() {
    let files = vec![
      make_fingerprinted("a.jpg", [1u8; 32], Some(vec![0x00u8; 1])),
      make_fingerprinted("b.jpg", [2u8; 32], Some(vec![0xFFu8; 1])),
    ];

    let dupes = find_near_duplicates(&files, 9);

    assert_eq!(dupes.len(), 1);
  }

  #[test]
  fn finds_the_close_pair_among_8_byte_hashes() {
    // Three files: two within distance 2, one far away.
    let mut near = vec![0u8; 8];
    near[0] = 0b0000_0011;
    let far = vec![0xFFu8; 8];

    let files = vec![
      make_fingerprinted("a.jpg", [1u8; 32], Some(vec![0u8; 8])),
      make_fingerprinted("b.jpg", [2u8; 32], Some(near)),
      make_fingerprinted("c.jpg", [3u8; 32], Some(far)),
    ];

    let dupes = find_near_duplicates(&files, 5);

    assert_eq!(dupes.len(), 1);
    assert_eq!(dupes[0].canonical, 0);
    assert_eq!(dupes[0].duplicates, vec![1]);
  }

  #[test]
  fn skips_files_without_perceptual_hash() {
    let files = vec![
      make_fingerprinted("a.jpg", [1u8; 32], Some(vec![0u8])),
      make_fingerprinted("b.jpg", [2u8; 32], None),
    ];

    let dupes = find_near_duplicates(&files, 10);

    assert!(dupes.is_empty());
  }

  // --- helpers ---

  fn make_fingerprinted(
    name: &str,
    hash: [u8; 32],
    phash: Option<Vec<u8>>,
  ) -> FingerprintedFile {
    FingerprintedFile {
      scanned: ScannedFile {
        path: Path::new(name).to_path_buf(),
        scan_root: PathBuf::from("/downloads"),
        size: 100,
        modified: SystemTime::now(),
        file_type: FileType::Image(ImageFormat::Jpg),
      },
      blake3_hash: hash,
      perceptual_hash: phash,
    }
  }

  fn create_test_png(
    width: u32,
    height: u32,
    rgba: &[u8],
  ) -> Vec<u8> {
    use image::{ImageBuffer, RgbaImage};
    let img: RgbaImage =
      ImageBuffer::from_raw(width, height, rgba.to_vec()).unwrap();
    let mut buf = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut buf);
    img.write_to(&mut cursor, image::ImageFormat::Png).unwrap();
    buf
  }
}

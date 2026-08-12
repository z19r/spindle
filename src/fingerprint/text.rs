//! Fuzzy text similarity via 64-bit simhash over word 3-shingles.
//! Catches edited copies of the same document that byte-hashing misses.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::model::{DuplicateSet, DuplicateType, FingerprintedFile};

/// Hamming threshold for "same document, lightly edited".
pub const SIMHASH_THRESHOLD: u32 = 6;

/// Skip huge text files — similarity on multi-MB logs is noise.
const MAX_TEXT_BYTES: u64 = 2 * 1024 * 1024;

/// Too few tokens → simhash carries no signal.
const MIN_TOKENS: usize = 12;

pub fn find_similar_text(
  files: &[FingerprintedFile],
) -> Vec<DuplicateSet> {
  // (file index, simhash)
  let hashed: Vec<(usize, u64)> = files
    .iter()
    .enumerate()
    .filter(|(_, f)| {
      f.scanned.file_type.is_text()
        && f.scanned.size <= MAX_TEXT_BYTES
    })
    .filter_map(|(idx, f)| {
      let content = std::fs::read_to_string(&f.scanned.path).ok()?;
      simhash(&content).map(|hash| (idx, hash))
    })
    .collect();

  let mut results = Vec::new();
  let mut paired = std::collections::HashSet::new();

  for a in 0..hashed.len() {
    let (i, hash_a) = hashed[a];
    if paired.contains(&i) {
      continue;
    }
    let mut group = Vec::new();
    let mut min_distance = u32::MAX;

    for &(j, hash_b) in hashed.iter().skip(a + 1) {
      if paired.contains(&j) {
        continue;
      }
      // Byte-identical pairs belong to exact dedupe, not here.
      if files[i].blake3_hash == files[j].blake3_hash {
        continue;
      }
      let distance = (hash_a ^ hash_b).count_ones();
      if distance <= SIMHASH_THRESHOLD {
        group.push(j);
        min_distance = min_distance.min(distance);
        paired.insert(j);
      }
    }

    if !group.is_empty() {
      paired.insert(i);
      results.push(DuplicateSet {
        canonical: i,
        duplicates: group,
        duplicate_type: DuplicateType::SimilarText {
          distance: min_distance,
        },
      });
    }
  }
  results
}

/// 64-bit simhash over word 3-shingles of the normalized text.
/// `None` when the text is too short to fingerprint meaningfully.
fn simhash(text: &str) -> Option<u64> {
  let tokens: Vec<String> = text
    .split(|c: char| !c.is_alphanumeric())
    .filter(|t| !t.is_empty())
    .map(|t| t.to_lowercase())
    .collect();
  if tokens.len() < MIN_TOKENS {
    return None;
  }

  let mut weights = [0i64; 64];
  for shingle in tokens.windows(3) {
    let mut hasher = DefaultHasher::new();
    shingle.hash(&mut hasher);
    let h = hasher.finish();
    for (bit, weight) in weights.iter_mut().enumerate() {
      if h >> bit & 1 == 1 {
        *weight += 1;
      } else {
        *weight -= 1;
      }
    }
  }

  let mut hash = 0u64;
  for (bit, &weight) in weights.iter().enumerate() {
    if weight > 0 {
      hash |= 1 << bit;
    }
  }
  Some(hash)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::fingerprint::fingerprint_files;
  use crate::scanner::scan_directory;
  use tempfile::TempDir;

  const LEASE: &str = "RESIDENTIAL LEASE AGREEMENT between Alice \
    Johnson and Bob Smith for the property at 123 Main Street. The \
    monthly rent is two thousand dollars, due on the first of each \
    month. The lease term begins January first and runs for twelve \
    months with an option to renew at the end of the term.";

  fn detect(dir: &TempDir) -> Vec<DuplicateSet> {
    let files =
      fingerprint_files(scan_directory(dir.path()).unwrap()).unwrap();
    find_similar_text(&files)
  }

  #[test]
  fn detects_lightly_edited_copy() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("lease_v1.txt"), LEASE).unwrap();
    let edited = LEASE.replace("two thousand", "twenty one hundred");
    std::fs::write(dir.path().join("lease_v2.txt"), edited).unwrap();

    let sets = detect(&dir);

    assert_eq!(sets.len(), 1);
    assert!(matches!(
      sets[0].duplicate_type,
      DuplicateType::SimilarText { .. }
    ));
  }

  #[test]
  fn unrelated_documents_do_not_match() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("lease.txt"), LEASE).unwrap();
    std::fs::write(
      dir.path().join("recipe.txt"),
      "Combine flour sugar and butter in a large mixing bowl then \
       fold in the chocolate chips and bake at three hundred fifty \
       degrees for twelve minutes until the edges turn golden brown \
       and the center is just set before cooling on a wire rack.",
    )
    .unwrap();

    assert!(detect(&dir).is_empty());
  }

  #[test]
  fn byte_identical_files_are_left_to_exact_dedupe() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.txt"), LEASE).unwrap();
    std::fs::write(dir.path().join("b.txt"), LEASE).unwrap();

    assert!(detect(&dir).is_empty());
  }

  #[test]
  fn tiny_files_are_skipped() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello world").unwrap();
    std::fs::write(dir.path().join("b.txt"), "hello there").unwrap();

    assert!(detect(&dir).is_empty());
  }

  #[test]
  fn simhash_is_stable() {
    assert_eq!(simhash(LEASE), simhash(LEASE));
  }
}

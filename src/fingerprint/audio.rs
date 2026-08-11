//! Acoustic fingerprint matching via chromaprint's `fpcalc` when it's
//! installed — catches the same recording across formats/bitrates.
//! Degrades gracefully (no matches, one log line) when it isn't.

use std::path::Path;
use std::sync::OnceLock;

use crate::model::{DuplicateSet, DuplicateType, FingerprintedFile};

/// Minimum percent bit-similarity to call two streams the same
/// recording.
const MIN_SCORE: u32 = 90;

/// Fingerprints shorter than this carry too little signal.
const MIN_FP_LEN: usize = 32;

fn fpcalc_available() -> bool {
  static AVAILABLE: OnceLock<bool> = OnceLock::new();
  *AVAILABLE.get_or_init(|| {
    let ok = std::process::Command::new("fpcalc")
      .arg("-version")
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::null())
      .status()
      .map(|s| s.success())
      .unwrap_or(false);
    if !ok {
      tracing::info!(
        "fpcalc (chromaprint) not found — audio similarity \
         detection disabled, exact-hash matching still applies"
      );
    }
    ok
  })
}

pub fn find_similar_audio(
  files: &[FingerprintedFile],
) -> Vec<DuplicateSet> {
  if !fpcalc_available() {
    return Vec::new();
  }

  let fingerprints: Vec<(usize, Vec<u32>)> = files
    .iter()
    .enumerate()
    .filter(|(_, f)| {
      matches!(f.scanned.file_type, crate::model::FileType::Audio(_))
    })
    .filter_map(|(idx, f)| {
      let fp = fingerprint(&f.scanned.path)?;
      (fp.len() >= MIN_FP_LEN).then_some((idx, fp))
    })
    .collect();

  let mut results = Vec::new();
  let mut paired = std::collections::HashSet::new();

  for a in 0..fingerprints.len() {
    let (i, ref fp_a) = fingerprints[a];
    if paired.contains(&i) {
      continue;
    }
    let mut group = Vec::new();
    let mut best_score = 0;

    for (j, fp_b) in fingerprints.iter().skip(a + 1) {
      if paired.contains(j) {
        continue;
      }
      if files[i].blake3_hash == files[*j].blake3_hash {
        continue; // exact dedupe's job
      }
      let score = similarity_percent(fp_a, fp_b);
      if score >= MIN_SCORE {
        group.push(*j);
        best_score = best_score.max(score);
        paired.insert(*j);
      }
    }

    if !group.is_empty() {
      paired.insert(i);
      results.push(DuplicateSet {
        canonical: i,
        duplicates: group,
        duplicate_type: DuplicateType::SimilarAudio {
          score: best_score,
        },
      });
    }
  }
  results
}

/// Raw chromaprint fingerprint (a stream of u32s) via
/// `fpcalc -raw -plain`.
fn fingerprint(path: &Path) -> Option<Vec<u32>> {
  let output = std::process::Command::new("fpcalc")
    .arg("-raw")
    .arg("-plain")
    .arg(path)
    .output()
    .ok()?;
  if !output.status.success() {
    return None;
  }
  let text = String::from_utf8_lossy(&output.stdout);
  let values: Vec<u32> = text
    .trim()
    .split(',')
    .filter_map(|v| v.trim().parse().ok())
    .collect();
  (!values.is_empty()).then_some(values)
}

/// Percent of matching bits over the aligned overlap of two
/// fingerprint streams.
fn similarity_percent(a: &[u32], b: &[u32]) -> u32 {
  let overlap = a.len().min(b.len());
  if overlap == 0 {
    return 0;
  }
  // Wildly different lengths are different recordings regardless of
  // how the overlap compares.
  let longer = a.len().max(b.len());
  if overlap * 10 < longer * 8 {
    return 0;
  }
  let matching_bits: u64 = a
    .iter()
    .zip(b.iter())
    .map(|(x, y)| u64::from((x ^ y).count_zeros()))
    .sum();
  (matching_bits * 100 / (overlap as u64 * 32)) as u32
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn identical_fingerprints_score_100() {
    let fp: Vec<u32> = (0..64).collect();
    assert_eq!(similarity_percent(&fp, &fp), 100);
  }

  #[test]
  fn inverted_fingerprints_score_0() {
    let a = vec![0u32; 64];
    let b = vec![u32::MAX; 64];
    assert_eq!(similarity_percent(&a, &b), 0);
  }

  #[test]
  fn very_different_lengths_never_match() {
    let a = vec![7u32; 200];
    let b = vec![7u32; 40];
    assert_eq!(similarity_percent(&a, &b), 0);
  }

  #[test]
  fn no_audio_files_yields_no_matches() {
    assert!(find_similar_audio(&[]).is_empty());
  }
}

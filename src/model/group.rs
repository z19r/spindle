use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum DuplicateType {
  Exact,
  NearDuplicate {
    distance: u32,
  },
  /// An archive whose entries are all present, extracted, in a
  /// directory — the archive is redundant.
  ArchiveMatch,
  /// Text files whose normalized content is near-identical
  /// (simhash hamming distance).
  SimilarText {
    distance: u32,
  },
  /// Audio files with matching acoustic fingerprints
  /// (percent bit-similarity of chromaprint streams).
  SimilarAudio {
    score: u32,
  },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateSet {
  pub canonical: usize,
  pub duplicates: Vec<usize>,
  pub duplicate_type: DuplicateType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberDestination {
  pub index: usize,
  pub dest_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileGroup {
  pub id: usize,
  pub label: String,
  pub rationale: String,
  pub members: Vec<usize>,
  pub member_destinations: Vec<MemberDestination>,
  pub suggested_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedGroup {
  pub label: String,
  pub rationale: String,
  #[serde(default)]
  pub member_indices: Vec<usize>,
  #[serde(default, rename = "members")]
  pub member_destinations: Vec<MemberDestination>,
}

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{DuplicateSet, FileGroup};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMove {
  pub from: PathBuf,
  pub to: PathBuf,
  pub group_id: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStats {
  pub total_files: usize,
  pub groups_created: usize,
  pub duplicates_found: usize,
  pub space_to_reclaim: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReorgPlan {
  pub groups: Vec<FileGroup>,
  pub duplicates: Vec<DuplicateSet>,
  pub moves: Vec<FileMove>,
  pub stats: PlanStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedPlan {
  pub moves: Vec<FileMove>,
  pub deletions: Vec<PathBuf>,
  pub skipped_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionReport {
  /// Journal run id — the handle for `spindle --undo-run <id>`.
  pub run_id: String,
  pub moves_completed: Vec<FileMove>,
  pub moves_failed: Vec<(FileMove, String)>,
  /// Originals that were staged into the trash (not deleted yet).
  pub deletions_staged: Vec<PathBuf>,
  /// Total size of staged deletions, reclaimable via purge.
  pub bytes_staged: u64,
  /// `None` when the run did nothing (nothing to undo).
  pub journal_path: Option<PathBuf>,
  pub journal_error: Option<String>,
}

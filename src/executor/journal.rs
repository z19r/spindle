//! Append-only run journal: one JSON file per execute run, recording every
//! move and staged deletion so any run can be reversed later.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const JOURNAL_VERSION: u32 = 1;

/// Suffix a journal file gains once its run has been undone, so it is
/// skipped when picking "the latest run" but kept for the record.
const UNDONE_SUFFIX: &str = ".undone.json";
const ACTIVE_SUFFIX: &str = ".json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalMove {
  pub from: PathBuf,
  /// The actual destination after any collision auto-rename.
  pub to: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalDeletion {
  pub original: PathBuf,
  /// Where the file was staged inside the trash directory.
  pub trashed_to: PathBuf,
  pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunJournal {
  #[serde(default = "default_version")]
  pub version: u32,
  pub run_id: String,
  pub timestamp: String,
  #[serde(default)]
  pub moves: Vec<JournalMove>,
  #[serde(default)]
  pub deletions: Vec<JournalDeletion>,
}

fn default_version() -> u32 {
  JOURNAL_VERSION
}

impl RunJournal {
  pub fn new(run_id: impl Into<String>) -> Self {
    Self {
      version: JOURNAL_VERSION,
      run_id: run_id.into(),
      timestamp: chrono::Utc::now().to_rfc3339(),
      moves: Vec::new(),
      deletions: Vec::new(),
    }
  }

  pub fn is_empty(&self) -> bool {
    self.moves.is_empty() && self.deletions.is_empty()
  }

  pub fn path_in(&self, journal_dir: &Path) -> PathBuf {
    journal_dir.join(format!("{}{ACTIVE_SUFFIX}", self.run_id))
  }

  pub fn save(&self, journal_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(journal_dir).with_context(|| {
      format!(
        "Failed to create journal dir: {}",
        journal_dir.display()
      )
    })?;
    let path = self.path_in(journal_dir);
    let json = serde_json::to_string_pretty(self)
      .context("Failed to serialize run journal")?;
    std::fs::write(&path, json).with_context(|| {
      format!("Failed to write journal: {}", path.display())
    })?;
    Ok(path)
  }
}

/// Load a specific run's journal by id.
pub fn load(journal_dir: &Path, run_id: &str) -> Result<RunJournal> {
  let path = journal_dir.join(format!("{run_id}{ACTIVE_SUFFIX}"));
  let content =
    std::fs::read_to_string(&path).with_context(|| {
      format!("No journal for run {run_id} at {}", path.display())
    })?;
  serde_json::from_str(&content).with_context(|| {
    format!("Corrupt journal: {}", path.display())
  })
}

/// Run ids of journals that have not been undone, newest first.
/// Run ids sort lexicographically because they start with a
/// `YYYYMMDD-HHMMSS` timestamp.
pub fn list_runs(journal_dir: &Path) -> Vec<String> {
  let Ok(entries) = std::fs::read_dir(journal_dir) else {
    return Vec::new();
  };
  let mut runs: Vec<String> = entries
    .filter_map(|e| e.ok())
    .filter_map(|e| e.file_name().into_string().ok())
    .filter(|name| {
      name.ends_with(ACTIVE_SUFFIX) && !name.ends_with(UNDONE_SUFFIX)
    })
    .map(|name| name.trim_end_matches(ACTIVE_SUFFIX).to_string())
    .collect();
  runs.sort_unstable_by(|a, b| b.cmp(a));
  runs
}

/// Mark a run's journal as undone so it stops showing up in `list_runs`.
pub fn mark_undone(journal_dir: &Path, run_id: &str) -> Result<()> {
  let from = journal_dir.join(format!("{run_id}{ACTIVE_SUFFIX}"));
  let to = journal_dir.join(format!("{run_id}{UNDONE_SUFFIX}"));
  std::fs::rename(&from, &to).with_context(|| {
    format!("Failed to mark journal undone: {}", from.display())
  })
}

/// A fresh, collision-resistant run id: UTC timestamp + pid.
pub fn new_run_id() -> String {
  format!(
    "{}-{}",
    chrono::Utc::now().format("%Y%m%d-%H%M%S%3f"),
    std::process::id()
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

  #[test]
  fn save_load_roundtrips() {
    let dir = TempDir::new().unwrap();
    let mut journal = RunJournal::new("20260811-000000000-1");
    journal.moves.push(JournalMove {
      from: PathBuf::from("/a"),
      to: PathBuf::from("/b"),
    });
    journal.deletions.push(JournalDeletion {
      original: PathBuf::from("/c"),
      trashed_to: PathBuf::from("/trash/c"),
      size: 42,
    });

    journal.save(dir.path()).unwrap();
    let loaded = load(dir.path(), "20260811-000000000-1").unwrap();

    assert_eq!(loaded.moves.len(), 1);
    assert_eq!(loaded.deletions.len(), 1);
    assert_eq!(loaded.deletions[0].size, 42);
  }

  #[test]
  fn list_runs_newest_first_and_skips_undone() {
    let dir = TempDir::new().unwrap();
    RunJournal::new("20260810-000000000-1")
      .save(dir.path())
      .unwrap();
    RunJournal::new("20260811-000000000-1")
      .save(dir.path())
      .unwrap();
    RunJournal::new("20260809-000000000-1")
      .save(dir.path())
      .unwrap();
    mark_undone(dir.path(), "20260811-000000000-1").unwrap();

    let runs = list_runs(dir.path());

    assert_eq!(
      runs,
      vec!["20260810-000000000-1", "20260809-000000000-1"]
    );
  }

  #[test]
  fn list_runs_empty_when_dir_missing() {
    assert!(list_runs(Path::new("/nonexistent/journal")).is_empty());
  }

  #[test]
  fn load_missing_run_errors() {
    let dir = TempDir::new().unwrap();
    assert!(load(dir.path(), "nope").is_err());
  }

  #[test]
  fn run_ids_are_unique_enough() {
    assert_ne!(new_run_id(), {
      std::thread::sleep(std::time::Duration::from_millis(2));
      new_run_id()
    });
  }
}

//! Append-only run journal: one JSON file per execute run, recording every
//! move and staged deletion so any run can be reversed later.
//!
//! While a run executes, each step is appended to `<run>.inprogress.jsonl`
//! the moment it happens, so a run killed halfway still leaves a record
//! of what it moved. A finished run turns that log into `<run>.json`;
//! an unfinished one is turned into it the next time journals are read.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const JOURNAL_VERSION: u32 = 1;

/// Suffix a journal file gains once its run has been undone, so it is
/// skipped when picking "the latest run" but kept for the record.
const UNDONE_SUFFIX: &str = ".undone.json";
const ACTIVE_SUFFIX: &str = ".json";
/// The step-by-step log of a run that is executing, or was cut short.
const INPROGRESS_SUFFIX: &str = ".inprogress.jsonl";

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
    crate::config::write_atomic(&path, json.as_bytes())
      .with_context(|| {
        format!("Failed to write journal: {}", path.display())
      })?;
    Ok(path)
  }
}

/// One line of an in-progress log.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Step {
  Move(JournalMove),
  Deletion(JournalDeletion),
}

/// Records a run step by step as it executes. Every step reaches the
/// operating system before the next file is touched, so a crash or
/// Ctrl-C loses at most the step in flight.
pub struct JournalWriter {
  journal: RunJournal,
  journal_dir: PathBuf,
  log: std::fs::File,
  log_path: PathBuf,
}

impl JournalWriter {
  /// Open the log before anything moves. A run that cannot be
  /// journaled must not start: it could not be undone.
  pub fn begin(journal_dir: &Path, run_id: &str) -> Result<Self> {
    std::fs::create_dir_all(journal_dir).with_context(|| {
      format!(
        "Failed to create journal dir: {}",
        journal_dir.display()
      )
    })?;
    let log_path =
      journal_dir.join(format!("{run_id}{INPROGRESS_SUFFIX}"));
    let log = std::fs::OpenOptions::new()
      .create_new(true)
      .append(true)
      .open(&log_path)
      .with_context(|| {
        format!("Failed to start journal: {}", log_path.display())
      })?;
    Ok(Self {
      journal: RunJournal::new(run_id),
      journal_dir: journal_dir.to_path_buf(),
      log,
      log_path,
    })
  }

  pub fn record_move(&mut self, mv: JournalMove) -> Result<()> {
    self.append(&Step::Move(mv.clone()))?;
    self.journal.moves.push(mv);
    Ok(())
  }

  pub fn record_deletion(
    &mut self,
    del: JournalDeletion,
  ) -> Result<()> {
    self.append(&Step::Deletion(del.clone()))?;
    self.journal.deletions.push(del);
    Ok(())
  }

  fn append(&mut self, step: &Step) -> Result<()> {
    use std::io::Write;
    let mut line = serde_json::to_vec(step)
      .context("Failed to encode journal step")?;
    line.push(b'\n');
    // One write per line on an unbuffered file: the line is with the
    // OS when this returns, and survives the process dying.
    self.log.write_all(&line).with_context(|| {
      format!("Failed to append to {}", self.log_path.display())
    })
  }

  /// Write the finished journal and drop the step log. `None` when the
  /// run did nothing, so there is nothing to undo.
  pub fn finish(self) -> Result<Option<PathBuf>> {
    let path = if self.journal.is_empty() {
      None
    } else {
      Some(self.journal.save(&self.journal_dir)?)
    };
    let _ = std::fs::remove_file(&self.log_path);
    Ok(path)
  }
}

/// Turn the step logs of runs that never finished into journals, so
/// they can be listed and undone like any other run. A torn last line
/// (the process died mid-write) is skipped; every line before it is a
/// step that completed.
fn recover_interrupted(journal_dir: &Path) {
  let Ok(entries) = std::fs::read_dir(journal_dir) else {
    return;
  };
  for entry in entries.filter_map(|e| e.ok()) {
    let name = entry.file_name().to_string_lossy().to_string();
    let Some(run_id) = name.strip_suffix(INPROGRESS_SUFFIX) else {
      continue;
    };
    let log_path = entry.path();
    let Ok(text) = std::fs::read_to_string(&log_path) else {
      continue;
    };
    let mut journal = RunJournal::new(run_id);
    for step in
      text.lines().filter_map(|l| serde_json::from_str(l).ok())
    {
      match step {
        Step::Move(mv) => journal.moves.push(mv),
        Step::Deletion(del) => journal.deletions.push(del),
      }
    }
    if !journal.is_empty() {
      if let Err(e) = journal.save(journal_dir) {
        tracing::warn!(error = %e, run_id, "Could not recover journal");
        continue;
      }
      tracing::warn!(
        run_id,
        "Recovered the journal of an interrupted run"
      );
    }
    let _ = std::fs::remove_file(&log_path);
  }
}

/// Load a specific run's journal by id.
pub fn load(journal_dir: &Path, run_id: &str) -> Result<RunJournal> {
  recover_interrupted(journal_dir);
  let path = journal_dir.join(format!("{run_id}{ACTIVE_SUFFIX}"));
  let content =
    std::fs::read_to_string(&path).with_context(|| {
      format!("No journal for run {run_id} at {}", path.display())
    })?;
  serde_json::from_str(&content)
    .with_context(|| format!("Corrupt journal: {}", path.display()))
}

/// Run ids of journals that have not been undone, newest first.
/// Run ids sort lexicographically because they start with a
/// `YYYYMMDD-HHMMSS` timestamp.
pub fn list_runs(journal_dir: &Path) -> Vec<String> {
  recover_interrupted(journal_dir);
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

  /// A run killed halfway leaves its step log. Listing journals turns
  /// it into a normal, undoable journal, torn last line and all.
  #[test]
  fn an_interrupted_run_is_recovered_from_its_step_log() {
    let dir = TempDir::new().unwrap();
    let mut writer =
      JournalWriter::begin(dir.path(), "20260811-000000000-7")
        .unwrap();
    writer
      .record_move(JournalMove {
        from: PathBuf::from("/a"),
        to: PathBuf::from("/b"),
      })
      .unwrap();
    // The process dies here: no finish(), and half a line on disk.
    let log =
      dir.path().join("20260811-000000000-7.inprogress.jsonl");
    let mut text = std::fs::read_to_string(&log).unwrap();
    text.push_str("{\"move\":{\"from\":\"/c");
    std::fs::write(&log, text).unwrap();
    drop(writer);

    assert_eq!(list_runs(dir.path()), vec!["20260811-000000000-7"]);
    let journal = load(dir.path(), "20260811-000000000-7").unwrap();
    assert_eq!(journal.moves.len(), 1);
    assert_eq!(journal.moves[0].to, PathBuf::from("/b"));
    assert!(!log.exists());
  }

  #[test]
  fn finishing_an_empty_run_leaves_no_files() {
    let dir = TempDir::new().unwrap();
    let writer =
      JournalWriter::begin(dir.path(), "run-empty").unwrap();

    assert_eq!(writer.finish().unwrap(), None);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
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

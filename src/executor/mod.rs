pub mod journal;
pub mod trash;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{ApprovedPlan, ExecutionReport, FileMove};
use journal::{JournalDeletion, JournalMove, RunJournal};

/// Where run journals and staged trash live.
#[derive(Debug, Clone)]
pub struct ExecutorPaths {
  pub journal_dir: PathBuf,
  pub trash_dir: PathBuf,
}

impl ExecutorPaths {
  /// Global defaults: `<data_dir>/spindle/{journal,trash}`.
  pub fn default_paths() -> Self {
    let base = directories::BaseDirs::new()
      .map(|d| d.data_dir().join("spindle"))
      .unwrap_or_else(|| PathBuf::from(".local/share/spindle"));
    Self::under(&base)
  }

  /// Both directories under one root — used by tests.
  pub fn under(root: &Path) -> Self {
    Self {
      journal_dir: root.join("journal"),
      trash_dir: root.join("trash"),
    }
  }
}

/// Legacy single-shot undo log (pre-journal). Still read by `undo()` so
/// `--undo-log <path>` keeps working for logs written by old versions.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UndoLog {
  moves: Vec<UndoEntry>,
  timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UndoEntry {
  original: PathBuf,
  moved_to: PathBuf,
}

pub fn execute_plan(
  plan: &ApprovedPlan,
  paths: &ExecutorPaths,
  output_dir: &Path,
) -> ExecutionReport {
  let run_id = journal::new_run_id();
  let mut run_journal = RunJournal::new(&run_id);

  let mut moves_completed = Vec::new();
  let mut moves_failed = Vec::new();

  for file_move in &plan.moves {
    if let Err(e) = validate_move_target(&file_move.to, output_dir) {
      moves_failed.push((file_move.clone(), e.to_string()));
      continue;
    }
    // Never overwrite: auto-rename on collision and record the real
    // destination so the ledger and the journal both stay truthful.
    let actual_to = unique_dest(&file_move.to);
    match move_file(&file_move.from, &actual_to) {
      Ok(()) => {
        run_journal.moves.push(JournalMove {
          from: file_move.from.clone(),
          to: actual_to.clone(),
        });
        moves_completed.push(FileMove {
          from: file_move.from.clone(),
          to: actual_to,
          group_id: file_move.group_id,
        });
      }
      Err(e) => {
        moves_failed.push((file_move.clone(), e.to_string()));
      }
    }
  }

  let mut deletions_staged = Vec::new();
  let mut bytes_staged = 0u64;
  for path in &plan.deletions {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let dest = trash::trash_path_for(&paths.trash_dir, &run_id, path);
    match move_file(path, &dest) {
      Ok(()) => {
        run_journal.deletions.push(JournalDeletion {
          original: path.clone(),
          trashed_to: dest,
          size,
        });
        bytes_staged += size;
        deletions_staged.push(path.clone());
      }
      Err(e) => {
        tracing::warn!(path = %path.display(), error = %e, "Failed to stage deletion");
      }
    }
  }

  let (journal_path, journal_error) = if run_journal.is_empty() {
    (None, None)
  } else {
    match run_journal.save(&paths.journal_dir) {
      Ok(path) => (Some(path), None),
      Err(e) => {
        tracing::error!(error = %e, "Failed to write run journal");
        (None, Some(e.to_string()))
      }
    }
  };

  ExecutionReport {
    run_id,
    moves_completed,
    moves_failed,
    deletions_staged,
    bytes_staged,
    journal_path,
    journal_error,
  }
}

fn validate_move_target(
  target: &Path,
  output_dir: &Path,
) -> Result<()> {
  let canonical_output = output_dir
    .canonicalize()
    .or_else(|_| {
      std::fs::create_dir_all(output_dir)?;
      output_dir.canonicalize()
    })
    .with_context(|| {
      format!("Cannot resolve output dir: {}", output_dir.display())
    })?;

  if let Some(parent) = target.parent() {
    std::fs::create_dir_all(parent).ok();
  }

  let canonical_target = target
    .parent()
    .unwrap_or(target)
    .canonicalize()
    .with_context(|| {
      format!("Cannot resolve target path: {}", target.display())
    })?;

  if !canonical_target.starts_with(&canonical_output) {
    anyhow::bail!(
      "Path traversal blocked: {} resolves outside output dir {}",
      target.display(),
      canonical_output.display()
    );
  }

  Ok(())
}

/// First non-existing variant of `to`: `name.ext`, `name (1).ext`, …
fn unique_dest(to: &Path) -> PathBuf {
  if !to.exists() {
    return to.to_path_buf();
  }
  let stem = to
    .file_stem()
    .map(|s| s.to_string_lossy().to_string())
    .unwrap_or_default();
  let ext = to.extension().map(|e| e.to_string_lossy().to_string());
  let parent = to.parent().unwrap_or(Path::new(""));
  for n in 1.. {
    let name = match &ext {
      Some(ext) => format!("{stem} ({n}).{ext}"),
      None => format!("{stem} ({n})"),
    };
    let candidate = parent.join(name);
    if !candidate.exists() {
      return candidate;
    }
  }
  unreachable!()
}

/// Move a file, falling back to copy + verify + remove when `rename`
/// can't cross filesystems.
fn move_file(from: &Path, to: &Path) -> Result<()> {
  if let Some(parent) = to.parent() {
    std::fs::create_dir_all(parent).with_context(|| {
      format!("Failed to create directory: {}", parent.display())
    })?;
  }

  match std::fs::rename(from, to) {
    Ok(()) => Ok(()),
    Err(rename_err) => {
      if !from.exists() {
        return Err(rename_err).with_context(|| {
          format!(
            "Failed to move {} -> {}",
            from.display(),
            to.display()
          )
        });
      }
      copy_verify_remove(from, to).with_context(|| {
        format!(
          "Cross-device move failed {} -> {}",
          from.display(),
          to.display()
        )
      })
    }
  }
}

fn copy_verify_remove(from: &Path, to: &Path) -> Result<()> {
  std::fs::copy(from, to).context("copy failed")?;
  let src_hash = crate::fingerprint::compute_blake3(from)?;
  let dst_hash = crate::fingerprint::compute_blake3(to)?;
  if src_hash != dst_hash {
    let _ = std::fs::remove_file(to);
    anyhow::bail!("copy verification failed (content mismatch)");
  }
  std::fs::remove_file(from).context("failed to remove source")?;
  Ok(())
}

pub struct UndoReport {
  pub run_id: String,
  pub restored: Vec<PathBuf>,
  pub failed: Vec<(PathBuf, String)>,
}

/// Reverse a journaled run: moves go back where they came from and
/// staged deletions are restored from trash. `None` targets the most
/// recent run that hasn't been undone. Individual failures (e.g. a
/// purged trash entry) are collected, not fatal.
pub fn undo_run(
  paths: &ExecutorPaths,
  run_id: Option<&str>,
) -> Result<UndoReport> {
  let run_id = match run_id {
    Some(id) => id.to_string(),
    None => journal::list_runs(&paths.journal_dir)
      .into_iter()
      .next()
      .context("Nothing to undo: no journaled runs found")?,
  };
  let run_journal = journal::load(&paths.journal_dir, &run_id)?;

  let mut restored = Vec::new();
  let mut failed = Vec::new();

  // Reverse moves last-first so nested collisions unwind cleanly.
  for mv in run_journal.moves.iter().rev() {
    match move_file(&mv.to, &mv.from) {
      Ok(()) => restored.push(mv.from.clone()),
      Err(e) => failed.push((mv.from.clone(), e.to_string())),
    }
  }
  for del in &run_journal.deletions {
    match move_file(&del.trashed_to, &del.original) {
      Ok(()) => restored.push(del.original.clone()),
      Err(e) => failed.push((del.original.clone(), e.to_string())),
    }
  }

  if failed.is_empty() {
    journal::mark_undone(&paths.journal_dir, &run_id)?;
  }

  Ok(UndoReport {
    run_id,
    restored,
    failed,
  })
}

/// Legacy undo for logs written by pre-journal versions (`--undo-log`).
pub fn undo(undo_log_path: &Path) -> Result<Vec<PathBuf>> {
  let content =
    std::fs::read_to_string(undo_log_path).with_context(|| {
      format!("Failed to read undo log: {}", undo_log_path.display())
    })?;
  let log: UndoLog = serde_json::from_str(&content)
    .context("Failed to parse undo log")?;

  let mut restored = Vec::new();
  for entry in &log.moves {
    move_file(&entry.moved_to, &entry.original).with_context(
      || {
        format!(
          "Failed to undo move: {} -> {}",
          entry.moved_to.display(),
          entry.original.display()
        )
      },
    )?;
    restored.push(entry.original.clone());
  }

  Ok(restored)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;
  use tempfile::TempDir;

  fn make_test_move(
    src_dir: &Path,
    dest_dir: &Path,
    name: &str,
  ) -> FileMove {
    let from = src_dir.join(name);
    fs::write(&from, format!("content of {name}")).unwrap();
    FileMove {
      from,
      to: dest_dir.join(name),
      group_id: 0,
    }
  }

  fn paths(dir: &TempDir) -> ExecutorPaths {
    ExecutorPaths::under(&dir.path().join("state"))
  }

  #[test]
  fn executes_single_file_move() {
    let src = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();

    let plan = ApprovedPlan {
      moves: vec![make_test_move(
        src.path(),
        dest.path(),
        "photo.jpg",
      )],
      deletions: vec![],
      skipped_files: vec![],
    };

    let report = execute_plan(&plan, &paths(&src), dest.path());

    assert_eq!(report.moves_completed.len(), 1);
    assert!(report.moves_failed.is_empty());
    assert!(dest.path().join("photo.jpg").exists());
    assert!(!src.path().join("photo.jpg").exists());
  }

  #[test]
  fn creates_destination_directories() {
    let src = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    let nested_dest = dest.path().join("deep/nested/dir");

    let from = src.path().join("file.jpg");
    fs::write(&from, "data").unwrap();
    let plan = ApprovedPlan {
      moves: vec![FileMove {
        from,
        to: nested_dest.join("file.jpg"),
        group_id: 0,
      }],
      deletions: vec![],
      skipped_files: vec![],
    };

    let report = execute_plan(&plan, &paths(&src), dest.path());

    assert_eq!(report.moves_completed.len(), 1);
    assert!(nested_dest.join("file.jpg").exists());
  }

  #[test]
  fn reports_failed_moves() {
    let dest = TempDir::new().unwrap();

    let plan = ApprovedPlan {
      moves: vec![FileMove {
        from: PathBuf::from("/nonexistent/source.jpg"),
        to: dest.path().join("dest.jpg"),
        group_id: 0,
      }],
      deletions: vec![],
      skipped_files: vec![],
    };

    let report = execute_plan(&plan, &paths(&dest), dest.path());

    assert!(report.moves_completed.is_empty());
    assert_eq!(report.moves_failed.len(), 1);
  }

  #[test]
  fn collision_auto_renames_instead_of_overwriting() {
    let src = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    fs::write(dest.path().join("photo.jpg"), "EXISTING").unwrap();

    let plan = ApprovedPlan {
      moves: vec![make_test_move(
        src.path(),
        dest.path(),
        "photo.jpg",
      )],
      deletions: vec![],
      skipped_files: vec![],
    };

    let report = execute_plan(&plan, &paths(&src), dest.path());

    assert_eq!(report.moves_completed.len(), 1);
    // Pre-existing file untouched, new file renamed alongside it.
    assert_eq!(
      fs::read_to_string(dest.path().join("photo.jpg")).unwrap(),
      "EXISTING"
    );
    let renamed = dest.path().join("photo (1).jpg");
    assert!(renamed.exists());
    assert_eq!(report.moves_completed[0].to, renamed);
  }

  #[test]
  fn writes_journal_with_actual_destinations() {
    let src = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    let paths = paths(&src);

    let plan = ApprovedPlan {
      moves: vec![make_test_move(src.path(), dest.path(), "a.jpg")],
      deletions: vec![],
      skipped_files: vec![],
    };

    let report = execute_plan(&plan, &paths, dest.path());

    let journal_path = report.journal_path.clone().unwrap();
    assert!(journal_path.exists());
    let loaded =
      journal::load(&paths.journal_dir, &report.run_id).unwrap();
    assert_eq!(loaded.moves.len(), 1);
    assert_eq!(loaded.moves[0].to, dest.path().join("a.jpg"));
  }

  #[test]
  fn empty_plan_writes_no_journal() {
    let dir = TempDir::new().unwrap();

    let plan = ApprovedPlan {
      moves: vec![],
      deletions: vec![],
      skipped_files: vec![],
    };

    let report = execute_plan(&plan, &paths(&dir), dir.path());

    assert!(report.journal_path.is_none());
    assert!(journal::list_runs(&paths(&dir).journal_dir).is_empty());
  }

  #[test]
  fn undo_restores_moves() {
    let src = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    let paths = paths(&src);

    let plan = ApprovedPlan {
      moves: vec![make_test_move(
        src.path(),
        dest.path(),
        "restore.jpg",
      )],
      deletions: vec![],
      skipped_files: vec![],
    };

    execute_plan(&plan, &paths, dest.path());
    assert!(!src.path().join("restore.jpg").exists());

    let undo = undo_run(&paths, None).unwrap();

    assert_eq!(undo.restored.len(), 1);
    assert!(undo.failed.is_empty());
    assert!(src.path().join("restore.jpg").exists());
    assert!(!dest.path().join("restore.jpg").exists());
    // Undone run no longer shows up as undoable.
    assert!(journal::list_runs(&paths.journal_dir).is_empty());
  }

  #[test]
  fn deletions_are_staged_to_trash_and_undoable() {
    let dir = TempDir::new().unwrap();
    let paths = paths(&dir);
    let dupe = dir.path().join("dupe.jpg");
    fs::write(&dupe, "duplicate").unwrap();

    let plan = ApprovedPlan {
      moves: vec![],
      deletions: vec![dupe.clone()],
      skipped_files: vec![],
    };

    let report = execute_plan(&plan, &paths, dir.path());

    assert_eq!(report.deletions_staged.len(), 1);
    assert_eq!(report.bytes_staged, 9);
    assert!(!dupe.exists());
    // The bytes are still on disk, inside the trash.
    let journal =
      journal::load(&paths.journal_dir, &report.run_id).unwrap();
    assert!(journal.deletions[0].trashed_to.exists());

    let undo = undo_run(&paths, Some(&report.run_id)).unwrap();

    assert!(undo.failed.is_empty());
    assert!(dupe.exists());
    assert_eq!(fs::read_to_string(&dupe).unwrap(), "duplicate");
  }

  #[test]
  fn purge_after_staging_frees_space() {
    let dir = TempDir::new().unwrap();
    let paths = paths(&dir);
    let dupe = dir.path().join("dupe.jpg");
    fs::write(&dupe, vec![0u8; 128]).unwrap();

    let plan = ApprovedPlan {
      moves: vec![],
      deletions: vec![dupe],
      skipped_files: vec![],
    };
    execute_plan(&plan, &paths, dir.path());

    let report = trash::purge(&paths.trash_dir, None).unwrap();

    assert_eq!(report.runs_purged, 1);
    assert_eq!(report.bytes_freed, 128);
  }

  #[test]
  fn undo_tolerates_purged_trash() {
    let dir = TempDir::new().unwrap();
    let paths = paths(&dir);
    let dupe = dir.path().join("dupe.jpg");
    fs::write(&dupe, "x").unwrap();

    let plan = ApprovedPlan {
      moves: vec![],
      deletions: vec![dupe.clone()],
      skipped_files: vec![],
    };
    let report = execute_plan(&plan, &paths, dir.path());
    trash::purge(&paths.trash_dir, None).unwrap();

    let undo = undo_run(&paths, Some(&report.run_id)).unwrap();

    assert!(undo.restored.is_empty());
    assert_eq!(undo.failed.len(), 1);
    assert!(!dupe.exists());
  }

  #[test]
  fn undo_deletion_of_nonexistent_file_is_graceful() {
    let dir = TempDir::new().unwrap();

    let plan = ApprovedPlan {
      moves: vec![],
      deletions: vec![PathBuf::from("/nonexistent/ghost.jpg")],
      skipped_files: vec![],
    };

    let report = execute_plan(&plan, &paths(&dir), dir.path());

    assert!(report.deletions_staged.is_empty());
  }

  #[test]
  fn multiple_moves_execute_sequentially() {
    let src = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();

    let plan = ApprovedPlan {
      moves: vec![
        make_test_move(src.path(), dest.path(), "first.jpg"),
        make_test_move(src.path(), dest.path(), "second.jpg"),
        make_test_move(src.path(), dest.path(), "third.jpg"),
      ],
      deletions: vec![],
      skipped_files: vec![],
    };

    let report = execute_plan(&plan, &paths(&src), dest.path());

    assert_eq!(report.moves_completed.len(), 3);
    assert!(dest.path().join("first.jpg").exists());
    assert!(dest.path().join("second.jpg").exists());
    assert!(dest.path().join("third.jpg").exists());
  }

  #[test]
  fn undo_run_errors_when_no_runs() {
    let dir = TempDir::new().unwrap();

    let result = undo_run(&paths(&dir), None);

    assert!(result.is_err());
  }

  #[test]
  fn legacy_undo_restores_files() {
    let src = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    let moved = dest.path().join("a.jpg");
    fs::write(&moved, "data").unwrap();

    let log = UndoLog {
      moves: vec![UndoEntry {
        original: src.path().join("a.jpg"),
        moved_to: moved.clone(),
      }],
      timestamp: "t".to_string(),
    };
    let log_path = dest.path().join("undo.json");
    fs::write(&log_path, serde_json::to_string(&log).unwrap())
      .unwrap();

    let restored = undo(&log_path).unwrap();

    assert_eq!(restored.len(), 1);
    assert!(src.path().join("a.jpg").exists());
    assert!(!moved.exists());
  }

  #[test]
  fn legacy_undo_fails_on_missing_log() {
    let result = undo(Path::new("/nonexistent/undo.json"));

    assert!(result.is_err());
  }

  #[test]
  fn blocks_path_traversal_outside_output_dir() {
    let src = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();

    let from = src.path().join("evil.jpg");
    fs::write(&from, "data").unwrap();

    let plan = ApprovedPlan {
      moves: vec![FileMove {
        from,
        to: output.path().join("../../etc/evil.jpg"),
        group_id: 0,
      }],
      deletions: vec![],
      skipped_files: vec![],
    };

    let report = execute_plan(&plan, &paths(&src), output.path());

    assert!(report.moves_completed.is_empty());
    assert_eq!(report.moves_failed.len(), 1);
    assert!(report.moves_failed[0].1.contains("traversal"));
  }

  #[test]
  fn unique_dest_counts_up() {
    let dir = TempDir::new().unwrap();
    let base = dir.path().join("f.txt");
    fs::write(&base, "0").unwrap();
    fs::write(dir.path().join("f (1).txt"), "1").unwrap();

    assert_eq!(unique_dest(&base), dir.path().join("f (2).txt"));
  }
}

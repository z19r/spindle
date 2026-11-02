pub mod journal;
pub mod trash;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{ApprovedPlan, ExecutionReport, FileMove};
use journal::{JournalDeletion, JournalMove, JournalWriter};

/// Where run journals and staged trash live.
#[derive(Debug, Clone)]
pub struct ExecutorPaths {
  pub journal_dir: PathBuf,
  pub trash_dir: PathBuf,
}

impl ExecutorPaths {
  /// Global defaults: `<data_dir>/spindle/{journal,trash}`.
  pub fn default_paths() -> Self {
    Self::under(&crate::config::data_dir())
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
  let mut report = ExecutionReport {
    run_id: run_id.clone(),
    moves_completed: Vec::new(),
    moves_failed: Vec::new(),
    deletions_staged: Vec::new(),
    bytes_staged: 0,
    journal_path: None,
    journal_error: None,
  };

  // Nothing moves until the run can be journaled: an unrecorded move
  // could never be undone.
  let mut writer = match JournalWriter::begin(
    &paths.journal_dir,
    &run_id,
  ) {
    Ok(writer) => writer,
    Err(e) => {
      tracing::error!(error = %e, "Cannot journal this run; not starting");
      let reason = format!("not started: {e:#}");
      report.moves_failed = plan
        .moves
        .iter()
        .map(|mv| (mv.clone(), reason.clone()))
        .collect();
      report.journal_error = Some(format!("{e:#}"));
      return report;
    }
  };

  for (i, file_move) in plan.moves.iter().enumerate() {
    if let Err(e) = validate_move_target(&file_move.to, output_dir) {
      report.moves_failed.push((file_move.clone(), e.to_string()));
      continue;
    }
    // Never overwrite: auto-rename on collision and record the real
    // destination so the ledger and the journal both stay truthful.
    let actual_to = unique_dest(&file_move.to);
    if let Err(e) = move_file(&file_move.from, &actual_to) {
      report.moves_failed.push((file_move.clone(), e.to_string()));
      continue;
    }
    report.moves_completed.push(FileMove {
      from: file_move.from.clone(),
      to: actual_to.clone(),
      group_id: file_move.group_id,
    });
    let step = JournalMove {
      from: file_move.from.clone(),
      to: actual_to,
    };
    if let Err(e) = writer.record_move(step) {
      // The journal can no longer keep up; stop before moving files
      // it would not know about.
      let reason = "not started: the run journal stopped writing";
      report.moves_failed.extend(
        plan.moves[i + 1..]
          .iter()
          .map(|mv| (mv.clone(), reason.to_string())),
      );
      return finish_report(report, writer, Some(e));
    }
  }

  for path in &plan.deletions {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let dest = trash::trash_path_for(&paths.trash_dir, &run_id, path);
    if let Err(e) = move_file(path, &dest) {
      tracing::warn!(
        path = %path.display(),
        error = %e,
        "Failed to stage deletion"
      );
      continue;
    }
    report.bytes_staged += size;
    report.deletions_staged.push(path.clone());
    let step = JournalDeletion {
      original: path.clone(),
      trashed_to: dest,
      size,
    };
    if let Err(e) = writer.record_deletion(step) {
      return finish_report(report, writer, Some(e));
    }
  }

  finish_report(report, writer, None)
}

/// Write the finished journal into the report, keeping the first
/// journal error if one already happened.
fn finish_report(
  mut report: ExecutionReport,
  writer: JournalWriter,
  error: Option<anyhow::Error>,
) -> ExecutionReport {
  match writer.finish() {
    Ok(path) => report.journal_path = path,
    Err(e) => {
      tracing::error!(error = %e, "Failed to write run journal");
      report.journal_error = Some(format!("{e:#}"));
    }
  }
  if let Some(e) = error {
    tracing::error!(error = %e, "Run journal stopped writing");
    report.journal_error = Some(format!("{e:#}"));
  }
  report
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
  /// Restores whose original path was taken by something else, as
  /// (wanted, actually restored to). The file is back and nothing was
  /// destroyed, but it does not have its old name.
  pub renamed: Vec<(PathBuf, PathBuf)>,
  /// Organized destinations whose file was moved back. Their ledger
  /// entries must go, or the next run would see the restored file at
  /// its old path and skip it as already organized.
  pub unfiled: Vec<PathBuf>,
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
  let mut run_journal = journal::load(&paths.journal_dir, &run_id)?;

  let mut restored = Vec::new();
  let mut failed = Vec::new();
  let mut renamed = Vec::new();
  let mut unfiled = Vec::new();

  // Reverse moves last-first so nested collisions unwind cleanly.
  let moves = std::mem::take(&mut run_journal.moves);
  for mv in moves.into_iter().rev() {
    if restore(
      &mv.to,
      &mv.from,
      &mut restored,
      &mut failed,
      &mut renamed,
    ) {
      unfiled.push(mv.to);
    } else {
      run_journal.moves.insert(0, mv);
    }
  }
  let deletions = std::mem::take(&mut run_journal.deletions);
  for del in deletions {
    if !restore(
      &del.trashed_to,
      &del.original,
      &mut restored,
      &mut failed,
      &mut renamed,
    ) {
      run_journal.deletions.push(del);
    }
  }

  // Keep only what is still out of place, so a retry once the cause
  // is fixed finishes the undo instead of redoing the restored files.
  if failed.is_empty() {
    journal::mark_undone(&paths.journal_dir, &run_id)?;
  } else {
    run_journal.save(&paths.journal_dir)?;
  }

  Ok(UndoReport {
    run_id,
    restored,
    failed,
    renamed,
    unfiled,
  })
}

/// Put one file back, never on top of another.
///
/// `execute_plan` auto-renames rather than overwrite; undo has to do
/// the same, because `move_file` starts with `rename`, which replaces
/// the destination without a word. The path a run emptied can be
/// occupied again by the time it is undone — a re-download with the
/// same name, or a later run that filed something there — and undo is
/// the operation people reach for precisely because they expect it not
/// to lose anything.
fn restore(
  from: &Path,
  wanted: &Path,
  restored: &mut Vec<PathBuf>,
  failed: &mut Vec<(PathBuf, String)>,
  renamed: &mut Vec<(PathBuf, PathBuf)>,
) -> bool {
  let dest = unique_dest(wanted);
  match move_file(from, &dest) {
    Ok(()) => {
      if dest != wanted {
        renamed.push((wanted.to_path_buf(), dest.clone()));
      }
      restored.push(dest);
      true
    }
    Err(e) => {
      failed.push((wanted.to_path_buf(), e.to_string()));
      false
    }
  }
}

/// Legacy undo for logs written by pre-journal versions (`--undo-log`).
/// Restores like [`undo_run`]: never over another file, and one
/// failure does not stop the rest.
pub fn undo(undo_log_path: &Path) -> Result<UndoReport> {
  let content =
    std::fs::read_to_string(undo_log_path).with_context(|| {
      format!("Failed to read undo log: {}", undo_log_path.display())
    })?;
  let log: UndoLog = serde_json::from_str(&content)
    .context("Failed to parse undo log")?;

  let mut report = UndoReport {
    run_id: undo_log_path.display().to_string(),
    restored: Vec::new(),
    failed: Vec::new(),
    renamed: Vec::new(),
    unfiled: Vec::new(),
  };
  for entry in log.moves.iter().rev() {
    if restore(
      &entry.moved_to,
      &entry.original,
      &mut report.restored,
      &mut report.failed,
      &mut report.renamed,
    ) {
      report.unfiled.push(entry.moved_to.clone());
    }
  }
  Ok(report)
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

  /// A run that cannot be journaled could not be undone, so it must
  /// not move anything.
  #[test]
  fn nothing_moves_when_the_journal_cannot_be_written() {
    let src = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    let blocked = src.path().join("not-a-dir");
    fs::write(&blocked, "a file where the journal dir should be")
      .unwrap();
    let paths = ExecutorPaths {
      journal_dir: blocked,
      trash_dir: src.path().join("trash"),
    };
    let plan = ApprovedPlan {
      moves: vec![make_test_move(src.path(), dest.path(), "a.jpg")],
      deletions: vec![],
      skipped_files: vec![],
    };

    let report = execute_plan(&plan, &paths, dest.path());

    assert!(report.moves_completed.is_empty());
    assert_eq!(report.moves_failed.len(), 1);
    assert!(report.journal_error.is_some());
    assert!(src.path().join("a.jpg").exists());
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
    assert_eq!(undo.unfiled, vec![dest.path().join("restore.jpg")]);
    // Undone run no longer shows up as undoable.
    assert!(journal::list_runs(&paths.journal_dir).is_empty());
  }

  /// The path a run emptied can be occupied again before the undo —
  /// a re-download with the same name is enough. Undo must not put
  /// the old file back on top of it.
  #[test]
  fn undo_does_not_overwrite_a_reclaimed_origin() {
    let src = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    let paths = paths(&src);
    let origin = src.path().join("restore.jpg");

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
    assert!(!origin.exists());

    // Something else takes the name while the run is organized away.
    fs::write(&origin, "the new one").unwrap();

    let undo = undo_run(&paths, None).unwrap();

    assert!(undo.failed.is_empty());
    assert_eq!(
      fs::read_to_string(&origin).unwrap(),
      "the new one",
      "undo overwrote the file that had taken the path back"
    );
    let beside = src.path().join("restore (1).jpg");
    assert!(beside.exists(), "the undone file went nowhere");
    assert_eq!(undo.restored, vec![beside.clone()]);
    assert_eq!(undo.renamed, vec![(origin, beside)]);
  }

  /// Same hazard on the deletions half: a staged duplicate goes back
  /// to a path that is no longer free.
  #[test]
  fn restoring_a_deletion_does_not_overwrite_the_original_path() {
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
    assert!(!dupe.exists());

    fs::write(&dupe, "something else entirely").unwrap();

    let undo = undo_run(&paths, Some(&report.run_id)).unwrap();

    assert!(undo.failed.is_empty());
    assert_eq!(
      fs::read_to_string(&dupe).unwrap(),
      "something else entirely"
    );
    let beside = dir.path().join("dupe (1).jpg");
    assert_eq!(fs::read_to_string(&beside).unwrap(), "duplicate");
    assert_eq!(undo.renamed, vec![(dupe, beside)]);
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
  fn a_failed_undo_can_be_retried() {
    let src = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    let paths = paths(&src);
    let plan = ApprovedPlan {
      moves: vec![
        make_test_move(src.path(), dest.path(), "a.jpg"),
        make_test_move(src.path(), dest.path(), "b.jpg"),
      ],
      deletions: vec![],
      skipped_files: vec![],
    };
    execute_plan(&plan, &paths, dest.path());
    // b.jpg goes missing from the organized folder for a while.
    let held = src.path().join("held.jpg");
    fs::rename(dest.path().join("b.jpg"), &held).unwrap();

    let first = undo_run(&paths, None).unwrap();
    assert_eq!(first.restored, vec![src.path().join("a.jpg")]);
    assert_eq!(first.failed.len(), 1);

    fs::rename(&held, dest.path().join("b.jpg")).unwrap();
    let second = undo_run(&paths, None).unwrap();

    assert_eq!(second.restored, vec![src.path().join("b.jpg")]);
    assert!(second.failed.is_empty());
    assert!(journal::list_runs(&paths.journal_dir).is_empty());
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

    let report = undo(&log_path).unwrap();

    assert_eq!(report.restored.len(), 1);
    assert_eq!(report.unfiled, vec![moved.clone()]);
    assert!(src.path().join("a.jpg").exists());
    assert!(!moved.exists());
  }

  #[test]
  fn legacy_undo_keeps_going_and_never_overwrites() {
    let src = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    let moved = dest.path().join("a.jpg");
    fs::write(&moved, "old").unwrap();
    // Something new took the original name since the run.
    fs::write(src.path().join("a.jpg"), "new").unwrap();

    let log = UndoLog {
      moves: vec![
        UndoEntry {
          original: src.path().join("gone.jpg"),
          moved_to: dest.path().join("gone.jpg"),
        },
        UndoEntry {
          original: src.path().join("a.jpg"),
          moved_to: moved.clone(),
        },
      ],
      timestamp: "t".to_string(),
    };
    let log_path = dest.path().join("undo.json");
    fs::write(&log_path, serde_json::to_string(&log).unwrap())
      .unwrap();

    let report = undo(&log_path).unwrap();

    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.renamed.len(), 1);
    let new = fs::read_to_string(src.path().join("a.jpg")).unwrap();
    assert_eq!(new, "new");
    let restored = fs::read_to_string(&report.renamed[0].1).unwrap();
    assert_eq!(restored, "old");
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

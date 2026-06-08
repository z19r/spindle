use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{ApprovedPlan, ExecutionReport, FileMove};

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
  undo_log_path: &Path,
  output_dir: &Path,
) -> ExecutionReport {
  let mut moves_completed = Vec::new();
  let mut moves_failed = Vec::new();
  let mut undo_entries = Vec::new();

  for file_move in &plan.moves {
    if let Err(e) = validate_move_target(&file_move.to, output_dir) {
      moves_failed.push((file_move.clone(), e.to_string()));
      continue;
    }
    match execute_single_move(file_move) {
      Ok(()) => {
        undo_entries.push(UndoEntry {
          original: file_move.from.clone(),
          moved_to: file_move.to.clone(),
        });
        moves_completed.push(file_move.clone());
      }
      Err(e) => {
        moves_failed.push((file_move.clone(), e.to_string()));
      }
    }
  }

  let mut deletions_completed = Vec::new();
  for path in &plan.deletions {
    if let Err(e) = std::fs::remove_file(path) {
      tracing::warn!(path = %path.display(), error = %e, "Failed to delete file");
    } else {
      deletions_completed.push(path.clone());
    }
  }

  let undo_log = UndoLog {
    moves: undo_entries,
    timestamp: chrono::Utc::now().to_rfc3339(),
  };
  let undo_log_error = match write_undo_log(undo_log_path, &undo_log)
  {
    Ok(()) => None,
    Err(e) => {
      tracing::error!(path = %undo_log_path.display(), error = %e, "Failed to write undo log");
      Some(e.to_string())
    }
  };

  ExecutionReport {
    moves_completed,
    moves_failed,
    deletions_completed,
    undo_log_path: undo_log_path.to_path_buf(),
    undo_log_error,
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

fn execute_single_move(file_move: &FileMove) -> Result<()> {
  if let Some(parent) = file_move.to.parent() {
    std::fs::create_dir_all(parent).with_context(|| {
      format!("Failed to create directory: {}", parent.display())
    })?;
  }

  std::fs::rename(&file_move.from, &file_move.to).with_context(|| {
    format!(
      "Failed to move {} -> {}",
      file_move.from.display(),
      file_move.to.display()
    )
  })
}

fn write_undo_log(path: &Path, log: &UndoLog) -> Result<()> {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent).with_context(|| {
      format!("Failed to create undo log dir: {}", parent.display())
    })?;
  }
  let json = serde_json::to_string_pretty(log)
    .context("Failed to serialize undo log")?;
  std::fs::write(path, json).with_context(|| {
    format!("Failed to write undo log: {}", path.display())
  })?;
  Ok(())
}

pub fn undo(undo_log_path: &Path) -> Result<Vec<PathBuf>> {
  let content =
    std::fs::read_to_string(undo_log_path).with_context(|| {
      format!("Failed to read undo log: {}", undo_log_path.display())
    })?;
  let log: UndoLog = serde_json::from_str(&content)
    .context("Failed to parse undo log")?;

  let mut restored = Vec::new();
  for entry in &log.moves {
    if let Some(parent) = entry.original.parent() {
      std::fs::create_dir_all(parent).ok();
    }
    std::fs::rename(&entry.moved_to, &entry.original).with_context(
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

  #[test]
  fn executes_single_file_move() {
    let src = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    let undo_path = src.path().join("undo.json");

    let plan = ApprovedPlan {
      moves: vec![make_test_move(
        src.path(),
        dest.path(),
        "photo.jpg",
      )],
      deletions: vec![],
      skipped_files: vec![],
    };

    let report = execute_plan(&plan, &undo_path, dest.path());

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
    let undo_path = src.path().join("undo.json");

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

    let report = execute_plan(&plan, &undo_path, dest.path());

    assert_eq!(report.moves_completed.len(), 1);
    assert!(nested_dest.join("file.jpg").exists());
  }

  #[test]
  fn reports_failed_moves() {
    let dest = TempDir::new().unwrap();
    let undo_path = dest.path().join("undo.json");

    let plan = ApprovedPlan {
      moves: vec![FileMove {
        from: PathBuf::from("/nonexistent/source.jpg"),
        to: dest.path().join("dest.jpg"),
        group_id: 0,
      }],
      deletions: vec![],
      skipped_files: vec![],
    };

    let report = execute_plan(&plan, &undo_path, dest.path());

    assert!(report.moves_completed.is_empty());
    assert_eq!(report.moves_failed.len(), 1);
  }

  #[test]
  fn writes_undo_log() {
    let src = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    let undo_path = dest.path().join("undo.json");

    let plan = ApprovedPlan {
      moves: vec![make_test_move(src.path(), dest.path(), "a.jpg")],
      deletions: vec![],
      skipped_files: vec![],
    };

    execute_plan(&plan, &undo_path, dest.path());

    assert!(undo_path.exists());
    let content = fs::read_to_string(&undo_path).unwrap();
    let log: UndoLog = serde_json::from_str(&content).unwrap();
    assert_eq!(log.moves.len(), 1);
  }

  #[test]
  fn undo_restores_files() {
    let src = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    let undo_path = dest.path().join("undo.json");

    let plan = ApprovedPlan {
      moves: vec![make_test_move(
        src.path(),
        dest.path(),
        "restore.jpg",
      )],
      deletions: vec![],
      skipped_files: vec![],
    };

    execute_plan(&plan, &undo_path, dest.path());
    assert!(!src.path().join("restore.jpg").exists());
    assert!(dest.path().join("restore.jpg").exists());

    let restored = undo(&undo_path).unwrap();

    assert_eq!(restored.len(), 1);
    assert!(src.path().join("restore.jpg").exists());
    assert!(!dest.path().join("restore.jpg").exists());
  }

  #[test]
  fn handles_deletions() {
    let dir = TempDir::new().unwrap();
    let file_to_delete = dir.path().join("dupe.jpg");
    fs::write(&file_to_delete, "duplicate").unwrap();
    let undo_path = dir.path().join("undo.json");

    let plan = ApprovedPlan {
      moves: vec![],
      deletions: vec![file_to_delete.clone()],
      skipped_files: vec![],
    };

    let report = execute_plan(&plan, &undo_path, dir.path());

    assert_eq!(report.deletions_completed.len(), 1);
    assert!(!file_to_delete.exists());
  }

  #[test]
  fn deletion_of_nonexistent_file_is_graceful() {
    let dir = TempDir::new().unwrap();
    let undo_path = dir.path().join("undo.json");

    let plan = ApprovedPlan {
      moves: vec![],
      deletions: vec![PathBuf::from("/nonexistent/ghost.jpg")],
      skipped_files: vec![],
    };

    let report = execute_plan(&plan, &undo_path, dir.path());

    assert!(report.deletions_completed.is_empty());
  }

  #[test]
  fn multiple_moves_execute_sequentially() {
    let src = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    let undo_path = src.path().join("undo.json");

    let plan = ApprovedPlan {
      moves: vec![
        make_test_move(src.path(), dest.path(), "first.jpg"),
        make_test_move(src.path(), dest.path(), "second.jpg"),
        make_test_move(src.path(), dest.path(), "third.jpg"),
      ],
      deletions: vec![],
      skipped_files: vec![],
    };

    let report = execute_plan(&plan, &undo_path, dest.path());

    assert_eq!(report.moves_completed.len(), 3);
    assert!(dest.path().join("first.jpg").exists());
    assert!(dest.path().join("second.jpg").exists());
    assert!(dest.path().join("third.jpg").exists());
  }

  #[test]
  fn undo_log_path_stored_in_report() {
    let dir = TempDir::new().unwrap();
    let undo_path = dir.path().join("my_undo.json");

    let plan = ApprovedPlan {
      moves: vec![],
      deletions: vec![],
      skipped_files: vec![],
    };

    let report = execute_plan(&plan, &undo_path, dir.path());

    assert_eq!(report.undo_log_path, undo_path);
  }

  #[test]
  fn undo_fails_on_missing_log() {
    let result = undo(Path::new("/nonexistent/undo.json"));

    assert!(result.is_err());
  }

  #[test]
  fn blocks_path_traversal_outside_output_dir() {
    let src = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let undo_path = output.path().join("undo.json");

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

    let report = execute_plan(&plan, &undo_path, output.path());

    assert!(report.moves_completed.is_empty());
    assert_eq!(report.moves_failed.len(), 1);
    assert!(report.moves_failed[0].1.contains("traversal"));
  }
}

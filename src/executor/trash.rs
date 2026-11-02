//! Staged trash: "deleted" files move to a per-run trash directory instead
//! of being removed, so every deletion is undoable until an explicit purge.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::{journal, ExecutorPaths};

/// Where a file lands inside the trash: the original absolute path is
/// mirrored under `<trash_dir>/<run_id>/` so restores are unambiguous.
pub fn trash_path_for(
  trash_dir: &Path,
  run_id: &str,
  original: &Path,
) -> PathBuf {
  let mut dest = trash_dir.join(run_id);
  for component in original.components() {
    use std::path::Component;
    match component {
      Component::Normal(part) => dest.push(part),
      Component::Prefix(prefix) => {
        // Windows drive prefix — keep it as a plain directory name.
        dest
          .push(prefix.as_os_str().to_string_lossy().replace(':', ""))
      }
      _ => {}
    }
  }
  dest
}

pub struct PurgeReport {
  pub runs_purged: usize,
  pub bytes_freed: u64,
}

/// The directory a deletion on another drive is staged in, at the
/// top of that drive.
pub const FOREIGN_TRASH: &str = ".spindle-trash";

/// Where to stage `file` for deletion: the usual trash when it is on
/// the same drive, so staging is a rename. Otherwise `.spindle-trash`
/// at the top of the file's own drive, so deleting a large file from
/// an external disk does not copy it onto the home drive. Falls back
/// to the usual trash when that cannot be created.
pub fn trash_dir_for(trash_dir: &Path, file: &Path) -> PathBuf {
  #[cfg(unix)]
  if let Some(root) = foreign_root(trash_dir, file) {
    if std::fs::create_dir_all(&root).is_ok() {
      return root;
    }
  }
  trash_dir.to_path_buf()
}

#[cfg(unix)]
fn foreign_root(trash_dir: &Path, file: &Path) -> Option<PathBuf> {
  use std::os::unix::fs::MetadataExt;
  let dev = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
  let file_dev = dev(file)?;
  // The trash dir may not exist yet; its nearest ancestor that does
  // is on the same drive.
  let trash_dev = trash_dir.ancestors().find_map(dev)?;
  if file_dev == trash_dev {
    return None;
  }
  let mount = file
    .ancestors()
    .skip(1)
    .take_while(|a| dev(a) == Some(file_dev))
    .last()?;
  Some(mount.join(FOREIGN_TRASH))
}

/// Permanently delete staged trash: the usual trash and every
/// `.spindle-trash` an undoable run staged files in. `older_than_days:
/// None` purges everything; otherwise only run directories whose
/// newest content is older than the cutoff.
pub fn purge(
  paths: &ExecutorPaths,
  older_than_days: Option<u64>,
) -> Result<PurgeReport> {
  let mut report = PurgeReport {
    runs_purged: 0,
    bytes_freed: 0,
  };
  let cutoff = older_than_days.map(|days| {
    std::time::SystemTime::now()
      - std::time::Duration::from_secs(days * 24 * 60 * 60)
  });

  let mut roots = vec![paths.trash_dir.clone()];
  for run_id in journal::list_runs(&paths.journal_dir) {
    let Ok(run) = journal::load(&paths.journal_dir, &run_id) else {
      continue;
    };
    for del in &run.deletions {
      let root = del
        .trashed_to
        .ancestors()
        .find(|a| a.file_name() == Some(FOREIGN_TRASH.as_ref()));
      if let Some(root) = root {
        if !roots.iter().any(|r| r == root) {
          roots.push(root.to_path_buf());
        }
      }
    }
  }

  for root in &roots {
    purge_root(root, cutoff, &mut report)?;
  }
  Ok(report)
}

fn purge_root(
  trash_dir: &Path,
  cutoff: Option<std::time::SystemTime>,
  report: &mut PurgeReport,
) -> Result<()> {
  let entries = match std::fs::read_dir(trash_dir) {
    Ok(entries) => entries,
    Err(_) => return Ok(()), // nothing staged yet
  };

  for entry in entries.filter_map(|e| e.ok()) {
    let path = entry.path();
    if !path.is_dir() {
      continue;
    }
    if let Some(cutoff) = cutoff {
      let modified = entry
        .metadata()
        .and_then(|m| m.modified())
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
      if modified > cutoff {
        continue;
      }
    }
    report.bytes_freed += dir_size(&path);
    std::fs::remove_dir_all(&path).with_context(|| {
      format!("Failed to purge {}", path.display())
    })?;
    report.runs_purged += 1;
  }
  Ok(())
}

fn dir_size(dir: &Path) -> u64 {
  walkdir::WalkDir::new(dir)
    .into_iter()
    .filter_map(|e| e.ok())
    .filter(|e| e.file_type().is_file())
    .filter_map(|e| e.metadata().ok())
    .map(|m| m.len())
    .sum()
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

  #[test]
  fn trash_path_mirrors_original() {
    let dest = trash_path_for(
      Path::new("/trash"),
      "run-1",
      Path::new("/home/z/downloads/a.jpg"),
    );

    assert_eq!(
      dest,
      PathBuf::from("/trash/run-1/home/z/downloads/a.jpg")
    );
  }

  #[test]
  fn purge_everything_frees_bytes() {
    let dir = TempDir::new().unwrap();
    let paths = ExecutorPaths::under(dir.path());
    let run = paths.trash_dir.join("run-1/home/z");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(run.join("a.jpg"), vec![0u8; 100]).unwrap();

    let report = purge(&paths, None).unwrap();

    assert_eq!(report.runs_purged, 1);
    assert_eq!(report.bytes_freed, 100);
    assert!(!paths.trash_dir.join("run-1").exists());
  }

  #[test]
  fn purge_respects_age_cutoff() {
    let dir = TempDir::new().unwrap();
    let paths = ExecutorPaths::under(dir.path());
    let run = paths.trash_dir.join("run-recent");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(run.join("a.jpg"), b"x").unwrap();

    // A 30-day cutoff must not touch a directory created just now.
    let report = purge(&paths, Some(30)).unwrap();

    assert_eq!(report.runs_purged, 0);
    assert!(run.exists());
  }

  #[test]
  fn purge_missing_trash_dir_is_noop() {
    let paths = ExecutorPaths::under(Path::new("/nonexistent"));
    let report = purge(&paths, None).unwrap();

    assert_eq!(report.runs_purged, 0);
    assert_eq!(report.bytes_freed, 0);
  }

  #[test]
  fn purge_empties_trash_on_other_drives() {
    let dir = TempDir::new().unwrap();
    let paths = ExecutorPaths::under(&dir.path().join("state"));
    let staged = dir
      .path()
      .join("drive")
      .join(FOREIGN_TRASH)
      .join("run-1/media/a.jpg");
    std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
    std::fs::write(&staged, vec![0u8; 10]).unwrap();
    let mut run = journal::RunJournal::new("run-1");
    run.deletions.push(journal::JournalDeletion {
      original: PathBuf::from("/media/a.jpg"),
      trashed_to: staged.clone(),
      size: 10,
    });
    run.save(&paths.journal_dir).unwrap();

    let report = purge(&paths, None).unwrap();

    assert_eq!(report.runs_purged, 1);
    assert_eq!(report.bytes_freed, 10);
    assert!(!staged.exists());
  }

  #[test]
  fn files_on_the_same_drive_use_the_usual_trash() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("a.jpg");
    std::fs::write(&file, b"x").unwrap();
    let trash = dir.path().join("state/trash");

    assert_eq!(trash_dir_for(&trash, &file), trash);
  }
}

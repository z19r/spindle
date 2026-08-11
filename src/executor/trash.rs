//! Staged trash: "deleted" files move to a per-run trash directory instead
//! of being removed, so every deletion is undoable until an explicit purge.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

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

/// Permanently delete staged trash. `older_than_days: None` purges
/// everything; otherwise only run directories whose newest content is
/// older than the cutoff.
pub fn purge(
  trash_dir: &Path,
  older_than_days: Option<u64>,
) -> Result<PurgeReport> {
  let mut report = PurgeReport {
    runs_purged: 0,
    bytes_freed: 0,
  };
  let entries = match std::fs::read_dir(trash_dir) {
    Ok(entries) => entries,
    Err(_) => return Ok(report), // nothing staged yet
  };

  let cutoff = older_than_days.map(|days| {
    std::time::SystemTime::now()
      - std::time::Duration::from_secs(days * 24 * 60 * 60)
  });

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
  Ok(report)
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
    let run = dir.path().join("run-1/home/z");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(run.join("a.jpg"), vec![0u8; 100]).unwrap();

    let report = purge(dir.path(), None).unwrap();

    assert_eq!(report.runs_purged, 1);
    assert_eq!(report.bytes_freed, 100);
    assert!(!dir.path().join("run-1").exists());
  }

  #[test]
  fn purge_respects_age_cutoff() {
    let dir = TempDir::new().unwrap();
    let run = dir.path().join("run-recent");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(run.join("a.jpg"), b"x").unwrap();

    // A 30-day cutoff must not touch a directory created just now.
    let report = purge(dir.path(), Some(30)).unwrap();

    assert_eq!(report.runs_purged, 0);
    assert!(run.exists());
  }

  #[test]
  fn purge_missing_trash_dir_is_noop() {
    let report =
      purge(Path::new("/nonexistent/trash"), None).unwrap();

    assert_eq!(report.runs_purged, 0);
    assert_eq!(report.bytes_freed, 0);
  }
}

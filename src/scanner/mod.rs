use std::path::Path;

use anyhow::Result;

use crate::model::{FileCategory, FileType, ScannedFile};

#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
  pub include_trash: bool,
  pub type_filter: Vec<FileCategory>,
}

fn is_trash_path(path: &Path) -> bool {
  path.components().any(|c| {
    let s = c.as_os_str().to_string_lossy();
    s == ".Trash"
      || s.starts_with(".Trash-")
      || s == "$RECYCLE.BIN"
      || s == "$Recycle.Bin"
  })
}

pub fn scan_directory(path: &Path) -> Result<Vec<ScannedFile>> {
  scan_directory_opts(path, false)
}

pub fn scan_directory_opts(
  path: &Path,
  include_trash: bool,
) -> Result<Vec<ScannedFile>> {
  if !path.exists() {
    anyhow::bail!("Directory does not exist: {}", path.display());
  }

  let mut files = Vec::new();

  for entry in walkdir::WalkDir::new(path)
    .into_iter()
    .filter_entry(|e| {
      if include_trash {
        return true;
      }
      !is_trash_path(e.path())
    })
    .filter_map(|e| match e {
      Ok(entry) => Some(entry),
      Err(err) => {
        tracing::warn!(error = %err, "Skipping unreadable entry during scan");
        None
      }
    })
  {
    if !entry.file_type().is_file() {
      continue;
    }

    let file_type = detect_file_type(entry.path());

    let metadata = entry.metadata()?;
    files.push(ScannedFile {
      path: entry.path().to_path_buf(),
      scan_root: path.to_path_buf(),
      size: metadata.len(),
      modified: metadata.modified()?,
      file_type,
    });
  }

  Ok(files)
}

pub fn scan_directories(
  paths: &[impl AsRef<Path>],
) -> Result<Vec<ScannedFile>> {
  scan_directories_opts(paths, false)
}

pub fn scan_directories_opts(
  paths: &[impl AsRef<Path>],
  include_trash: bool,
) -> Result<Vec<ScannedFile>> {
  scan_directories_filtered(
    paths,
    &ScanOptions {
      include_trash,
      ..Default::default()
    },
  )
}

pub fn scan_directories_filtered(
  paths: &[impl AsRef<Path>],
  opts: &ScanOptions,
) -> Result<Vec<ScannedFile>> {
  let mut all_files = Vec::new();
  let mut seen_paths = std::collections::HashSet::new();

  for path in paths {
    let path = path.as_ref();
    if !path.exists() {
      tracing::warn!(path = %path.display(), "Skipping nonexistent directory");
      continue;
    }

    let files = scan_directory_opts(path, opts.include_trash)?;
    for file in files {
      if !opts.type_filter.is_empty() {
        let dominated = file
          .file_type
          .category()
          .map(|c| opts.type_filter.contains(&c))
          .unwrap_or(false);
        if !dominated {
          continue;
        }
      }

      let canonical = file
        .path
        .canonicalize()
        .unwrap_or_else(|_| file.path.clone());
      if seen_paths.insert(canonical) {
        all_files.push(file);
      }
    }
  }

  Ok(all_files)
}

fn detect_file_type(path: &Path) -> FileType {
  if let Some(ft) = FileType::from_magic_bytes(path) {
    return ft;
  }
  path
    .extension()
    .and_then(|ext| ext.to_str())
    .map(FileType::from_extension)
    .unwrap_or(FileType::Other)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;
  use tempfile::TempDir;

  fn create_test_dir() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("photo.jpg"), b"fake jpg").unwrap();
    fs::write(dir.path().join("image.png"), b"fake png").unwrap();
    fs::write(dir.path().join("animation.gif"), b"fake gif").unwrap();
    fs::write(dir.path().join("readme.txt"), b"not an image")
      .unwrap();
    fs::write(dir.path().join("data.csv"), b"not an image").unwrap();
    dir
  }

  #[test]
  fn finds_all_files() {
    let dir = create_test_dir();

    let results = scan_directory(dir.path()).unwrap();

    assert_eq!(results.len(), 5);
    let extensions: Vec<Option<&str>> = results
      .iter()
      .map(|f| f.path.extension().and_then(|e| e.to_str()))
      .collect();
    assert!(extensions.contains(&Some("jpg")));
    assert!(extensions.contains(&Some("png")));
    assert!(extensions.contains(&Some("gif")));
    assert!(extensions.contains(&Some("txt")));
    assert!(extensions.contains(&Some("csv")));
  }

  #[test]
  fn classifies_non_media_files() {
    let dir = create_test_dir();

    let results = scan_directory(dir.path()).unwrap();

    let txt = results
      .iter()
      .find(|f| f.path.extension().map_or(false, |e| e == "txt"))
      .unwrap();
    assert_eq!(
      txt.file_type,
      crate::model::FileType::Document(
        crate::model::DocumentFormat::Txt
      )
    );
  }

  #[test]
  fn populates_file_metadata() {
    let dir = create_test_dir();

    let results = scan_directory(dir.path()).unwrap();
    let jpg = results
      .iter()
      .find(|f| f.path.extension().map_or(false, |e| e == "jpg"))
      .unwrap();

    assert_eq!(jpg.size, 8); // b"fake jpg" = 8 bytes
    assert!(jpg.file_type.is_image());
  }

  #[test]
  fn recurses_into_subdirectories() {
    let dir = TempDir::new().unwrap();
    let sub = dir.path().join("subdir");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("nested.png"), b"nested").unwrap();
    fs::write(dir.path().join("top.jpg"), b"top").unwrap();

    let results = scan_directory(dir.path()).unwrap();

    assert_eq!(results.len(), 2);
  }

  #[test]
  fn returns_empty_vec_for_empty_directory() {
    let dir = TempDir::new().unwrap();

    let results = scan_directory(dir.path()).unwrap();

    assert!(results.is_empty());
  }

  #[test]
  fn returns_error_for_nonexistent_path() {
    let result = scan_directory(Path::new("/nonexistent/path/xyz"));

    assert!(result.is_err());
  }

  #[test]
  fn scan_directories_merges_multiple_paths() {
    let dir1 = TempDir::new().unwrap();
    let dir2 = TempDir::new().unwrap();
    fs::write(dir1.path().join("a.jpg"), b"aaa").unwrap();
    fs::write(dir2.path().join("b.png"), b"bbb").unwrap();

    let results =
      scan_directories(&[dir1.path(), dir2.path()]).unwrap();

    assert_eq!(results.len(), 2);
  }

  #[test]
  fn scan_directories_deduplicates_overlapping_paths() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("x.jpg"), b"xxx").unwrap();

    let results =
      scan_directories(&[dir.path(), dir.path()]).unwrap();

    assert_eq!(results.len(), 1);
  }

  #[test]
  fn scan_directories_skips_nonexistent_with_warning() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("real.png"), b"real").unwrap();
    let fake = Path::new("/nonexistent/garbage/path");

    let results = scan_directories(&[dir.path(), fake]).unwrap();

    assert_eq!(results.len(), 1);
  }

  #[test]
  fn handles_files_without_extension() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("Makefile"), b"all: build").unwrap();
    fs::write(dir.path().join("photo.jpg"), b"img").unwrap();

    let results = scan_directory(dir.path()).unwrap();

    assert_eq!(results.len(), 2);
    let makefile = results
      .iter()
      .find(|f| f.path.file_name().unwrap() == "Makefile")
      .unwrap();
    assert_eq!(makefile.file_type, crate::model::FileType::Other);
  }

  #[test]
  fn handles_hidden_files() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".hidden.jpg"), b"hidden").unwrap();
    fs::write(dir.path().join("visible.jpg"), b"visible").unwrap();

    let results = scan_directory(dir.path()).unwrap();

    // Hidden image files should still be found
    assert_eq!(results.len(), 2);
  }

  #[test]
  fn detects_webp_format() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("modern.webp"), b"webp data").unwrap();

    let results = scan_directory(dir.path()).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(
      results[0].file_type,
      crate::model::FileType::Image(crate::model::ImageFormat::Webp)
    );
  }

  #[test]
  fn case_insensitive_extension_matching() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("PHOTO.JPG"), b"upper").unwrap();
    fs::write(dir.path().join("image.Png"), b"mixed").unwrap();

    let results = scan_directory(dir.path()).unwrap();

    assert_eq!(results.len(), 2);
  }

  #[test]
  fn excludes_trash_by_default() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("keep.jpg"), b"data").unwrap();
    let trash = dir.path().join(".Trash");
    fs::create_dir_all(&trash).unwrap();
    fs::write(trash.join("deleted.jpg"), b"gone").unwrap();
    let trash2 = dir.path().join(".Trash-1000");
    fs::create_dir_all(&trash2).unwrap();
    fs::write(trash2.join("also_gone.png"), b"gone2").unwrap();

    let results = scan_directory(dir.path()).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].path.ends_with("keep.jpg"));
  }

  #[test]
  fn includes_trash_with_flag() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("keep.jpg"), b"data").unwrap();
    let trash = dir.path().join(".Trash");
    fs::create_dir_all(&trash).unwrap();
    fs::write(trash.join("deleted.jpg"), b"gone").unwrap();

    let results = scan_directory_opts(dir.path(), true).unwrap();
    assert_eq!(results.len(), 2);
  }

  #[test]
  fn is_trash_path_detects_variants() {
    use std::path::PathBuf;
    assert!(is_trash_path(&PathBuf::from(
      "/home/user/.Trash/file.jpg"
    )));
    assert!(is_trash_path(&PathBuf::from(
      "/mnt/disk/.Trash-1000/files/old.png"
    )));
    assert!(is_trash_path(&PathBuf::from("/d/$RECYCLE.BIN/stuff")));
    assert!(!is_trash_path(&PathBuf::from(
      "/home/user/photos/cat.jpg"
    )));
    assert!(!is_trash_path(&PathBuf::from(
      "/home/user/Trash/not_hidden.jpg"
    )));
  }

  #[test]
  fn type_filter_includes_matching_categories() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("photo.jpg"), b"img").unwrap();
    fs::write(dir.path().join("readme.txt"), b"text").unwrap();
    fs::write(dir.path().join("song.mp3"), b"audio").unwrap();

    let opts = ScanOptions {
      include_trash: false,
      type_filter: vec![FileCategory::Image],
    };
    let results =
      scan_directories_filtered(&[dir.path()], &opts).unwrap();

    assert_eq!(results.len(), 1);
    assert!(results[0].path.ends_with("photo.jpg"));
  }

  #[test]
  fn type_filter_allows_multiple_categories() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("photo.jpg"), b"img").unwrap();
    fs::write(dir.path().join("readme.txt"), b"text").unwrap();
    fs::write(dir.path().join("song.mp3"), b"audio").unwrap();

    let opts = ScanOptions {
      include_trash: false,
      type_filter: vec![FileCategory::Image, FileCategory::Audio],
    };
    let results =
      scan_directories_filtered(&[dir.path()], &opts).unwrap();

    assert_eq!(results.len(), 2);
  }

  #[test]
  fn empty_type_filter_returns_all() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("photo.jpg"), b"img").unwrap();
    fs::write(dir.path().join("readme.txt"), b"text").unwrap();

    let opts = ScanOptions {
      include_trash: false,
      type_filter: vec![],
    };
    let results =
      scan_directories_filtered(&[dir.path()], &opts).unwrap();

    assert_eq!(results.len(), 2);
  }

  #[test]
  fn magic_bytes_overrides_wrong_extension() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("actually_png.txt");
    let img: image::RgbaImage =
      image::ImageBuffer::from_raw(1, 1, vec![255, 0, 0, 255])
        .unwrap();
    img
      .save_with_format(&path, image::ImageFormat::Png)
      .unwrap();

    let results = scan_directory(dir.path()).unwrap();

    assert_eq!(results.len(), 1);
    assert!(
      results[0].file_type.is_image(),
      "Expected image type from magic bytes, got {:?}",
      results[0].file_type
    );
  }
}

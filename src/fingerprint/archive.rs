//! Archive ↔ extracted-directory matching: a `thing.zip` whose entries
//! all exist on disk (same relative paths, same content) next to it is
//! redundant — one copy can safely go.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::model::{
  ArchiveFormat, DuplicateSet, DuplicateType, FileType,
  FingerprintedFile,
};

/// One file entry inside an archive: relative path + content hash.
struct ArchiveEntry {
  rel_path: PathBuf,
  hash: [u8; 32],
}

/// Find archives whose full contents exist extracted among the scanned
/// files. Each match becomes a `DuplicateSet` whose canonical is one of
/// the extracted files and whose sole duplicate is the archive — so the
/// default action ("keep canonical, drop duplicates") deletes the
/// archive and never the extracted tree.
pub fn find_archive_matches(
  files: &[FingerprintedFile],
) -> Vec<DuplicateSet> {
  let mut by_hash: HashMap<[u8; 32], Vec<usize>> = HashMap::new();
  for (idx, file) in files.iter().enumerate() {
    if !matches!(file.scanned.file_type, FileType::Archive(_)) {
      by_hash.entry(file.blake3_hash).or_default().push(idx);
    }
  }
  let by_path: HashMap<&Path, usize> = files
    .iter()
    .enumerate()
    .map(|(idx, f)| (f.scanned.path.as_path(), idx))
    .collect();

  let mut results = Vec::new();
  for (zip_idx, file) in files.iter().enumerate() {
    if !matches!(
      file.scanned.file_type,
      FileType::Archive(ArchiveFormat::Zip)
    ) {
      continue;
    }
    let Ok(entries) = zip_entry_hashes(&file.scanned.path) else {
      continue;
    };
    if entries.is_empty() {
      continue;
    }

    if let Some(canonical) =
      find_extracted_root(&entries, files, &by_hash, &by_path)
    {
      results.push(DuplicateSet {
        canonical,
        duplicates: vec![zip_idx],
        duplicate_type: DuplicateType::ArchiveMatch,
      });
    }
  }
  results
}

/// Streaming blake3 of every file entry in a zip, with its safe
/// relative path.
fn zip_entry_hashes(
  archive_path: &Path,
) -> anyhow::Result<Vec<ArchiveEntry>> {
  let file = std::fs::File::open(archive_path)?;
  let mut archive = zip::ZipArchive::new(file)?;
  let mut entries = Vec::new();

  for i in 0..archive.len() {
    let mut entry = archive.by_index(i)?;
    if entry.is_dir() {
      continue;
    }
    let Some(rel_path) = entry.enclosed_name() else {
      continue;
    };
    let rel_path = rel_path.to_path_buf();

    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
      let n = entry.read(&mut buf)?;
      if n == 0 {
        break;
      }
      hasher.update(&buf[..n]);
    }
    entries.push(ArchiveEntry {
      rel_path,
      hash: *hasher.finalize().as_bytes(),
    });
  }
  Ok(entries)
}

/// Locate a directory D such that every archive entry exists at
/// `D/<rel_path>` with matching content. Returns the index of one of
/// the extracted files (the first entry's match) as the canonical.
fn find_extracted_root(
  entries: &[ArchiveEntry],
  files: &[FingerprintedFile],
  by_hash: &HashMap<[u8; 32], Vec<usize>>,
  by_path: &HashMap<&Path, usize>,
) -> Option<usize> {
  let first = &entries[0];
  // Candidate roots: scanned files with the first entry's content
  // whose path ends with the first entry's relative path.
  let candidates = by_hash.get(&first.hash)?;

  'candidate: for &cand_idx in candidates {
    let cand_path = &files[cand_idx].scanned.path;
    let Some(root) = strip_suffix_path(cand_path, &first.rel_path)
    else {
      continue;
    };

    for entry in entries {
      let expected = root.join(&entry.rel_path);
      let Some(&idx) = by_path.get(expected.as_path()) else {
        continue 'candidate;
      };
      if files[idx].blake3_hash != entry.hash {
        continue 'candidate;
      }
    }
    return Some(cand_idx);
  }
  None
}

/// `/a/b/thing/sub/file.txt` minus suffix `sub/file.txt` → `/a/b/thing`.
fn strip_suffix_path(path: &Path, suffix: &Path) -> Option<PathBuf> {
  let path_parts: Vec<_> = path.components().collect();
  let suffix_parts: Vec<_> = suffix.components().collect();
  if suffix_parts.len() >= path_parts.len() {
    return None;
  }
  let split = path_parts.len() - suffix_parts.len();
  if path_parts[split..] != suffix_parts[..] {
    return None;
  }
  Some(path_parts[..split].iter().collect())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::fingerprint::{compute_blake3, fingerprint_files};
  use crate::scanner::scan_directory;
  use std::io::Write;
  use tempfile::TempDir;

  fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
      .compression_method(zip::CompressionMethod::Deflated);
    for (name, content) in entries {
      writer.start_file(*name, options).unwrap();
      writer.write_all(content).unwrap();
    }
    writer.finish().unwrap();
  }

  fn fingerprint_dir(dir: &Path) -> Vec<FingerprintedFile> {
    fingerprint_files(scan_directory(dir).unwrap()).unwrap()
  }

  #[test]
  fn detects_zip_matching_extracted_folder() {
    let dir = TempDir::new().unwrap();
    let extracted = dir.path().join("thing");
    std::fs::create_dir_all(extracted.join("nested")).unwrap();
    std::fs::write(extracted.join("one.txt"), b"first").unwrap();
    std::fs::write(extracted.join("nested/two.txt"), b"second")
      .unwrap();
    write_zip(
      &dir.path().join("thing.zip"),
      &[("one.txt", b"first"), ("nested/two.txt", b"second")],
    );

    let files = fingerprint_dir(dir.path());
    let matches = find_archive_matches(&files);

    assert_eq!(matches.len(), 1);
    assert_eq!(
      matches[0].duplicate_type,
      DuplicateType::ArchiveMatch
    );
    // The duplicate (deletable) side is the zip, never the tree.
    let dup = &files[matches[0].duplicates[0]];
    assert!(dup.scanned.path.ends_with("thing.zip"));
    let canon = &files[matches[0].canonical];
    assert!(canon.scanned.path.starts_with(&extracted));
  }

  #[test]
  fn no_match_when_content_differs() {
    let dir = TempDir::new().unwrap();
    let extracted = dir.path().join("thing");
    std::fs::create_dir_all(&extracted).unwrap();
    std::fs::write(extracted.join("one.txt"), b"MODIFIED").unwrap();
    write_zip(
      &dir.path().join("thing.zip"),
      &[("one.txt", b"first")],
    );

    let files = fingerprint_dir(dir.path());

    assert!(find_archive_matches(&files).is_empty());
  }

  #[test]
  fn no_match_when_entry_missing_on_disk() {
    let dir = TempDir::new().unwrap();
    let extracted = dir.path().join("thing");
    std::fs::create_dir_all(&extracted).unwrap();
    std::fs::write(extracted.join("one.txt"), b"first").unwrap();
    write_zip(
      &dir.path().join("thing.zip"),
      &[("one.txt", b"first"), ("two.txt", b"second")],
    );

    let files = fingerprint_dir(dir.path());

    assert!(find_archive_matches(&files).is_empty());
  }

  #[test]
  fn strip_suffix_path_works() {
    assert_eq!(
      strip_suffix_path(
        Path::new("/a/b/thing/sub/file.txt"),
        Path::new("sub/file.txt"),
      ),
      Some(PathBuf::from("/a/b/thing"))
    );
    assert_eq!(
      strip_suffix_path(Path::new("/a/f.txt"), Path::new("g.txt")),
      None
    );
  }

  #[test]
  fn zip_entry_hashes_match_file_hashes() {
    let dir = TempDir::new().unwrap();
    let zip_path = dir.path().join("t.zip");
    write_zip(&zip_path, &[("data.bin", b"payload")]);
    let loose = dir.path().join("data.bin");
    std::fs::write(&loose, b"payload").unwrap();

    let entries = zip_entry_hashes(&zip_path).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].hash, compute_blake3(&loose).unwrap());
  }
}

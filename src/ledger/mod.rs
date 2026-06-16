//! Persistent record of files that have already been organized.
//!
//! The ledger lets repeat runs skip files spindle has already filed away
//! (keyed by path **and** content hash) while still letting brand-new files
//! be compared against that history — flagged as exact duplicates of
//! organized content, or sorted into an existing group folder.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Bump when the on-disk shape changes incompatibly.
const LEDGER_VERSION: u32 = 1;

/// One organized file: where it came from, where it landed, its content
/// hash, and which group folder it joined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
  pub source_path: PathBuf,
  pub dest_path: PathBuf,
  pub blake3_hex: String,
  pub group_label: String,
  pub organized_at: String,
}

/// A previously-created group folder, surfaced so new files can be sorted
/// into it instead of spawning a near-duplicate folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingGroup {
  pub label: String,
  pub dest_dir: PathBuf,
}

/// A new candidate that is byte-identical to something already organized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizedDuplicate {
  /// The new file's current path.
  pub path: PathBuf,
  /// Where the already-organized copy lives.
  pub organized_at: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ledger {
  #[serde(default = "default_version")]
  pub version: u32,
  #[serde(default)]
  pub entries: Vec<LedgerEntry>,
}

fn default_version() -> u32 {
  LEDGER_VERSION
}

impl Default for Ledger {
  fn default() -> Self {
    Self {
      version: LEDGER_VERSION,
      entries: Vec::new(),
    }
  }
}

/// Hex-encode a blake3 digest the same way the analysis cache does.
pub fn hash_hex(blake3_hash: &[u8; 32]) -> String {
  hex::encode(blake3_hash)
}

/// Default global ledger location: `<data_dir>/spindle/ledger.json`.
pub fn default_ledger_path() -> PathBuf {
  directories::BaseDirs::new()
    .map(|d| d.data_dir().join("spindle").join("ledger.json"))
    .unwrap_or_else(|| {
      PathBuf::from(".local/share/spindle/ledger.json")
    })
}

impl Ledger {
  /// Load the ledger from disk. A missing or corrupt file yields an empty
  /// ledger rather than an error — a broken ledger must never block a run.
  pub fn load(path: &Path) -> Self {
    let Ok(content) = std::fs::read_to_string(path) else {
      return Self::default();
    };
    match serde_json::from_str::<Self>(&content) {
      Ok(ledger) if ledger.version == LEDGER_VERSION => ledger,
      Ok(ledger) => {
        tracing::warn!(
          path = %path.display(),
          found = ledger.version,
          expected = LEDGER_VERSION,
          "Ledger version mismatch; starting fresh"
        );
        Self::default()
      }
      Err(err) => {
        tracing::warn!(
          path = %path.display(),
          error = %err,
          "Ignoring corrupt ledger; starting fresh"
        );
        Self::default()
      }
    }
  }

  /// Persist the ledger, creating parent directories as needed.
  pub fn save(&self, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent).with_context(|| {
        format!("Failed to create ledger dir: {}", parent.display())
      })?;
    }
    let json = serde_json::to_string_pretty(self)
      .context("Failed to serialize ledger")?;
    std::fs::write(path, json).with_context(|| {
      format!("Failed to write ledger: {}", path.display())
    })
  }

  /// Append an organized-file record.
  pub fn record(&mut self, entry: LedgerEntry) {
    self.entries.push(entry);
  }

  /// Has this exact file (matching content hash at this exact path) already
  /// been organized? Matches either the source or the destination path so a
  /// re-scan of either location recognizes the file.
  pub fn is_organized(&self, path: &Path, blake3_hex: &str) -> bool {
    self.entries.iter().any(|e| {
      e.blake3_hex == blake3_hex
        && (e.dest_path == path || e.source_path == path)
    })
  }

  /// If a *new* file (different path) is byte-identical to organized
  /// content, return where that organized copy lives.
  pub fn duplicate_of(
    &self,
    blake3_hex: &str,
  ) -> Option<&LedgerEntry> {
    self.entries.iter().find(|e| e.blake3_hex == blake3_hex)
  }

  /// Distinct group folders previously created under `output_dir`, so new
  /// files can be routed into them rather than into fresh near-duplicates.
  pub fn existing_groups_under(
    &self,
    output_dir: &Path,
  ) -> Vec<ExistingGroup> {
    let mut seen = BTreeSet::new();
    let mut groups = Vec::new();
    for entry in &self.entries {
      let Some(dest_dir) = entry.dest_path.parent() else {
        continue;
      };
      if !dest_dir.starts_with(output_dir) {
        continue;
      }
      let key = (entry.group_label.clone(), dest_dir.to_path_buf());
      if seen.insert(key) {
        groups.push(ExistingGroup {
          label: entry.group_label.clone(),
          dest_dir: dest_dir.to_path_buf(),
        });
      }
    }
    groups
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

  fn entry(
    source: &str,
    dest: &str,
    hash: &str,
    label: &str,
  ) -> LedgerEntry {
    LedgerEntry {
      source_path: PathBuf::from(source),
      dest_path: PathBuf::from(dest),
      blake3_hex: hash.to_string(),
      group_label: label.to_string(),
      organized_at: "2026-06-13T00:00:00Z".to_string(),
    }
  }

  #[test]
  fn load_returns_empty_when_file_missing() {
    let dir = TempDir::new().unwrap();
    let ledger = Ledger::load(&dir.path().join("nope.json"));

    assert!(ledger.entries.is_empty());
    assert_eq!(ledger.version, LEDGER_VERSION);
  }

  #[test]
  fn load_returns_empty_when_file_corrupt() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("ledger.json");
    std::fs::write(&path, "{ not valid json").unwrap();

    let ledger = Ledger::load(&path);

    assert!(ledger.entries.is_empty());
  }

  #[test]
  fn load_returns_empty_on_version_mismatch() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("ledger.json");
    std::fs::write(
      &path,
      r#"{"version":999,"entries":[{"source_path":"/a","dest_path":"/b","blake3_hex":"h","group_label":"L","organized_at":"t"}]}"#,
    )
    .unwrap();

    let ledger = Ledger::load(&path);

    assert!(ledger.entries.is_empty());
    assert_eq!(ledger.version, LEDGER_VERSION);
  }

  #[test]
  fn save_then_load_roundtrips() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("nested/ledger.json");
    let mut ledger = Ledger::default();
    ledger.record(entry(
      "/src/a.jpg",
      "/out/beach/a.jpg",
      "abc123",
      "Beach",
    ));

    ledger.save(&path).unwrap();
    let loaded = Ledger::load(&path);

    assert_eq!(loaded.entries, ledger.entries);
  }

  #[test]
  fn is_organized_matches_on_dest_path_and_hash() {
    let mut ledger = Ledger::default();
    ledger.record(entry(
      "/src/a.jpg",
      "/out/beach/a.jpg",
      "hashA",
      "Beach",
    ));

    assert!(
      ledger.is_organized(Path::new("/out/beach/a.jpg"), "hashA")
    );
  }

  #[test]
  fn is_organized_matches_on_source_path_and_hash() {
    let mut ledger = Ledger::default();
    ledger.record(entry(
      "/src/a.jpg",
      "/out/beach/a.jpg",
      "hashA",
      "Beach",
    ));

    assert!(ledger.is_organized(Path::new("/src/a.jpg"), "hashA"));
  }

  #[test]
  fn is_organized_false_when_hash_differs() {
    let mut ledger = Ledger::default();
    ledger.record(entry(
      "/src/a.jpg",
      "/out/beach/a.jpg",
      "hashA",
      "Beach",
    ));

    assert!(
      !ledger.is_organized(Path::new("/out/beach/a.jpg"), "hashB")
    );
  }

  #[test]
  fn is_organized_false_for_same_content_at_new_path() {
    // path+hash semantics: a copy elsewhere is NOT "already organized".
    let mut ledger = Ledger::default();
    ledger.record(entry(
      "/src/a.jpg",
      "/out/beach/a.jpg",
      "hashA",
      "Beach",
    ));

    assert!(
      !ledger.is_organized(Path::new("/downloads/copy.jpg"), "hashA")
    );
  }

  #[test]
  fn duplicate_of_finds_same_content_regardless_of_path() {
    let mut ledger = Ledger::default();
    ledger.record(entry(
      "/src/a.jpg",
      "/out/beach/a.jpg",
      "hashA",
      "Beach",
    ));

    let found = ledger.duplicate_of("hashA").unwrap();
    assert_eq!(found.dest_path, PathBuf::from("/out/beach/a.jpg"));
    assert!(ledger.duplicate_of("missing").is_none());
  }

  #[test]
  fn existing_groups_dedupes_and_filters_by_output_dir() {
    let mut ledger = Ledger::default();
    ledger.record(entry(
      "/src/a.jpg",
      "/out/beach/a.jpg",
      "h1",
      "Beach",
    ));
    ledger.record(entry(
      "/src/b.jpg",
      "/out/beach/b.jpg",
      "h2",
      "Beach",
    ));
    ledger.record(entry(
      "/src/c.jpg",
      "/out/dogs/c.jpg",
      "h3",
      "Dogs",
    ));
    // A different destination root — must be excluded.
    ledger.record(entry(
      "/src/d.jpg",
      "/elsewhere/cats/d.jpg",
      "h4",
      "Cats",
    ));

    let groups = ledger.existing_groups_under(Path::new("/out"));

    assert_eq!(groups.len(), 2);
    assert!(groups.iter().any(|g| g.label == "Beach"));
    assert!(groups.iter().any(|g| g.label == "Dogs"));
    assert!(!groups.iter().any(|g| g.label == "Cats"));
  }

  #[test]
  fn hash_hex_matches_hex_encode() {
    let mut bytes = [0u8; 32];
    bytes[0] = 0xde;
    bytes[1] = 0xad;
    assert!(hash_hex(&bytes).starts_with("dead"));
  }
}

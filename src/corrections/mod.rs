//! What the user changed in the review screen, remembered across runs
//! so grouping stops proposing the same thing twice: group renames
//! (old label → new label) and per-file placements (content hash →
//! the label the user finally filed it under).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const CORRECTIONS_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rename {
  pub from: String,
  pub to: String,
  pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
  pub blake3_hex: String,
  pub label: String,
  pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Corrections {
  #[serde(default = "default_version")]
  pub version: u32,
  #[serde(default)]
  pub renames: Vec<Rename>,
  #[serde(default)]
  pub placements: Vec<Placement>,
}

fn default_version() -> u32 {
  CORRECTIONS_VERSION
}

impl Default for Corrections {
  fn default() -> Self {
    Self {
      version: CORRECTIONS_VERSION,
      renames: Vec::new(),
      placements: Vec::new(),
    }
  }
}

/// `<data_dir>/spindle/corrections.json`, beside the ledger.
pub fn default_corrections_path() -> PathBuf {
  directories::BaseDirs::new()
    .map(|d| d.data_dir().join("spindle"))
    .unwrap_or_else(|| PathBuf::from(".local/share/spindle"))
    .join("corrections.json")
}

impl Corrections {
  /// Missing, unreadable, or wrong-version files yield an empty set:
  /// corrections are a hint, never a reason to fail a run.
  pub fn load(path: &Path) -> Self {
    let Ok(text) = std::fs::read_to_string(path) else {
      return Self::default();
    };
    match serde_json::from_str::<Self>(&text) {
      Ok(c) if c.version == CORRECTIONS_VERSION => c,
      Ok(_) => {
        tracing::warn!(path = %path.display(), "Ignoring corrections with a different version");
        Self::default()
      }
      Err(e) => {
        tracing::warn!(path = %path.display(), error = %e, "Ignoring unreadable corrections");
        Self::default()
      }
    }
  }

  pub fn save(&self, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent).with_context(|| {
        format!("Failed to create {}", parent.display())
      })?;
    }
    let json = serde_json::to_string_pretty(self)
      .context("Failed to serialize corrections")?;
    std::fs::write(path, json).with_context(|| {
      format!("Failed to write corrections: {}", path.display())
    })
  }

  /// Remember that the user relabelled `from` as `to`. A later rename of
  /// the same `from` replaces the earlier one.
  pub fn record_rename(&mut self, from: &str, to: &str) {
    let (from, to) = (from.trim(), to.trim());
    if from.is_empty() || to.is_empty() || from == to {
      return;
    }
    self.renames.retain(|r| r.from != from);
    self.renames.push(Rename {
      from: from.to_string(),
      to: to.to_string(),
      at: now(),
    });
  }

  /// Remember where the user filed a file. Latest wins per hash.
  pub fn record_placement(&mut self, blake3_hex: &str, label: &str) {
    let label = label.trim();
    if label.is_empty() {
      return;
    }
    self.placements.retain(|p| p.blake3_hex != blake3_hex);
    self.placements.push(Placement {
      blake3_hex: blake3_hex.to_string(),
      label: label.to_string(),
      at: now(),
    });
  }

  /// (from, to) pairs, oldest first.
  pub fn renames(&self) -> Vec<(String, String)> {
    self
      .renames
      .iter()
      .map(|r| (r.from.clone(), r.to.clone()))
      .collect()
  }

  /// Labels the user chose for files present in this run, by hash.
  pub fn placements_for(
    &self,
    hashes: &[String],
  ) -> HashMap<String, String> {
    self
      .placements
      .iter()
      .filter(|p| hashes.contains(&p.blake3_hex))
      .map(|p| (p.blake3_hex.clone(), p.label.clone()))
      .collect()
  }

  /// Stable text that changes whenever the hints for `hashes` would,
  /// for salting the grouping cache key.
  pub fn cache_salt(&self, hashes: &[String]) -> String {
    let mut parts: Vec<String> = self
      .renames
      .iter()
      .map(|r| format!("rename:{}>{}", r.from, r.to))
      .collect();
    let mut placed: Vec<String> = self
      .placements_for(hashes)
      .into_iter()
      .map(|(h, l)| format!("place:{h}>{l}"))
      .collect();
    placed.sort();
    parts.extend(placed);
    parts.join("\n")
  }
}

/// What the user changed between the proposed plan and what they
/// executed: groups relabelled (same id, new label) and files that
/// ended up under a different label than proposed. System groups are
/// never recorded as a destination.
pub fn derive(
  original: &crate::model::ReorgPlan,
  final_groups: &[crate::model::FileGroup],
  completed_moves: &[crate::model::FileMove],
  hash_by_path: &HashMap<PathBuf, String>,
) -> Derived {
  let system = crate::group::validate::SYSTEM_LABELS;
  let is_system = |label: &str| {
    system.iter().any(|s| s.eq_ignore_ascii_case(label))
  };

  let original_label_by_id: HashMap<usize, &str> = original
    .groups
    .iter()
    .map(|g| (g.id, g.label.as_str()))
    .collect();
  let final_label_by_id: HashMap<usize, &str> = final_groups
    .iter()
    .map(|g| (g.id, g.label.as_str()))
    .collect();

  let mut renames = Vec::new();
  for g in final_groups {
    if let Some(&old) = original_label_by_id.get(&g.id) {
      if old.trim() != g.label.trim()
        && !is_system(old)
        && !is_system(&g.label)
      {
        renames.push((old.to_string(), g.label.clone()));
      }
    }
  }

  let original_label_by_path: HashMap<&Path, &str> = original
    .moves
    .iter()
    .filter_map(|m| {
      original_label_by_id
        .get(&m.group_id)
        .map(|l| (m.from.as_path(), *l))
    })
    .collect();
  let renamed_from: HashMap<&str, &str> = renames
    .iter()
    .map(|(from, to)| (from.as_str(), to.as_str()))
    .collect();

  let mut placements = Vec::new();
  for mv in completed_moves {
    let Some(&final_label) = final_label_by_id.get(&mv.group_id)
    else {
      continue;
    };
    if is_system(final_label) {
      continue;
    }
    let Some(hex) = hash_by_path.get(&mv.from) else {
      continue;
    };
    let proposed =
      original_label_by_path.get(mv.from.as_path()).copied();
    // A file that merely followed its group's rename is not a placement.
    let followed_rename = proposed
      .and_then(|p| renamed_from.get(p))
      .map(|to| *to == final_label)
      .unwrap_or(false);
    if proposed != Some(final_label) && !followed_rename {
      placements.push((hex.clone(), final_label.to_string()));
    }
  }

  Derived {
    renames,
    placements,
  }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Derived {
  pub renames: Vec<(String, String)>,
  pub placements: Vec<(String, String)>,
}

impl Corrections {
  pub fn absorb(&mut self, derived: &Derived) {
    for (from, to) in &derived.renames {
      self.record_rename(from, to);
    }
    for (hex, label) in &derived.placements {
      self.record_placement(hex, label);
    }
  }
}

fn now() -> String {
  chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

  #[test]
  fn load_is_empty_for_missing_corrupt_or_old_files() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("none.json");
    assert!(Corrections::load(&missing).renames.is_empty());

    let corrupt = dir.path().join("bad.json");
    std::fs::write(&corrupt, "{not json").unwrap();
    assert!(Corrections::load(&corrupt).placements.is_empty());

    let old = dir.path().join("old.json");
    std::fs::write(
      &old,
      r#"{"version": 0, "renames": [{"from":"a","to":"b","at":"x"}]}"#,
    )
    .unwrap();
    assert!(Corrections::load(&old).renames.is_empty());
  }

  #[test]
  fn save_then_load_roundtrips() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("nested").join("corrections.json");
    let mut c = Corrections::default();
    c.record_rename("Photos/Pets", "Personal/Pets/Biscuit");
    c.record_placement("abc", "Work/Acme Corp");
    c.save(&path).unwrap();

    let loaded = Corrections::load(&path);
    assert_eq!(
      loaded.renames(),
      vec![(
        "Photos/Pets".to_string(),
        "Personal/Pets/Biscuit".to_string()
      )]
    );
    assert_eq!(
      loaded.placements_for(&["abc".to_string()]),
      HashMap::from([(
        "abc".to_string(),
        "Work/Acme Corp".to_string()
      )])
    );
  }

  #[test]
  fn latest_rename_and_placement_win() {
    let mut c = Corrections::default();
    c.record_rename("A", "B");
    c.record_rename("A", "C");
    c.record_rename("X", "Y");
    assert_eq!(
      c.renames(),
      vec![
        ("A".to_string(), "C".to_string()),
        ("X".to_string(), "Y".to_string())
      ]
    );

    c.record_placement("h1", "One");
    c.record_placement("h1", "Two");
    assert_eq!(c.placements.len(), 1);
    assert_eq!(c.placements_for(&["h1".to_string()])["h1"], "Two");
  }

  #[test]
  fn renaming_to_the_same_label_is_ignored() {
    let mut c = Corrections::default();
    c.record_rename("Same", "Same");
    c.record_rename("  Same ", "Same");
    assert!(c.renames().is_empty());
  }

  #[test]
  fn placements_for_only_returns_present_hashes() {
    let mut c = Corrections::default();
    c.record_placement("h1", "One");
    c.record_placement("h2", "Two");
    let got = c.placements_for(&["h2".to_string(), "h9".to_string()]);
    assert_eq!(got.len(), 1);
    assert_eq!(got["h2"], "Two");
  }

  fn plan_fixture() -> (
    crate::model::ReorgPlan,
    Vec<crate::model::FileGroup>,
    HashMap<PathBuf, String>,
  ) {
    use crate::model::{FileGroup, FileMove, PlanStats, ReorgPlan};
    let g = |id: usize, label: &str, members: Vec<usize>| FileGroup {
      id,
      label: label.to_string(),
      rationale: String::new(),
      members,
      member_destinations: vec![],
      suggested_path: PathBuf::from(label.to_lowercase()),
      member_notes: vec![],
    };
    let mv = |from: &str, group_id: usize| FileMove {
      from: PathBuf::from(from),
      to: PathBuf::from(format!("/out/{from}")),
      group_id,
    };
    let plan = ReorgPlan {
      groups: vec![
        g(0, "Photos/Pets", vec![0, 1]),
        g(1, "Work/Acme", vec![2]),
        g(2, "Unsorted", vec![3]),
      ],
      duplicates: vec![],
      moves: vec![
        mv("/dl/dog1.jpg", 0),
        mv("/dl/dog2.jpg", 0),
        mv("/dl/contract.pdf", 1),
        mv("/dl/odd.bin", 2),
      ],
      stats: PlanStats {
        total_files: 4,
        groups_created: 3,
        duplicates_found: 0,
        space_to_reclaim: 0,
      },
    };
    // User renamed group 0, moved dog2 to Work/Acme, made a new group
    // for odd.bin.
    let final_groups = vec![
      g(0, "Personal/Pets/Biscuit", vec![0]),
      g(1, "Work/Acme", vec![2, 1]),
      g(3, "Software", vec![3]),
    ];
    let hashes = HashMap::from([
      (PathBuf::from("/dl/dog1.jpg"), "h-dog1".to_string()),
      (PathBuf::from("/dl/dog2.jpg"), "h-dog2".to_string()),
      (PathBuf::from("/dl/contract.pdf"), "h-contract".to_string()),
      (PathBuf::from("/dl/odd.bin"), "h-odd".to_string()),
    ]);
    (plan, final_groups, hashes)
  }

  #[test]
  fn derive_finds_renames_and_changed_placements_only() {
    use crate::model::FileMove;
    let (plan, final_groups, hashes) = plan_fixture();
    let completed = vec![
      FileMove {
        from: PathBuf::from("/dl/dog1.jpg"),
        to: PathBuf::from("/out/personal/pets/biscuit/dog1.jpg"),
        group_id: 0,
      },
      FileMove {
        from: PathBuf::from("/dl/dog2.jpg"),
        to: PathBuf::from("/out/work/acme/dog2.jpg"),
        group_id: 1,
      },
      FileMove {
        from: PathBuf::from("/dl/contract.pdf"),
        to: PathBuf::from("/out/work/acme/contract.pdf"),
        group_id: 1,
      },
      FileMove {
        from: PathBuf::from("/dl/odd.bin"),
        to: PathBuf::from("/out/software/odd.bin"),
        group_id: 3,
      },
    ];

    let d = derive(&plan, &final_groups, &completed, &hashes);

    assert_eq!(
      d.renames,
      vec![(
        "Photos/Pets".to_string(),
        "Personal/Pets/Biscuit".to_string()
      )]
    );
    let mut placements = d.placements.clone();
    placements.sort();
    assert_eq!(
      placements,
      vec![
        ("h-dog2".to_string(), "Work/Acme".to_string()),
        ("h-odd".to_string(), "Software".to_string()),
      ]
    );
  }

  #[test]
  fn derive_ignores_moves_into_system_groups() {
    use crate::model::FileMove;
    let (plan, mut final_groups, hashes) = plan_fixture();
    final_groups.push(crate::model::FileGroup {
      id: 2,
      label: "Unsorted".to_string(),
      rationale: String::new(),
      members: vec![2],
      member_destinations: vec![],
      suggested_path: PathBuf::from("unsorted"),
      member_notes: vec![],
    });
    let completed = vec![FileMove {
      from: PathBuf::from("/dl/contract.pdf"),
      to: PathBuf::from("/out/unsorted/contract.pdf"),
      group_id: 2,
    }];
    let d = derive(&plan, &final_groups, &completed, &hashes);
    assert!(d.placements.is_empty());
  }

  #[test]
  fn cache_salt_tracks_relevant_hints_only() {
    let mut c = Corrections::default();
    let hashes = vec!["h1".to_string()];
    let empty = c.cache_salt(&hashes);
    c.record_placement("h9", "Elsewhere");
    assert_eq!(c.cache_salt(&hashes), empty, "unrelated hash");
    c.record_placement("h1", "Here");
    let with_placement = c.cache_salt(&hashes);
    assert_ne!(with_placement, empty);
    c.record_rename("A", "B");
    assert_ne!(c.cache_salt(&hashes), with_placement);
  }
}

use serde::{Deserialize, Serialize};

/// A top-level folder the user wants everything sorted under. Routing
/// assigns each file to exactly one area before grouping proposes the
/// sub-folders beneath it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Area {
  pub name: String,
  #[serde(default)]
  pub description: String,
}

impl Area {
  pub fn new(name: &str, description: &str) -> Self {
    Self {
      name: name.to_string(),
      description: description.to_string(),
    }
  }
}

/// The areas shipped when `[taxonomy]` is absent from config.toml.
pub fn default_areas() -> Vec<Area> {
  vec![
    Area::new(
      "Work",
      "jobs, clients, projects, meetings, job hunting",
    ),
    Area::new(
      "Personal",
      "family, friends, pets, hobbies, recipes, home life, trips and \
       their photos",
    ),
    Area::new("Finance", "taxes, bills, invoices, receipts, banking"),
    Area::new(
      "Legal",
      "contracts, leases, court matters, official letters",
    ),
    Area::new("Health", "medical records, lab results, fitness"),
    Area::new(
      "Photos",
      "photo and video collections with no other home: camera dumps, \
       screenshots kept for their own sake — a trip's or pet's photos \
       belong with that trip or pet instead",
    ),
    Area::new(
      "Private",
      "nudity, intimate or explicit photos and videos, and anything \
       flagged sensitive; never mixed with family or trip photos",
    ),
    Area::new(
      "Media",
      "music, podcasts, movies, ebooks for consumption",
    ),
    Area::new(
      "Software",
      "installers, disk images, downloaded programs, source code",
    ),
    Area::new(
      "Reference",
      "manuals, notes, cheat sheets, learning material",
    ),
  ]
}

/// A file's routed area, by fingerprinted index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutedFile {
  pub index: usize,
  pub area: String,
}

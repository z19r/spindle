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
      "family, friends, hobbies, recipes, home life, travel",
    ),
    Area::new("Finance", "taxes, bills, invoices, receipts, banking"),
    Area::new(
      "Legal",
      "contracts, leases, court matters, official letters",
    ),
    Area::new("Health", "medical records, lab results, fitness"),
    Area::new(
      "Photos",
      "photographs and videos of people, pets, places",
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

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentDescription {
  pub summary: String,
  pub tags: Vec<String>,
  pub suggested_category: String,
  pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSummary {
  pub index: usize,
  pub filename: String,
  pub source_path: String,
  pub description: ContentDescription,
  pub metadata_hint: String,
}

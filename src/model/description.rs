use serde::{Deserialize, Serialize};

/// Where a description came from. Only `Ai` confidence is meaningful;
/// the other two are placeholders and must not be judged by it.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum DescriptionSource {
  /// Produced by the model from the file's content.
  #[default]
  Ai,
  /// Derived from the filename and extension only.
  Filename,
  /// Content could not be analyzed (unsupported type, missing tool).
  Unanalyzed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentDescription {
  pub summary: String,
  pub tags: Vec<String>,
  pub suggested_category: String,
  pub confidence: f64,
  #[serde(default)]
  pub source: DescriptionSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSummary {
  pub index: usize,
  pub filename: String,
  pub source_path: String,
  pub description: ContentDescription,
  pub metadata_hint: String,
}

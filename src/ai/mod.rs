mod claude;
mod prompts;

pub use claude::ClaudeProvider;
pub use prompts::*;

use anyhow::Result;

use crate::model::{ContentDescription, FileSummary, ProposedGroup};

pub struct DescribeContext {
  pub filename: String,
  pub file_type_label: String,
  pub file_size: u64,
  pub metadata_hint: Option<String>,
}

pub trait AiProvider: Send + Sync {
  fn describe_image(
    &self,
    image_data: &[u8],
    mime_type: &str,
    context: &DescribeContext,
  ) -> impl std::future::Future<Output = Result<ContentDescription>> + Send;

  fn propose_groups(
    &self,
    files: &[FileSummary],
  ) -> impl std::future::Future<Output = Result<Vec<ProposedGroup>>> + Send;
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn describe_context_holds_metadata() {
    let ctx = DescribeContext {
      filename: "vacation.jpg".to_string(),
      file_type_label: "JPEG image".to_string(),
      file_size: 2048,
      metadata_hint: Some("Taken 2024-06-15, Canon EOS".to_string()),
    };

    assert_eq!(ctx.filename, "vacation.jpg");
    assert_eq!(ctx.file_size, 2048);
    assert!(ctx.metadata_hint.is_some());
  }
}

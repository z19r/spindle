mod claude;
mod prompts;

pub use claude::ClaudeProvider;
pub use prompts::*;

use anyhow::Result;

use crate::model::{
  Area, ContentDescription, FileSummary, ProposedGroup, RoutedFile,
};

/// The model produced a reply that is syntactically fine but useless:
/// truncated at max_tokens while looping, or groups with no members.
/// Callers retry once on this before falling back.
#[derive(Debug, Clone)]
pub struct DegenerateReply(pub String);

impl std::fmt::Display for DegenerateReply {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "degenerate model reply: {}", self.0)
  }
}

impl std::error::Error for DegenerateReply {}

pub struct DescribeContext {
  pub filename: String,
  pub file_type_label: String,
  pub file_size: u64,
  pub metadata_hint: Option<String>,
}

/// Content payload for a single describe request.
pub enum DescribePayload {
  Image { data: Vec<u8>, mime_type: String },
  Text { excerpt: String },
}

/// A self-contained describe request, suitable for batch submission.
pub struct DescribeRequest {
  pub payload: DescribePayload,
  pub context: DescribeContext,
}

/// Tokens consumed by one model so far this run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
  pub calls: u64,
  pub input_tokens: u64,
  pub cache_read_tokens: u64,
  pub output_tokens: u64,
}

impl Usage {
  pub fn add(&mut self, other: Usage) {
    self.calls += other.calls;
    self.input_tokens += other.input_tokens;
    self.cache_read_tokens += other.cache_read_tokens;
    self.output_tokens += other.output_tokens;
  }
}

/// Everything a grouping call may be told besides the files.
#[derive(Debug, Clone, Copy, Default)]
pub struct GroupingHints<'a> {
  pub area: Option<&'a Area>,
  pub existing_labels: &'a [String],
  pub organized_context: &'a [(String, Vec<ContentDescription>)],
  /// (old label, new label) pairs the user applied in earlier runs.
  pub renames: &'a [(String, String)],
}

pub trait AiProvider: Send + Sync {
  fn describe_image(
    &self,
    image_data: &[u8],
    mime_type: &str,
    context: &DescribeContext,
  ) -> impl std::future::Future<Output = Result<ContentDescription>> + Send;

  /// Describe a file from an excerpt of its text content.
  fn describe_text(
    &self,
    excerpt: &str,
    context: &DescribeContext,
  ) -> impl std::future::Future<Output = Result<ContentDescription>> + Send
  {
    let _ = (excerpt, context);
    async {
      anyhow::bail!("text analysis not supported by this provider")
    }
  }

  fn propose_groups(
    &self,
    files: &[FileSummary],
  ) -> impl std::future::Future<Output = Result<Vec<ProposedGroup>>> + Send;

  /// Group files while aware of folders previous runs already created.
  /// New files can then be routed into an existing group instead of a
  /// fresh near-duplicate. The default ignores `existing_labels`;
  /// providers that drive the prompt should override this.
  fn propose_groups_with_context(
    &self,
    files: &[FileSummary],
    existing_labels: &[String],
  ) -> impl std::future::Future<Output = Result<Vec<ProposedGroup>>> + Send
  {
    let _ = existing_labels;
    self.propose_groups(files)
  }

  /// Group files with both label names and rich content descriptions
  /// from the organized pool. Falls back to label-only context.
  fn propose_groups_with_organized_context(
    &self,
    files: &[FileSummary],
    existing_labels: &[String],
    _organized_context: &[(String, Vec<ContentDescription>)],
  ) -> impl std::future::Future<Output = Result<Vec<ProposedGroup>>> + Send
  {
    self.propose_groups_with_context(files, existing_labels)
  }

  /// Assign each file to one of `areas` (stage one of grouping). The
  /// default cannot route; the pipeline then groups in a single stage.
  fn route_files(
    &self,
    files: &[FileSummary],
    areas: &[Area],
  ) -> impl std::future::Future<Output = Result<Vec<RoutedFile>>> + Send
  {
    let _ = (files, areas);
    async { anyhow::bail!("routing not supported by this provider") }
  }

  /// Group files already known to belong under `area`; labels must
  /// start with the area name. The default ignores the area (the
  /// pipeline enforces the prefix afterwards).
  fn propose_groups_in_area(
    &self,
    files: &[FileSummary],
    area: &Area,
    existing_labels: &[String],
    organized_context: &[(String, Vec<ContentDescription>)],
  ) -> impl std::future::Future<Output = Result<Vec<ProposedGroup>>> + Send
  {
    let _ = area;
    self.propose_groups_with_organized_context(
      files,
      existing_labels,
      organized_context,
    )
  }

  /// Group with every hint the pipeline has: an optional area, folders
  /// from earlier runs, sample contents of those folders, and the
  /// user's past renames. The default drops the renames and dispatches
  /// to the narrower methods; providers that build prompts override it.
  fn propose_groups_with_hints(
    &self,
    files: &[FileSummary],
    hints: &GroupingHints<'_>,
  ) -> impl std::future::Future<Output = Result<Vec<ProposedGroup>>> + Send
  {
    async move {
      if let Some(area) = hints.area {
        self
          .propose_groups_in_area(
            files,
            area,
            hints.existing_labels,
            hints.organized_context,
          )
          .await
      } else {
        self
          .propose_groups_with_organized_context(
            files,
            hints.existing_labels,
            hints.organized_context,
          )
          .await
      }
    }
  }

  /// Tokens spent so far, per model id. Providers that do not meter
  /// return nothing.
  fn usage(&self) -> Vec<(String, Usage)> {
    Vec::new()
  }

  /// Describe many files in one operation. The default falls back to
  /// sequential individual calls; providers with a native batch API
  /// (50% discount) should override this.
  fn describe_batch(
    &self,
    requests: Vec<DescribeRequest>,
  ) -> impl std::future::Future<Output = Vec<Result<ContentDescription>>>
       + Send {
    async move {
      let mut results = Vec::with_capacity(requests.len());
      for request in &requests {
        let result = match &request.payload {
          DescribePayload::Image { data, mime_type } => {
            self
              .describe_image(data, mime_type, &request.context)
              .await
          }
          DescribePayload::Text { excerpt } => {
            self.describe_text(excerpt, &request.context).await
          }
        };
        results.push(result);
      }
      results
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::DescriptionSource;

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

  #[tokio::test]
  async fn default_describe_batch_falls_back_to_individual_calls() {
    struct CountingProvider {
      images: std::sync::atomic::AtomicUsize,
      texts: std::sync::atomic::AtomicUsize,
    }

    impl AiProvider for CountingProvider {
      async fn describe_image(
        &self,
        _: &[u8],
        _: &str,
        _: &DescribeContext,
      ) -> Result<ContentDescription> {
        self
          .images
          .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(stub_description("an image"))
      }

      async fn describe_text(
        &self,
        _: &str,
        _: &DescribeContext,
      ) -> Result<ContentDescription> {
        self.texts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(stub_description("a text"))
      }

      async fn propose_groups(
        &self,
        _: &[FileSummary],
      ) -> Result<Vec<ProposedGroup>> {
        panic!("unused");
      }
    }

    fn stub_description(summary: &str) -> ContentDescription {
      ContentDescription {
        summary: summary.to_string(),
        tags: vec![],
        suggested_category: "other".to_string(),
        confidence: 0.5,
        source: DescriptionSource::Ai,
      }
    }

    fn ctx(name: &str) -> DescribeContext {
      DescribeContext {
        filename: name.to_string(),
        file_type_label: "test".to_string(),
        file_size: 1,
        metadata_hint: None,
      }
    }

    let provider = CountingProvider {
      images: std::sync::atomic::AtomicUsize::new(0),
      texts: std::sync::atomic::AtomicUsize::new(0),
    };

    let requests = vec![
      DescribeRequest {
        payload: DescribePayload::Image {
          data: vec![0xFF],
          mime_type: "image/jpeg".to_string(),
        },
        context: ctx("a.jpg"),
      },
      DescribeRequest {
        payload: DescribePayload::Text {
          excerpt: "hello".to_string(),
        },
        context: ctx("b.txt"),
      },
    ];

    let results = provider.describe_batch(requests).await;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].as_ref().unwrap().summary, "an image");
    assert_eq!(results[1].as_ref().unwrap().summary, "a text");
    assert_eq!(
      provider.images.load(std::sync::atomic::Ordering::SeqCst),
      1
    );
    assert_eq!(
      provider.texts.load(std::sync::atomic::Ordering::SeqCst),
      1
    );
  }
}

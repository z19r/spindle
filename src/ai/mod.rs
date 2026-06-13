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

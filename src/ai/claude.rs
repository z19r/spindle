use anyhow::{Context, Result};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};

use crate::model::{ContentDescription, FileSummary, ProposedGroup};

use super::{
  AiProvider, DescribeContext, DescribePayload, DescribeRequest,
};

const LOCAL_API_KEY: &str =
  "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";
const LOCAL_BASE_URL: &str = "http://localhost:8787";

pub struct ClaudeProvider {
  client: Client,
  api_key: String,
  model: String,
  base_url: String,
  max_retries: usize,
  poll_interval: std::time::Duration,
}

const DEFAULT_BATCH_POLL_INTERVAL: std::time::Duration =
  std::time::Duration::from_secs(5);

#[derive(Serialize)]
struct Message {
  role: &'static str,
  content: Vec<ContentBlock>,
}

#[derive(Serialize)]
struct SystemBlock {
  #[serde(rename = "type")]
  block_type: &'static str,
  text: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  cache_control: Option<CacheControl>,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum ContentBlock {
  #[serde(rename = "text")]
  Text {
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
  },
  #[serde(rename = "image")]
  Image { source: ImageSource },
}

#[derive(Serialize)]
struct ImageSource {
  #[serde(rename = "type")]
  source_type: &'static str,
  media_type: String,
  data: String,
}

#[derive(Serialize)]
struct CacheControl {
  #[serde(rename = "type")]
  cache_type: &'static str,
}

impl CacheControl {
  fn ephemeral() -> Self {
    Self {
      cache_type: "ephemeral",
    }
  }
}

#[derive(Serialize)]
struct ApiRequest {
  model: String,
  max_tokens: u32,
  cache_control: CacheControl,
  #[serde(skip_serializing_if = "Option::is_none")]
  system: Option<Vec<SystemBlock>>,
  messages: Vec<Message>,
}

fn cached_system_block(text: impl Into<String>) -> SystemBlock {
  SystemBlock {
    block_type: "text",
    text: text.into(),
    cache_control: Some(CacheControl::ephemeral()),
  }
}

fn cached_api_request(
  model: String,
  max_tokens: u32,
  system: Option<Vec<SystemBlock>>,
  messages: Vec<Message>,
) -> ApiRequest {
  ApiRequest {
    model,
    max_tokens,
    cache_control: CacheControl::ephemeral(),
    system,
    messages,
  }
}

#[derive(Deserialize)]
struct ApiResponse {
  content: Vec<ResponseBlock>,
  stop_reason: Option<String>,
}

#[derive(Serialize)]
struct BatchRequestItem {
  custom_id: String,
  params: ApiRequest,
}

#[derive(Serialize)]
struct BatchSubmitBody {
  requests: Vec<BatchRequestItem>,
}

#[derive(Deserialize)]
struct BatchStatus {
  id: String,
  processing_status: String,
}

#[derive(Deserialize)]
struct BatchResultLine {
  custom_id: String,
  result: BatchResult,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum BatchResult {
  #[serde(rename = "succeeded")]
  Succeeded { message: ApiResponse },
  #[serde(rename = "errored")]
  Errored { error: serde_json::Value },
  #[serde(rename = "canceled")]
  Canceled,
  #[serde(rename = "expired")]
  Expired,
}

#[derive(Deserialize)]
struct ResponseBlock {
  text: Option<String>,
}

impl ClaudeProvider {
  pub fn new(
    api_key: impl Into<String>,
    model: impl Into<String>,
    max_retries: usize,
  ) -> Self {
    let api_key = if cfg!(test) {
      api_key.into()
    } else {
      LOCAL_API_KEY.to_string()
    };

    Self {
      client: Client::new(),
      api_key,
      model: model.into(),
      base_url: LOCAL_BASE_URL.to_string(),
      max_retries,
      poll_interval: DEFAULT_BATCH_POLL_INTERVAL,
    }
  }

  #[cfg(test)]
  pub fn with_poll_interval(
    self,
    interval: std::time::Duration,
  ) -> Self {
    let mut this = self;
    this.poll_interval = interval;
    this
  }

  pub fn with_base_url(self, url: impl Into<String>) -> Self {
    #[cfg(test)]
    {
      let mut this = self;
      this.base_url = url.into();
      this
    }
    #[cfg(not(test))]
    {
      let _ = url;
      self
    }
  }

  async fn send_request(
    &self,
    request: ApiRequest,
  ) -> Result<String> {
    let endpoint = format!("{}/v1/messages", self.base_url);
    let fqdn = api_endpoint_fqdn(&self.base_url);
    log_claude_api_call(&self.api_key, &endpoint, &fqdn);

    let mut last_err: Option<anyhow::Error> = None;

    for attempt in 0..=self.max_retries {
      let result = self
        .client
        .post(&endpoint)
        .header("x-api-key", &self.api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&request)
        .send()
        .await;

      let response = match result {
        Ok(resp) => resp,
        Err(e) => {
          if attempt < self.max_retries {
            let delay = 1u64 << attempt;
            tracing::warn!(
              attempt = attempt + 1,
              max = self.max_retries,
              delay_secs = delay,
              error = %e,
              "Request failed, retrying"
            );
            tokio::time::sleep(std::time::Duration::from_secs(delay))
              .await;
            last_err = Some(e.into());
            continue;
          }
          return Err(e)
            .context("Failed to send request to Claude API");
        }
      };

      let status = response.status();

      if is_retryable_status(status) && attempt < self.max_retries {
        let body = response.text().await.unwrap_or_default();
        let delay = 1u64 << attempt;
        tracing::warn!(
          attempt = attempt + 1,
          max = self.max_retries,
          status = %status,
          delay_secs = delay,
          "Retryable API error ({}): {}",
          status,
          preview(&body, 200),
        );
        tokio::time::sleep(std::time::Duration::from_secs(delay))
          .await;
        last_err = Some(anyhow::anyhow!(
          "Claude API error ({}): {}",
          status,
          body
        ));
        continue;
      }

      if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Claude API error ({}): {}", status, body);
      }

      let api_response: ApiResponse = response
        .json()
        .await
        .context("Failed to parse Claude API response")?;

      return response_text(api_response);
    }

    Err(last_err.unwrap_or_else(|| {
      anyhow::anyhow!("All retry attempts exhausted")
    }))
  }

  /// Build the Messages API request for a describe payload — shared
  /// by the individual and batch paths.
  fn describe_api_request(
    &self,
    payload: &DescribePayload,
    context: &DescribeContext,
  ) -> ApiRequest {
    let system_text = format!(
      "{task}\n\n{instructions}",
      task = super::describe_system_prompt(),
      instructions = super::describe_response_instructions(),
    );

    let content = match payload {
      DescribePayload::Image { data, mime_type } => {
        use base64::Engine;
        let encoded =
          base64::engine::general_purpose::STANDARD.encode(data);
        vec![
          ContentBlock::Image {
            source: ImageSource {
              source_type: "base64",
              media_type: mime_type.clone(),
              data: encoded,
            },
          },
          ContentBlock::Text {
            text: super::describe_user_prompt(context),
            cache_control: None,
          },
        ]
      }
      DescribePayload::Text { excerpt } => vec![ContentBlock::Text {
        text: super::describe_text_user_prompt(context, excerpt),
        cache_control: None,
      }],
    };

    cached_api_request(
      self.model.clone(),
      1024,
      Some(vec![cached_system_block(system_text)]),
      vec![Message {
        role: "user",
        content,
      }],
    )
  }

  /// Submit describe requests as a message batch, poll until it
  /// ends, and collect per-item results in submission order.
  async fn run_message_batch(
    &self,
    requests: &[DescribeRequest],
  ) -> Result<Vec<Result<ContentDescription>>> {
    let items: Vec<BatchRequestItem> = requests
      .iter()
      .enumerate()
      .map(|(i, r)| BatchRequestItem {
        custom_id: format!("req-{i}"),
        params: self.describe_api_request(&r.payload, &r.context),
      })
      .collect();

    let endpoint = format!("{}/v1/messages/batches", self.base_url);
    let response = self
      .client
      .post(&endpoint)
      .header("x-api-key", &self.api_key)
      .header("anthropic-version", "2023-06-01")
      .header("content-type", "application/json")
      .json(&BatchSubmitBody { requests: items })
      .send()
      .await
      .context("Failed to submit message batch")?;

    if !response.status().is_success() {
      let status = response.status();
      let body = response.text().await.unwrap_or_default();
      anyhow::bail!(
        "Batch submission failed ({}): {}",
        status,
        preview(&body, 200)
      );
    }

    let mut batch: BatchStatus = response
      .json()
      .await
      .context("Failed to parse batch submission response")?;

    tracing::info!(
      batch_id = %batch.id,
      requests = requests.len(),
      "Submitted message batch"
    );

    let status_endpoint =
      format!("{}/v1/messages/batches/{}", self.base_url, batch.id);

    while batch.processing_status != "ended" {
      tokio::time::sleep(self.poll_interval).await;
      let response = self
        .client
        .get(&status_endpoint)
        .header("x-api-key", &self.api_key)
        .header("anthropic-version", "2023-06-01")
        .send()
        .await
        .context("Failed to poll batch status")?;

      if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!(
          "Batch status poll failed ({}): {}",
          status,
          preview(&body, 200)
        );
      }

      batch = response
        .json()
        .await
        .context("Failed to parse batch status response")?;
      tracing::debug!(
        batch_id = %batch.id,
        status = %batch.processing_status,
        "Polled message batch"
      );
    }

    let results_endpoint = format!("{status_endpoint}/results");
    let response = self
      .client
      .get(&results_endpoint)
      .header("x-api-key", &self.api_key)
      .header("anthropic-version", "2023-06-01")
      .send()
      .await
      .context("Failed to fetch batch results")?;

    if !response.status().is_success() {
      let status = response.status();
      let body = response.text().await.unwrap_or_default();
      anyhow::bail!(
        "Batch results fetch failed ({}): {}",
        status,
        preview(&body, 200)
      );
    }

    let body = response
      .text()
      .await
      .context("Failed to read batch results body")?;

    let mut by_id = std::collections::HashMap::new();
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
      let parsed: BatchResultLine = serde_json::from_str(line)
        .with_context(|| {
          format!(
            "Failed to parse batch result line: {}",
            preview(line, 200)
          )
        })?;

      let outcome = match parsed.result {
        BatchResult::Succeeded { message } => response_text(message)
          .and_then(|text| {
            serde_json::from_str::<ContentDescription>(&text)
              .context("Failed to parse description JSON from Claude")
          }),
        BatchResult::Errored { error } => {
          Err(anyhow::anyhow!("Batch item failed: {error}"))
        }
        BatchResult::Canceled => {
          Err(anyhow::anyhow!("Batch item canceled"))
        }
        BatchResult::Expired => {
          Err(anyhow::anyhow!("Batch item expired"))
        }
      };
      by_id.insert(parsed.custom_id, outcome);
    }

    Ok(
      (0..requests.len())
        .map(|i| {
          by_id.remove(&format!("req-{i}")).unwrap_or_else(|| {
            Err(anyhow::anyhow!("Missing batch result for req-{i}"))
          })
        })
        .collect(),
    )
  }
}

/// Extract the JSON text payload from a successful API response,
/// rejecting truncated responses.
fn response_text(api_response: ApiResponse) -> Result<String> {
  let truncated =
    api_response.stop_reason.as_deref() == Some("max_tokens");

  let raw = api_response
    .content
    .into_iter()
    .find_map(|block| block.text)
    .context("No text content in Claude API response")?;

  if truncated {
    anyhow::bail!(
      "Claude response truncated (hit max_tokens limit). \
       Increase max_tokens or reduce the input size. \
       Partial response ({} bytes): {}",
      raw.len(),
      preview(&raw, 500),
    );
  }

  Ok(extract_json(&raw))
}

fn is_retryable_status(status: reqwest::StatusCode) -> bool {
  matches!(status.as_u16(), 429 | 500 | 502 | 503 | 529)
}

fn api_endpoint_fqdn(base_url: &str) -> String {
  Url::parse(base_url)
    .ok()
    .and_then(|url| url.host_str().map(str::to_string))
    .unwrap_or_else(|| base_url.to_string())
}

fn log_claude_api_call(_api_key: &str, endpoint: &str, fqdn: &str) {
  tracing::debug!(
    endpoint = %endpoint,
    fqdn = %fqdn,
    "Claude API request"
  );
}

fn preview(s: &str, max: usize) -> String {
  if s.len() <= max {
    s.to_string()
  } else {
    format!("{}…(truncated, {} bytes total)", &s[..max], s.len())
  }
}

fn extract_json(raw: &str) -> String {
  let trimmed = raw.trim();

  if trimmed.starts_with('{') || trimmed.starts_with('[') {
    return trimmed.to_string();
  }

  if let Some(start) = trimmed.find("```") {
    let after_fence = &trimmed[start + 3..];
    let content_start =
      after_fence.find('\n').map(|i| i + 1).unwrap_or(0);
    let content = &after_fence[content_start..];
    if let Some(end) = content.find("```") {
      return content[..end].trim().to_string();
    }
    if let Some(open) = content.find(['{', '[']) {
      return content[open..].trim().to_string();
    }
  }

  if let Some(start) = trimmed.find('{') {
    let mut depth = 0;
    let mut in_string = false;
    let mut escape = false;
    for (i, ch) in trimmed[start..].char_indices() {
      if escape {
        escape = false;
        continue;
      }
      if in_string {
        match ch {
          '\\' => escape = true,
          '"' => in_string = false,
          _ => {}
        }
        continue;
      }
      match ch {
        '"' => in_string = true,
        '{' => depth += 1,
        '}' => {
          depth -= 1;
          if depth == 0 {
            return trimmed[start..start + i + 1].to_string();
          }
        }
        _ => {}
      }
    }
    return trimmed[start..].to_string();
  }

  trimmed.to_string()
}

impl AiProvider for ClaudeProvider {
  async fn describe_image(
    &self,
    image_data: &[u8],
    mime_type: &str,
    context: &DescribeContext,
  ) -> Result<ContentDescription> {
    let payload = DescribePayload::Image {
      data: image_data.to_vec(),
      mime_type: mime_type.to_string(),
    };
    let request = self.describe_api_request(&payload, context);

    let text = self.send_request(request).await?;
    let description: ContentDescription = serde_json::from_str(&text)
      .context("Failed to parse description JSON from Claude")?;
    Ok(description)
  }

  async fn describe_text(
    &self,
    excerpt: &str,
    context: &DescribeContext,
  ) -> Result<ContentDescription> {
    let payload = DescribePayload::Text {
      excerpt: excerpt.to_string(),
    };
    let request = self.describe_api_request(&payload, context);

    let text = self.send_request(request).await?;
    let description: ContentDescription = serde_json::from_str(&text)
      .context("Failed to parse description JSON from Claude")?;
    Ok(description)
  }

  async fn describe_batch(
    &self,
    requests: Vec<DescribeRequest>,
  ) -> Vec<Result<ContentDescription>> {
    match self.run_message_batch(&requests).await {
      Ok(results) => results,
      Err(err) => {
        // Batch-level failure (submission/poll/fetch): every item
        // fails with the shared cause.
        let msg = format!("{err:#}");
        requests
          .iter()
          .map(|_| Err(anyhow::anyhow!("{msg}")))
          .collect()
      }
    }
  }

  async fn propose_groups(
    &self,
    files: &[FileSummary],
  ) -> Result<Vec<ProposedGroup>> {
    let request = cached_api_request(
      self.model.clone(),
      32_768,
      Some(vec![cached_system_block(super::group_system_prompt())]),
      vec![Message {
        role: "user",
        content: vec![ContentBlock::Text {
          text: super::group_user_prompt(files),
          cache_control: None,
        }],
      }],
    );

    let text = self.send_request(request).await?;

    #[derive(Deserialize)]
    struct GroupResponse {
      groups: Vec<ProposedGroup>,
    }

    let response: GroupResponse = serde_json::from_str(&text)
      .with_context(|| {
        let preview = if text.len() > 500 {
          format!("{}...(truncated, {} bytes total)", &text[..500], text.len())
        } else {
          text.clone()
        };
        format!(
          "Failed to parse groups JSON from Claude. Raw response:\n{}",
          preview
        )
      })?;

    let groups = response
      .groups
      .into_iter()
      .map(|mut g| {
        if !g.member_destinations.is_empty()
          && g.member_indices.is_empty()
        {
          g.member_indices =
            g.member_destinations.iter().map(|m| m.index).collect();
        }
        g
      })
      .collect();

    Ok(groups)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn api_request_serializes_prompt_caching_fields() {
    let request = cached_api_request(
      "claude-sonnet-4-20250514".to_string(),
      1024,
      Some(vec![cached_system_block("static instructions")]),
      vec![Message {
        role: "user",
        content: vec![ContentBlock::Text {
          text: "dynamic".to_string(),
          cache_control: None,
        }],
      }],
    );

    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["cache_control"]["type"], "ephemeral");
    assert_eq!(
      json["system"][0]["cache_control"]["type"],
      "ephemeral"
    );
  }

  use wiremock::matchers::{header, method, path};
  use wiremock::{Mock, MockServer, ResponseTemplate};

  #[tokio::test]
  async fn describe_image_sends_correct_headers() {
    let server = MockServer::start().await;

    let response_body = serde_json::json!({
      "content": [{"type": "text", "text": "{\"summary\":\"A red pixel\",\"tags\":[\"red\"],\"suggested_category\":\"photo\",\"confidence\":0.9}"}]
    });

    Mock::given(method("POST"))
      .and(path("/v1/messages"))
      .and(header("x-api-key", "test-key"))
      .and(header("anthropic-version", "2023-06-01"))
      .respond_with(
        ResponseTemplate::new(200).set_body_json(&response_body),
      )
      .expect(1)
      .mount(&server)
      .await;

    let provider = ClaudeProvider::new(
      "test-key".to_string(),
      "claude-sonnet-4-20250514".to_string(),
      0,
    )
    .with_base_url(server.uri());

    let ctx = DescribeContext {
      filename: "red.png".to_string(),
      file_type_label: "PNG".to_string(),
      file_size: 100,
      metadata_hint: None,
    };

    let result = provider
      .describe_image(&[0xFF, 0x00, 0x00], "image/png", &ctx)
      .await
      .unwrap();

    assert_eq!(result.summary, "A red pixel");
    assert_eq!(result.suggested_category, "photo");
  }

  #[tokio::test]
  async fn describe_image_handles_api_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
      .and(path("/v1/messages"))
      .respond_with(
        ResponseTemplate::new(401).set_body_string("invalid api key"),
      )
      .mount(&server)
      .await;

    let provider = ClaudeProvider::new(
      "bad-key".to_string(),
      "claude-sonnet-4-20250514".to_string(),
      0,
    )
    .with_base_url(server.uri());

    let ctx = DescribeContext {
      filename: "x.jpg".to_string(),
      file_type_label: "JPEG".to_string(),
      file_size: 50,
      metadata_hint: None,
    };

    let result =
      provider.describe_image(&[0x00], "image/jpeg", &ctx).await;

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("401"));
  }

  #[tokio::test]
  async fn propose_groups_parses_response() {
    let server = MockServer::start().await;

    let response_body = serde_json::json!({
      "content": [{"type": "text", "text": "{\"groups\":[{\"label\":\"Beach Photos\",\"rationale\":\"All beach scenes\",\"member_indices\":[0,1]}]}"}]
    });

    Mock::given(method("POST"))
      .and(path("/v1/messages"))
      .respond_with(
        ResponseTemplate::new(200).set_body_json(&response_body),
      )
      .mount(&server)
      .await;

    let provider = ClaudeProvider::new(
      "test-key".to_string(),
      "claude-sonnet-4-20250514".to_string(),
      0,
    )
    .with_base_url(server.uri());

    let files = vec![
      FileSummary {
        index: 0,
        filename: "beach1.jpg".to_string(),
        source_path: "beach1.jpg".to_string(),
        description: ContentDescription {
          summary: "Beach".to_string(),
          tags: vec![],
          suggested_category: "photo".to_string(),
          confidence: 0.9,
        },
        metadata_hint: String::new(),
      },
      FileSummary {
        index: 1,
        filename: "beach2.jpg".to_string(),
        source_path: "beach2.jpg".to_string(),
        description: ContentDescription {
          summary: "Beach again".to_string(),
          tags: vec![],
          suggested_category: "photo".to_string(),
          confidence: 0.9,
        },
        metadata_hint: String::new(),
      },
    ];

    let groups = provider.propose_groups(&files).await.unwrap();

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].label, "Beach Photos");
    assert_eq!(groups[0].member_indices, vec![0, 1]);
  }

  #[tokio::test]
  async fn propose_groups_handles_malformed_json() {
    let server = MockServer::start().await;

    let response_body = serde_json::json!({
      "content": [{"type": "text", "text": "not valid json at all"}]
    });

    Mock::given(method("POST"))
      .and(path("/v1/messages"))
      .respond_with(
        ResponseTemplate::new(200).set_body_json(&response_body),
      )
      .mount(&server)
      .await;

    let provider = ClaudeProvider::new(
      "test-key".to_string(),
      "claude-sonnet-4-20250514".to_string(),
      0,
    )
    .with_base_url(server.uri());

    let result = provider.propose_groups(&[]).await;

    assert!(result.is_err());
  }

  #[tokio::test]
  async fn propose_groups_derives_indices_from_members() {
    let server = MockServer::start().await;

    let response_body = serde_json::json!({
      "content": [{"type": "text", "text": "{\"groups\":[{\"label\":\"Cats\",\"rationale\":\"Cat photos\",\"members\":[{\"index\":0,\"dest_name\":\"cat1.jpg\"},{\"index\":2,\"dest_name\":\"porn/cat3.jpg\"}]}]}"}]
    });

    Mock::given(method("POST"))
      .and(path("/v1/messages"))
      .respond_with(
        ResponseTemplate::new(200).set_body_json(&response_body),
      )
      .mount(&server)
      .await;

    let provider = ClaudeProvider::new(
      "test-key".to_string(),
      "claude-sonnet-4-20250514".to_string(),
      0,
    )
    .with_base_url(server.uri());

    let groups = provider.propose_groups(&[]).await.unwrap();

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].member_indices, vec![0, 2]);
    assert_eq!(groups[0].member_destinations.len(), 2);
    assert_eq!(
      groups[0].member_destinations[0].dest_name,
      "cat1.jpg"
    );
    assert_eq!(
      groups[0].member_destinations[1].dest_name,
      "porn/cat3.jpg"
    );
  }

  #[test]
  fn extract_json_passes_through_raw_json() {
    let input = r#"{"groups": []}"#;
    assert_eq!(extract_json(input), input);
  }

  #[test]
  fn extract_json_strips_markdown_fences() {
    let input =
      "Here's the result:\n```json\n{\"groups\": []}\n```\n";
    assert_eq!(extract_json(input), r#"{"groups": []}"#);
  }

  #[test]
  fn extract_json_finds_object_in_preamble() {
    let input =
      "Sure! Here is the grouping:\n{\"groups\": [{\"label\": \"A\"}]}";
    assert_eq!(
      extract_json(input),
      r#"{"groups": [{"label": "A"}]}"#
    );
  }

  #[test]
  fn extract_json_handles_whitespace() {
    let input = "  \n  {\"key\": \"value\"}  \n  ";
    assert_eq!(extract_json(input), r#"{"key": "value"}"#);
  }

  fn batch_test_requests() -> Vec<DescribeRequest> {
    vec![
      DescribeRequest {
        payload: DescribePayload::Image {
          data: vec![0xFF, 0xD8],
          mime_type: "image/jpeg".to_string(),
        },
        context: DescribeContext {
          filename: "a.jpg".to_string(),
          file_type_label: "JPEG image".to_string(),
          file_size: 2,
          metadata_hint: None,
        },
      },
      DescribeRequest {
        payload: DescribePayload::Text {
          excerpt: "LEASE AGREEMENT".to_string(),
        },
        context: DescribeContext {
          filename: "lease.txt".to_string(),
          file_type_label: "TXT document".to_string(),
          file_size: 15,
          metadata_hint: None,
        },
      },
    ]
  }

  fn batch_description_json(summary: &str) -> String {
    serde_json::json!({
      "summary": summary,
      "tags": ["t"],
      "suggested_category": "other",
      "confidence": 0.9,
    })
    .to_string()
  }

  fn batch_result_line(custom_id: &str, summary: &str) -> String {
    serde_json::json!({
      "custom_id": custom_id,
      "result": {
        "type": "succeeded",
        "message": {
          "content": [
            {"type": "text", "text": batch_description_json(summary)}
          ],
          "stop_reason": "end_turn",
        },
      },
    })
    .to_string()
  }

  #[tokio::test]
  async fn describe_batch_submits_and_orders_results() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
      .and(path("/v1/messages/batches"))
      .and(header("x-api-key", "test-key"))
      .respond_with(ResponseTemplate::new(200).set_body_json(
        serde_json::json!({
          "id": "msgbatch_01",
          "processing_status": "ended",
        }),
      ))
      .mount(&server)
      .await;

    // Results returned out of submission order on purpose.
    let results_body = format!(
      "{}\n{}\n",
      batch_result_line("req-1", "a lease"),
      batch_result_line("req-0", "a photo"),
    );
    Mock::given(method("GET"))
      .and(path("/v1/messages/batches/msgbatch_01/results"))
      .respond_with(
        ResponseTemplate::new(200).set_body_string(results_body),
      )
      .mount(&server)
      .await;

    let provider = ClaudeProvider::new(
      "test-key".to_string(),
      "claude-fable-5".to_string(),
      0,
    )
    .with_base_url(server.uri());

    let results =
      provider.describe_batch(batch_test_requests()).await;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].as_ref().unwrap().summary, "a photo");
    assert_eq!(results[1].as_ref().unwrap().summary, "a lease");
  }

  #[tokio::test]
  async fn describe_batch_polls_until_ended() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
      .and(path("/v1/messages/batches"))
      .respond_with(ResponseTemplate::new(200).set_body_json(
        serde_json::json!({
          "id": "msgbatch_02",
          "processing_status": "in_progress",
        }),
      ))
      .mount(&server)
      .await;

    // First poll still in progress, then ended.
    Mock::given(method("GET"))
      .and(path("/v1/messages/batches/msgbatch_02"))
      .respond_with(ResponseTemplate::new(200).set_body_json(
        serde_json::json!({
          "id": "msgbatch_02",
          "processing_status": "in_progress",
        }),
      ))
      .up_to_n_times(1)
      .mount(&server)
      .await;
    Mock::given(method("GET"))
      .and(path("/v1/messages/batches/msgbatch_02"))
      .respond_with(ResponseTemplate::new(200).set_body_json(
        serde_json::json!({
          "id": "msgbatch_02",
          "processing_status": "ended",
        }),
      ))
      .mount(&server)
      .await;

    Mock::given(method("GET"))
      .and(path("/v1/messages/batches/msgbatch_02/results"))
      .respond_with(ResponseTemplate::new(200).set_body_string(
        format!(
          "{}\n{}\n",
          batch_result_line("req-0", "a photo"),
          batch_result_line("req-1", "a lease"),
        ),
      ))
      .mount(&server)
      .await;

    let provider = ClaudeProvider::new(
      "test-key".to_string(),
      "claude-fable-5".to_string(),
      0,
    )
    .with_base_url(server.uri())
    .with_poll_interval(std::time::Duration::ZERO);

    let results =
      provider.describe_batch(batch_test_requests()).await;

    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.is_ok()));
  }

  #[tokio::test]
  async fn describe_batch_reports_errored_items_individually() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
      .and(path("/v1/messages/batches"))
      .respond_with(ResponseTemplate::new(200).set_body_json(
        serde_json::json!({
          "id": "msgbatch_03",
          "processing_status": "ended",
        }),
      ))
      .mount(&server)
      .await;

    let errored = serde_json::json!({
      "custom_id": "req-1",
      "result": {
        "type": "errored",
        "error": {"type": "invalid_request", "message": "too large"},
      },
    })
    .to_string();
    Mock::given(method("GET"))
      .and(path("/v1/messages/batches/msgbatch_03/results"))
      .respond_with(ResponseTemplate::new(200).set_body_string(
        format!(
          "{}\n{}\n",
          batch_result_line("req-0", "a photo"),
          errored,
        ),
      ))
      .mount(&server)
      .await;

    let provider = ClaudeProvider::new(
      "test-key".to_string(),
      "claude-fable-5".to_string(),
      0,
    )
    .with_base_url(server.uri());

    let results =
      provider.describe_batch(batch_test_requests()).await;

    assert!(results[0].is_ok());
    let err = results[1].as_ref().unwrap_err().to_string();
    assert!(err.contains("too large"));
  }

  #[tokio::test]
  async fn describe_batch_fails_all_items_on_submission_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
      .and(path("/v1/messages/batches"))
      .respond_with(
        ResponseTemplate::new(401).set_body_string("bad key"),
      )
      .mount(&server)
      .await;

    let provider = ClaudeProvider::new(
      "test-key".to_string(),
      "claude-fable-5".to_string(),
      0,
    )
    .with_base_url(server.uri());

    let results =
      provider.describe_batch(batch_test_requests()).await;

    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.is_err()));
  }
}

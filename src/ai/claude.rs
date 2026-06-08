use anyhow::{Context, Result};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};

use crate::model::{ContentDescription, FileSummary, ProposedGroup};

use super::{AiProvider, DescribeContext};

const LOCAL_API_KEY: &str =
  "REDACTED_KEY_ROTATE_IMMEDIATELY";
const LOCAL_BASE_URL: &str = "http://localhost:8787";

pub struct ClaudeProvider {
  client: Client,
  api_key: String,
  model: String,
  base_url: String,
}

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

#[derive(Deserialize)]
struct ResponseBlock {
  text: Option<String>,
}

impl ClaudeProvider {
  pub fn new(
    api_key: impl Into<String>,
    model: impl Into<String>,
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
    }
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

    let response = self
      .client
      .post(&endpoint)
      .header("x-api-key", &self.api_key)
      .header("anthropic-version", "2023-06-01")
      .header("content-type", "application/json")
      .json(&request)
      .send()
      .await
      .context("Failed to send request to Claude API")?;

    let status = response.status();
    if !status.is_success() {
      let body = response.text().await.unwrap_or_default();
      anyhow::bail!("Claude API error ({}): {}", status, body);
    }

    let api_response: ApiResponse = response
      .json()
      .await
      .context("Failed to parse Claude API response")?;

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
    use base64::Engine;
    let encoded =
      base64::engine::general_purpose::STANDARD.encode(image_data);
    let user_text = super::describe_user_prompt(context);
    let system_text = format!(
      "{task}\n\n{instructions}",
      task = super::describe_system_prompt(),
      instructions = super::describe_response_instructions(),
    );

    let request = cached_api_request(
      self.model.clone(),
      1024,
      Some(vec![cached_system_block(system_text)]),
      vec![Message {
        role: "user",
        content: vec![
          ContentBlock::Image {
            source: ImageSource {
              source_type: "base64",
              media_type: mime_type.to_string(),
              data: encoded,
            },
          },
          ContentBlock::Text {
            text: user_text,
            cache_control: None,
          },
        ],
      }],
    );

    let text = self.send_request(request).await?;
    let description: ContentDescription = serde_json::from_str(&text)
      .context("Failed to parse description JSON from Claude")?;
    Ok(description)
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
}

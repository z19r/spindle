use anyhow::{Context, Result};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};

use std::collections::HashMap;

use super::{DegenerateReply, GroupingHints, Usage};
use crate::model::{
  Area, ContentDescription, FileSummary, ProposedGroup, RoutedFile,
};

use super::{
  AiProvider, DescribeContext, DescribePayload, DescribeRequest,
};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Largest reply budget for any grouping call.
const GROUP_MAX_TOKENS: u32 = 32_768;

/// Reply budget for a grouping call over `files` files: room for each
/// member line plus rationales, tight enough that a looping reply
/// fails in seconds rather than minutes.
fn group_reply_budget(files: usize) -> u32 {
  (3_000 + 450 * files as u32).min(GROUP_MAX_TOKENS)
}

/// Upper bound on one API call. Grouping replies can legitimately take
/// a minute or two; anything beyond this is a stall worth retrying.
const DEFAULT_REQUEST_TIMEOUT: std::time::Duration =
  std::time::Duration::from_secs(300);

/// A streaming reply that sends nothing for this long is treated as
/// stalled and retried.
const DEFAULT_IDLE_TIMEOUT: std::time::Duration =
  std::time::Duration::from_secs(60);

fn http_client(timeout: std::time::Duration) -> Client {
  Client::builder()
    .timeout(timeout)
    .build()
    .unwrap_or_else(|_| Client::new())
}

pub struct ClaudeProvider {
  client: Client,
  idle_timeout: std::time::Duration,
  /// Tokens spent per model, for the run summary.
  spent: std::sync::Mutex<HashMap<String, Usage>>,
  api_key: String,
  /// Model for the grouping call (the hard reasoning step).
  model: String,
  /// Model for per-file descriptions (high volume, mostly vision).
  describe_model: String,
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
  /// Structured-output constraint: the response must validate
  /// against a JSON schema. `extract_json` remains as a fallback for
  /// proxies/models that ignore it.
  #[serde(skip_serializing_if = "Option::is_none")]
  output_config: Option<serde_json::Value>,
  /// Read the reply as server-sent events so a stalled connection or a
  /// looping generation is caught within seconds.
  #[serde(skip_serializing_if = "Option::is_none")]
  stream: Option<bool>,
}

impl ApiRequest {
  fn streaming(mut self) -> Self {
    self.stream = Some(true);
    self
  }
}

/// `output_config.format` payload constraining the response to
/// `ContentDescription`.
fn describe_output_config() -> serde_json::Value {
  serde_json::json!({
    "format": {
      "type": "json_schema",
      "schema": {
        "type": "object",
        "properties": {
          "summary": {"type": "string"},
          "tags": {"type": "array", "items": {"type": "string"}},
          "suggested_category": {"type": "string"},
          "confidence": {"type": "number"}
        },
        "required": [
          "summary", "tags", "suggested_category", "confidence"
        ],
        "additionalProperties": false
      }
    }
  })
}

/// `output_config.format` payload for routing: `area` is an enum of the
/// configured names, so the model cannot invent one.
fn route_output_config(areas: &[Area]) -> serde_json::Value {
  let names: Vec<&str> =
    areas.iter().map(|a| a.name.as_str()).collect();
  serde_json::json!({
    "format": {
      "type": "json_schema",
      "schema": {
        "type": "object",
        "properties": {
          "assignments": {
            "type": "array",
            "minItems": 1,
            "items": {
              "type": "object",
              "properties": {
                "index": {"type": "integer"},
                "area": {"type": "string", "enum": names}
              },
              "required": ["index", "area"],
              "additionalProperties": false
            }
          }
        },
        "required": ["assignments"],
        "additionalProperties": false
      }
    }
  })
}

/// `output_config.format` payload constraining the grouping response.
fn group_output_config() -> serde_json::Value {
  serde_json::json!({
    "format": {
      "type": "json_schema",
      "schema": {
        "type": "object",
        "properties": {
          "groups": {
            "type": "array",
            "minItems": 1,
            "items": {
              "type": "object",
              "properties": {
                "label": {"type": "string"},
                "rationale": {"type": "string"},
                "members": {
                  "type": "array",
                  "minItems": 1,
                  "items": {
                    "type": "object",
                    "properties": {
                      "index": {"type": "integer"},
                      "dest_name": {"type": "string"}
                    },
                    "required": ["index", "dest_name"],
                    "additionalProperties": false
                  }
                }
              },
              "required": ["label", "rationale", "members"],
              "additionalProperties": false
            }
          }
        },
        "required": ["groups"],
        "additionalProperties": false
      }
    }
  })
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
  output_config: Option<serde_json::Value>,
) -> ApiRequest {
  ApiRequest {
    model,
    max_tokens,
    cache_control: CacheControl::ephemeral(),
    system,
    messages,
    output_config,
    stream: None,
  }
}

#[derive(Deserialize)]
struct ApiResponse {
  content: Vec<ResponseBlock>,
  stop_reason: Option<String>,
  #[serde(default)]
  usage: Option<ApiUsage>,
}

#[derive(Deserialize, Default, Clone, Copy)]
struct ApiUsage {
  #[serde(default)]
  input_tokens: u64,
  #[serde(default)]
  output_tokens: u64,
  #[serde(default)]
  cache_read_input_tokens: u64,
}

impl From<ApiUsage> for Usage {
  fn from(u: ApiUsage) -> Self {
    Usage {
      calls: 1,
      input_tokens: u.input_tokens + u.cache_read_input_tokens,
      cache_read_tokens: u.cache_read_input_tokens,
      output_tokens: u.output_tokens,
    }
  }
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
    let model = model.into();
    Self {
      client: http_client(DEFAULT_REQUEST_TIMEOUT),
      idle_timeout: DEFAULT_IDLE_TIMEOUT,
      spent: std::sync::Mutex::new(HashMap::new()),
      api_key: api_key.into(),
      describe_model: model.clone(),
      model,
      base_url: DEFAULT_BASE_URL.to_string(),
      max_retries,
      poll_interval: DEFAULT_BATCH_POLL_INTERVAL,
    }
  }

  /// Use a different (usually cheaper) model for per-file
  /// descriptions than for grouping.
  pub fn with_describe_model(self, model: impl Into<String>) -> Self {
    let mut this = self;
    this.describe_model = model.into();
    this
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

  /// Give up on a single API call after `timeout`; the call is then
  /// retried like any other transport error.
  pub fn with_timeout(self, timeout: std::time::Duration) -> Self {
    let mut this = self;
    this.client = http_client(timeout);
    this
  }

  fn record_usage(&self, model: &str, usage: Usage) {
    if let Ok(mut spent) = self.spent.lock() {
      spent.entry(model.to_string()).or_default().add(usage);
    }
  }

  /// Treat a streaming reply as stalled after `idle` without bytes.
  pub fn with_idle_timeout(self, idle: std::time::Duration) -> Self {
    let mut this = self;
    this.idle_timeout = idle;
    this
  }

  /// Override the API base URL (e.g. for a proxy). Trailing slashes are
  /// trimmed so endpoint joining stays correct.
  pub fn with_base_url(self, url: impl Into<String>) -> Self {
    let mut this = self;
    this.base_url = url.into().trim_end_matches('/').to_string();
    this
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

      // Proxies and mocks may ignore `stream`; only parse events when
      // the server actually sent them.
      let is_event_stream = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.starts_with("text/event-stream"))
        .unwrap_or(false);
      if request.stream == Some(true) && is_event_stream {
        let reply = collect_sse_text(
          response.bytes_stream(),
          self.idle_timeout,
        )
        .await?;
        tracing::debug!(
          bytes = reply.text.len(),
          output_tokens = ?reply.output_tokens,
          max_tokens = request.max_tokens,
          stop_reason = ?reply.stop_reason,
          "Streamed reply complete"
        );
        self.record_usage(
          &request.model,
          Usage {
            calls: 1,
            input_tokens: reply.input_tokens
              + reply.cache_read_tokens,
            cache_read_tokens: reply.cache_read_tokens,
            output_tokens: reply.output_tokens.unwrap_or(0),
          },
        );
        return finish_text(reply.text, reply.stop_reason.as_deref());
      }

      let api_response: ApiResponse = response
        .json()
        .await
        .context("Failed to parse Claude API response")?;
      if let Some(usage) = api_response.usage {
        self.record_usage(&request.model, usage.into());
      }

      return response_text(api_response);
    }

    Err(last_err.unwrap_or_else(|| {
      anyhow::anyhow!("All retry attempts exhausted")
    }))
  }

  /// One grouping call: cached system prompt, schema-constrained
  /// reply, member indices derived from destinations.
  async fn send_group_request(
    &self,
    user_prompt: String,
    max_tokens: u32,
  ) -> Result<Vec<ProposedGroup>> {
    let request = cached_api_request(
      self.model.clone(),
      max_tokens,
      Some(vec![cached_system_block(super::group_system_prompt())]),
      vec![Message {
        role: "user",
        content: vec![ContentBlock::Text {
          text: user_prompt,
          cache_control: None,
        }],
      }],
      Some(group_output_config()),
    )
    .streaming();

    tracing::debug!(
      prompt = %preview(&request_prompt_text(&request), 6000),
      "Grouping prompt"
    );
    let text = self.send_request(request).await?;
    tracing::debug!(raw = %preview(&text, 6000), "Grouping reply");

    #[derive(Deserialize)]
    struct GroupResponse {
      groups: Vec<ProposedGroup>,
    }

    let response: GroupResponse = serde_json::from_str(&text)
      .with_context(|| {
        format!(
          "Failed to parse groups JSON from Claude. Raw response:\n{}",
          preview(&text, 500)
        )
      })?;

    Ok(finish_groups(response.groups, &text))
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
      self.describe_model.clone(),
      1024,
      Some(vec![cached_system_block(system_text)]),
      vec![Message {
        role: "user",
        content,
      }],
      Some(describe_output_config()),
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
        BatchResult::Succeeded { message } => {
          if let Some(usage) = message.usage {
            self.record_usage(&self.describe_model, usage.into());
          }
          response_text(message)
        }
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

/// The user-turn text of a request, for debug logging. Image blocks
/// are skipped so a photo never lands in the log.
fn request_prompt_text(request: &ApiRequest) -> String {
  request
    .messages
    .iter()
    .flat_map(|m| m.content.iter())
    .filter_map(|c| match c {
      ContentBlock::Text { text, .. } => Some(text.as_str()),
      _ => None,
    })
    .collect::<Vec<_>>()
    .join("\n")
}

/// Normalise parsed groups: derive member indices from destinations and
/// log the raw reply when the model placed nothing, which otherwise
/// looks like a healthy response with empty groups.
fn finish_groups(
  groups: Vec<ProposedGroup>,
  raw: &str,
) -> Vec<ProposedGroup> {
  let groups: Vec<ProposedGroup> = groups
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
  if groups.iter().all(|g| g.member_indices.is_empty()) {
    tracing::warn!(
      groups = groups.len(),
      raw = %preview(raw, 4000),
      "Grouping response placed no files"
    );
  }
  groups
}

/// Extract the JSON text payload from a successful API response,
/// rejecting truncated responses.
fn response_text(api_response: ApiResponse) -> Result<String> {
  let raw = api_response
    .content
    .into_iter()
    .find_map(|block| block.text)
    .context("No text content in Claude API response")?;
  finish_text(raw, api_response.stop_reason.as_deref())
}

fn finish_text(
  raw: String,
  stop_reason: Option<&str>,
) -> Result<String> {
  if stop_reason == Some("max_tokens") {
    return Err(
      DegenerateReply(format!(
        "reply truncated at max_tokens ({} bytes): {}",
        raw.len(),
        preview(&raw, 300),
      ))
      .into(),
    );
  }
  if raw.trim().is_empty() {
    anyhow::bail!("No text content in Claude API response");
  }
  Ok(extract_json(&raw))
}

/// Text and stop reason assembled from a streamed reply.
#[derive(Debug)]
struct StreamedReply {
  text: String,
  stop_reason: Option<String>,
  output_tokens: Option<u64>,
  input_tokens: u64,
  cache_read_tokens: u64,
}

/// Assemble the text of a Messages API SSE stream. Fails as a
/// `DegenerateReply` when nothing arrives for `idle` or when the text
/// starts repeating itself, so a looping generation is cut off early.
async fn collect_sse_text<S, B, E>(
  stream: S,
  idle: std::time::Duration,
) -> Result<StreamedReply>
where
  S: futures::Stream<Item = std::result::Result<B, E>>,
  B: AsRef<[u8]>,
  E: std::fmt::Display,
{
  use futures::StreamExt;
  let mut stream = std::pin::pin!(stream);
  let mut buffer = String::new();
  let mut reply = StreamedReply {
    text: String::new(),
    stop_reason: None,
    output_tokens: None,
    input_tokens: 0,
    cache_read_tokens: 0,
  };
  let mut checked_at = 0usize;

  loop {
    let next = match tokio::time::timeout(idle, stream.next()).await {
      Ok(next) => next,
      Err(_) => {
        return Err(
          DegenerateReply(format!(
            "no bytes for {}s ({} bytes received)",
            idle.as_secs(),
            reply.text.len()
          ))
          .into(),
        );
      }
    };
    let chunk = match next {
      Some(Ok(chunk)) => chunk,
      Some(Err(e)) => anyhow::bail!("Stream read failed: {e}"),
      None => break,
    };
    buffer.push_str(&String::from_utf8_lossy(chunk.as_ref()));

    while let Some(pos) = buffer.find("\n\n") {
      let event = buffer[..pos].to_string();
      buffer.drain(..pos + 2);
      for line in event.lines() {
        let Some(data) = line.strip_prefix("data:") else {
          continue;
        };
        let Ok(value) =
          serde_json::from_str::<serde_json::Value>(data.trim())
        else {
          continue;
        };
        match value["type"].as_str() {
          Some("content_block_delta") => {
            if let Some(t) = value["delta"]["text"].as_str() {
              reply.text.push_str(t);
            }
          }
          Some("message_start") => {
            let usage = &value["message"]["usage"];
            reply.input_tokens =
              usage["input_tokens"].as_u64().unwrap_or(0);
            reply.cache_read_tokens =
              usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
          }
          Some("message_delta") => {
            if let Some(r) = value["delta"]["stop_reason"].as_str() {
              reply.stop_reason = Some(r.to_string());
            }
            if let Some(n) = value["usage"]["output_tokens"].as_u64()
            {
              reply.output_tokens = Some(n);
            }
          }
          Some("error") => {
            anyhow::bail!(
              "Claude API stream error: {}",
              value["error"]["message"].as_str().unwrap_or("unknown")
            );
          }
          _ => {}
        }
      }
    }

    if reply.text.len() >= checked_at + 1024 {
      checked_at = reply.text.len();
      if looks_degenerate(&reply.text) {
        return Err(
          DegenerateReply(format!(
            "repetition loop after {} bytes: {}",
            reply.text.len(),
            preview(&reply.text, 200)
          ))
          .into(),
        );
      }
    }
  }
  Ok(reply)
}

/// True when the tail of `text` is one short unit repeated many times,
/// the signature of a model stuck in a loop.
fn looks_degenerate(text: &str) -> bool {
  const TAIL: usize = 600;
  const MIN_REPEATS: usize = 40;
  let bytes = text.as_bytes();
  if bytes.len() < TAIL {
    return false;
  }
  let tail = &bytes[bytes.len() - TAIL..];
  (1..=12).any(|unit| {
    let pattern = &tail[TAIL - unit..];
    let repeats = tail
      .rchunks_exact(unit)
      .take_while(|c| *c == pattern)
      .count();
    repeats >= MIN_REPEATS && repeats * unit >= TAIL / 2
  })
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
  fn usage(&self) -> Vec<(String, Usage)> {
    let mut out: Vec<(String, Usage)> = self
      .spent
      .lock()
      .map(|m| m.iter().map(|(k, v)| (k.clone(), *v)).collect())
      .unwrap_or_default();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
  }

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
    self.propose_groups_with_context(files, &[]).await
  }

  async fn propose_groups_with_context(
    &self,
    files: &[FileSummary],
    existing_labels: &[String],
  ) -> Result<Vec<ProposedGroup>> {
    let user_prompt = format!(
      "{}{}",
      super::group_user_prompt(files),
      super::group_existing_groups_note(existing_labels),
    );
    self.send_group_request(user_prompt, GROUP_MAX_TOKENS).await
  }

  async fn route_files(
    &self,
    files: &[FileSummary],
    areas: &[Area],
  ) -> Result<Vec<RoutedFile>> {
    let request = cached_api_request(
      self.describe_model.clone(),
      8_192,
      Some(vec![cached_system_block(super::route_system_prompt(
        areas,
      ))]),
      vec![Message {
        role: "user",
        content: vec![ContentBlock::Text {
          text: super::route_user_prompt(files),
          cache_control: None,
        }],
      }],
      Some(route_output_config(areas)),
    )
    .streaming();
    let text = self.send_request(request).await?;

    #[derive(Deserialize)]
    struct RouteResponse {
      assignments: Vec<RoutedFile>,
    }
    let response: RouteResponse =
      serde_json::from_str(&text).with_context(|| {
        format!(
          "Failed to parse routing JSON from Claude. Raw response:\n{}",
          preview(&text, 500)
        )
      })?;
    Ok(response.assignments)
  }

  async fn propose_groups_in_area(
    &self,
    files: &[FileSummary],
    area: &Area,
    existing_labels: &[String],
    organized_context: &[(
      String,
      Vec<crate::model::ContentDescription>,
    )],
  ) -> Result<Vec<ProposedGroup>> {
    self
      .propose_groups_with_hints(
        files,
        &GroupingHints {
          area: Some(area),
          existing_labels,
          organized_context,
          renames: &[],
        },
      )
      .await
  }

  async fn propose_groups_with_hints(
    &self,
    files: &[FileSummary],
    hints: &GroupingHints<'_>,
  ) -> Result<Vec<ProposedGroup>> {
    let user_prompt = format!(
      "{}{}{}{}{}",
      super::group_user_prompt(files),
      hints.area.map(super::group_area_note).unwrap_or_default(),
      super::group_organized_context(hints.organized_context),
      super::group_existing_groups_note(hints.existing_labels),
      super::group_corrections_note(hints.renames),
    );
    let budget = group_reply_budget(files.len());
    self.send_group_request(user_prompt, budget).await
  }

  async fn propose_groups_with_organized_context(
    &self,
    files: &[FileSummary],
    existing_labels: &[String],
    organized_context: &[(
      String,
      Vec<crate::model::ContentDescription>,
    )],
  ) -> Result<Vec<ProposedGroup>> {
    if organized_context.is_empty() {
      return self
        .propose_groups_with_context(files, existing_labels)
        .await;
    }
    let user_prompt = format!(
      "{}{}{}",
      super::group_user_prompt(files),
      super::group_organized_context(organized_context),
      super::group_existing_groups_note(existing_labels),
    );
    self.send_group_request(user_prompt, GROUP_MAX_TOKENS).await
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::DescriptionSource;

  /// The grammar must forbid a group with no members and a reply with
  /// no groups: both parsed fine yet placed nothing in real runs.
  #[test]
  fn route_schema_constrains_area_to_the_configured_names() {
    let areas = vec![
      crate::model::Area::new("Work", "jobs"),
      crate::model::Area::new("Personal", "life"),
    ];
    let cfg = route_output_config(&areas);
    let assignments =
      &cfg["format"]["schema"]["properties"]["assignments"];
    assert_eq!(assignments["minItems"], 1);
    assert_eq!(
      assignments["items"]["properties"]["area"]["enum"],
      serde_json::json!(["Work", "Personal"])
    );
  }

  #[test]
  fn group_schema_requires_at_least_one_group_and_member() {
    let cfg = group_output_config();
    let groups = &cfg["format"]["schema"]["properties"]["groups"];
    assert_eq!(groups["minItems"], 1);
    assert_eq!(
      groups["items"]["properties"]["members"]["minItems"],
      1
    );
  }

  fn sse(events: &[serde_json::Value]) -> String {
    events
      .iter()
      .map(|e| format!("event: {}\ndata: {}\n\n", e["type"], e))
      .collect()
  }

  #[tokio::test]
  async fn usage_is_recorded_from_json_replies() {
    let server = MockServer::start().await;
    let json_body = serde_json::json!({
      "content": [{"type": "text", "text": "{\"summary\":\"A red pixel\",\"tags\":[],\"suggested_category\":\"photo\",\"confidence\":0.9}"}],
      "usage": {"input_tokens": 1200, "output_tokens": 40, "cache_read_input_tokens": 1000}
    });
    Mock::given(method("POST"))
      .and(path("/v1/messages"))
      .respond_with(
        ResponseTemplate::new(200).set_body_json(&json_body),
      )
      .mount(&server)
      .await;
    let provider = ClaudeProvider::new(
      "test-key".to_string(),
      "claude-opus-5".to_string(),
      0,
    )
    .with_describe_model("claude-haiku-4-5")
    .with_base_url(server.uri());
    let ctx = DescribeContext {
      filename: "red.png".to_string(),
      file_type_label: "PNG".to_string(),
      file_size: 100,
      metadata_hint: None,
    };
    for _ in 0..2 {
      provider
        .describe_image(&[0xFF], "image/png", &ctx)
        .await
        .unwrap();
    }

    let usage = provider.usage();
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].0, "claude-haiku-4-5");
    assert_eq!(
      usage[0].1,
      Usage {
        calls: 2,
        input_tokens: 4400,
        cache_read_tokens: 2000,
        output_tokens: 80
      }
    );
  }

  #[tokio::test]
  async fn streamed_usage_comes_from_message_start_and_delta() {
    let body = sse(&[
      serde_json::json!({"type":"message_start","message":{"usage":{"input_tokens":900,"cache_read_input_tokens":300}}}),
      serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"{}"}}),
      serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":55}}),
    ]);
    let stream =
      futures::stream::iter(vec![Ok::<_, String>(body.into_bytes())]);
    let reply =
      collect_sse_text(stream, std::time::Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(reply.input_tokens, 900);
    assert_eq!(reply.cache_read_tokens, 300);
    assert_eq!(reply.output_tokens, Some(55));
  }

  #[tokio::test]
  async fn grouping_reads_a_streamed_reply() {
    let server = MockServer::start().await;
    let body = sse(&[
      serde_json::json!({"type":"message_start","message":{}}),
      serde_json::json!({"type":"content_block_start","index":0}),
      serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"{\"groups\":[{\"label\":\"Work\","}}),
      serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"\"rationale\":\"r\",\"members\":[{\"index\":0,\"dest_name\":\"a\"}]}]}"}}),
      serde_json::json!({"type":"content_block_stop","index":0}),
      serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
      serde_json::json!({"type":"message_stop"}),
    ]);
    Mock::given(method("POST"))
      .and(path("/v1/messages"))
      .respond_with(
        ResponseTemplate::new(200)
          .set_body_raw(body.into_bytes(), "text/event-stream"),
      )
      .expect(1)
      .mount(&server)
      .await;
    let provider = ClaudeProvider::new(
      "test-key".to_string(),
      "claude-sonnet-4-6".to_string(),
      0,
    )
    .with_base_url(server.uri());

    let groups = provider.propose_groups(&[]).await.unwrap();

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].label, "Work");
    assert_eq!(groups[0].member_indices, vec![0]);
  }

  #[tokio::test]
  async fn streamed_max_tokens_is_degenerate() {
    let body = sse(&[
      serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"{\"groups\":["}}),
      serde_json::json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"}}),
    ]);
    let stream =
      futures::stream::iter(vec![Ok::<_, String>(body.into_bytes())]);
    let reply =
      collect_sse_text(stream, std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let err = finish_text(reply.text, reply.stop_reason.as_deref())
      .unwrap_err();
    assert!(
      err.downcast_ref::<DegenerateReply>().is_some(),
      "{err:#}"
    );
  }

  #[tokio::test]
  async fn idle_stream_is_degenerate() {
    use futures::StreamExt;
    let head = sse(&[
      serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"{"}}),
    ]);
    let stream =
      futures::stream::iter(vec![Ok::<_, String>(head.into_bytes())])
        .chain(futures::stream::pending());
    let err =
      collect_sse_text(stream, std::time::Duration::from_millis(50))
        .await
        .unwrap_err();
    let e =
      err.downcast_ref::<DegenerateReply>().expect("degenerate");
    assert!(e.0.contains("no bytes"), "{}", e.0);
  }

  #[tokio::test]
  async fn repetition_loop_is_cut_off_early() {
    let filler = " ---".repeat(600);
    let body = sse(&[
      serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"{\"groups\":[{\"label\":\"x\",\"rationale\":\"a"}}),
      serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text":filler}}),
      serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text":filler}}),
      serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
    ]);
    let chunks: Vec<std::result::Result<Vec<u8>, String>> = body
      .as_bytes()
      .chunks(512)
      .map(|c| Ok(c.to_vec()))
      .collect();
    let err = collect_sse_text(
      futures::stream::iter(chunks),
      std::time::Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    let e =
      err.downcast_ref::<DegenerateReply>().expect("degenerate");
    assert!(e.0.contains("repetition loop"), "{}", e.0);
  }

  #[test]
  fn group_reply_budget_scales_with_files_and_caps() {
    assert_eq!(group_reply_budget(5), 5_250);
    assert_eq!(group_reply_budget(35), 18_750);
    assert_eq!(group_reply_budget(200), GROUP_MAX_TOKENS);
  }

  #[test]
  fn looks_degenerate_only_for_long_repeats() {
    assert!(!looks_degenerate("short"));
    let healthy = "{\"groups\":[{\"label\":\"Work/Acme\",\"rationale\":\"notes and mockups for the redesign\"}]}".repeat(12);
    assert!(!looks_degenerate(&healthy));
    let looping =
      format!("{{\"rationale\":\"x{}", " ---".repeat(200));
    assert!(looks_degenerate(&looping));
    let dashes = format!("abc{}", "-".repeat(700));
    assert!(looks_degenerate(&dashes));
  }

  #[tokio::test]
  async fn truncated_reply_is_a_degenerate_reply_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
      .and(path("/v1/messages"))
      .respond_with(ResponseTemplate::new(200).set_body_json(
        serde_json::json!({
          "content": [{"type": "text", "text": "{\"groups\":[{\"label\":\"x\",\"rationale\":\" --- --- ---"}],
          "stop_reason": "max_tokens"
        }),
      ))
      .mount(&server)
      .await;
    let provider = ClaudeProvider::new(
      "test-key".to_string(),
      "claude-sonnet-4-6".to_string(),
      0,
    )
    .with_base_url(server.uri());

    let err = provider.propose_groups(&[]).await.unwrap_err();

    assert!(
      err.downcast_ref::<DegenerateReply>().is_some(),
      "expected DegenerateReply, got {err:#}"
    );
  }

  #[test]
  fn request_prompt_text_keeps_text_blocks_only() {
    let request = cached_api_request(
      "m".to_string(),
      10,
      None,
      vec![Message {
        role: "user",
        content: vec![
          ContentBlock::Image {
            source: ImageSource {
              source_type: "base64",
              media_type: "image/png".to_string(),
              data: "AAAA".to_string(),
            },
          },
          ContentBlock::Text {
            text: "Organize these 2 files:".to_string(),
            cache_control: None,
          },
        ],
      }],
      None,
    );
    assert_eq!(
      request_prompt_text(&request),
      "Organize these 2 files:"
    );
  }

  #[test]
  fn api_request_serializes_prompt_caching_fields() {
    let request = cached_api_request(
      "claude-sonnet-4-6".to_string(),
      1024,
      Some(vec![cached_system_block("static instructions")]),
      vec![Message {
        role: "user",
        content: vec![ContentBlock::Text {
          text: "dynamic".to_string(),
          cache_control: None,
        }],
      }],
      None,
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

  /// A stalled API reply must not hang the run: the request times out,
  /// is retried like any transport error, and finally fails loudly.
  #[tokio::test]
  async fn stalled_response_times_out_and_is_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
      .and(path("/v1/messages"))
      .respond_with(
        ResponseTemplate::new(200)
          .set_delay(std::time::Duration::from_millis(400))
          .set_body_json(serde_json::json!({"content": []})),
      )
      .expect(2)
      .mount(&server)
      .await;

    let provider = ClaudeProvider::new(
      "test-key".to_string(),
      "claude-sonnet-4-6".to_string(),
      1,
    )
    .with_base_url(server.uri())
    .with_timeout(std::time::Duration::from_millis(50));

    let ctx = DescribeContext {
      filename: "red.png".to_string(),
      file_type_label: "PNG".to_string(),
      file_size: 100,
      metadata_hint: None,
    };
    let started = std::time::Instant::now();
    let err = provider
      .describe_image(&[0xFF, 0x00, 0x00], "image/png", &ctx)
      .await
      .unwrap_err();

    assert!(
      format!("{err:#}").to_lowercase().contains("timed out")
        || format!("{err:#}").to_lowercase().contains("timeout"),
      "{err:#}"
    );
    // Two attempts of 50 ms plus one 1 s backoff, never the 800 ms of
    // two full stalled replies plus backoff.
    assert!(
      started.elapsed() < std::time::Duration::from_millis(1800)
    );
  }

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
      "claude-sonnet-4-6".to_string(),
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
      "claude-sonnet-4-6".to_string(),
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
      "claude-sonnet-4-6".to_string(),
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
          source: DescriptionSource::Ai,
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
          source: DescriptionSource::Ai,
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
      "claude-sonnet-4-6".to_string(),
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
      "claude-sonnet-4-6".to_string(),
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
      "claude-opus-4-8".to_string(),
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
      "claude-opus-4-8".to_string(),
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
      "claude-opus-4-8".to_string(),
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
      "claude-opus-4-8".to_string(),
      0,
    )
    .with_base_url(server.uri());

    let results =
      provider.describe_batch(batch_test_requests()).await;

    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.is_err()));
  }
}

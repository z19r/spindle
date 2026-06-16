use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::ai::{AiProvider, DescribeContext};
use crate::model::{
  ContentDescription, FingerprintedFile, MemberDestination,
  ProposedGroup,
};

pub struct AnalyzeOptions {
  pub cache_dir: PathBuf,
  pub max_concurrent: usize,
  /// Use the Batch API (50% cheaper, async) instead of concurrent
  /// individual requests.
  pub use_batch_api: bool,
}

impl Default for AnalyzeOptions {
  fn default() -> Self {
    Self {
      cache_dir: default_cache_dir(),
      max_concurrent: 5,
      use_batch_api: false,
    }
  }
}

fn default_cache_dir() -> PathBuf {
  directories::BaseDirs::new()
    .map(|d| d.cache_dir().join("spindle"))
    .unwrap_or_else(|| PathBuf::from(".cache/spindle"))
}

/// Bump when the describe prompts change materially — old cached
/// descriptions are too shallow for the new grouping to work with.
const ANALYSIS_CACHE_VERSION: u32 = 2;

fn cache_path(cache_dir: &Path, blake3_hash: &[u8; 32]) -> PathBuf {
  let hex = hex::encode(blake3_hash);
  cache_dir.join(format!("{hex}.v{ANALYSIS_CACHE_VERSION}.json"))
}

pub async fn read_cache(
  cache_dir: &Path,
  blake3_hash: &[u8; 32],
) -> Option<ContentDescription> {
  let path = cache_path(cache_dir, blake3_hash);
  let content = tokio::fs::read_to_string(&path).await.ok()?;
  serde_json::from_str(&content).ok()
}

pub async fn write_cache(
  cache_dir: &Path,
  blake3_hash: &[u8; 32],
  description: &ContentDescription,
) -> Result<()> {
  tokio::fs::create_dir_all(cache_dir)
    .await
    .with_context(|| {
      format!("Failed to create cache dir: {}", cache_dir.display())
    })?;
  let path = cache_path(cache_dir, blake3_hash);
  let json = serde_json::to_string_pretty(description)
    .context("Failed to serialize description")?;
  tokio::fs::write(&path, json).await.with_context(|| {
    format!("Failed to write cache: {}", path.display())
  })?;
  Ok(())
}

/// Bump when the grouping prompt changes materially.
/// v3: nested folder-path labels are offered as an option, not mandated.
const GROUP_CACHE_VERSION: u32 = 3;

/// Cached grouping, addressed by content hash rather than positional index
/// so it can be replayed across runs even if scan order differs.
#[derive(Debug, Serialize, Deserialize)]
struct CachedGroupMember {
  blake3_hex: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  dest_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedGroup {
  label: String,
  rationale: String,
  members: Vec<CachedGroupMember>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedGrouping {
  version: u32,
  groups: Vec<CachedGroup>,
}

/// Stable key for a grouping request: the SET of file content hashes plus
/// the existing folder labels. Order-independent — any add/remove/relabel
/// changes the key and forces a fresh grouping.
pub fn group_cache_key(
  hashes: &[[u8; 32]],
  existing_labels: &[String],
) -> String {
  let mut hexes: Vec<String> =
    hashes.iter().map(hex::encode).collect();
  hexes.sort();
  let mut labels: Vec<&String> = existing_labels.iter().collect();
  labels.sort();

  let mut hasher = blake3::Hasher::new();
  for h in &hexes {
    hasher.update(h.as_bytes());
    hasher.update(b"\n");
  }
  hasher.update(b"--labels--\n");
  for l in labels {
    hasher.update(l.as_bytes());
    hasher.update(b"\n");
  }
  hex::encode(hasher.finalize().as_bytes())
}

fn group_cache_path(cache_dir: &Path, key: &str) -> PathBuf {
  cache_dir.join(format!("groups.{key}.v{GROUP_CACHE_VERSION}.json"))
}

/// Read a cached grouping and remap it onto the current run's indices via
/// `hash_to_index`. Returns `None` on miss, version mismatch, corruption,
/// or if any cached member is absent from the current file set.
pub async fn read_cached_grouping(
  cache_dir: &Path,
  key: &str,
  hash_to_index: &HashMap<String, usize>,
) -> Option<Vec<ProposedGroup>> {
  let path = group_cache_path(cache_dir, key);
  let content = tokio::fs::read_to_string(&path).await.ok()?;
  let cached: CachedGrouping = serde_json::from_str(&content).ok()?;
  if cached.version != GROUP_CACHE_VERSION {
    return None;
  }

  let mut groups = Vec::with_capacity(cached.groups.len());
  for group in cached.groups {
    let mut member_indices = Vec::new();
    let mut member_destinations = Vec::new();
    for member in group.members {
      let index = *hash_to_index.get(&member.blake3_hex)?;
      member_indices.push(index);
      if let Some(dest_name) = member.dest_name {
        member_destinations
          .push(MemberDestination { index, dest_name });
      }
    }
    groups.push(ProposedGroup {
      label: group.label,
      rationale: group.rationale,
      member_indices,
      member_destinations,
    });
  }
  Some(groups)
}

/// Persist a grouping in content-addressed form for future runs.
pub async fn write_cached_grouping(
  cache_dir: &Path,
  key: &str,
  groups: &[ProposedGroup],
  index_to_hash: &HashMap<usize, String>,
) -> Result<()> {
  let cached = CachedGrouping {
    version: GROUP_CACHE_VERSION,
    groups: groups
      .iter()
      .map(|g| {
        let dest_by_index: HashMap<usize, &str> = g
          .member_destinations
          .iter()
          .map(|d| (d.index, d.dest_name.as_str()))
          .collect();
        let members = g
          .member_indices
          .iter()
          .filter_map(|idx| {
            index_to_hash.get(idx).map(|hex| CachedGroupMember {
              blake3_hex: hex.clone(),
              dest_name: dest_by_index
                .get(idx)
                .map(|s| s.to_string()),
            })
          })
          .collect();
        CachedGroup {
          label: g.label.clone(),
          rationale: g.rationale.clone(),
          members,
        }
      })
      .collect(),
  };

  tokio::fs::create_dir_all(cache_dir)
    .await
    .with_context(|| {
      format!("Failed to create cache dir: {}", cache_dir.display())
    })?;
  let path = group_cache_path(cache_dir, key);
  let json = serde_json::to_string_pretty(&cached)
    .context("Failed to serialize grouping")?;
  tokio::fs::write(&path, json).await.with_context(|| {
    format!("Failed to write grouping cache: {}", path.display())
  })?;
  Ok(())
}

pub async fn analyze_file(
  provider: &impl AiProvider,
  file: &FingerprintedFile,
  options: &AnalyzeOptions,
) -> Result<ContentDescription> {
  if let Some(cached) =
    read_cache(&options.cache_dir, &file.blake3_hash).await
  {
    return Ok(cached);
  }

  let filename = file
    .scanned
    .path
    .file_name()
    .unwrap_or_default()
    .to_string_lossy()
    .to_string();

  let description = if file.scanned.file_type.is_video() {
    analyze_video(provider, file, &filename).await?
  } else if file.scanned.file_type.is_image() {
    analyze_image(provider, file, &filename).await?
  } else if matches!(
    file.scanned.file_type,
    crate::model::FileType::Document(_)
  ) {
    analyze_document(provider, file, &filename).await?
  } else {
    describe_by_filename(file, &filename)
  };

  let _ =
    write_cache(&options.cache_dir, &file.blake3_hash, &description)
      .await;

  Ok(description)
}

fn describe_by_filename(
  file: &FingerprintedFile,
  filename: &str,
) -> ContentDescription {
  let category = match file.scanned.file_type {
    crate::model::FileType::Document(_) => "document",
    crate::model::FileType::Audio(_) => "audio",
    crate::model::FileType::Archive(_) => "archive",
    _ => "other",
  };

  let ext = file
    .scanned
    .path
    .extension()
    .and_then(|e| e.to_str())
    .unwrap_or("unknown");

  ContentDescription {
    summary: format!("{category} file: {filename}"),
    tags: vec![category.to_string(), ext.to_string()],
    suggested_category: category.to_string(),
    confidence: 0.5,
  }
}

/// Max bytes of extracted text sent to the API per document
/// (~2k tokens).
const MAX_TEXT_EXCERPT_BYTES: usize = 8 * 1024;

/// Analyze a document by extracting its text content. Falls back to
/// a filename-only description when no text can be extracted
/// (scanned PDFs, binary formats like doc/docx).
async fn analyze_document(
  provider: &impl AiProvider,
  file: &FingerprintedFile,
  filename: &str,
) -> Result<ContentDescription> {
  let excerpt = extract_document_text(file).await;

  let excerpt = match excerpt {
    Some(text) if !text.trim().is_empty() => text,
    _ => return Ok(describe_by_filename(file, filename)),
  };

  let context = DescribeContext {
    filename: filename.to_string(),
    file_type_label: document_type_label(file),
    file_size: file.scanned.size,
    metadata_hint: None,
  };

  provider.describe_text(&excerpt, &context).await
}

fn document_type_label(file: &FingerprintedFile) -> String {
  let ext = file
    .scanned
    .path
    .extension()
    .and_then(|e| e.to_str())
    .unwrap_or("unknown");
  format!("{} document", ext.to_uppercase())
}

/// Extract a text excerpt from a document file. Returns None for
/// formats we can't extract (doc, docx, rtf) or unreadable files.
async fn extract_document_text(
  file: &FingerprintedFile,
) -> Option<String> {
  use crate::model::DocumentFormat as Df;
  use crate::model::FileType;

  let format = match file.scanned.file_type {
    FileType::Document(f) => f,
    _ => return None,
  };

  match format {
    Df::Pdf => extract_pdf_text(&file.scanned.path).await,
    Df::Txt
    | Df::Md
    | Df::Csv
    | Df::Json
    | Df::Xml
    | Df::Html
    | Df::Yaml
    | Df::Toml => read_text_excerpt(&file.scanned.path).await,
    Df::Doc | Df::Docx | Df::Rtf => None,
  }
}

async fn read_text_excerpt(path: &std::path::Path) -> Option<String> {
  use tokio::io::AsyncReadExt;

  let mut f = tokio::fs::File::open(path).await.ok()?;
  let mut buf = vec![0u8; MAX_TEXT_EXCERPT_BYTES];
  let mut filled = 0;
  while filled < buf.len() {
    match f.read(&mut buf[filled..]).await {
      Ok(0) => break,
      Ok(n) => filled += n,
      Err(_) => return None,
    }
  }
  buf.truncate(filled);
  Some(String::from_utf8_lossy(&buf).into_owned())
}

async fn extract_pdf_text(path: &std::path::Path) -> Option<String> {
  let path = path.to_path_buf();
  // pdf-extract is sync and can panic on malformed PDFs — isolate
  // it on a blocking thread and treat panics as "no text".
  let result = tokio::task::spawn_blocking(move || {
    std::panic::catch_unwind(|| pdf_extract::extract_text(&path))
  })
  .await
  .ok()?;

  match result {
    Ok(Ok(text)) => Some(truncate_to_excerpt(text)),
    _ => None,
  }
}

fn truncate_to_excerpt(text: String) -> String {
  if text.len() <= MAX_TEXT_EXCERPT_BYTES {
    return text;
  }
  let mut end = MAX_TEXT_EXCERPT_BYTES;
  while !text.is_char_boundary(end) {
    end -= 1;
  }
  text[..end].to_string()
}

async fn analyze_image(
  provider: &impl AiProvider,
  file: &FingerprintedFile,
  filename: &str,
) -> Result<ContentDescription> {
  let image_data =
    tokio::fs::read(&file.scanned.path).await.with_context(|| {
      format!("Failed to read file: {}", file.scanned.path.display())
    })?;

  let context = DescribeContext {
    filename: filename.to_string(),
    file_type_label: file.scanned.file_type.mime_type().to_string(),
    file_size: file.scanned.size,
    metadata_hint: None,
  };

  provider
    .describe_image(
      &image_data,
      file.scanned.file_type.mime_type(),
      &context,
    )
    .await
}

#[cfg(feature = "video")]
async fn analyze_video(
  provider: &impl AiProvider,
  file: &FingerprintedFile,
  filename: &str,
) -> Result<ContentDescription> {
  use crate::video;

  const MAX_KEYFRAMES: usize = 3;

  let frames =
    video::extract_keyframes(&file.scanned.path, MAX_KEYFRAMES)
      .await
      .with_context(|| {
        format!("Failed to extract keyframes from {filename}")
      })?;

  if frames.is_empty() {
    anyhow::bail!("No keyframes extracted from {filename}");
  }

  let context = DescribeContext {
    filename: filename.to_string(),
    file_type_label: format!(
      "{} (keyframe)",
      file.scanned.file_type.mime_type()
    ),
    file_size: file.scanned.size,
    metadata_hint: Some(format!(
      "Video keyframe at {:.1}s — describe the visual content/theme",
      frames[0].timestamp_secs
    )),
  };

  let first_desc = provider
    .describe_image(&frames[0].png_data, "image/png", &context)
    .await?;

  if frames.len() == 1 {
    return Ok(first_desc);
  }

  let mut all_tags = first_desc.tags.clone();
  let mut summaries = vec![first_desc.summary.clone()];

  for frame in &frames[1..] {
    let ctx = DescribeContext {
      filename: filename.to_string(),
      file_type_label: format!(
        "{} (keyframe)",
        file.scanned.file_type.mime_type()
      ),
      file_size: file.scanned.size,
      metadata_hint: Some(format!(
        "Video keyframe at {:.1}s — describe the visual content/theme",
        frame.timestamp_secs
      )),
    };
    if let Ok(desc) = provider
      .describe_image(&frame.png_data, "image/png", &ctx)
      .await
    {
      summaries.push(desc.summary);
      all_tags.extend(desc.tags);
    }
  }

  all_tags.sort();
  all_tags.dedup();

  Ok(ContentDescription {
    summary: summaries.join(" | "),
    tags: all_tags,
    suggested_category: first_desc.suggested_category,
    confidence: first_desc.confidence,
  })
}

#[cfg(not(feature = "video"))]
async fn analyze_video(
  _provider: &impl AiProvider,
  file: &FingerprintedFile,
  filename: &str,
) -> Result<ContentDescription> {
  tracing::warn!(
    file = %filename,
    "Video analysis requires the 'video' feature flag — skipping {}",
    file.scanned.path.display()
  );
  Ok(ContentDescription {
    summary: format!(
      "Video file: {filename} (enable 'video' feature for content analysis)"
    ),
    tags: vec!["video".to_string(), "unanalyzed".to_string()],
    suggested_category: "other".to_string(),
    confidence: 0.0,
  })
}

pub async fn analyze_batch(
  provider: &impl AiProvider,
  files: &[FingerprintedFile],
  options: &AnalyzeOptions,
) -> Vec<Result<ContentDescription>> {
  if options.use_batch_api {
    return analyze_batch_via_api(provider, files, options).await;
  }

  use futures::stream::{self, StreamExt};

  let semaphore =
    std::sync::Arc::new(Semaphore::new(options.max_concurrent));

  stream::iter(files)
    .map(|file| {
      let sem = semaphore.clone();
      async move {
        let _permit = sem
          .acquire()
          .await
          .map_err(|e| anyhow::anyhow!("Semaphore closed: {}", e))?;
        analyze_file(provider, file, options).await
      }
    })
    .buffer_unordered(options.max_concurrent)
    .collect()
    .await
}

/// What a file contributes to a batch run.
enum BatchSlot {
  /// Already resolved locally (cache hit, fallback description, or
  /// a local error such as an unreadable image).
  Resolved(Result<ContentDescription>),
  /// Needs an API call; index into the submitted request list.
  Submitted(usize),
}

/// Analyze files through the provider's batch interface. Cache hits
/// and filename-only fallbacks resolve locally; everything else is
/// submitted as one batch. Videos go through the regular per-file
/// path since keyframe extraction is multi-request.
async fn analyze_batch_via_api(
  provider: &impl AiProvider,
  files: &[FingerprintedFile],
  options: &AnalyzeOptions,
) -> Vec<Result<ContentDescription>> {
  use crate::ai::{DescribePayload, DescribeRequest};

  let mut slots = Vec::with_capacity(files.len());
  let mut requests = Vec::new();

  for file in files {
    if let Some(cached) =
      read_cache(&options.cache_dir, &file.blake3_hash).await
    {
      slots.push(BatchSlot::Resolved(Ok(cached)));
      continue;
    }

    let filename = file
      .scanned
      .path
      .file_name()
      .unwrap_or_default()
      .to_string_lossy()
      .to_string();

    if file.scanned.file_type.is_video() {
      slots.push(BatchSlot::Resolved(
        analyze_file(provider, file, options).await,
      ));
      continue;
    }

    if file.scanned.file_type.is_image() {
      match tokio::fs::read(&file.scanned.path).await {
        Ok(data) => {
          requests.push(DescribeRequest {
            payload: DescribePayload::Image {
              data,
              mime_type: file
                .scanned
                .file_type
                .mime_type()
                .to_string(),
            },
            context: DescribeContext {
              filename,
              file_type_label: file
                .scanned
                .file_type
                .mime_type()
                .to_string(),
              file_size: file.scanned.size,
              metadata_hint: None,
            },
          });
          slots.push(BatchSlot::Submitted(requests.len() - 1));
        }
        Err(e) => {
          slots.push(BatchSlot::Resolved(Err(anyhow::anyhow!(
            "Failed to read file {}: {e}",
            file.scanned.path.display()
          ))));
        }
      }
      continue;
    }

    if matches!(
      file.scanned.file_type,
      crate::model::FileType::Document(_)
    ) {
      match extract_document_text(file).await {
        Some(text) if !text.trim().is_empty() => {
          requests.push(DescribeRequest {
            payload: DescribePayload::Text { excerpt: text },
            context: DescribeContext {
              filename,
              file_type_label: document_type_label(file),
              file_size: file.scanned.size,
              metadata_hint: None,
            },
          });
          slots.push(BatchSlot::Submitted(requests.len() - 1));
        }
        _ => {
          slots.push(BatchSlot::Resolved(Ok(describe_by_filename(
            file, &filename,
          ))));
        }
      }
      continue;
    }

    slots.push(BatchSlot::Resolved(Ok(describe_by_filename(
      file, &filename,
    ))));
  }

  let mut batch_results = if requests.is_empty() {
    Vec::new()
  } else {
    provider.describe_batch(requests).await
  };

  let mut out = Vec::with_capacity(files.len());
  for (file, slot) in files.iter().zip(slots) {
    let result = match slot {
      BatchSlot::Resolved(r) => r,
      BatchSlot::Submitted(i) => std::mem::replace(
        &mut batch_results[i],
        Err(anyhow::anyhow!("Batch result already taken")),
      ),
    };

    if let Ok(desc) = &result {
      let _ =
        write_cache(&options.cache_dir, &file.blake3_hash, desc)
          .await;
    }
    out.push(result);
  }

  out
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::SystemTime;
  use tempfile::TempDir;

  #[cfg(not(feature = "video"))]
  use crate::model::VideoFormat;
  use crate::model::{FileType, ImageFormat, ScannedFile};

  fn make_test_file(
    dir: &Path,
    name: &str,
    content: &[u8],
  ) -> FingerprintedFile {
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    FingerprintedFile {
      scanned: ScannedFile {
        path,
        scan_root: dir.to_path_buf(),
        size: content.len() as u64,
        modified: SystemTime::now(),
        file_type: FileType::Image(ImageFormat::Jpg),
      },
      blake3_hash: *blake3::hash(content).as_bytes(),
      perceptual_hash: None,
    }
  }

  fn sample_description() -> ContentDescription {
    ContentDescription {
      summary: "A sunset over the ocean".to_string(),
      tags: vec!["sunset".to_string(), "ocean".to_string()],
      suggested_category: "photo".to_string(),
      confidence: 0.92,
    }
  }

  #[test]
  fn cache_path_uses_hex_hash() {
    let hash = [0xABu8; 32];
    let path = cache_path(Path::new("/cache"), &hash);

    assert_eq!(
      path,
      PathBuf::from(format!(
        "/cache/{}.v{}.json",
        hex::encode([0xAB; 32]),
        ANALYSIS_CACHE_VERSION
      ))
    );
  }

  #[tokio::test]
  async fn write_and_read_cache_roundtrips() {
    let dir = TempDir::new().unwrap();
    let hash = [1u8; 32];
    let desc = sample_description();

    write_cache(dir.path(), &hash, &desc).await.unwrap();
    let loaded = read_cache(dir.path(), &hash).await.unwrap();

    assert_eq!(loaded.summary, "A sunset over the ocean");
    assert_eq!(loaded.tags, vec!["sunset", "ocean"]);
    assert_eq!(loaded.confidence, 0.92);
  }

  #[tokio::test]
  async fn read_cache_returns_none_for_missing() {
    let dir = TempDir::new().unwrap();
    let hash = [99u8; 32];

    let result = read_cache(dir.path(), &hash).await;

    assert!(result.is_none());
  }

  #[tokio::test]
  async fn write_cache_creates_directory() {
    let dir = TempDir::new().unwrap();
    let nested = dir.path().join("deep/nested/cache");
    let hash = [2u8; 32];

    write_cache(&nested, &hash, &sample_description())
      .await
      .unwrap();

    assert!(nested.exists());
  }

  #[tokio::test]
  async fn read_cache_returns_none_for_corrupted_json() {
    let dir = TempDir::new().unwrap();
    let hash = [3u8; 32];
    let path = cache_path(dir.path(), &hash);
    std::fs::write(&path, "not json at all").unwrap();

    let result = read_cache(dir.path(), &hash).await;

    assert!(result.is_none());
  }

  #[test]
  fn default_options_has_sane_concurrency() {
    let opts = AnalyzeOptions::default();

    assert_eq!(opts.max_concurrent, 5);
  }

  #[test]
  fn group_cache_key_is_order_independent() {
    let a = [1u8; 32];
    let b = [2u8; 32];

    assert_eq!(
      group_cache_key(&[a, b], &[]),
      group_cache_key(&[b, a], &[])
    );
  }

  #[test]
  fn group_cache_key_changes_with_files_and_labels() {
    let a = [1u8; 32];
    let b = [2u8; 32];

    assert_ne!(
      group_cache_key(&[a], &[]),
      group_cache_key(&[a, b], &[])
    );
    assert_ne!(
      group_cache_key(&[a], &[]),
      group_cache_key(&[a], &["Beach".to_string()])
    );
  }

  #[tokio::test]
  async fn group_cache_roundtrips_and_remaps_by_hash() {
    let dir = TempDir::new().unwrap();
    let h0 = [1u8; 32];
    let h1 = [2u8; 32];
    let hex0 = hex::encode(h0);
    let hex1 = hex::encode(h1);

    let groups = vec![ProposedGroup {
      label: "Beach".to_string(),
      rationale: "sandy".to_string(),
      member_indices: vec![0, 1],
      member_destinations: vec![MemberDestination {
        index: 0,
        dest_name: "a.jpg".to_string(),
      }],
    }];
    let key = group_cache_key(&[h0, h1], &[]);
    let index_to_hash =
      HashMap::from([(0, hex0.clone()), (1, hex1.clone())]);

    write_cached_grouping(dir.path(), &key, &groups, &index_to_hash)
      .await
      .unwrap();

    // Replay with DIFFERENT indices (e.g. a different scan order).
    let hash_to_index =
      HashMap::from([(hex0, 7usize), (hex1, 3usize)]);
    let loaded =
      read_cached_grouping(dir.path(), &key, &hash_to_index)
        .await
        .unwrap();

    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].label, "Beach");
    assert_eq!(loaded[0].member_indices, vec![7, 3]);
    assert_eq!(loaded[0].member_destinations[0].index, 7);
    assert_eq!(loaded[0].member_destinations[0].dest_name, "a.jpg");
  }

  #[tokio::test]
  async fn group_cache_misses_when_member_not_in_current_set() {
    let dir = TempDir::new().unwrap();
    let h0 = [1u8; 32];
    let h1 = [2u8; 32];
    let hex0 = hex::encode(h0);

    let groups = vec![ProposedGroup {
      label: "Beach".to_string(),
      rationale: "sandy".to_string(),
      member_indices: vec![0, 1],
      member_destinations: vec![],
    }];
    let key = group_cache_key(&[h0, h1], &[]);
    let index_to_hash =
      HashMap::from([(0, hex0.clone()), (1, hex::encode(h1))]);
    write_cached_grouping(dir.path(), &key, &groups, &index_to_hash)
      .await
      .unwrap();

    // Current set is missing h1 — the cached grouping can't be remapped.
    let hash_to_index = HashMap::from([(hex0, 0usize)]);
    let loaded =
      read_cached_grouping(dir.path(), &key, &hash_to_index).await;

    assert!(loaded.is_none());
  }

  #[tokio::test]
  async fn analyze_file_returns_cached_result() {
    let dir = TempDir::new().unwrap();
    let file_dir = TempDir::new().unwrap();
    let file =
      make_test_file(file_dir.path(), "test.jpg", b"image bytes");
    let desc = sample_description();

    write_cache(dir.path(), &file.blake3_hash, &desc)
      .await
      .unwrap();

    let opts = AnalyzeOptions {
      cache_dir: dir.path().to_path_buf(),
      max_concurrent: 1,
      use_batch_api: false,
    };

    struct PanicProvider;
    impl AiProvider for PanicProvider {
      async fn describe_image(
        &self,
        _: &[u8],
        _: &str,
        _: &DescribeContext,
      ) -> Result<ContentDescription> {
        panic!("Should not be called when cache exists");
      }
      async fn propose_groups(
        &self,
        _: &[crate::model::FileSummary],
      ) -> Result<Vec<crate::model::ProposedGroup>> {
        panic!("unused");
      }
    }

    let result =
      analyze_file(&PanicProvider, &file, &opts).await.unwrap();

    assert_eq!(result.summary, "A sunset over the ocean");
  }

  #[tokio::test]
  async fn analyze_file_calls_provider_on_cache_miss() {
    let cache_dir = TempDir::new().unwrap();
    let file_dir = TempDir::new().unwrap();
    let file =
      make_test_file(file_dir.path(), "photo.jpg", b"raw image");

    let opts = AnalyzeOptions {
      cache_dir: cache_dir.path().to_path_buf(),
      max_concurrent: 1,
      use_batch_api: false,
    };

    struct FakeProvider;
    impl AiProvider for FakeProvider {
      async fn describe_image(
        &self,
        _: &[u8],
        _: &str,
        _: &DescribeContext,
      ) -> Result<ContentDescription> {
        Ok(ContentDescription {
          summary: "From provider".to_string(),
          tags: vec![],
          suggested_category: "photo".to_string(),
          confidence: 0.8,
        })
      }
      async fn propose_groups(
        &self,
        _: &[crate::model::FileSummary],
      ) -> Result<Vec<crate::model::ProposedGroup>> {
        Ok(vec![])
      }
    }

    let result =
      analyze_file(&FakeProvider, &file, &opts).await.unwrap();

    assert_eq!(result.summary, "From provider");
  }

  #[tokio::test]
  async fn analyze_file_writes_to_cache_after_success() {
    let cache_dir = TempDir::new().unwrap();
    let file_dir = TempDir::new().unwrap();
    let file =
      make_test_file(file_dir.path(), "new.jpg", b"new image");

    let opts = AnalyzeOptions {
      cache_dir: cache_dir.path().to_path_buf(),
      max_concurrent: 1,
      use_batch_api: false,
    };

    struct FakeProvider;
    impl AiProvider for FakeProvider {
      async fn describe_image(
        &self,
        _: &[u8],
        _: &str,
        _: &DescribeContext,
      ) -> Result<ContentDescription> {
        Ok(sample_description())
      }
      async fn propose_groups(
        &self,
        _: &[crate::model::FileSummary],
      ) -> Result<Vec<crate::model::ProposedGroup>> {
        Ok(vec![])
      }
    }

    analyze_file(&FakeProvider, &file, &opts).await.unwrap();

    let cached =
      read_cache(cache_dir.path(), &file.blake3_hash).await;
    assert!(cached.is_some());
    assert_eq!(cached.unwrap().summary, "A sunset over the ocean");
  }

  #[cfg(not(feature = "video"))]
  fn make_video_file(
    dir: &Path,
    name: &str,
    content: &[u8],
  ) -> FingerprintedFile {
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    FingerprintedFile {
      scanned: ScannedFile {
        path,
        scan_root: dir.to_path_buf(),
        size: content.len() as u64,
        modified: SystemTime::now(),
        file_type: FileType::Video(VideoFormat::Mp4),
      },
      blake3_hash: *blake3::hash(content).as_bytes(),
      perceptual_hash: None,
    }
  }

  #[cfg(not(feature = "video"))]
  #[tokio::test]
  async fn analyze_video_without_feature_returns_placeholder() {
    let cache_dir = TempDir::new().unwrap();
    let file_dir = TempDir::new().unwrap();
    let file =
      make_video_file(file_dir.path(), "clip.mp4", b"fake video");

    let opts = AnalyzeOptions {
      cache_dir: cache_dir.path().to_path_buf(),
      max_concurrent: 1,
      use_batch_api: false,
    };

    struct UnusedProvider;
    impl AiProvider for UnusedProvider {
      async fn describe_image(
        &self,
        _: &[u8],
        _: &str,
        _: &DescribeContext,
      ) -> Result<ContentDescription> {
        panic!("Should not call AI when video feature is disabled");
      }
      async fn propose_groups(
        &self,
        _: &[crate::model::FileSummary],
      ) -> Result<Vec<crate::model::ProposedGroup>> {
        panic!("unused");
      }
    }

    let result =
      analyze_file(&UnusedProvider, &file, &opts).await.unwrap();

    assert!(result.summary.contains("clip.mp4"));
    assert!(result.summary.contains("video"));
    assert_eq!(result.confidence, 0.0);
    assert!(result.tags.contains(&"unanalyzed".to_string()));
  }

  #[cfg(not(feature = "video"))]
  #[tokio::test]
  async fn analyze_video_file_caches_result() {
    let cache_dir = TempDir::new().unwrap();
    let file_dir = TempDir::new().unwrap();
    let file =
      make_video_file(file_dir.path(), "movie.mp4", b"video bytes");

    let opts = AnalyzeOptions {
      cache_dir: cache_dir.path().to_path_buf(),
      max_concurrent: 1,
      use_batch_api: false,
    };

    struct StubProvider;
    impl AiProvider for StubProvider {
      async fn describe_image(
        &self,
        _: &[u8],
        _: &str,
        _: &DescribeContext,
      ) -> Result<ContentDescription> {
        Ok(ContentDescription {
          summary: "stub".to_string(),
          tags: vec![],
          suggested_category: "other".to_string(),
          confidence: 0.5,
        })
      }
      async fn propose_groups(
        &self,
        _: &[crate::model::FileSummary],
      ) -> Result<Vec<crate::model::ProposedGroup>> {
        Ok(vec![])
      }
    }

    analyze_file(&StubProvider, &file, &opts).await.unwrap();

    let cached =
      read_cache(cache_dir.path(), &file.blake3_hash).await;
    assert!(cached.is_some());
  }

  fn make_document_file(
    dir: &Path,
    name: &str,
    content: &[u8],
    format: crate::model::DocumentFormat,
  ) -> FingerprintedFile {
    let mut file = make_test_file(dir, name, content);
    file.scanned.file_type = FileType::Document(format);
    file
  }

  /// Provider that records the excerpt passed to describe_text.
  struct TextCapturingProvider {
    captured: std::sync::Mutex<Option<String>>,
  }

  impl AiProvider for TextCapturingProvider {
    async fn describe_image(
      &self,
      _: &[u8],
      _: &str,
      _: &DescribeContext,
    ) -> Result<ContentDescription> {
      panic!("describe_image should not be called for documents");
    }

    async fn describe_text(
      &self,
      excerpt: &str,
      _: &DescribeContext,
    ) -> Result<ContentDescription> {
      *self.captured.lock().unwrap() = Some(excerpt.to_string());
      Ok(ContentDescription {
        summary: "Lease agreement for 123 Main St".to_string(),
        tags: vec!["lease".to_string(), "legal".to_string()],
        suggested_category: "legal".to_string(),
        confidence: 0.9,
      })
    }

    async fn propose_groups(
      &self,
      _: &[crate::model::FileSummary],
    ) -> Result<Vec<crate::model::ProposedGroup>> {
      panic!("unused");
    }
  }

  #[tokio::test]
  async fn analyze_file_sends_text_content_for_documents() {
    let cache_dir = TempDir::new().unwrap();
    let file_dir = TempDir::new().unwrap();
    let file = make_document_file(
      file_dir.path(),
      "lease.txt",
      b"RESIDENTIAL LEASE AGREEMENT between Alice and Bob",
      crate::model::DocumentFormat::Txt,
    );

    let opts = AnalyzeOptions {
      cache_dir: cache_dir.path().to_path_buf(),
      max_concurrent: 1,
      use_batch_api: false,
    };

    let provider = TextCapturingProvider {
      captured: std::sync::Mutex::new(None),
    };

    let result = analyze_file(&provider, &file, &opts).await.unwrap();

    let captured = provider.captured.lock().unwrap();
    assert!(captured
      .as_deref()
      .unwrap()
      .contains("RESIDENTIAL LEASE AGREEMENT"));
    assert_eq!(result.suggested_category, "legal");
  }

  #[tokio::test]
  async fn analyze_file_falls_back_to_filename_for_docx() {
    let cache_dir = TempDir::new().unwrap();
    let file_dir = TempDir::new().unwrap();
    let file = make_document_file(
      file_dir.path(),
      "report.docx",
      b"PK\x03\x04 binary docx bytes",
      crate::model::DocumentFormat::Docx,
    );

    let opts = AnalyzeOptions {
      cache_dir: cache_dir.path().to_path_buf(),
      max_concurrent: 1,
      use_batch_api: false,
    };

    let provider = TextCapturingProvider {
      captured: std::sync::Mutex::new(None),
    };

    let result = analyze_file(&provider, &file, &opts).await.unwrap();

    assert!(provider.captured.lock().unwrap().is_none());
    assert_eq!(result.suggested_category, "document");
    assert!(result.summary.contains("report.docx"));
  }

  #[tokio::test]
  async fn analyze_file_falls_back_when_document_is_empty() {
    let cache_dir = TempDir::new().unwrap();
    let file_dir = TempDir::new().unwrap();
    let file = make_document_file(
      file_dir.path(),
      "empty.txt",
      b"   \n\t ",
      crate::model::DocumentFormat::Txt,
    );

    let opts = AnalyzeOptions {
      cache_dir: cache_dir.path().to_path_buf(),
      max_concurrent: 1,
      use_batch_api: false,
    };

    let provider = TextCapturingProvider {
      captured: std::sync::Mutex::new(None),
    };

    let result = analyze_file(&provider, &file, &opts).await.unwrap();

    assert!(provider.captured.lock().unwrap().is_none());
    assert_eq!(result.suggested_category, "document");
  }

  #[tokio::test]
  async fn read_text_excerpt_caps_at_limit() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("big.txt");
    std::fs::write(&path, "x".repeat(MAX_TEXT_EXCERPT_BYTES * 3))
      .unwrap();

    let excerpt = read_text_excerpt(&path).await.unwrap();

    assert_eq!(excerpt.len(), MAX_TEXT_EXCERPT_BYTES);
  }

  #[test]
  fn truncate_to_excerpt_respects_char_boundaries() {
    let mut text = "a".repeat(MAX_TEXT_EXCERPT_BYTES - 1);
    text.push('é');
    text.push_str("trailing");

    let truncated = truncate_to_excerpt(text);

    assert!(truncated.len() <= MAX_TEXT_EXCERPT_BYTES);
    assert!(truncated.is_char_boundary(truncated.len()));
  }

  /// Provider that only answers through describe_batch and records
  /// how many requests it received.
  struct BatchOnlyProvider {
    batch_sizes: std::sync::Mutex<Vec<usize>>,
  }

  impl BatchOnlyProvider {
    fn new() -> Self {
      Self {
        batch_sizes: std::sync::Mutex::new(Vec::new()),
      }
    }
  }

  impl AiProvider for BatchOnlyProvider {
    async fn describe_image(
      &self,
      _: &[u8],
      _: &str,
      _: &DescribeContext,
    ) -> Result<ContentDescription> {
      panic!("individual describe_image used in batch mode");
    }

    async fn describe_text(
      &self,
      _: &str,
      _: &DescribeContext,
    ) -> Result<ContentDescription> {
      panic!("individual describe_text used in batch mode");
    }

    async fn describe_batch(
      &self,
      requests: Vec<crate::ai::DescribeRequest>,
    ) -> Vec<Result<ContentDescription>> {
      self.batch_sizes.lock().unwrap().push(requests.len());
      requests
        .iter()
        .map(|r| {
          Ok(ContentDescription {
            summary: format!("batched: {}", r.context.filename),
            tags: vec!["batch".to_string()],
            suggested_category: "other".to_string(),
            confidence: 0.9,
          })
        })
        .collect()
    }

    async fn propose_groups(
      &self,
      _: &[crate::model::FileSummary],
    ) -> Result<Vec<crate::model::ProposedGroup>> {
      panic!("unused");
    }
  }

  #[tokio::test]
  async fn analyze_batch_uses_batch_api_when_enabled() {
    let cache_dir = TempDir::new().unwrap();
    let file_dir = TempDir::new().unwrap();
    let image =
      make_test_file(file_dir.path(), "photo.jpg", b"jpeg bytes");
    let doc = make_document_file(
      file_dir.path(),
      "lease.txt",
      b"LEASE AGREEMENT terms",
      crate::model::DocumentFormat::Txt,
    );

    let opts = AnalyzeOptions {
      cache_dir: cache_dir.path().to_path_buf(),
      max_concurrent: 1,
      use_batch_api: true,
    };

    let provider = BatchOnlyProvider::new();
    let results =
      analyze_batch(&provider, &[image.clone(), doc], &opts).await;

    assert_eq!(results.len(), 2);
    assert_eq!(
      results[0].as_ref().unwrap().summary,
      "batched: photo.jpg"
    );
    assert_eq!(
      results[1].as_ref().unwrap().summary,
      "batched: lease.txt"
    );
    // One batch containing both requests
    assert_eq!(*provider.batch_sizes.lock().unwrap(), vec![2]);

    // Results were cached for the next run
    assert!(read_cache(cache_dir.path(), &image.blake3_hash)
      .await
      .is_some());
  }

  #[tokio::test]
  async fn analyze_batch_api_skips_cached_files() {
    let cache_dir = TempDir::new().unwrap();
    let file_dir = TempDir::new().unwrap();
    let cached_file =
      make_test_file(file_dir.path(), "seen.jpg", b"old bytes");
    let new_file =
      make_test_file(file_dir.path(), "new.jpg", b"new bytes");

    write_cache(
      cache_dir.path(),
      &cached_file.blake3_hash,
      &sample_description(),
    )
    .await
    .unwrap();

    let opts = AnalyzeOptions {
      cache_dir: cache_dir.path().to_path_buf(),
      max_concurrent: 1,
      use_batch_api: true,
    };

    let provider = BatchOnlyProvider::new();
    let results =
      analyze_batch(&provider, &[cached_file, new_file], &opts).await;

    assert_eq!(
      results[0].as_ref().unwrap().summary,
      "A sunset over the ocean"
    );
    assert_eq!(
      results[1].as_ref().unwrap().summary,
      "batched: new.jpg"
    );
    // Only the uncached file was submitted
    assert_eq!(*provider.batch_sizes.lock().unwrap(), vec![1]);
  }

  #[tokio::test]
  async fn analyze_batch_api_resolves_unbatchable_files_locally() {
    let cache_dir = TempDir::new().unwrap();
    let file_dir = TempDir::new().unwrap();
    let docx = make_document_file(
      file_dir.path(),
      "report.docx",
      b"PK\x03\x04 binary",
      crate::model::DocumentFormat::Docx,
    );

    let opts = AnalyzeOptions {
      cache_dir: cache_dir.path().to_path_buf(),
      max_concurrent: 1,
      use_batch_api: true,
    };

    let provider = BatchOnlyProvider::new();
    let results = analyze_batch(&provider, &[docx], &opts).await;

    assert_eq!(
      results[0].as_ref().unwrap().suggested_category,
      "document"
    );
    // Nothing was submitted to the API
    assert!(provider.batch_sizes.lock().unwrap().is_empty());
  }
}

pub mod image_prep;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::ai::{AiProvider, DescribeContext};
use crate::model::{
  ContentDescription, DescriptionSource, FingerprintedFile,
  MemberDestination, ProposedGroup,
};

pub struct AnalyzeOptions {
  pub cache_dir: PathBuf,
  pub max_concurrent: usize,
  /// Use the Batch API (50% cheaper, async) instead of concurrent
  /// individual requests.
  pub use_batch_api: bool,
  pub introspect_archives: bool,
  pub max_archive_files: usize,
  pub max_archive_file_size_mb: u64,
  /// Extract video keyframes with ffmpeg. Callers set this from
  /// `video::ffmpeg_available()`; tests turn it off.
  pub use_ffmpeg: bool,
}

impl Default for AnalyzeOptions {
  fn default() -> Self {
    Self {
      cache_dir: default_cache_dir(),
      max_concurrent: 5,
      use_batch_api: false,
      introspect_archives: true,
      max_archive_files: 20,
      max_archive_file_size_mb: 50,
      use_ffmpeg: false,
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
const ANALYSIS_CACHE_VERSION: u32 = 3;

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
  let cached: ContentDescription =
    serde_json::from_str(&content).ok()?;
  // A stale placeholder is worth retrying, not reusing.
  (cached.source != DescriptionSource::Unanalyzed).then_some(cached)
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
const GROUP_CACHE_VERSION: u32 = 4;

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
/// `hash_to_indices`. Byte-identical files share a hash, so each hash
/// maps to every index carrying it and each cached member consumes one.
/// Returns `None` on miss, version mismatch, corruption, or if any
/// cached member has no unconsumed index in the current file set.
pub async fn read_cached_grouping(
  cache_dir: &Path,
  key: &str,
  hash_to_indices: &HashMap<String, Vec<usize>>,
) -> Option<Vec<ProposedGroup>> {
  let path = group_cache_path(cache_dir, key);
  let content = tokio::fs::read_to_string(&path).await.ok()?;
  let cached: CachedGrouping = serde_json::from_str(&content).ok()?;
  if cached.version != GROUP_CACHE_VERSION {
    return None;
  }

  let mut remaining: HashMap<
    &str,
    std::collections::VecDeque<usize>,
  > = hash_to_indices
    .iter()
    .map(|(hex, idxs)| (hex.as_str(), idxs.iter().copied().collect()))
    .collect();

  let mut groups = Vec::with_capacity(cached.groups.len());
  for group in cached.groups {
    let mut member_indices = Vec::new();
    let mut member_destinations = Vec::new();
    for member in group.members {
      let index =
        remaining.get_mut(member.blake3_hex.as_str())?.pop_front()?;
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
      member_notes: vec![],
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
    analyze_video(provider, file, &filename, options).await?
  } else if file.scanned.file_type.is_image() {
    analyze_image(provider, file, &filename).await?
  } else if matches!(
    file.scanned.file_type,
    crate::model::FileType::Document(_)
  ) {
    analyze_document(provider, file, &filename).await?
  } else if matches!(
    file.scanned.file_type,
    crate::model::FileType::Archive(_)
  ) && options.introspect_archives
  {
    analyze_archive(provider, file, &filename, options).await?
  } else {
    describe_by_filename(file, &filename)
  };

  // Placeholders for content we could not analyze are never cached:
  // installing ffmpeg (or fixing the file) must take effect next run.
  if description.source != DescriptionSource::Unanalyzed {
    let _ = write_cache(
      &options.cache_dir,
      &file.blake3_hash,
      &description,
    )
    .await;
  }

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
    source: DescriptionSource::Filename,
  }
}

const BYTES_PER_MB: u64 = 1_000_000;

/// Analyze an archive by extracting its contents to a temp dir,
/// running each inner file through the normal analysis pipeline, and
/// synthesizing a single description.
async fn analyze_archive(
  provider: &impl AiProvider,
  file: &FingerprintedFile,
  filename: &str,
  options: &AnalyzeOptions,
) -> Result<ContentDescription> {
  let inner_descriptions =
    extract_and_analyze_archive(provider, file, options).await;

  if inner_descriptions.is_empty() {
    return Ok(describe_by_filename(file, filename));
  }

  let mut all_tags = Vec::new();
  let mut summaries = Vec::new();
  let mut categories: HashMap<String, usize> = HashMap::new();

  for (inner_name, desc) in &inner_descriptions {
    summaries.push(format!("{inner_name}: {}", desc.summary));
    all_tags.extend(desc.tags.clone());
    *categories
      .entry(desc.suggested_category.clone())
      .or_default() += 1;
  }

  all_tags.sort();
  all_tags.dedup();

  let top_category = categories
    .into_iter()
    .max_by_key(|(_, count)| *count)
    .map(|(cat, _)| cat)
    .unwrap_or_else(|| "other".to_string());

  let items_summary = if summaries.len() <= 5 {
    summaries.join("; ")
  } else {
    let first_five = summaries[..5].join("; ");
    format!("{}; ... and {} more", first_five, summaries.len() - 5)
  };

  Ok(ContentDescription {
    summary: format!(
      "Archive ({} files): {}",
      inner_descriptions.len(),
      items_summary,
    ),
    tags: all_tags,
    suggested_category: top_category,
    confidence: 0.8,
    source: DescriptionSource::Ai,
  })
}

async fn extract_and_analyze_archive(
  provider: &impl AiProvider,
  file: &FingerprintedFile,
  options: &AnalyzeOptions,
) -> Vec<(String, ContentDescription)> {
  use crate::model::ArchiveFormat;

  let format = match file.scanned.file_type {
    crate::model::FileType::Archive(f) => f,
    _ => return Vec::new(),
  };

  let tmp = match tempfile::TempDir::new() {
    Ok(t) => t,
    Err(e) => {
      tracing::warn!(error = %e, "Failed to create temp dir for archive introspection");
      return Vec::new();
    }
  };

  let extracted = match format {
    ArchiveFormat::Zip => {
      extract_zip(&file.scanned.path, tmp.path(), options)
    }
    _ => {
      tracing::debug!(
        format = ?format,
        "Archive format not yet supported for introspection"
      );
      return Vec::new();
    }
  };

  let extracted = match extracted {
    Ok(files) => files,
    Err(e) => {
      tracing::warn!(
        path = %file.scanned.path.display(),
        error = %e,
        "Failed to extract archive"
      );
      return Vec::new();
    }
  };

  let inner_opts = AnalyzeOptions {
    cache_dir: options.cache_dir.clone(),
    max_concurrent: options.max_concurrent,
    use_batch_api: false,
    introspect_archives: false,
    max_archive_files: 0,
    max_archive_file_size_mb: 0,
    use_ffmpeg: options.use_ffmpeg,
  };

  let mut results = Vec::new();
  for inner_file in &extracted {
    let inner_name = inner_file
      .scanned
      .path
      .file_name()
      .unwrap_or_default()
      .to_string_lossy()
      .to_string();
    match Box::pin(analyze_file(provider, inner_file, &inner_opts))
      .await
    {
      Ok(desc) => results.push((inner_name, desc)),
      Err(e) => {
        tracing::debug!(
          file = %inner_name,
          error = %e,
          "Failed to analyze inner archive file"
        );
      }
    }
  }
  results
}

fn extract_zip(
  archive_path: &std::path::Path,
  dest: &std::path::Path,
  options: &AnalyzeOptions,
) -> Result<Vec<FingerprintedFile>> {
  use crate::scanner::scan_directory;

  let file = std::fs::File::open(archive_path)
    .with_context(|| format!("Opening {}", archive_path.display()))?;
  let mut archive =
    zip::ZipArchive::new(file).with_context(|| {
      format!("Reading ZIP {}", archive_path.display())
    })?;

  let max_size = options.max_archive_file_size_mb * BYTES_PER_MB;
  let mut extracted_count = 0;

  for i in 0..archive.len() {
    if extracted_count >= options.max_archive_files {
      break;
    }

    let mut entry = match archive.by_index(i) {
      Ok(e) => e,
      Err(_) => continue,
    };

    if entry.is_dir() {
      continue;
    }

    if entry.size() > max_size {
      continue;
    }

    let entry_name = match entry.enclosed_name() {
      Some(name) => name.to_path_buf(),
      None => continue,
    };

    if entry_name
      .components()
      .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
    {
      continue;
    }

    let inner_ext = entry_name
      .extension()
      .and_then(|e| e.to_str())
      .unwrap_or("");
    let inner_type =
      crate::model::FileType::from_extension(inner_ext);
    if matches!(inner_type, crate::model::FileType::Archive(_))
      || matches!(inner_type, crate::model::FileType::Other)
    {
      continue;
    }

    // Preserve the entry's relative path — flattening by filename
    // makes same-named files in different folders clobber each other.
    let dest_path = dest.join(&entry_name);
    if let Some(parent) = dest_path.parent() {
      if std::fs::create_dir_all(parent).is_err() {
        continue;
      }
    }

    let mut out = match std::fs::File::create(&dest_path) {
      Ok(f) => f,
      Err(_) => continue,
    };
    if std::io::copy(&mut entry, &mut out).is_err() {
      continue;
    }

    extracted_count += 1;
  }

  let scanned = scan_directory(dest).unwrap_or_default();
  Ok(
    crate::fingerprint::fingerprint_files(scanned)
      .unwrap_or_default(),
  )
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
    metadata_hint: mtime_hint(file),
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
    Df::Docx => extract_docx_text(&file.scanned.path).await,
    Df::Rtf => extract_rtf_text(&file.scanned.path).await,
    Df::Doc => None,
  }
}

/// docx is a zip: the body text lives in `word/document.xml`.
async fn extract_docx_text(path: &std::path::Path) -> Option<String> {
  let path = path.to_path_buf();
  tokio::task::spawn_blocking(move || {
    let file = std::fs::File::open(&path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let entry = archive.by_name("word/document.xml").ok()?;
    let mut xml = String::new();
    use std::io::Read;
    entry
      .take(4 * MAX_TEXT_EXCERPT_BYTES as u64)
      .read_to_string(&mut xml)
      .ok()?;
    let text = strip_docx_xml(&xml);
    (!text.trim().is_empty()).then(|| truncate_to_excerpt(text))
  })
  .await
  .ok()?
}

/// Keep character data, turn paragraph ends into newlines, decode the
/// few entities Word actually emits.
fn strip_docx_xml(xml: &str) -> String {
  let mut out = String::with_capacity(xml.len() / 4);
  let mut in_tag = false;
  let mut tag = String::new();
  for c in xml.chars() {
    match c {
      '<' => {
        in_tag = true;
        tag.clear();
      }
      '>' => {
        in_tag = false;
        if tag == "/w:p" && !out.ends_with('\n') {
          out.push('\n');
        }
      }
      _ if in_tag => tag.push(c),
      _ => out.push(c),
    }
  }
  out
    .replace("&amp;", "&")
    .replace("&lt;", "<")
    .replace("&gt;", ">")
    .replace("&quot;", "\"")
    .replace("&apos;", "'")
}

/// RTF: drop control words and groups, keep plain text.
async fn extract_rtf_text(path: &std::path::Path) -> Option<String> {
  let raw = read_text_excerpt(path).await?;
  let text = strip_rtf(&raw);
  (!text.trim().is_empty()).then_some(text)
}

fn strip_rtf(rtf: &str) -> String {
  let mut out = String::with_capacity(rtf.len() / 2);
  let mut chars = rtf.chars().peekable();
  while let Some(c) = chars.next() {
    match c {
      '{' | '}' => {}
      '\\' => {
        match chars.peek() {
          // escaped literals
          Some('\\') | Some('{') | Some('}') => {
            out.push(chars.next().unwrap());
          }
          // \'hh hex escape — skip the two hex digits
          Some('\'') => {
            chars.next();
            chars.next();
            chars.next();
          }
          _ => {
            // control word: letters then optional numeric parameter
            let mut word = String::new();
            while let Some(&next) = chars.peek() {
              if next.is_ascii_alphabetic() {
                word.push(chars.next().unwrap());
              } else {
                break;
              }
            }
            while chars
              .peek()
              .is_some_and(|n| n.is_ascii_digit() || *n == '-')
            {
              chars.next();
            }
            // the delimiting space belongs to the control word
            if chars.peek() == Some(&' ') {
              chars.next();
            }
            if word == "par" || word == "line" {
              out.push('\n');
            }
          }
        }
      }
      '\r' | '\n' => {}
      _ => out.push(c),
    }
  }
  out
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

/// EXIF hint for an image: capture date, camera, GPS presence — the
/// strongest grouping signals a photo carries.
pub(crate) fn exif_hint(path: &std::path::Path) -> Option<String> {
  let file = std::fs::File::open(path).ok()?;
  let mut reader = std::io::BufReader::new(file);
  let exif =
    exif::Reader::new().read_from_container(&mut reader).ok()?;

  let mut parts = Vec::new();
  if let Some(field) =
    exif.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)
  {
    parts.push(format!("Taken {}", field.display_value()));
  }
  if let Some(field) =
    exif.get_field(exif::Tag::Model, exif::In::PRIMARY)
  {
    let model = field.display_value().to_string();
    parts.push(format!("Camera {}", model.trim_matches('"')));
  }
  if exif
    .get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY)
    .is_some()
  {
    parts.push("GPS-tagged".to_string());
  }
  (!parts.is_empty()).then(|| parts.join(", "))
}

/// Filesystem modification date — a weak but universal signal, used
/// when a file has no richer metadata.
fn mtime_hint(file: &FingerprintedFile) -> Option<String> {
  let modified =
    chrono::DateTime::<chrono::Utc>::from(file.scanned.modified);
  Some(format!("Modified {}", modified.format("%Y-%m-%d")))
}

pub(crate) fn image_metadata_hint(
  file: &FingerprintedFile,
) -> Option<String> {
  exif_hint(&file.scanned.path).or_else(|| mtime_hint(file))
}

async fn analyze_image(
  provider: &impl AiProvider,
  file: &FingerprintedFile,
  filename: &str,
) -> Result<ContentDescription> {
  let prepared = prepare_image(file).await?;

  let context = DescribeContext {
    filename: filename.to_string(),
    file_type_label: file.scanned.file_type.mime_type().to_string(),
    file_size: file.scanned.size,
    metadata_hint: image_metadata_hint(file),
  };

  provider
    .describe_image(&prepared.data, prepared.mime_type, &context)
    .await
}

/// Decode/downscale/transcode on a blocking thread so a big HEIC
/// doesn't stall the async runtime.
async fn prepare_image(
  file: &FingerprintedFile,
) -> Result<image_prep::PreparedImage> {
  let path = file.scanned.path.clone();
  let mime = file.scanned.file_type.mime_type();
  tokio::task::spawn_blocking(move || {
    image_prep::prepare_for_upload(&path, mime)
  })
  .await
  .context("image preparation task failed")?
}

/// Describe a video from up to three evenly spaced keyframes. Without
/// ffmpeg (or when extraction fails) the file is marked unanalyzed
/// rather than failed, so it stays in the plan with a clear note.
async fn analyze_video(
  provider: &impl AiProvider,
  file: &FingerprintedFile,
  filename: &str,
  options: &AnalyzeOptions,
) -> Result<ContentDescription> {
  const MAX_KEYFRAMES: usize = 3;

  if !options.use_ffmpeg {
    return Ok(unanalyzed_video(filename, "ffmpeg not found"));
  }

  let frames = match crate::video::extract_keyframes(
    &file.scanned.path,
    MAX_KEYFRAMES,
  )
  .await
  {
    Ok(frames) if !frames.is_empty() => frames,
    Ok(_) => {
      return Ok(unanalyzed_video(filename, "no keyframes extracted"))
    }
    Err(e) => {
      tracing::warn!(
        file = %filename,
        error = %e,
        "Keyframe extraction failed"
      );
      return Ok(unanalyzed_video(
        filename,
        "keyframe extraction failed",
      ));
    }
  };

  let frame_context = |ts: f64| DescribeContext {
    filename: filename.to_string(),
    file_type_label: format!(
      "{} (keyframe)",
      file.scanned.file_type.mime_type()
    ),
    file_size: file.scanned.size,
    metadata_hint: Some(format!(
      "Video keyframe at {ts:.1}s — describe the visual content/theme"
    )),
  };

  let first_desc = provider
    .describe_image(
      &frames[0].png_data,
      "image/png",
      &frame_context(frames[0].timestamp_secs),
    )
    .await?;

  if frames.len() == 1 {
    return Ok(first_desc);
  }

  let mut all_tags = first_desc.tags.clone();
  let mut summaries = vec![first_desc.summary.clone()];

  for frame in &frames[1..] {
    if let Ok(desc) = provider
      .describe_image(
        &frame.png_data,
        "image/png",
        &frame_context(frame.timestamp_secs),
      )
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
    source: DescriptionSource::Ai,
  })
}

fn unanalyzed_video(
  filename: &str,
  reason: &str,
) -> ContentDescription {
  ContentDescription {
    summary: format!(
      "Video file: {filename} ({reason} — content not analyzed)"
    ),
    tags: vec!["video".to_string(), "unanalyzed".to_string()],
    suggested_category: "other".to_string(),
    confidence: 0.0,
    source: DescriptionSource::Unanalyzed,
  }
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
    .buffered(options.max_concurrent)
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
      match prepare_image(file).await {
        Ok(prepared) => {
          requests.push(DescribeRequest {
            payload: DescribePayload::Image {
              data: prepared.data,
              mime_type: prepared.mime_type.to_string(),
            },
            context: DescribeContext {
              filename,
              file_type_label: file
                .scanned
                .file_type
                .mime_type()
                .to_string(),
              file_size: file.scanned.size,
              metadata_hint: image_metadata_hint(file),
            },
          });
          slots.push(BatchSlot::Submitted(requests.len() - 1));
        }
        Err(e) => {
          slots.push(BatchSlot::Resolved(Err(e)));
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
              metadata_hint: mtime_hint(file),
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

  use crate::model::VideoFormat;
  use crate::model::{FileType, ImageFormat, ScannedFile};

  /// Image fixture: `content` only seeds the hash and pixel colour; the
  /// file on disk is a real 1x1 PNG so upload preparation can decode it.
  fn make_test_file(
    dir: &Path,
    name: &str,
    content: &[u8],
  ) -> FingerprintedFile {
    let path = dir.join(name);
    let seed = blake3::hash(content);
    let b = seed.as_bytes();
    std::fs::write(
      &path,
      create_test_png(1, 1, &[b[0], b[1], b[2], 255]),
    )
    .unwrap();
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
      source: DescriptionSource::Ai,
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
      member_notes: vec![],
    }];
    let key = group_cache_key(&[h0, h1], &[]);
    let index_to_hash =
      HashMap::from([(0, hex0.clone()), (1, hex1.clone())]);

    write_cached_grouping(dir.path(), &key, &groups, &index_to_hash)
      .await
      .unwrap();

    // Replay with DIFFERENT indices (e.g. a different scan order).
    let hash_to_indices =
      HashMap::from([(hex0, vec![7usize]), (hex1, vec![3usize])]);
    let loaded =
      read_cached_grouping(dir.path(), &key, &hash_to_indices)
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
      member_notes: vec![],
    }];
    let key = group_cache_key(&[h0, h1], &[]);
    let index_to_hash =
      HashMap::from([(0, hex0.clone()), (1, hex::encode(h1))]);
    write_cached_grouping(dir.path(), &key, &groups, &index_to_hash)
      .await
      .unwrap();

    // Current set is missing h1 — the cached grouping can't be remapped.
    let hash_to_indices = HashMap::from([(hex0, vec![0usize])]);
    let loaded =
      read_cached_grouping(dir.path(), &key, &hash_to_indices).await;

    assert!(loaded.is_none());
  }

  #[tokio::test]
  async fn group_cache_keeps_exact_duplicates_as_separate_members() {
    let dir = TempDir::new().unwrap();
    let same = [9u8; 32];
    let other = [2u8; 32];
    let hex_same = hex::encode(same);
    let hex_other = hex::encode(other);

    // Indices 0 and 2 are byte-identical files; 1 is different.
    let groups = vec![ProposedGroup {
      label: "Lease".to_string(),
      rationale: "same apartment".to_string(),
      member_indices: vec![0, 1, 2],
      member_destinations: vec![],
      member_notes: vec![],
    }];
    let key = group_cache_key(&[same, other, same], &[]);
    let index_to_hash = HashMap::from([
      (0, hex_same.clone()),
      (1, hex_other.clone()),
      (2, hex_same.clone()),
    ]);
    write_cached_grouping(dir.path(), &key, &groups, &index_to_hash)
      .await
      .unwrap();

    let hash_to_indices = HashMap::from([
      (hex_same, vec![0usize, 2]),
      (hex_other, vec![1usize]),
    ]);
    let loaded =
      read_cached_grouping(dir.path(), &key, &hash_to_indices)
        .await
        .unwrap();

    let mut members = loaded[0].member_indices.clone();
    members.sort_unstable();
    assert_eq!(members, vec![0, 1, 2]);
  }

  #[tokio::test]
  async fn group_cache_misses_when_duplicate_count_shrinks() {
    let dir = TempDir::new().unwrap();
    let same = [9u8; 32];
    let hex_same = hex::encode(same);
    let groups = vec![ProposedGroup {
      label: "Lease".to_string(),
      rationale: String::new(),
      member_indices: vec![0, 1],
      member_destinations: vec![],
      member_notes: vec![],
    }];
    let key = group_cache_key(&[same, same], &[]);
    let index_to_hash =
      HashMap::from([(0, hex_same.clone()), (1, hex_same.clone())]);
    write_cached_grouping(dir.path(), &key, &groups, &index_to_hash)
      .await
      .unwrap();

    // Only one copy present now: two cached members can't both map.
    let hash_to_indices = HashMap::from([(hex_same, vec![0usize])]);
    let loaded =
      read_cached_grouping(dir.path(), &key, &hash_to_indices).await;
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
      ..Default::default()
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
      ..Default::default()
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
          source: DescriptionSource::Ai,
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
      ..Default::default()
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

  #[tokio::test]
  async fn analyze_video_without_ffmpeg_returns_unanalyzed() {
    let cache_dir = TempDir::new().unwrap();
    let file_dir = TempDir::new().unwrap();
    let file =
      make_video_file(file_dir.path(), "clip.mp4", b"fake video");

    let opts = AnalyzeOptions {
      cache_dir: cache_dir.path().to_path_buf(),
      max_concurrent: 1,
      use_batch_api: false,
      ..Default::default()
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
    assert!(result.summary.to_lowercase().contains("video"));
    assert!(result.summary.contains("ffmpeg not found"));
    assert_eq!(result.confidence, 0.0);
    assert_eq!(result.source, DescriptionSource::Unanalyzed);
    assert!(result.tags.contains(&"unanalyzed".to_string()));
  }

  /// Provider that records what it was asked to describe.
  struct RecordingProvider {
    calls: std::sync::Mutex<Vec<(String, String)>>,
  }

  impl AiProvider for RecordingProvider {
    async fn describe_image(
      &self,
      data: &[u8],
      mime_type: &str,
      context: &DescribeContext,
    ) -> Result<ContentDescription> {
      assert!(!data.is_empty());
      self.calls.lock().unwrap().push((
        mime_type.to_string(),
        context.file_type_label.clone(),
      ));
      Ok(ContentDescription {
        summary: "a red frame".to_string(),
        tags: vec!["red".to_string()],
        suggested_category: "art".to_string(),
        confidence: 0.9,
        source: DescriptionSource::Ai,
      })
    }
    async fn propose_groups(
      &self,
      _: &[crate::model::FileSummary],
    ) -> Result<Vec<crate::model::ProposedGroup>> {
      Ok(vec![])
    }
  }

  #[tokio::test]
  async fn analyze_video_with_ffmpeg_describes_a_keyframe() {
    if !crate::video::ffmpeg_available() {
      eprintln!("ffmpeg not on PATH; skipping");
      return;
    }
    let cache_dir = TempDir::new().unwrap();
    let file_dir = TempDir::new().unwrap();
    let path = file_dir.path().join("red.mp4");
    let status = std::process::Command::new("ffmpeg")
      .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
      .arg("color=c=red:s=32x32:d=1")
      .args(["-r", "5", "-pix_fmt", "yuv420p"])
      .arg(&path)
      .status()
      .unwrap();
    assert!(status.success());
    let bytes = std::fs::read(&path).unwrap();
    let mut file =
      make_video_file(file_dir.path(), "red.mp4", &bytes);
    file.scanned.size = bytes.len() as u64;

    let opts = AnalyzeOptions {
      cache_dir: cache_dir.path().to_path_buf(),
      use_ffmpeg: true,
      ..Default::default()
    };
    let provider = RecordingProvider {
      calls: Default::default(),
    };

    let result = analyze_file(&provider, &file, &opts).await.unwrap();

    assert_eq!(result.source, DescriptionSource::Ai);
    // One description per extracted keyframe, joined.
    assert!(
      result.summary.starts_with("a red frame"),
      "{}",
      result.summary
    );
    let calls = provider.calls.lock().unwrap();
    assert!(!calls.is_empty() && calls.len() <= 3);
    assert_eq!(calls[0].0, "image/png");
    assert!(calls[0].1.contains("keyframe"));
  }

  #[tokio::test]
  async fn unanalyzed_descriptions_are_not_cached() {
    let cache_dir = TempDir::new().unwrap();
    let file_dir = TempDir::new().unwrap();
    let file =
      make_video_file(file_dir.path(), "clip.mp4", b"fake video");
    let opts = AnalyzeOptions {
      cache_dir: cache_dir.path().to_path_buf(),
      use_ffmpeg: false,
      ..Default::default()
    };
    struct UnusedProvider;
    impl AiProvider for UnusedProvider {
      async fn describe_image(
        &self,
        _: &[u8],
        _: &str,
        _: &DescribeContext,
      ) -> Result<ContentDescription> {
        panic!("not called without ffmpeg");
      }
      async fn propose_groups(
        &self,
        _: &[crate::model::FileSummary],
      ) -> Result<Vec<crate::model::ProposedGroup>> {
        Ok(vec![])
      }
    }

    let result =
      analyze_file(&UnusedProvider, &file, &opts).await.unwrap();
    assert_eq!(result.source, DescriptionSource::Unanalyzed);

    // Installing ffmpeg later must take effect, so nothing was cached.
    assert!(read_cache(cache_dir.path(), &file.blake3_hash)
      .await
      .is_none());
  }

  #[tokio::test]
  async fn cached_unanalyzed_placeholder_is_treated_as_a_miss() {
    let cache_dir = TempDir::new().unwrap();
    let hash = [7u8; 32];
    let stale = ContentDescription {
      summary: "Video file: x.mp4 (ffmpeg not found)".to_string(),
      tags: vec![],
      suggested_category: "other".to_string(),
      confidence: 0.0,
      source: DescriptionSource::Unanalyzed,
    };
    write_cache(cache_dir.path(), &hash, &stale).await.unwrap();

    assert!(read_cache(cache_dir.path(), &hash).await.is_none());
  }

  #[tokio::test]
  async fn analyze_video_with_broken_ffmpeg_input_is_unanalyzed() {
    if !crate::video::ffmpeg_available() {
      eprintln!("ffmpeg not on PATH; skipping");
      return;
    }
    let cache_dir = TempDir::new().unwrap();
    let file_dir = TempDir::new().unwrap();
    let file =
      make_video_file(file_dir.path(), "junk.mp4", b"not a video");
    let opts = AnalyzeOptions {
      cache_dir: cache_dir.path().to_path_buf(),
      use_ffmpeg: true,
      ..Default::default()
    };
    struct UnusedProvider;
    impl AiProvider for UnusedProvider {
      async fn describe_image(
        &self,
        _: &[u8],
        _: &str,
        _: &DescribeContext,
      ) -> Result<ContentDescription> {
        panic!("must not be called for an undecodable video");
      }
      async fn propose_groups(
        &self,
        _: &[crate::model::FileSummary],
      ) -> Result<Vec<crate::model::ProposedGroup>> {
        Ok(vec![])
      }
    }

    let result =
      analyze_file(&UnusedProvider, &file, &opts).await.unwrap();

    assert_eq!(result.source, DescriptionSource::Unanalyzed);
    assert!(result.summary.contains("junk.mp4"));
  }

  #[tokio::test]
  async fn analyze_video_placeholder_is_not_cached() {
    let cache_dir = TempDir::new().unwrap();
    let file_dir = TempDir::new().unwrap();
    let file =
      make_video_file(file_dir.path(), "movie.mp4", b"video bytes");

    let opts = AnalyzeOptions {
      cache_dir: cache_dir.path().to_path_buf(),
      max_concurrent: 1,
      use_batch_api: false,
      ..Default::default()
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
          source: DescriptionSource::Ai,
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
    assert!(cached.is_none(), "placeholder must not be cached");
  }

  fn make_document_file(
    dir: &Path,
    name: &str,
    content: &[u8],
    format: crate::model::DocumentFormat,
  ) -> FingerprintedFile {
    let mut file = make_test_file(dir, name, content);
    // Documents need their real bytes on disk for text extraction.
    std::fs::write(&file.scanned.path, content).unwrap();
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
        source: DescriptionSource::Ai,
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
      ..Default::default()
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
      ..Default::default()
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
      ..Default::default()
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
            source: DescriptionSource::Ai,
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
      ..Default::default()
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
      ..Default::default()
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
      ..Default::default()
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

  #[test]
  fn strip_docx_xml_keeps_text_and_paragraphs() {
    let xml = "<w:document><w:p><w:r><w:t>Hello &amp; \
               welcome</w:t></w:r></w:p><w:p><w:r><w:t>Second \
               paragraph</w:t></w:r></w:p></w:document>";

    let text = strip_docx_xml(xml);

    assert!(text.contains("Hello & welcome"));
    assert!(text.contains("\nSecond paragraph"));
    assert!(!text.contains('<'));
  }

  #[tokio::test]
  async fn extract_docx_text_reads_real_docx_structure() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("report.docx");
    let file = std::fs::File::create(&path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    writer.start_file("word/document.xml", options).unwrap();
    std::io::Write::write_all(
      &mut writer,
      b"<w:document><w:p><w:t>QUARTERLY REPORT for Acme \
        Corp</w:t></w:p></w:document>",
    )
    .unwrap();
    writer.finish().unwrap();

    let text = extract_docx_text(&path).await.unwrap();

    assert!(text.contains("QUARTERLY REPORT for Acme Corp"));
  }

  #[test]
  fn strip_rtf_removes_control_words() {
    let rtf = r"{\rtf1\ansi\deff0 {\fonttbl {\f0 Times;}}\f0\fs24 Hello \b bold\b0  world.\par Second line.}";

    let text = strip_rtf(rtf);

    assert!(text.contains("Hello"));
    assert!(text.contains("bold"));
    assert!(text.contains("world."));
    assert!(text.contains("\nSecond line."));
    assert!(!text.contains("rtf1"));
    assert!(!text.contains('\\'));
  }

  /// Provider whose calls finish in reverse submission order, so a
  /// collector that yields in completion order would scramble results.
  struct ReverseOrderProvider;

  impl AiProvider for ReverseOrderProvider {
    async fn describe_image(
      &self,
      _image_data: &[u8],
      _mime_type: &str,
      context: &DescribeContext,
    ) -> Result<ContentDescription> {
      // img0 waits longest, img4 returns first.
      let idx: u64 = context
        .filename
        .trim_start_matches("img")
        .trim_end_matches(".png")
        .parse()
        .unwrap();
      tokio::time::sleep(std::time::Duration::from_millis(
        (5 - idx) * 20,
      ))
      .await;
      Ok(ContentDescription {
        summary: format!("description of {}", context.filename),
        tags: vec![],
        suggested_category: "other".to_string(),
        confidence: 0.9,
        source: DescriptionSource::Ai,
      })
    }

    async fn propose_groups(
      &self,
      _files: &[crate::model::FileSummary],
    ) -> Result<Vec<crate::model::ProposedGroup>> {
      Ok(vec![])
    }
  }

  #[tokio::test]
  async fn analyze_batch_preserves_input_order_under_concurrency() {
    let file_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();
    let files: Vec<FingerprintedFile> = (0..5)
      .map(|i| {
        let png = create_test_png(1, 1, &[i as u8, 0, 0, 255]);
        let path = file_dir.path().join(format!("img{i}.png"));
        std::fs::write(&path, &png).unwrap();
        FingerprintedFile {
          scanned: crate::model::ScannedFile {
            path: path.clone(),
            scan_root: file_dir.path().to_path_buf(),
            size: png.len() as u64,
            modified: std::time::SystemTime::now(),
            file_type: crate::model::FileType::Image(
              crate::model::ImageFormat::Png,
            ),
          },
          blake3_hash: crate::fingerprint::compute_blake3(&path)
            .unwrap(),
          perceptual_hash: None,
        }
      })
      .collect();
    let opts = AnalyzeOptions {
      cache_dir: cache_dir.path().to_path_buf(),
      max_concurrent: 5,
      use_batch_api: false,
      ..Default::default()
    };

    let results =
      analyze_batch(&ReverseOrderProvider, &files, &opts).await;

    assert_eq!(results.len(), 5);
    for (i, result) in results.iter().enumerate() {
      let summary = &result.as_ref().unwrap().summary;
      assert_eq!(
        summary,
        &format!("description of img{i}.png"),
        "result {i} belongs to a different file"
      );
    }
  }

  #[test]
  fn exif_hint_none_for_plain_png() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("plain.png");
    std::fs::write(&path, create_test_png(1, 1, &[1, 2, 3, 255]))
      .unwrap();

    assert!(exif_hint(&path).is_none());
  }

  fn create_test_png(
    width: u32,
    height: u32,
    rgba: &[u8],
  ) -> Vec<u8> {
    use image::{ImageBuffer, RgbaImage};
    let img: RgbaImage =
      ImageBuffer::from_raw(width, height, rgba.to_vec()).unwrap();
    let mut buf = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut buf);
    img.write_to(&mut cursor, image::ImageFormat::Png).unwrap();
    buf
  }

  #[test]
  fn extract_zip_extracts_supported_files() {
    let dir = TempDir::new().unwrap();
    let zip_path = dir.path().join("test.zip");

    let file = std::fs::File::create(&zip_path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
      .compression_method(zip::CompressionMethod::Stored);
    writer.start_file("hello.txt", options).unwrap();
    std::io::Write::write_all(&mut writer, b"Hello world").unwrap();
    writer.start_file("photo.jpg", options).unwrap();
    std::io::Write::write_all(&mut writer, b"\xFF\xD8\xFF\xE0fake")
      .unwrap();
    writer.finish().unwrap();

    let dest = dir.path().join("extracted");
    std::fs::create_dir(&dest).unwrap();

    let opts = AnalyzeOptions {
      max_archive_files: 10,
      max_archive_file_size_mb: 50,
      ..Default::default()
    };
    let files = extract_zip(&zip_path, &dest, &opts).unwrap();

    assert_eq!(files.len(), 2);
  }

  #[test]
  fn extract_zip_skips_hidden_and_nested_archives() {
    let dir = TempDir::new().unwrap();
    let zip_path = dir.path().join("test.zip");

    let file = std::fs::File::create(&zip_path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
      .compression_method(zip::CompressionMethod::Stored);
    writer.start_file("visible.txt", options).unwrap();
    std::io::Write::write_all(&mut writer, b"ok").unwrap();
    writer.start_file(".hidden/secret.txt", options).unwrap();
    std::io::Write::write_all(&mut writer, b"nope").unwrap();
    writer.start_file("inner.zip", options).unwrap();
    std::io::Write::write_all(&mut writer, b"nested").unwrap();
    writer.finish().unwrap();

    let dest = dir.path().join("extracted");
    std::fs::create_dir(&dest).unwrap();

    let opts = AnalyzeOptions::default();
    let files = extract_zip(&zip_path, &dest, &opts).unwrap();

    assert_eq!(files.len(), 1);
    let name =
      files[0].scanned.path.file_name().unwrap().to_string_lossy();
    assert_eq!(name, "visible.txt");
  }

  #[test]
  fn extract_zip_respects_max_files_limit() {
    let dir = TempDir::new().unwrap();
    let zip_path = dir.path().join("test.zip");

    let file = std::fs::File::create(&zip_path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
      .compression_method(zip::CompressionMethod::Stored);
    for i in 0..10 {
      writer.start_file(format!("file{i}.txt"), options).unwrap();
      std::io::Write::write_all(&mut writer, b"data").unwrap();
    }
    writer.finish().unwrap();

    let dest = dir.path().join("extracted");
    std::fs::create_dir(&dest).unwrap();

    let opts = AnalyzeOptions {
      max_archive_files: 3,
      ..Default::default()
    };
    let files = extract_zip(&zip_path, &dest, &opts).unwrap();

    assert_eq!(files.len(), 3);
  }
}

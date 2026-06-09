use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::sync::Semaphore;

use crate::ai::{AiProvider, DescribeContext};
use crate::model::{ContentDescription, FingerprintedFile};

pub struct AnalyzeOptions {
  pub cache_dir: PathBuf,
  pub max_concurrent: usize,
}

impl Default for AnalyzeOptions {
  fn default() -> Self {
    Self {
      cache_dir: default_cache_dir(),
      max_concurrent: 5,
    }
  }
}

fn default_cache_dir() -> PathBuf {
  directories::BaseDirs::new()
    .map(|d| d.cache_dir().join("spindle"))
    .unwrap_or_else(|| PathBuf::from(".cache/spindle"))
}

fn cache_path(cache_dir: &Path, blake3_hash: &[u8; 32]) -> PathBuf {
  let hex = hex::encode(blake3_hash);
  cache_dir.join(format!("{hex}.json"))
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
        "/cache/{}.json",
        hex::encode([0xAB; 32])
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
}

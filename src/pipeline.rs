use std::path::PathBuf;

use anyhow::Result;

const BYTES_PER_MB: u64 = 1_000_000;
use tokio::sync::mpsc;

use crate::ai::AiProvider;
use crate::analyze::{analyze_batch, AnalyzeOptions};
use crate::cost::estimate_cost;
use crate::fingerprint::{
  find_exact_duplicates, find_near_duplicates, fingerprint_files,
};
use crate::group::build_groups;
use crate::model::FileCategory;
use crate::model::{
  DuplicateSet, FileSummary, FingerprintedFile, ProposedGroup,
  ReorgPlan,
};
use crate::plan::propose_plan;
use crate::scanner::{scan_directories_filtered, ScanOptions};

#[derive(Debug, Clone)]
pub struct DuplicateDetail {
  pub canonical: String,
  pub duplicate: String,
  pub kind: String,
}

#[derive(Debug, Clone)]
pub enum PipelineEvent {
  ScanComplete {
    file_count: usize,
  },
  FingerprintComplete {
    file_count: usize,
    exact_dupes: usize,
    near_dupes: usize,
    duplicate_details: Vec<DuplicateDetail>,
  },
  CostEstimated {
    estimated_usd: f64,
    file_count: usize,
  },
  AnalysisStarted {
    file_count: usize,
  },
  FileAnalyzed {
    filename: String,
  },
  AnalysisComplete {
    succeeded: usize,
    failed: usize,
    failed_files: Vec<(String, String)>,
  },
  GroupingFailed {
    error: String,
  },
  GroupingComplete {
    group_count: usize,
  },
  PlanReady,
}

pub struct PipelineConfig {
  pub target_dirs: Vec<PathBuf>,
  pub output_dir: PathBuf,
  pub no_ai: bool,
  pub max_files: usize,
  pub max_file_size_mb: u64,
  pub max_cost: Option<f64>,
  pub near_duplicate_threshold: u32,
  pub cache_dir: PathBuf,
  pub max_concurrent: usize,
  pub include_trash: bool,
  pub type_filter: Vec<FileCategory>,
}

#[derive(Debug)]
pub struct PipelineResult {
  pub plan: ReorgPlan,
  pub fingerprinted: Vec<FingerprintedFile>,
  pub all_dupes: Vec<DuplicateSet>,
}

pub async fn run<P: AiProvider>(
  provider: &P,
  config: &PipelineConfig,
  tx: mpsc::Sender<PipelineEvent>,
) -> Result<PipelineResult> {
  let scan_opts = ScanOptions {
    include_trash: config.include_trash,
    type_filter: config.type_filter.clone(),
  };
  let scanned =
    scan_directories_filtered(&config.target_dirs, &scan_opts)?;
  if scanned.is_empty() {
    anyhow::bail!("No supported files found in target directories");
  }

  let _ = tx
    .send(PipelineEvent::ScanComplete {
      file_count: scanned.len(),
    })
    .await;

  let fingerprinted = fingerprint_files(scanned)?;
  let exact_dupes = find_exact_duplicates(&fingerprinted);
  let near_dupes = find_near_duplicates(
    &fingerprinted,
    config.near_duplicate_threshold,
  );

  let resolve_name = |idx: usize| -> String {
    fingerprinted
      .get(idx)
      .map(|f| {
        f.scanned
          .path
          .file_name()
          .unwrap_or_default()
          .to_string_lossy()
          .to_string()
      })
      .unwrap_or_else(|| format!("[index {}]", idx))
  };

  let mut duplicate_details = Vec::new();
  for set in &exact_dupes {
    let canonical = resolve_name(set.canonical);
    for &dup_idx in &set.duplicates {
      duplicate_details.push(DuplicateDetail {
        canonical: canonical.clone(),
        duplicate: resolve_name(dup_idx),
        kind: "exact".to_string(),
      });
    }
  }
  for set in &near_dupes {
    let canonical = resolve_name(set.canonical);
    let kind = match set.duplicate_type {
      crate::model::DuplicateType::NearDuplicate { distance } => {
        format!("near({})", distance)
      }
      _ => "near".to_string(),
    };
    for &dup_idx in &set.duplicates {
      duplicate_details.push(DuplicateDetail {
        canonical: canonical.clone(),
        duplicate: resolve_name(dup_idx),
        kind: kind.clone(),
      });
    }
  }

  let _ = tx
    .send(PipelineEvent::FingerprintComplete {
      file_count: fingerprinted.len(),
      exact_dupes: exact_dupes
        .iter()
        .map(|d| d.duplicates.len())
        .sum(),
      near_dupes: near_dupes.iter().map(|d| d.duplicates.len()).sum(),
      duplicate_details,
    })
    .await;

  let proposed_groups = if config.no_ai {
    vec![ProposedGroup {
      label: "All Files".to_string(),
      rationale: "AI analysis skipped".to_string(),
      member_indices: (0..fingerprinted.len()).collect(),
      member_destinations: vec![],
    }]
  } else {
    run_ai_pipeline(provider, &fingerprinted, config, &tx).await?
  };

  let all_dupes: Vec<DuplicateSet> =
    exact_dupes.into_iter().chain(near_dupes).collect();

  let groups = build_groups(&proposed_groups, &config.output_dir);

  let _ = tx
    .send(PipelineEvent::GroupingComplete {
      group_count: groups.len(),
    })
    .await;

  let plan = propose_plan(
    &config.output_dir,
    &groups,
    &all_dupes,
    &fingerprinted,
  );

  let _ = tx.send(PipelineEvent::PlanReady).await;

  Ok(PipelineResult {
    plan,
    fingerprinted,
    all_dupes,
  })
}

async fn run_ai_pipeline<P: AiProvider>(
  provider: &P,
  fingerprinted: &[FingerprintedFile],
  config: &PipelineConfig,
  tx: &mpsc::Sender<PipelineEvent>,
) -> Result<Vec<ProposedGroup>> {
  let files_to_analyze: Vec<_> = fingerprinted
    .iter()
    .enumerate()
    .filter(|(_, f)| {
      f.scanned.size <= config.max_file_size_mb * BYTES_PER_MB
    })
    .take(config.max_files)
    .collect();

  let analyze_count = files_to_analyze.len();
  let cost_est = estimate_cost(analyze_count);

  let _ = tx
    .send(PipelineEvent::CostEstimated {
      estimated_usd: cost_est.estimated_cost_usd,
      file_count: analyze_count,
    })
    .await;

  if let Some(max_cost) = config.max_cost {
    if cost_est.estimated_cost_usd > max_cost {
      anyhow::bail!(
        "Estimated cost ${:.4} exceeds limit ${:.4}. \
         Reduce file count with --max-files or raise the limit.",
        cost_est.estimated_cost_usd,
        max_cost
      );
    }
  }

  let _ = tx
    .send(PipelineEvent::AnalysisStarted {
      file_count: analyze_count,
    })
    .await;

  let opts = AnalyzeOptions {
    cache_dir: config.cache_dir.clone(),
    max_concurrent: config.max_concurrent,
  };

  let subset: Vec<_> =
    files_to_analyze.iter().map(|(_, f)| (*f).clone()).collect();
  let results = analyze_batch(provider, &subset, &opts).await;

  let mut summaries = Vec::new();
  let mut failed_files = Vec::new();
  for ((idx, f), result) in
    files_to_analyze.iter().zip(results.iter())
  {
    let filename = f
      .scanned
      .path
      .file_name()
      .unwrap_or_default()
      .to_string_lossy()
      .to_string();

    let _ = tx
      .send(PipelineEvent::FileAnalyzed {
        filename: filename.clone(),
      })
      .await;

    match result {
      Ok(desc) => {
        let source_path = f
          .scanned
          .relative_path()
          .to_string_lossy()
          .replace('\\', "/");

        summaries.push(FileSummary {
          index: *idx,
          filename,
          source_path,
          description: desc.clone(),
          metadata_hint: String::new(),
        });
      }
      Err(err) => {
        failed_files.push((filename, err.to_string()));
      }
    }
  }

  let succeeded = summaries.len();
  let failed = failed_files.len();

  let _ = tx
    .send(PipelineEvent::AnalysisComplete {
      succeeded,
      failed,
      failed_files,
    })
    .await;

  match provider.propose_groups(&summaries).await {
    Ok(groups) => Ok(groups),
    Err(err) => {
      let _ = tx
        .send(PipelineEvent::GroupingFailed {
          error: format!("{err:#}"),
        })
        .await;
      Ok(vec![ProposedGroup {
        label: "All Files".to_string(),
        rationale: format!(
          "Semantic grouping failed ({err:#}), \
           falling back to single group"
        ),
        member_indices: (0..summaries.len()).collect(),
        member_destinations: vec![],
      }])
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::ai::DescribeContext;
  use crate::model::ContentDescription;
  use std::fs;
  use tempfile::TempDir;

  struct FakeProvider;

  impl AiProvider for FakeProvider {
    async fn describe_image(
      &self,
      _image_data: &[u8],
      _mime_type: &str,
      context: &DescribeContext,
    ) -> anyhow::Result<ContentDescription> {
      Ok(ContentDescription {
        summary: format!("Description of {}", context.filename),
        tags: vec!["test".to_string()],
        suggested_category: "photo".to_string(),
        confidence: 0.85,
      })
    }

    async fn propose_groups(
      &self,
      files: &[FileSummary],
    ) -> anyhow::Result<Vec<ProposedGroup>> {
      Ok(vec![ProposedGroup {
        label: "All Test Files".to_string(),
        rationale: "Grouped for testing".to_string(),
        member_indices: (0..files.len()).collect(),
        member_destinations: vec![],
      }])
    }
  }

  fn create_test_png(r: u8, g: u8, b: u8) -> Vec<u8> {
    use image::{ImageBuffer, RgbaImage};
    let img: RgbaImage =
      ImageBuffer::from_raw(1, 1, vec![r, g, b, 255]).unwrap();
    let mut buf = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut buf);
    img.write_to(&mut cursor, image::ImageFormat::Png).unwrap();
    buf
  }

  #[tokio::test]
  async fn pipeline_runs_with_no_ai() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    fs::write(
      source.path().join("a.png"),
      create_test_png(255, 0, 0),
    )
    .unwrap();
    fs::write(
      source.path().join("b.png"),
      create_test_png(0, 255, 0),
    )
    .unwrap();

    let config = PipelineConfig {
      target_dirs: vec![source.path().to_path_buf()],
      output_dir: output.path().to_path_buf(),
      no_ai: true,
      max_files: 500,
      max_file_size_mb: 100,
      max_cost: None,
      near_duplicate_threshold: 8,
      cache_dir: cache.path().to_path_buf(),
      max_concurrent: 2,
      include_trash: false,
      type_filter: vec![],
    };

    let (tx, mut rx) = mpsc::channel(64);
    let result = run(&FakeProvider, &config, tx).await.unwrap();

    assert_eq!(result.plan.stats.total_files, 2);
    assert_eq!(result.plan.stats.groups_created, 1);

    let mut events = vec![];
    while let Ok(ev) = rx.try_recv() {
      events.push(ev);
    }
    assert!(events
      .iter()
      .any(|e| matches!(e, PipelineEvent::ScanComplete { .. })));
    assert!(events
      .iter()
      .any(|e| matches!(e, PipelineEvent::PlanReady)));
  }

  #[tokio::test]
  async fn pipeline_runs_with_ai() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    fs::write(
      source.path().join("red.png"),
      create_test_png(255, 0, 0),
    )
    .unwrap();
    fs::write(
      source.path().join("blue.png"),
      create_test_png(0, 0, 255),
    )
    .unwrap();

    let config = PipelineConfig {
      target_dirs: vec![source.path().to_path_buf()],
      output_dir: output.path().to_path_buf(),
      no_ai: false,
      max_files: 500,
      max_file_size_mb: 100,
      max_cost: None,
      near_duplicate_threshold: 8,
      cache_dir: cache.path().to_path_buf(),
      max_concurrent: 2,
      include_trash: false,
      type_filter: vec![],
    };

    let (tx, mut rx) = mpsc::channel(64);
    let result = run(&FakeProvider, &config, tx).await.unwrap();

    assert!(result.plan.stats.total_files > 0);

    let mut events = vec![];
    while let Ok(ev) = rx.try_recv() {
      events.push(ev);
    }
    assert!(events
      .iter()
      .any(|e| matches!(e, PipelineEvent::AnalysisStarted { .. })));
    assert!(events
      .iter()
      .any(|e| matches!(e, PipelineEvent::AnalysisComplete { .. })));
    assert!(events
      .iter()
      .any(|e| matches!(e, PipelineEvent::GroupingComplete { .. })));
  }

  #[tokio::test]
  async fn pipeline_empty_dir_returns_error() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    let config = PipelineConfig {
      target_dirs: vec![source.path().to_path_buf()],
      output_dir: output.path().to_path_buf(),
      no_ai: true,
      max_files: 500,
      max_file_size_mb: 100,
      max_cost: None,
      near_duplicate_threshold: 8,
      cache_dir: cache.path().to_path_buf(),
      max_concurrent: 2,
      include_trash: false,
      type_filter: vec![],
    };

    let (tx, _rx) = mpsc::channel(64);
    let result = run(&FakeProvider, &config, tx).await;

    assert!(result.is_err());
    assert!(result
      .unwrap_err()
      .to_string()
      .contains("No supported files"));
  }

  #[tokio::test]
  async fn pipeline_respects_max_cost() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    for i in 0..100 {
      fs::write(
        source.path().join(format!("img{i}.png")),
        create_test_png((i * 2) as u8, 0, 0),
      )
      .unwrap();
    }

    let config = PipelineConfig {
      target_dirs: vec![source.path().to_path_buf()],
      output_dir: output.path().to_path_buf(),
      no_ai: false,
      max_files: 500,
      max_file_size_mb: 100,
      max_cost: Some(0.0001),
      near_duplicate_threshold: 8,
      cache_dir: cache.path().to_path_buf(),
      max_concurrent: 2,
      include_trash: false,
      type_filter: vec![],
    };

    let (tx, _rx) = mpsc::channel(64);
    let result = run(&FakeProvider, &config, tx).await;

    assert!(result.is_err());
    assert!(result
      .unwrap_err()
      .to_string()
      .contains("exceeds limit"));
  }

  #[tokio::test]
  async fn pipeline_detects_exact_duplicates() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    let png = create_test_png(128, 128, 128);
    fs::write(source.path().join("original.png"), &png).unwrap();
    fs::write(source.path().join("copy.png"), &png).unwrap();

    let config = PipelineConfig {
      target_dirs: vec![source.path().to_path_buf()],
      output_dir: output.path().to_path_buf(),
      no_ai: true,
      max_files: 500,
      max_file_size_mb: 100,
      max_cost: None,
      near_duplicate_threshold: 8,
      cache_dir: cache.path().to_path_buf(),
      max_concurrent: 2,
      include_trash: false,
      type_filter: vec![],
    };

    let (tx, mut rx) = mpsc::channel(64);
    let result = run(&FakeProvider, &config, tx).await.unwrap();

    assert!(!result.all_dupes.is_empty());
    assert!(result.plan.stats.duplicates_found >= 1);

    let mut events = vec![];
    while let Ok(ev) = rx.try_recv() {
      events.push(ev);
    }
    let fp_event = events.iter().find(|e| {
      matches!(e, PipelineEvent::FingerprintComplete { .. })
    });
    assert!(matches!(
      fp_event,
      Some(PipelineEvent::FingerprintComplete { exact_dupes: 1, .. })
    ));
  }
}

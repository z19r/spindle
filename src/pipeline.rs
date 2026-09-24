use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;

const BYTES_PER_MB: u64 = 1_000_000;
use tokio::sync::mpsc;

use crate::ai::AiProvider;
use crate::analyze::{
  analyze_batch, group_cache_key, read_cache, read_cached_grouping,
  read_cached_routing, write_cached_grouping, write_cached_routing,
  AnalyzeOptions,
};
use crate::cost::estimate_cost;
use crate::fingerprint::{
  find_exact_duplicates, find_near_duplicates, fingerprint_files,
};
use crate::group::build_groups;
use crate::ledger::{Ledger, OrganizedDuplicate};
use crate::model::{
  Area, ContentDescription, DuplicateSet, FileSummary,
  FingerprintedFile, MemberNote, ProposedGroup, ReorgPlan,
  RoutedFile,
};
use crate::model::{DescriptionSource, FileCategory};
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
    cached: usize,
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
  /// Stage-one routing finished: how many areas received files and how
  /// many files had no usable area (grouped without a constraint).
  RoutingComplete {
    areas_used: usize,
    unrouted: usize,
  },
  /// What deterministic label validation changed (see
  /// `group::validate`).
  LabelsNormalised {
    merged: usize,
    collapsed: usize,
    rewritten: usize,
  },
  GroupingComplete {
    group_count: usize,
    /// Files placed in the `Unsorted` group rather than a real one.
    unsorted: usize,
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
  pub use_batch_api: bool,
  pub introspect_archives: bool,
  pub max_archive_files: usize,
  pub max_archive_file_size_mb: u64,
  pub use_organized_context: bool,
  /// Path to the persistent "already organized" ledger. `None` disables
  /// both candidate exclusion and recording.
  pub ledger_path: Option<PathBuf>,
  /// Model id used for grouping — drives cost estimation.
  pub model: String,
  /// Model id used for per-file descriptions.
  pub describe_model: String,
  /// Top-level areas for two-stage grouping. Empty = single stage.
  pub taxonomy: Vec<Area>,
}

#[derive(Debug)]
pub struct PipelineResult {
  pub plan: ReorgPlan,
  pub fingerprinted: Vec<FingerprintedFile>,
  pub all_dupes: Vec<DuplicateSet>,
  /// New candidates that are byte-identical to already-organized files.
  pub organized_duplicates: Vec<OrganizedDuplicate>,
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

  let mut fingerprinted = fingerprint_files(scanned)?;

  // Drop files a previous run already organized (matched by path + content
  // hash); collect any brand-new files that are byte-identical to
  // already-organized content so they can be surfaced as duplicates.
  let ledger = config.ledger_path.as_ref().map(|p| Ledger::load(p));
  let organized_duplicates =
    apply_ledger_exclusion(&mut fingerprinted, ledger.as_ref());
  if fingerprinted.is_empty() {
    // Everything scanned was already organized. That's success, not an
    // error — return an empty plan so the caller reports "nothing to do"
    // (and still surfaces any identical-to-organized duplicates).
    let plan = propose_plan(&config.output_dir, &[], &[], &[]);
    let _ = tx.send(PipelineEvent::PlanReady).await;
    return Ok(PipelineResult {
      plan,
      fingerprinted: vec![],
      all_dupes: vec![],
      organized_duplicates,
    });
  }

  let exact_dupes = find_exact_duplicates(&fingerprinted);
  let mut near_dupes = find_near_duplicates(
    &fingerprinted,
    config.near_duplicate_threshold,
  );
  near_dupes.extend(
    crate::fingerprint::archive::find_archive_matches(&fingerprinted),
  );
  near_dupes.extend(crate::fingerprint::text::find_similar_text(
    &fingerprinted,
  ));
  near_dupes.extend(crate::fingerprint::audio::find_similar_audio(
    &fingerprinted,
  ));

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
      crate::model::DuplicateType::ArchiveMatch => {
        "archive≡folder".to_string()
      }
      crate::model::DuplicateType::SimilarText { distance } => {
        format!("text({})", distance)
      }
      crate::model::DuplicateType::SimilarAudio { score } => {
        format!("audio({}%)", score)
      }
      crate::model::DuplicateType::Exact => "exact".to_string(),
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
      member_notes: vec![],
    }]
  } else {
    let existing_labels: Vec<String> = ledger
      .as_ref()
      .map(|l| {
        l.existing_groups_under(&config.output_dir)
          .into_iter()
          .map(|g| g.label)
          .collect()
      })
      .unwrap_or_default();
    let organized_context = if config.use_organized_context {
      load_organized_context(
        ledger.as_ref(),
        &config.output_dir,
        &config.cache_dir,
      )
      .await
    } else {
      Vec::new()
    };
    run_ai_pipeline(
      provider,
      &fingerprinted,
      &existing_labels,
      &organized_context,
      config,
      &tx,
    )
    .await?
  };

  let all_dupes: Vec<DuplicateSet> =
    exact_dupes.into_iter().chain(near_dupes).collect();

  let groups = build_groups(&proposed_groups, &config.output_dir);

  let _ = tx
    .send(PipelineEvent::GroupingComplete {
      group_count: groups.len(),
      unsorted: groups
        .iter()
        .filter(|g| g.label == UNSORTED_LABEL)
        .map(|g| g.members.len())
        .sum(),
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
    organized_duplicates,
  })
}

/// Remove already-organized files from the candidate set and return new
/// files that are byte-identical to organized content. With no ledger,
/// nothing is excluded.
fn apply_ledger_exclusion(
  fingerprinted: &mut Vec<FingerprintedFile>,
  ledger: Option<&Ledger>,
) -> Vec<OrganizedDuplicate> {
  let Some(ledger) = ledger else {
    return Vec::new();
  };
  let before = fingerprinted.len();
  let mut duplicates = Vec::new();
  fingerprinted.retain(|f| {
    let hex = crate::ledger::hash_hex(&f.blake3_hash);
    if ledger.is_organized(&f.scanned.path, &hex) {
      return false;
    }
    if let Some(entry) = ledger.duplicate_of(&hex) {
      duplicates.push(OrganizedDuplicate {
        path: f.scanned.path.clone(),
        organized_at: entry.dest_path.clone(),
      });
    }
    true
  });
  let skipped = before - fingerprinted.len();
  if skipped > 0 {
    tracing::info!(skipped, "Excluded already-organized files");
  }
  duplicates
}

async fn load_organized_context(
  ledger: Option<&Ledger>,
  output_dir: &std::path::Path,
  cache_dir: &std::path::Path,
) -> Vec<(String, Vec<ContentDescription>)> {
  let Some(ledger) = ledger else {
    return Vec::new();
  };
  let group_hashes = ledger.group_content_hashes(output_dir, 5);
  let mut context = Vec::with_capacity(group_hashes.len());
  for (label, hashes) in group_hashes {
    let mut descriptions = Vec::new();
    for hex in &hashes {
      let mut hash_bytes = [0u8; 32];
      if hex::decode_to_slice(hex, &mut hash_bytes).is_ok() {
        if let Some(desc) = read_cache(cache_dir, &hash_bytes).await {
          descriptions.push(desc);
        }
      }
    }
    if !descriptions.is_empty() {
      context.push((label, descriptions));
    }
  }
  context
}

async fn run_ai_pipeline<P: AiProvider>(
  provider: &P,
  fingerprinted: &[FingerprintedFile],
  existing_labels: &[String],
  organized_context: &[(String, Vec<ContentDescription>)],
  config: &PipelineConfig,
  tx: &mpsc::Sender<PipelineEvent>,
) -> Result<Vec<ProposedGroup>> {
  let size_cap = config.max_file_size_mb * BYTES_PER_MB;
  let mut files_to_analyze: Vec<(usize, &FingerprintedFile)> =
    Vec::new();
  // Files that never reach the model still belong to the user; keep
  // them, with the reason, for the Unsorted group.
  let mut skipped: Vec<(usize, String)> = Vec::new();
  for (idx, f) in fingerprinted.iter().enumerate() {
    if f.scanned.size > size_cap {
      skipped.push((
        idx,
        format!(
          "not analyzed: larger than {} MB",
          config.max_file_size_mb
        ),
      ));
    } else if files_to_analyze.len() >= config.max_files {
      skipped.push((
        idx,
        format!(
          "not analyzed: beyond --max-files ({})",
          config.max_files
        ),
      ));
    } else {
      files_to_analyze.push((idx, f));
    }
  }

  let analyze_count = files_to_analyze.len();

  // Files whose description is already cached cost nothing — only the
  // uncached ones are actually sent to Claude, so estimate and report
  // against that count.
  let mut cached = 0;
  for (_, f) in &files_to_analyze {
    if read_cache(&config.cache_dir, &f.blake3_hash)
      .await
      .is_some()
    {
      cached += 1;
    }
  }
  let to_send = analyze_count - cached;
  let cost_est =
    estimate_cost(to_send, &config.describe_model, &config.model);

  let _ = tx
    .send(PipelineEvent::CostEstimated {
      estimated_usd: cost_est.estimated_cost_usd,
      file_count: to_send,
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
      cached,
    })
    .await;

  let opts = AnalyzeOptions {
    cache_dir: config.cache_dir.clone(),
    max_concurrent: config.max_concurrent,
    use_batch_api: config.use_batch_api,
    introspect_archives: config.introspect_archives,
    max_archive_files: config.max_archive_files,
    max_archive_file_size_mb: config.max_archive_file_size_mb,
    use_ffmpeg: crate::video::ffmpeg_available(),
  };

  let subset: Vec<_> =
    files_to_analyze.iter().map(|(_, f)| (*f).clone()).collect();
  let results = analyze_batch(provider, &subset, &opts).await;

  let mut summaries = Vec::new();
  let mut failed_files = Vec::new();
  let mut failed_notes: Vec<(usize, String)> = Vec::new();
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
        failed_notes
          .push((*idx, format!("analysis failed: {err:#}")));
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

  // Grouping is the one Claude call left on an otherwise-cached run. Key it
  // by the exact set of file hashes + existing folder labels so an unchanged
  // run replays the cached grouping and sends zero tokens.
  let by_index: HashMap<usize, &FingerprintedFile> =
    files_to_analyze.iter().map(|(idx, f)| (*idx, *f)).collect();
  let mut index_to_hash = HashMap::new();
  // Exact duplicates share a hash; keep every index so a cached
  // grouping can restore each copy separately.
  let mut hash_to_indices: HashMap<String, Vec<usize>> =
    HashMap::new();
  let mut summary_hashes = Vec::with_capacity(summaries.len());
  for summary in &summaries {
    if let Some(f) = by_index.get(&summary.index) {
      let hex = crate::ledger::hash_hex(&f.blake3_hash);
      summary_hashes.push(f.blake3_hash);
      index_to_hash.insert(summary.index, hex.clone());
      hash_to_indices.entry(hex).or_default().push(summary.index);
    }
  }

  // The taxonomy shapes the result, so it salts the cache key too.
  let mut cache_salt: Vec<String> = existing_labels.to_vec();
  cache_salt.extend(
    config.taxonomy.iter().map(|a| format!("area:{}", a.name)),
  );
  let cache_key = group_cache_key(&summary_hashes, &cache_salt);
  if let Some(groups) = read_cached_grouping(
    &config.cache_dir,
    &cache_key,
    &hash_to_indices,
  )
  .await
  {
    tracing::info!("Reused cached grouping (no Claude call)");
    let groups = validate_and_report(groups, tx).await;
    return Ok(add_unsorted(
      groups,
      &summaries,
      skipped,
      failed_notes,
    ));
  }

  let two_stage = TwoStage {
    areas: &config.taxonomy,
    cache_dir: &config.cache_dir,
    summary_hashes: &summary_hashes,
    hash_to_indices: &hash_to_indices,
    index_to_hash: &index_to_hash,
  };
  let groups = match propose_groups_two_stage(
    provider,
    &summaries,
    existing_labels,
    organized_context,
    &two_stage,
    tx,
  )
  .await
  {
    Ok(groups) => {
      let groups = quarantine_low_confidence(groups, &summaries);
      let _ = write_cached_grouping(
        &config.cache_dir,
        &cache_key,
        &groups,
        &index_to_hash,
      )
      .await;
      groups
    }
    Err(err) => {
      let _ = tx
        .send(PipelineEvent::GroupingFailed {
          error: format!("{err:#}"),
        })
        .await;
      vec![ProposedGroup {
        label: "All Files".to_string(),
        rationale: format!(
          "Semantic grouping failed ({err:#}), \
           falling back to single group"
        ),
        member_indices: summaries.iter().map(|s| s.index).collect(),
        member_destinations: vec![],
        member_notes: vec![],
      }]
    }
  };
  let groups = validate_and_report(groups, tx).await;
  Ok(add_unsorted(groups, &summaries, skipped, failed_notes))
}

/// Run label validation and tell the progress display what changed.
async fn validate_and_report(
  groups: Vec<ProposedGroup>,
  tx: &mpsc::Sender<PipelineEvent>,
) -> Vec<ProposedGroup> {
  let (groups, n) = crate::group::validate::validate_groups(groups);
  if n != crate::group::validate::Normalisation::default() {
    let _ = tx
      .send(PipelineEvent::LabelsNormalised {
        merged: n.merged,
        collapsed: n.collapsed,
        rewritten: n.rewritten,
      })
      .await;
  }
  groups
}

/// Label of the catch-all group for files the plan would otherwise
/// lose: omitted by the model, failed analysis, or never analyzed.
pub const UNSORTED_LABEL: &str = "Unsorted";

/// Guarantee every analyzed, failed, or skipped file appears in exactly
/// one group. Files the model left out join `Unsorted` with a per-file
/// note explaining why; empty groups are dropped.
fn add_unsorted(
  mut groups: Vec<ProposedGroup>,
  summaries: &[FileSummary],
  skipped: Vec<(usize, String)>,
  failed: Vec<(usize, String)>,
) -> Vec<ProposedGroup> {
  groups.retain(|g| !g.member_indices.is_empty());

  let placed: std::collections::HashSet<usize> = groups
    .iter()
    .flat_map(|g| g.member_indices.iter().copied())
    .collect();

  let mut notes: Vec<MemberNote> = summaries
    .iter()
    .filter(|s| !placed.contains(&s.index))
    .map(|s| MemberNote {
      index: s.index,
      note: "not placed by grouping".to_string(),
    })
    .collect();
  notes.extend(
    failed
      .into_iter()
      .chain(skipped)
      .map(|(index, note)| MemberNote { index, note }),
  );
  // Validation may already have produced an Unsorted group (type-word
  // labels); fold everything into one.
  if let Some(pos) =
    groups.iter().position(|g| g.label == UNSORTED_LABEL)
  {
    let existing = groups.remove(pos);
    notes.extend(existing.member_notes);
    for index in existing.member_indices {
      if !notes.iter().any(|n| n.index == index) {
        notes.push(MemberNote {
          index,
          note: "not placed by grouping".to_string(),
        });
      }
    }
  }
  if notes.is_empty() {
    return groups;
  }
  notes.sort_by_key(|n| n.index);
  notes.dedup_by_key(|n| n.index);

  let count = notes.len();
  groups.push(ProposedGroup {
    label: UNSORTED_LABEL.to_string(),
    rationale: format!(
      "{count} file(s) the plan could not place — see each file's note"
    ),
    member_indices: notes.iter().map(|n| n.index).collect(),
    member_destinations: vec![],
    member_notes: notes,
  });
  groups
}

/// One grouping call handles this many files well; beyond it the
/// prompt degrades, so batch by topic and merge on shared labels.
const MAX_GROUPING_BATCH: usize = 120;

/// Placements below this description confidence land in a visible
/// "Needs Review" group instead of silently joining a folder.
const MIN_PLACEMENT_CONFIDENCE: f64 = 0.6;

async fn propose_once<P: AiProvider>(
  provider: &P,
  summaries: &[FileSummary],
  existing_labels: &[String],
  organized_context: &[(String, Vec<ContentDescription>)],
  area: Option<&Area>,
) -> Result<Vec<ProposedGroup>> {
  // A garbled response can parse as groups with no members. Treat
  // "placed nothing" as a failed call and try once more before giving
  // up, so one bad generation doesn't unsort the whole run.
  for attempt in 0..2 {
    let groups = if let Some(area) = area {
      provider
        .propose_groups_in_area(
          summaries,
          area,
          existing_labels,
          organized_context,
        )
        .await?
    } else if organized_context.is_empty() {
      provider
        .propose_groups_with_context(summaries, existing_labels)
        .await?
    } else {
      provider
        .propose_groups_with_organized_context(
          summaries,
          existing_labels,
          organized_context,
        )
        .await?
    };
    let placed = groups.iter().any(|g| !g.member_indices.is_empty());
    if placed || summaries.is_empty() {
      return Ok(groups);
    }
    tracing::warn!(
      attempt = attempt + 1,
      files = summaries.len(),
      "Grouping response placed no files"
    );
  }
  anyhow::bail!(
    "grouping placed none of {} files after retry",
    summaries.len()
  )
}

/// Large sets are grouped in topic-coherent batches. Labels produced
/// by earlier batches feed later ones (so they reuse folders instead
/// of inventing near-duplicates), and same-label groups merge.
async fn propose_groups_batched<P: AiProvider>(
  provider: &P,
  summaries: &[FileSummary],
  existing_labels: &[String],
  organized_context: &[(String, Vec<ContentDescription>)],
  area: Option<&Area>,
) -> Result<Vec<ProposedGroup>> {
  if summaries.len() <= MAX_GROUPING_BATCH {
    return propose_once(
      provider,
      summaries,
      existing_labels,
      organized_context,
      area,
    )
    .await;
  }

  let mut ordered: Vec<FileSummary> = summaries.to_vec();
  ordered.sort_by(|a, b| {
    a.description
      .suggested_category
      .cmp(&b.description.suggested_category)
  });

  let mut labels: Vec<String> = existing_labels.to_vec();
  let mut order: Vec<String> = Vec::new();
  let mut merged: HashMap<String, ProposedGroup> = HashMap::new();

  for chunk in ordered.chunks(MAX_GROUPING_BATCH) {
    let groups =
      propose_once(provider, chunk, &labels, organized_context, area)
        .await?;
    for group in groups {
      if !labels.contains(&group.label) {
        labels.push(group.label.clone());
      }
      match merged.get_mut(&group.label) {
        Some(existing) => {
          existing.member_indices.extend(group.member_indices);
          existing
            .member_destinations
            .extend(group.member_destinations);
        }
        None => {
          order.push(group.label.clone());
          merged.insert(group.label.clone(), group);
        }
      }
    }
  }

  Ok(
    order
      .into_iter()
      .filter_map(|label| merged.remove(&label))
      .collect(),
  )
}

/// Files per routing call; short lines, so this stays well inside the
/// cheap model's comfort zone.
const ROUTE_BATCH: usize = 200;

/// Everything stage one needs besides the provider.
struct TwoStage<'a> {
  areas: &'a [Area],
  cache_dir: &'a std::path::Path,
  summary_hashes: &'a [[u8; 32]],
  hash_to_indices: &'a HashMap<String, Vec<usize>>,
  index_to_hash: &'a HashMap<usize, String>,
}

/// Stage one routes every file to a configured area (cached by content
/// set + taxonomy); stage two groups within each area so labels share
/// a top level and related files never straddle a batch boundary.
/// Routing failure or an empty taxonomy falls back to single-stage.
async fn propose_groups_two_stage<P: AiProvider>(
  provider: &P,
  summaries: &[FileSummary],
  existing_labels: &[String],
  organized_context: &[(String, Vec<ContentDescription>)],
  stage: &TwoStage<'_>,
  tx: &mpsc::Sender<PipelineEvent>,
) -> Result<Vec<ProposedGroup>> {
  if stage.areas.is_empty() || summaries.is_empty() {
    return propose_groups_batched(
      provider,
      summaries,
      existing_labels,
      organized_context,
      None,
    )
    .await;
  }

  let area_salt: Vec<String> = stage
    .areas
    .iter()
    .map(|a| format!("route:{}", a.name))
    .collect();
  let route_key = group_cache_key(stage.summary_hashes, &area_salt);
  let routed = match read_cached_routing(
    stage.cache_dir,
    &route_key,
    stage.hash_to_indices,
  )
  .await
  {
    Some(routed) => {
      tracing::info!("Reused cached routing (no Claude call)");
      routed
    }
    None => match route_all(provider, summaries, stage.areas).await {
      Ok(routed) => {
        let _ = write_cached_routing(
          stage.cache_dir,
          &route_key,
          &routed,
          stage.index_to_hash,
        )
        .await;
        routed
      }
      Err(err) => {
        tracing::warn!(
          error = %format!("{err:#}"),
          "Routing failed; grouping in a single stage"
        );
        return propose_groups_batched(
          provider,
          summaries,
          existing_labels,
          organized_context,
          None,
        )
        .await;
      }
    },
  };

  // Bucket summaries by area, in taxonomy order. Unknown or missing
  // areas are grouped afterwards without a constraint.
  let area_of: HashMap<usize, &Area> = routed
    .iter()
    .filter_map(|r| {
      let key = crate::eval::normalize_segment(&r.area);
      stage
        .areas
        .iter()
        .find(|a| crate::eval::normalize_segment(&a.name) == key)
        .map(|a| (r.index, a))
    })
    .collect();
  let mut buckets: Vec<(&Area, Vec<FileSummary>)> =
    stage.areas.iter().map(|a| (a, Vec::new())).collect();
  let mut unrouted: Vec<FileSummary> = Vec::new();
  for summary in summaries {
    match area_of.get(&summary.index) {
      Some(area) => {
        if let Some((_, files)) =
          buckets.iter_mut().find(|(a, _)| a.name == area.name)
        {
          files.push(summary.clone());
        }
      }
      None => unrouted.push(summary.clone()),
    }
  }
  let areas_used =
    buckets.iter().filter(|(_, f)| !f.is_empty()).count();
  let _ = tx
    .send(PipelineEvent::RoutingComplete {
      areas_used,
      unrouted: unrouted.len(),
    })
    .await;

  let mut labels: Vec<String> = existing_labels.to_vec();
  let mut all = Vec::new();
  for (area, files) in
    buckets.into_iter().filter(|(_, f)| !f.is_empty())
  {
    let groups = propose_groups_batched(
      provider,
      &files,
      &labels,
      organized_context,
      Some(area),
    )
    .await?;
    for mut group in groups {
      group.label = ensure_area_prefix(&group.label, area);
      if !labels.contains(&group.label) {
        labels.push(group.label.clone());
      }
      all.push(group);
    }
  }
  if !unrouted.is_empty() {
    let groups = propose_groups_batched(
      provider,
      &unrouted,
      &labels,
      organized_context,
      None,
    )
    .await?;
    all.extend(groups);
  }
  Ok(all)
}

async fn route_all<P: AiProvider>(
  provider: &P,
  summaries: &[FileSummary],
  areas: &[Area],
) -> Result<Vec<RoutedFile>> {
  let mut routed = Vec::with_capacity(summaries.len());
  for chunk in summaries.chunks(ROUTE_BATCH) {
    routed.extend(provider.route_files(chunk, areas).await?);
  }
  Ok(routed)
}

/// `Work/Acme` stays; `Acme` becomes `Work/Acme`; `work / acme` keeps
/// its spelling but is recognised as already prefixed.
fn ensure_area_prefix(label: &str, area: &Area) -> String {
  let first = label.split('/').next().unwrap_or("").trim();
  if crate::eval::normalize_segment(first)
    == crate::eval::normalize_segment(&area.name)
  {
    label.to_string()
  } else if label.trim().is_empty() {
    area.name.clone()
  } else {
    format!("{}/{}", area.name, label.trim())
  }
}

/// Pull low-confidence placements out of their groups into a visible
/// "Needs Review" bucket so shaky calls never silently file away.
fn quarantine_low_confidence(
  mut groups: Vec<ProposedGroup>,
  summaries: &[FileSummary],
) -> Vec<ProposedGroup> {
  let low: std::collections::HashSet<usize> = summaries
    .iter()
    .filter(|s| {
      s.description.source == DescriptionSource::Ai
        && s.description.confidence < MIN_PLACEMENT_CONFIDENCE
    })
    .map(|s| s.index)
    .collect();
  if low.is_empty() {
    return groups;
  }

  let mut quarantined = Vec::new();
  for group in &mut groups {
    group.member_indices.retain(|idx| {
      let keep = !low.contains(idx);
      if !keep {
        quarantined.push(*idx);
      }
      keep
    });
    group
      .member_destinations
      .retain(|dest| !low.contains(&dest.index));
  }
  groups.retain(|g| !g.member_indices.is_empty());

  if !quarantined.is_empty() {
    quarantined.sort_unstable();
    groups.push(ProposedGroup {
      label: "Needs Review".to_string(),
      rationale: "Content analysis was low-confidence — check \
                  these placements manually"
        .to_string(),
      member_indices: quarantined,
      member_destinations: vec![],
      member_notes: vec![],
    });
  }
  groups
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
        source: DescriptionSource::Ai,
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
        member_notes: vec![],
      }])
    }
  }

  /// Panics if any Claude method is called — used to prove a fully-cached
  /// run sends zero tokens.
  struct PanicProvider;

  impl AiProvider for PanicProvider {
    async fn describe_image(
      &self,
      _image_data: &[u8],
      _mime_type: &str,
      _context: &DescribeContext,
    ) -> anyhow::Result<ContentDescription> {
      panic!("describe_image called despite cached descriptions");
    }

    async fn propose_groups(
      &self,
      _files: &[FileSummary],
    ) -> anyhow::Result<Vec<ProposedGroup>> {
      panic!("propose_groups called despite cached grouping");
    }
  }

  fn summary(index: usize, confidence: f64) -> FileSummary {
    FileSummary {
      index,
      filename: format!("f{index}.jpg"),
      source_path: format!("f{index}.jpg"),
      description: ContentDescription {
        summary: format!("file {index}"),
        tags: vec![],
        suggested_category: "photo".to_string(),
        confidence,
        source: DescriptionSource::Ai,
      },
      metadata_hint: String::new(),
    }
  }

  #[test]
  fn quarantine_moves_low_confidence_to_needs_review() {
    let groups = vec![ProposedGroup {
      label: "Beach".to_string(),
      rationale: "sandy".to_string(),
      member_indices: vec![0, 1, 2],
      member_destinations: vec![],
      member_notes: vec![],
    }];
    let summaries =
      vec![summary(0, 0.9), summary(1, 0.3), summary(2, 0.95)];

    let result = quarantine_low_confidence(groups, &summaries);

    assert_eq!(result.len(), 2);
    assert_eq!(result[0].member_indices, vec![0, 2]);
    assert_eq!(result[1].label, "Needs Review");
    assert_eq!(result[1].member_indices, vec![1]);
  }

  #[test]
  fn quarantine_noop_when_all_confident() {
    let groups = vec![ProposedGroup {
      label: "Beach".to_string(),
      rationale: "sandy".to_string(),
      member_indices: vec![0],
      member_destinations: vec![],
      member_notes: vec![],
    }];

    let result =
      quarantine_low_confidence(groups.clone(), &[summary(0, 0.9)]);

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].member_indices, vec![0]);
  }

  #[tokio::test]
  async fn batched_grouping_merges_same_labels_across_chunks() {
    /// Groups every chunk under one shared label, so a >1-batch run
    /// must merge them into a single group.
    struct OneLabelProvider;
    impl AiProvider for OneLabelProvider {
      async fn describe_image(
        &self,
        _: &[u8],
        _: &str,
        _: &DescribeContext,
      ) -> anyhow::Result<ContentDescription> {
        panic!("unused");
      }
      async fn propose_groups(
        &self,
        files: &[FileSummary],
      ) -> anyhow::Result<Vec<ProposedGroup>> {
        Ok(vec![ProposedGroup {
          label: "Everything".to_string(),
          rationale: "one bucket".to_string(),
          member_indices: files.iter().map(|f| f.index).collect(),
          member_destinations: vec![],
          member_notes: vec![],
        }])
      }
    }

    let summaries: Vec<FileSummary> = (0..MAX_GROUPING_BATCH * 2 + 5)
      .map(|i| summary(i, 0.9))
      .collect();

    let groups = propose_groups_batched(
      &OneLabelProvider,
      &summaries,
      &[],
      &[],
      None,
    )
    .await
    .unwrap();

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].member_indices.len(), summaries.len());
  }

  #[tokio::test]
  async fn second_run_makes_no_claude_calls_when_unchanged() {
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

    let mk = || PipelineConfig {
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
      use_batch_api: false,
      introspect_archives: false,
      max_archive_files: 20,
      max_archive_file_size_mb: 50,
      use_organized_context: false,
      ledger_path: None,
      model: "claude-opus-5".to_string(),
      describe_model: "claude-haiku-4-5".to_string(),
      taxonomy: vec![],
    };

    // First run populates both the description and grouping caches.
    let (tx1, _rx1) = mpsc::channel(64);
    let first = run(&FakeProvider, &mk(), tx1).await.unwrap();
    assert!(!first.plan.groups.is_empty());

    // Second run over the unchanged set must not call Claude at all.
    let (tx2, _rx2) = mpsc::channel(64);
    let second = run(&PanicProvider, &mk(), tx2).await.unwrap();

    assert_eq!(second.plan.groups.len(), first.plan.groups.len());
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

  fn ledger_test_config(
    source: &std::path::Path,
    output: &std::path::Path,
    cache: &std::path::Path,
    ledger_path: Option<PathBuf>,
  ) -> PipelineConfig {
    PipelineConfig {
      target_dirs: vec![source.to_path_buf()],
      output_dir: output.to_path_buf(),
      no_ai: true,
      max_files: 500,
      max_file_size_mb: 100,
      max_cost: None,
      near_duplicate_threshold: 8,
      cache_dir: cache.to_path_buf(),
      max_concurrent: 2,
      include_trash: false,
      type_filter: vec![],
      use_batch_api: false,
      introspect_archives: false,
      max_archive_files: 20,
      max_archive_file_size_mb: 50,
      use_organized_context: false,
      ledger_path,
      model: "claude-opus-5".to_string(),
      describe_model: "claude-haiku-4-5".to_string(),
      taxonomy: crate::model::default_areas(),
    }
  }

  #[tokio::test]
  async fn pipeline_excludes_already_organized_files() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let ledger_path = output.path().join("ledger.json");

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

    // First pass with no ledger to learn the exact scanned path + hash.
    let cfg1 = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    let (tx1, _rx1) = mpsc::channel(64);
    let first = run(&FakeProvider, &cfg1, tx1).await.unwrap();
    let a = first
      .fingerprinted
      .iter()
      .find(|f| f.scanned.path.ends_with("a.png"))
      .unwrap();

    // Record a.png as already organized, then run again with the ledger.
    let mut ledger = Ledger::default();
    ledger.record(crate::ledger::LedgerEntry {
      source_path: a.scanned.path.clone(),
      dest_path: output.path().join("old/a.png"),
      blake3_hex: crate::ledger::hash_hex(&a.blake3_hash),
      group_label: "Old".to_string(),
      organized_at: "2026-01-01T00:00:00Z".to_string(),
    });
    ledger.save(&ledger_path).unwrap();

    let cfg2 = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      Some(ledger_path),
    );
    let (tx2, _rx2) = mpsc::channel(64);
    let second = run(&FakeProvider, &cfg2, tx2).await.unwrap();

    assert_eq!(second.fingerprinted.len(), 1);
    assert!(second.fingerprinted[0].scanned.path.ends_with("b.png"));
    assert!(second.organized_duplicates.is_empty());
  }

  #[tokio::test]
  async fn pipeline_returns_empty_plan_when_all_organized() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let ledger_path = output.path().join("ledger.json");

    fs::write(
      source.path().join("a.png"),
      create_test_png(255, 0, 0),
    )
    .unwrap();

    let cfg1 = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    let (tx1, _rx1) = mpsc::channel(64);
    let first = run(&FakeProvider, &cfg1, tx1).await.unwrap();

    let mut ledger = Ledger::default();
    for f in &first.fingerprinted {
      ledger.record(crate::ledger::LedgerEntry {
        source_path: f.scanned.path.clone(),
        dest_path: output.path().join("old/x.png"),
        blake3_hex: crate::ledger::hash_hex(&f.blake3_hash),
        group_label: "Old".to_string(),
        organized_at: "2026-01-01T00:00:00Z".to_string(),
      });
    }
    ledger.save(&ledger_path).unwrap();

    let cfg2 = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      Some(ledger_path),
    );
    let (tx2, _rx2) = mpsc::channel(64);
    // Must be Ok with an empty plan — "nothing to do" is success, not error.
    let second = run(&FakeProvider, &cfg2, tx2).await.unwrap();

    assert!(second.fingerprinted.is_empty());
    assert!(second.plan.groups.is_empty());
    assert!(second.plan.moves.is_empty());
  }

  #[tokio::test]
  async fn pipeline_flags_new_file_identical_to_organized() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let ledger_path = output.path().join("ledger.json");

    fs::write(
      source.path().join("a.png"),
      create_test_png(255, 0, 0),
    )
    .unwrap();

    let cfg1 = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    let (tx1, _rx1) = mpsc::channel(64);
    let first = run(&FakeProvider, &cfg1, tx1).await.unwrap();
    let a = &first.fingerprinted[0];

    // Same content, but organized under a DIFFERENT source path: it is not
    // excluded (path differs) but must be flagged as a duplicate.
    let mut ledger = Ledger::default();
    ledger.record(crate::ledger::LedgerEntry {
      source_path: PathBuf::from("/somewhere/else/original.png"),
      dest_path: output.path().join("old/original.png"),
      blake3_hex: crate::ledger::hash_hex(&a.blake3_hash),
      group_label: "Old".to_string(),
      organized_at: "2026-01-01T00:00:00Z".to_string(),
    });
    ledger.save(&ledger_path).unwrap();

    let cfg2 = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      Some(ledger_path),
    );
    let (tx2, _rx2) = mpsc::channel(64);
    let second = run(&FakeProvider, &cfg2, tx2).await.unwrap();

    assert_eq!(second.fingerprinted.len(), 1);
    assert_eq!(second.organized_duplicates.len(), 1);
    assert!(second.organized_duplicates[0]
      .organized_at
      .ends_with("old/original.png"));
  }

  #[tokio::test]
  async fn cached_grouping_keeps_both_copies_of_an_exact_duplicate() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let png = create_test_png(10, 20, 30);
    fs::write(source.path().join("a.png"), &png).unwrap();
    fs::write(source.path().join("a (1).png"), &png).unwrap();
    fs::write(
      source.path().join("b.png"),
      create_test_png(200, 0, 0),
    )
    .unwrap();
    let mut config = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    config.no_ai = false;

    // First run populates the description and grouping caches.
    let (tx, _rx) = mpsc::channel(64);
    let first = run(&FakeProvider, &config, tx).await.unwrap();
    // Second run replays the cached grouping.
    let (tx, _rx) = mpsc::channel(64);
    let second = run(&PanicProvider, &config, tx).await.unwrap();

    for result in [&first, &second] {
      let mut placed: Vec<usize> = result
        .plan
        .groups
        .iter()
        .flat_map(|g| g.members.iter().copied())
        .collect();
      placed.sort_unstable();
      assert_eq!(
        placed,
        vec![0, 1, 2],
        "every file placed exactly once"
      );
    }
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
      use_batch_api: false,
      introspect_archives: false,
      max_archive_files: 20,
      max_archive_file_size_mb: 50,
      use_organized_context: false,
      ledger_path: None,
      model: "claude-opus-5".to_string(),
      describe_model: "claude-haiku-4-5".to_string(),
      taxonomy: vec![],
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
      use_batch_api: false,
      introspect_archives: false,
      max_archive_files: 20,
      max_archive_file_size_mb: 50,
      use_organized_context: false,
      ledger_path: None,
      model: "claude-opus-5".to_string(),
      describe_model: "claude-haiku-4-5".to_string(),
      taxonomy: vec![],
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

  /// Describes every file except `skip` and always fails grouping,
  /// so the fallback group must map summary positions back to real
  /// file indices.
  struct SkipOneNoGroupProvider {
    skip: String,
  }

  impl AiProvider for SkipOneNoGroupProvider {
    async fn describe_image(
      &self,
      _image_data: &[u8],
      _mime_type: &str,
      context: &DescribeContext,
    ) -> anyhow::Result<ContentDescription> {
      if context.filename == self.skip {
        anyhow::bail!("simulated analysis failure");
      }
      Ok(ContentDescription {
        summary: format!("Description of {}", context.filename),
        tags: vec![],
        suggested_category: "photo".to_string(),
        confidence: 0.9,
        source: DescriptionSource::Ai,
      })
    }

    async fn propose_groups(
      &self,
      _files: &[FileSummary],
    ) -> anyhow::Result<Vec<ProposedGroup>> {
      anyhow::bail!("simulated grouping failure")
    }
  }

  #[tokio::test]
  async fn grouping_fallback_uses_file_indices_not_summary_positions()
  {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    for (name, rgb) in [
      ("a.png", (255, 0, 0)),
      ("b.png", (0, 255, 0)),
      ("c.png", (0, 0, 255)),
    ] {
      fs::write(
        source.path().join(name),
        create_test_png(rgb.0, rgb.1, rgb.2),
      )
      .unwrap();
    }
    // Fail whichever file the scanner yields first, so every summary
    // position is offset by one from its file index.
    let first = crate::scanner::scan_directory(source.path())
      .unwrap()[0]
      .path
      .file_name()
      .unwrap()
      .to_string_lossy()
      .to_string();
    let provider = SkipOneNoGroupProvider {
      skip: first.clone(),
    };
    let mut config = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    config.no_ai = false;

    let (tx, _rx) = mpsc::channel(64);
    let result = run(&provider, &config, tx).await.unwrap();

    let analyzed: Vec<usize> = result
      .fingerprinted
      .iter()
      .enumerate()
      .filter(|(_, f)| {
        f.scanned.path.file_name().unwrap().to_string_lossy() != first
      })
      .map(|(i, _)| i)
      .collect();
    let all_files = result
      .plan
      .groups
      .iter()
      .find(|g| g.label == "All Files")
      .expect("fallback group");
    let mut members = all_files.members.clone();
    members.sort_unstable();
    assert_eq!(members, analyzed);
    assert_every_file_placed_once(&result);
  }

  fn unsorted_group(
    result: &PipelineResult,
  ) -> &crate::model::FileGroup {
    result
      .plan
      .groups
      .iter()
      .find(|g| g.label == UNSORTED_LABEL)
      .expect("an Unsorted group")
  }

  fn assert_every_file_placed_once(result: &PipelineResult) {
    let mut placed: Vec<usize> = result
      .plan
      .groups
      .iter()
      .flat_map(|g| g.members.iter().copied())
      .collect();
    placed.sort_unstable();
    let expected: Vec<usize> =
      (0..result.fingerprinted.len()).collect();
    assert_eq!(placed, expected, "every file placed exactly once");
  }

  fn note_for(group: &crate::model::FileGroup, index: usize) -> &str {
    group
      .member_notes
      .iter()
      .find(|n| n.index == index)
      .map(|n| n.note.as_str())
      .unwrap_or_else(|| panic!("no note for index {index}"))
  }

  fn write_three_pngs(dir: &std::path::Path) {
    for (name, rgb) in [
      ("keep_a.png", (255, 0, 0)),
      ("keep_b.png", (0, 255, 0)),
      ("other.png", (0, 0, 255)),
    ] {
      fs::write(dir.join(name), create_test_png(rgb.0, rgb.1, rgb.2))
        .unwrap();
    }
  }

  /// Groups only files whose name starts with `keep`; omits the rest,
  /// as the real model is allowed to.
  struct PartialGroupProvider;

  impl AiProvider for PartialGroupProvider {
    async fn describe_image(
      &self,
      _image_data: &[u8],
      _mime_type: &str,
      context: &DescribeContext,
    ) -> anyhow::Result<ContentDescription> {
      Ok(ContentDescription {
        summary: format!("Description of {}", context.filename),
        tags: vec![],
        suggested_category: "photo".to_string(),
        confidence: 0.9,
        source: DescriptionSource::Ai,
      })
    }

    async fn propose_groups(
      &self,
      files: &[FileSummary],
    ) -> anyhow::Result<Vec<ProposedGroup>> {
      Ok(vec![ProposedGroup {
        label: "Kept".to_string(),
        rationale: "keepers".to_string(),
        member_indices: files
          .iter()
          .filter(|f| f.filename.starts_with("keep"))
          .map(|f| f.index)
          .collect(),
        member_destinations: vec![],
        member_notes: vec![],
      }])
    }
  }

  /// Returns member-less groups for the first `empty_calls` grouping
  /// calls, then a proper grouping. Mimics a garbled model response.
  struct FlakyGroupProvider {
    empty_calls: usize,
    calls: std::sync::atomic::AtomicUsize,
  }

  impl AiProvider for FlakyGroupProvider {
    async fn describe_image(
      &self,
      _image_data: &[u8],
      _mime_type: &str,
      context: &DescribeContext,
    ) -> anyhow::Result<ContentDescription> {
      Ok(ContentDescription {
        summary: format!("Description of {}", context.filename),
        tags: vec![],
        suggested_category: "photo".to_string(),
        confidence: 0.9,
        source: DescriptionSource::Ai,
      })
    }

    async fn propose_groups(
      &self,
      files: &[FileSummary],
    ) -> anyhow::Result<Vec<ProposedGroup>> {
      let n =
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
      let members = if n < self.empty_calls {
        vec![]
      } else {
        files.iter().map(|f| f.index).collect()
      };
      Ok(vec![ProposedGroup {
        label: "Everything".to_string(),
        rationale: String::new(),
        member_indices: members,
        member_destinations: vec![],
        member_notes: vec![],
      }])
    }
  }

  #[tokio::test]
  async fn grouping_with_no_placements_is_retried_once() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    write_three_pngs(source.path());
    let provider = FlakyGroupProvider {
      empty_calls: 1,
      calls: Default::default(),
    };
    let mut config = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    config.no_ai = false;

    let (tx, _rx) = mpsc::channel(64);
    let result = run(&provider, &config, tx).await.unwrap();

    assert_every_file_placed_once(&result);
    assert_eq!(result.plan.groups.len(), 1);
    assert_eq!(result.plan.groups[0].label, "Everything");
    assert_eq!(result.plan.groups[0].members.len(), 3);
  }

  #[tokio::test]
  async fn grouping_with_no_placements_twice_falls_back() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    write_three_pngs(source.path());
    let provider = FlakyGroupProvider {
      empty_calls: 5,
      calls: Default::default(),
    };
    let mut config = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    config.no_ai = false;

    let (tx, mut rx) = mpsc::channel(64);
    let result = run(&provider, &config, tx).await.unwrap();

    assert_every_file_placed_once(&result);
    assert_eq!(result.plan.groups.len(), 1);
    assert_eq!(result.plan.groups[0].label, "All Files");
    let mut failed = false;
    while let Ok(ev) = rx.try_recv() {
      if matches!(ev, PipelineEvent::GroupingFailed { .. }) {
        failed = true;
      }
    }
    assert!(failed, "GroupingFailed event expected");
    // Exactly one retry: two calls total.
    assert_eq!(
      provider.calls.load(std::sync::atomic::Ordering::SeqCst),
      2
    );
  }

  /// Returns whatever labels it is constructed with, one file each in
  /// order, cycling when there are more files than labels.
  struct LabelProvider {
    labels: Vec<&'static str>,
  }

  impl AiProvider for LabelProvider {
    async fn describe_image(
      &self,
      _image_data: &[u8],
      _mime_type: &str,
      context: &DescribeContext,
    ) -> anyhow::Result<ContentDescription> {
      Ok(ContentDescription {
        summary: format!("Description of {}", context.filename),
        tags: vec![],
        suggested_category: "photo".to_string(),
        confidence: 0.9,
        source: DescriptionSource::Ai,
      })
    }

    async fn propose_groups(
      &self,
      files: &[FileSummary],
    ) -> anyhow::Result<Vec<ProposedGroup>> {
      Ok(
        self
          .labels
          .iter()
          .enumerate()
          .map(|(i, label)| ProposedGroup {
            label: label.to_string(),
            rationale: String::new(),
            member_indices: files
              .iter()
              .enumerate()
              .filter(|(j, _)| j % self.labels.len() == i)
              .map(|(_, f)| f.index)
              .collect(),
            member_destinations: vec![],
            member_notes: vec![],
          })
          .collect(),
      )
    }
  }

  #[tokio::test]
  async fn labels_are_validated_and_the_event_reports_it() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    write_three_pngs(source.path());
    let provider = LabelProvider {
      labels: vec!["Work/Acme Corp/PDFs", "work / acme-corp"],
    };
    let mut config = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    config.no_ai = false;
    // LabelProvider cannot route; single-stage keeps the raw labels.
    config.taxonomy = vec![];

    let (tx, mut rx) = mpsc::channel(64);
    let result = run(&provider, &config, tx).await.unwrap();

    assert_every_file_placed_once(&result);
    let labels: Vec<&str> = result
      .plan
      .groups
      .iter()
      .map(|g| g.label.as_str())
      .collect();
    assert_eq!(labels, vec!["Work/Acme Corp"]);
    assert_eq!(result.plan.groups[0].members.len(), 3);

    let mut normalised = None;
    while let Ok(ev) = rx.try_recv() {
      if let PipelineEvent::LabelsNormalised {
        merged,
        collapsed,
        rewritten,
      } = ev
      {
        normalised = Some((merged, collapsed, rewritten));
      }
    }
    assert_eq!(normalised, Some((1, 0, 2)));
  }

  #[tokio::test]
  async fn type_word_only_labels_join_unsorted_with_a_note() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    write_three_pngs(source.path());
    // "Misc" carries no subject; "Kept" places only two files, so the
    // third is unplaced. Both must land in ONE Unsorted group.
    struct MiscProvider;
    impl AiProvider for MiscProvider {
      async fn describe_image(
        &self,
        _: &[u8],
        _: &str,
        context: &DescribeContext,
      ) -> anyhow::Result<ContentDescription> {
        Ok(ContentDescription {
          summary: format!("Description of {}", context.filename),
          tags: vec![],
          suggested_category: "photo".to_string(),
          confidence: 0.9,
          source: DescriptionSource::Ai,
        })
      }
      async fn propose_groups(
        &self,
        files: &[FileSummary],
      ) -> anyhow::Result<Vec<ProposedGroup>> {
        let mut idx: Vec<usize> =
          files.iter().map(|f| f.index).collect();
        idx.sort_unstable();
        Ok(vec![
          ProposedGroup {
            label: "Misc".to_string(),
            rationale: String::new(),
            member_indices: vec![idx[0]],
            member_destinations: vec![],
            member_notes: vec![],
          },
          ProposedGroup {
            label: "Kept".to_string(),
            rationale: String::new(),
            member_indices: vec![idx[1]],
            member_destinations: vec![],
            member_notes: vec![],
          },
        ])
      }
    }
    let mut config = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    config.no_ai = false;
    config.taxonomy = vec![];

    let (tx, _rx) = mpsc::channel(64);
    let result = run(&MiscProvider, &config, tx).await.unwrap();

    assert_every_file_placed_once(&result);
    let unsorted: Vec<&crate::model::FileGroup> = result
      .plan
      .groups
      .iter()
      .filter(|g| g.label == UNSORTED_LABEL)
      .collect();
    assert_eq!(unsorted.len(), 1, "exactly one Unsorted group");
    assert_eq!(unsorted[0].members.len(), 2);
    let notes: Vec<&str> = unsorted[0]
      .member_notes
      .iter()
      .map(|n| n.note.as_str())
      .collect();
    assert!(notes.contains(&crate::group::validate::TYPE_ONLY_NOTE));
    assert!(notes.contains(&"not placed by grouping"));
  }

  /// Routes `keep*` files to Work and everything else to Personal,
  /// then groups whatever it is given into one "Stuff" group. Counts
  /// calls so cache behaviour can be asserted.
  struct RoutingProvider {
    route_calls: std::sync::atomic::AtomicUsize,
    group_calls: std::sync::atomic::AtomicUsize,
    route_result: RouteBehaviour,
  }

  enum RouteBehaviour {
    ByName,
    Fail,
    Unknown,
  }

  impl RoutingProvider {
    fn new(route_result: RouteBehaviour) -> Self {
      Self {
        route_calls: Default::default(),
        group_calls: Default::default(),
        route_result,
      }
    }
    fn calls(&self) -> (usize, usize) {
      use std::sync::atomic::Ordering::SeqCst;
      (self.route_calls.load(SeqCst), self.group_calls.load(SeqCst))
    }
  }

  impl AiProvider for RoutingProvider {
    async fn describe_image(
      &self,
      _image_data: &[u8],
      _mime_type: &str,
      context: &DescribeContext,
    ) -> anyhow::Result<ContentDescription> {
      Ok(ContentDescription {
        summary: format!("Description of {}", context.filename),
        tags: vec![],
        suggested_category: "photo".to_string(),
        confidence: 0.9,
        source: DescriptionSource::Ai,
      })
    }

    async fn route_files(
      &self,
      files: &[FileSummary],
      _areas: &[Area],
    ) -> anyhow::Result<Vec<RoutedFile>> {
      self
        .route_calls
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
      match self.route_result {
        RouteBehaviour::Fail => anyhow::bail!("routing exploded"),
        RouteBehaviour::Unknown => Ok(
          files
            .iter()
            .map(|f| RoutedFile {
              index: f.index,
              area: "Bogus".to_string(),
            })
            .collect(),
        ),
        RouteBehaviour::ByName => Ok(
          files
            .iter()
            .map(|f| RoutedFile {
              index: f.index,
              area: if f.filename.starts_with("keep") {
                "Work".to_string()
              } else {
                "Personal".to_string()
              },
            })
            .collect(),
        ),
      }
    }

    async fn propose_groups(
      &self,
      files: &[FileSummary],
    ) -> anyhow::Result<Vec<ProposedGroup>> {
      self
        .group_calls
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
      Ok(vec![ProposedGroup {
        label: "Stuff".to_string(),
        rationale: String::new(),
        member_indices: files.iter().map(|f| f.index).collect(),
        member_destinations: vec![],
        member_notes: vec![],
      }])
    }
  }

  fn labels_of(result: &PipelineResult) -> Vec<String> {
    let mut v: Vec<String> =
      result.plan.groups.iter().map(|g| g.label.clone()).collect();
    v.sort();
    v
  }

  #[tokio::test]
  async fn two_stage_grouping_prefixes_labels_with_the_routed_area() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    write_three_pngs(source.path());
    let provider = RoutingProvider::new(RouteBehaviour::ByName);
    let mut config = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    config.no_ai = false;

    let (tx, mut rx) = mpsc::channel(64);
    let result = run(&provider, &config, tx).await.unwrap();

    assert_every_file_placed_once(&result);
    // "Personal/Stuff" holds one file, so validation folds it to the
    // area itself.
    assert_eq!(labels_of(&result), vec!["Personal", "Work/Stuff"]);
    let work = result
      .plan
      .groups
      .iter()
      .find(|g| g.label == "Work/Stuff")
      .unwrap();
    assert_eq!(work.members.len(), 2);
    // One routing call, one grouping call per area.
    assert_eq!(provider.calls(), (1, 2));

    let mut routed = None;
    while let Ok(ev) = rx.try_recv() {
      if let PipelineEvent::RoutingComplete {
        areas_used,
        unrouted,
      } = ev
      {
        routed = Some((areas_used, unrouted));
      }
    }
    assert_eq!(routed, Some((2, 0)));
  }

  #[tokio::test]
  async fn routing_failure_falls_back_to_single_stage() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    write_three_pngs(source.path());
    let provider = RoutingProvider::new(RouteBehaviour::Fail);
    let mut config = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    config.no_ai = false;

    let (tx, _rx) = mpsc::channel(64);
    let result = run(&provider, &config, tx).await.unwrap();

    assert_every_file_placed_once(&result);
    assert_eq!(labels_of(&result), vec!["Stuff"]);
    assert_eq!(provider.calls(), (1, 1));
  }

  #[tokio::test]
  async fn unknown_area_names_are_grouped_without_a_constraint() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    write_three_pngs(source.path());
    let provider = RoutingProvider::new(RouteBehaviour::Unknown);
    let mut config = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    config.no_ai = false;

    let (tx, mut rx) = mpsc::channel(64);
    let result = run(&provider, &config, tx).await.unwrap();

    assert_every_file_placed_once(&result);
    assert_eq!(labels_of(&result), vec!["Stuff"]);
    let mut routed = None;
    while let Ok(ev) = rx.try_recv() {
      if let PipelineEvent::RoutingComplete {
        areas_used,
        unrouted,
      } = ev
      {
        routed = Some((areas_used, unrouted));
      }
    }
    assert_eq!(routed, Some((0, 3)));
  }

  #[tokio::test]
  async fn second_run_reuses_cached_routing_and_grouping() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    write_three_pngs(source.path());
    let mut config = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    config.no_ai = false;

    let first = RoutingProvider::new(RouteBehaviour::ByName);
    let (tx, _rx) = mpsc::channel(64);
    let a = run(&first, &config, tx).await.unwrap();
    assert_eq!(first.calls(), (1, 2));

    let second = RoutingProvider::new(RouteBehaviour::Fail);
    let (tx, _rx) = mpsc::channel(64);
    let b = run(&second, &config, tx).await.unwrap();
    assert_eq!(second.calls(), (0, 0), "everything came from cache");
    assert_eq!(labels_of(&a), labels_of(&b));
  }

  #[tokio::test]
  async fn empty_taxonomy_groups_in_a_single_stage() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    write_three_pngs(source.path());
    let provider = RoutingProvider::new(RouteBehaviour::ByName);
    let mut config = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    config.no_ai = false;
    config.taxonomy = vec![];

    let (tx, _rx) = mpsc::channel(64);
    let result = run(&provider, &config, tx).await.unwrap();

    assert_eq!(labels_of(&result), vec!["Stuff"]);
    assert_eq!(provider.calls(), (0, 1));
  }

  #[test]
  fn quarantine_ignores_non_ai_descriptions() {
    let mut fallback = summary(1, 0.5);
    fallback.description.source = DescriptionSource::Filename;
    let mut unanalyzed = summary(2, 0.0);
    unanalyzed.description.source = DescriptionSource::Unanalyzed;
    let summaries = vec![summary(0, 0.3), fallback, unanalyzed];
    let groups = vec![ProposedGroup {
      label: "Beach".to_string(),
      rationale: String::new(),
      member_indices: vec![0, 1, 2],
      member_destinations: vec![],
      member_notes: vec![],
    }];

    let out = quarantine_low_confidence(groups, &summaries);

    assert_eq!(out.len(), 2);
    assert_eq!(out[0].member_indices, vec![1, 2]);
    assert_eq!(out[1].label, "Needs Review");
    assert_eq!(out[1].member_indices, vec![0]);
  }

  #[tokio::test]
  async fn unplaced_files_land_in_unsorted_with_notes() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    write_three_pngs(source.path());
    let mut config = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    config.no_ai = false;

    let (tx, mut rx) = mpsc::channel(64);
    let result =
      run(&PartialGroupProvider, &config, tx).await.unwrap();

    assert_every_file_placed_once(&result);
    let other = result
      .fingerprinted
      .iter()
      .position(|f| f.scanned.path.ends_with("other.png"))
      .unwrap();
    let unsorted = unsorted_group(&result);
    assert_eq!(unsorted.members, vec![other]);
    assert_eq!(note_for(unsorted, other), "not placed by grouping");

    let mut unsorted_count = None;
    while let Ok(ev) = rx.try_recv() {
      if let PipelineEvent::GroupingComplete { unsorted, .. } = ev {
        unsorted_count = Some(unsorted);
      }
    }
    assert_eq!(unsorted_count, Some(1));
  }

  #[tokio::test]
  async fn analysis_failures_land_in_unsorted_with_the_error() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    write_three_pngs(source.path());
    let provider = SkipOneNoGroupProvider {
      skip: "other.png".to_string(),
    };
    let mut config = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    config.no_ai = false;

    let (tx, _rx) = mpsc::channel(64);
    let result = run(&provider, &config, tx).await.unwrap();

    assert_every_file_placed_once(&result);
    let other = result
      .fingerprinted
      .iter()
      .position(|f| f.scanned.path.ends_with("other.png"))
      .unwrap();
    let unsorted = unsorted_group(&result);
    assert_eq!(unsorted.members, vec![other]);
    assert!(
      note_for(unsorted, other)
        .contains("simulated analysis failure"),
      "note: {}",
      note_for(unsorted, other)
    );
  }

  #[tokio::test]
  async fn files_beyond_max_files_land_in_unsorted() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    write_three_pngs(source.path());
    let mut config = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    config.no_ai = false;
    config.max_files = 1;

    let (tx, _rx) = mpsc::channel(64);
    let result = run(&FakeProvider, &config, tx).await.unwrap();

    assert_every_file_placed_once(&result);
    let unsorted = unsorted_group(&result);
    assert_eq!(unsorted.members.len(), 2);
    for &idx in &unsorted.members {
      assert_eq!(
        note_for(unsorted, idx),
        "not analyzed: beyond --max-files (1)"
      );
    }
  }

  #[tokio::test]
  async fn oversized_files_land_in_unsorted() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    write_three_pngs(source.path());
    let mut config = ledger_test_config(
      source.path(),
      output.path(),
      cache.path(),
      None,
    );
    config.no_ai = false;
    config.max_file_size_mb = 0;

    let (tx, _rx) = mpsc::channel(64);
    let result = run(&FakeProvider, &config, tx).await.unwrap();

    assert_every_file_placed_once(&result);
    let unsorted = unsorted_group(&result);
    assert_eq!(unsorted.members.len(), 3);
    for &idx in &unsorted.members {
      assert_eq!(
        note_for(unsorted, idx),
        "not analyzed: larger than 0 MB"
      );
    }
    // No empty real group survives.
    assert!(result.plan.groups.iter().all(|g| !g.members.is_empty()));
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
      use_batch_api: false,
      introspect_archives: false,
      max_archive_files: 20,
      max_archive_file_size_mb: 50,
      use_organized_context: false,
      ledger_path: None,
      model: "claude-opus-5".to_string(),
      describe_model: "claude-haiku-4-5".to_string(),
      taxonomy: vec![],
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
      use_batch_api: false,
      introspect_archives: false,
      max_archive_files: 20,
      max_archive_file_size_mb: 50,
      use_organized_context: false,
      ledger_path: None,
      model: "claude-opus-5".to_string(),
      describe_model: "claude-haiku-4-5".to_string(),
      taxonomy: vec![],
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
      use_batch_api: false,
      introspect_archives: false,
      max_archive_files: 20,
      max_archive_file_size_mb: 50,
      use_organized_context: false,
      ledger_path: None,
      model: "claude-opus-5".to_string(),
      describe_model: "claude-haiku-4-5".to_string(),
      taxonomy: vec![],
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

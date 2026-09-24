use std::collections::HashMap;
use std::path::Path;

use std::io::IsTerminal;

use anyhow::Result;
use clap::Parser;
use dotenvy::dotenv;
use tracing_subscriber::EnvFilter;

use spindle::ai::ClaudeProvider;
use spindle::config::{CliArgs, Config};
use spindle::executor::{
  execute_plan, journal, trash, ExecutorPaths,
};
use spindle::fingerprint::{
  find_exact_duplicates, fingerprint_files,
};
use spindle::ledger::{Ledger, LedgerEntry};
use spindle::model::{
  ApprovedPlan, ExecutionReport, FileGroup, FileMove,
  FingerprintedFile,
};
use spindle::pipeline::{self, PipelineConfig, PipelineEvent};
use spindle::progress::{self, PipelineProgress};
use spindle::scanner::{scan_directories_filtered, ScanOptions};
use spindle::tui::{
  self, ReviewAction, ReviewMode, ReviewState, TerminalSession,
};

#[tokio::main]
async fn main() -> Result<()> {
  dotenv().ok();
  let cli = CliArgs::parse();

  let filter = match cli.verbose {
    0 => "spindle=info",
    1 => "spindle=debug",
    _ => "spindle=trace",
  };
  tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::new(filter))
    .init();

  let config = Config::load(&cli)?;
  tracing::debug!(?config, "Loaded configuration");

  if cli.list_undo {
    return run_list_undo();
  }

  if cli.purge {
    return run_purge(cli.older_than);
  }

  if cli.undo || cli.undo_run.is_some() || cli.undo_log.is_some() {
    return run_undo(&cli);
  }

  if cli.dupes_only {
    return run_dupes_only(&cli, &config);
  }

  // The API key is only needed when the AI pipeline will actually run.
  let api_key = if cli.no_ai {
    String::new()
  } else {
    config.api_key()?.to_string()
  };
  let mut provider = ClaudeProvider::new(
    api_key,
    config.ai.model.clone(),
    config.ai.max_retries,
  )
  .with_describe_model(config.ai.describe_model.clone());
  if let Some(base_url) = &config.ai.base_url {
    provider = provider.with_base_url(base_url.clone());
  }

  let ledger_path = spindle::config::resolve_ledger_path(&cli);
  let corrections_path =
    spindle::config::resolve_corrections_path(&cli);

  let pipeline_config = PipelineConfig {
    target_dirs: config.general.target_dirs.clone(),
    output_dir: config.general.output_dir.clone(),
    no_ai: cli.no_ai,
    max_files: config.ai.max_files_to_analyze,
    max_file_size_mb: config.ai.skip_files_larger_than_mb,
    max_cost: cli.max_cost,
    near_duplicate_threshold: config
      .duplicates
      .near_duplicate_threshold,
    cache_dir: default_cache_dir(),
    max_concurrent: config.ai.max_concurrent_requests,
    include_trash: cli.include_trash,
    type_filter: cli.file_types.clone(),
    use_batch_api: cli.batch,
    introspect_archives: config.matching.introspect_archives,
    max_archive_files: config.matching.max_archive_files,
    max_archive_file_size_mb: config
      .matching
      .max_archive_file_size_mb,
    use_organized_context: config.matching.use_organized_context,
    ledger_path: ledger_path.clone(),
    model: config.ai.model.clone(),
    describe_model: config.ai.describe_model.clone(),
    taxonomy: config.taxonomy.areas.clone(),
    corrections_path: corrections_path.clone(),
  };

  let (tx, mut rx) = tokio::sync::mpsc::channel::<PipelineEvent>(64);

  // One terminal session covers progress and review, so the screen
  // never flashes back to the shell between them. Piped or redirected
  // output gets plain lines instead.
  let session = if std::io::stdout().is_terminal() {
    Some(TerminalSession::enter()?)
  } else {
    None
  };
  let event_handle: tokio::task::JoinHandle<(
    Option<TerminalSession>,
    Result<()>,
  )> = match session {
    Some(mut session) => tokio::task::spawn_blocking(move || {
      let outcome =
        tui::run_pipeline_progress(rx, &mut session.terminal);
      (Some(session), outcome)
    }),
    None => tokio::spawn(async move {
      let mut progress = PipelineProgress::new();
      while let Some(event) = rx.recv().await {
        progress.handle_event(&event);
      }
      (None, Ok(()))
    }),
  };

  let pipeline_result =
    pipeline::run(&provider, &pipeline_config, tx).await;
  let session = match event_handle.await {
    Ok((session, Ok(()))) => session,
    Ok((session, Err(e))) => {
      tracing::warn!(error = %e, "Progress display failed");
      session
    }
    Err(e) => {
      tracing::error!(error = %e, "Event handler task panicked");
      None
    }
  };
  // Restore the terminal before any error or plain output reaches it.
  let result = match pipeline_result {
    Ok(result) => result,
    Err(e) => {
      drop(session);
      return Err(e);
    }
  };

  let plan = &result.plan;
  let summary = format!(
    "{} groups · {} moves · {} duplicates ({} reclaimable)",
    plan.stats.groups_created,
    plan.moves.len(),
    plan.stats.duplicates_found,
    format_bytes(plan.stats.space_to_reclaim),
  );

  if plan.groups.is_empty() {
    drop(session);
    println!("\nPlan: {summary}.");
    print_organized_duplicates(&result);
    println!("Nothing to organize.");
    return Ok(());
  }

  let picker = ratatui_image::picker::Picker::from_query_stdio().ok();

  let review_state = ReviewState::new(
    plan.groups.clone(),
    plan.moves.clone(),
    config.general.output_dir.clone(),
    picker,
    ReviewMode::Organize,
  )
  .with_file_metadata(&result.fingerprinted)
  .with_duplicates(&result.all_dupes, &result.fingerprinted)
  .with_descriptions(&result.descriptions, &result.fingerprinted)
  .with_banner(&summary);

  let Some(mut session) = session else {
    anyhow::bail!(
      "Interactive review needs a terminal. Run spindle in a \
       terminal, or use --dry-run there to see the plan."
    );
  };
  let review = tui::run_review(review_state, &mut session.terminal);
  drop(session);
  let (action, review_state) = review?;

  println!("\nPlan: {summary}.");
  print_organized_duplicates(&result);

  match action {
    ReviewAction::Quit => {
      println!("Aborted by user.");
      return Ok(());
    }
    ReviewAction::Execute => {}
  }

  let recording =
    ledger_path.as_deref().map(|path| LedgerRecording {
      path,
      fingerprinted: &result.fingerprinted,
      groups: &plan.groups,
    });
  let corrections =
    corrections_path
      .as_deref()
      .map(|path| CorrectionsRecording {
        path,
        plan,
        fingerprinted: &result.fingerprinted,
      });
  execute_review(&cli, &config, &review_state, recording, corrections)
}

/// Everything needed to remember what the user changed in the review.
struct CorrectionsRecording<'a> {
  path: &'a Path,
  plan: &'a spindle::model::ReorgPlan,
  fingerprinted: &'a [FingerprintedFile],
}

/// Persist renames and re-filed files so the next run proposes them
/// that way from the start. Failures are logged, never fatal.
fn record_corrections(
  recording: &CorrectionsRecording<'_>,
  review_state: &ReviewState,
  completed: &[FileMove],
) {
  let hash_by_path: HashMap<std::path::PathBuf, String> = recording
    .fingerprinted
    .iter()
    .map(|f| {
      (
        f.scanned.path.clone(),
        spindle::ledger::hash_hex(&f.blake3_hash),
      )
    })
    .collect();
  let derived = spindle::corrections::derive(
    recording.plan,
    review_state.groups(),
    completed,
    &hash_by_path,
  );
  if derived.renames.is_empty() && derived.placements.is_empty() {
    return;
  }
  let mut corrections =
    spindle::corrections::Corrections::load(recording.path);
  corrections.absorb(&derived);
  match corrections.save(recording.path) {
    Ok(()) => println!(
      "Remembered {} rename(s) and {} re-filed file(s) for next time.",
      derived.renames.len(),
      derived.placements.len()
    ),
    Err(err) => tracing::warn!(
      path = %recording.path.display(),
      error = %err,
      "Failed to save review corrections"
    ),
  }
}

fn print_organized_duplicates(result: &pipeline::PipelineResult) {
  if result.organized_duplicates.is_empty() {
    return;
  }
  println!(
    "\n{} new file(s) are identical to already-organized content:",
    result.organized_duplicates.len()
  );
  for dup in &result.organized_duplicates {
    println!(
      "  {} ↔ {}",
      dup.path.display(),
      dup.organized_at.display()
    );
  }
}

fn run_undo(cli: &CliArgs) -> Result<()> {
  // Legacy escape hatch: an explicit --undo-log path uses the old
  // single-shot undo log format.
  if let Some(undo_path) = &cli.undo_log {
    println!("Undoing from legacy log: {}", undo_path.display());
    let restored = spindle::executor::undo(undo_path)?;
    println!(
      "Restored {} files to their original locations.",
      restored.len()
    );
    for path in &restored {
      println!("  ← {}", path.display());
    }
    return Ok(());
  }

  let paths = ExecutorPaths::default_paths();
  let report =
    spindle::executor::undo_run(&paths, cli.undo_run.as_deref())?;

  println!(
    "Undid run {}: {} restored, {} failed.",
    report.run_id,
    report.restored.len(),
    report.failed.len()
  );
  for path in &report.restored {
    println!("  ← {}", path.display());
  }
  for (path, err) in &report.failed {
    eprintln!("  ✗ {} ({err})", path.display());
  }
  if !report.failed.is_empty() {
    anyhow::bail!(
      "Some files could not be restored; the run journal was kept."
    );
  }
  Ok(())
}

fn run_list_undo() -> Result<()> {
  let paths = ExecutorPaths::default_paths();
  let runs = journal::list_runs(&paths.journal_dir);
  if runs.is_empty() {
    println!("No undoable runs.");
    return Ok(());
  }
  println!("Undoable runs (newest first):");
  for run_id in runs {
    match journal::load(&paths.journal_dir, &run_id) {
      Ok(j) => {
        let staged: u64 = j.deletions.iter().map(|d| d.size).sum();
        println!(
          "  {}  {} moves, {} deletions ({} in trash)",
          run_id,
          j.moves.len(),
          j.deletions.len(),
          format_bytes(staged),
        );
      }
      Err(_) => println!("  {run_id}  (unreadable journal)"),
    }
  }
  println!("\nUndo one with: spindle --undo-run <RUN_ID>");
  Ok(())
}

fn run_purge(older_than_days: Option<u64>) -> Result<()> {
  let paths = ExecutorPaths::default_paths();
  let report = trash::purge(&paths.trash_dir, older_than_days)?;
  println!(
    "Purged {} run(s) from trash, {} reclaimed.",
    report.runs_purged,
    format_bytes(report.bytes_freed),
  );
  Ok(())
}

fn run_dupes_only(cli: &CliArgs, config: &Config) -> Result<()> {
  print!("  Scanning...  ");
  let scan_opts = ScanOptions {
    include_trash: cli.include_trash,
    type_filter: cli.file_types.clone(),
  };
  let scanned = scan_directories_filtered(
    &config.general.target_dirs,
    &scan_opts,
  )?;
  println!(
    "{} {} files found",
    progress::rainbow_bar(1.0, progress::BAR_WIDTH),
    scanned.len(),
  );

  print!("  Fingerprinting...  ");
  let mut fingerprinted = fingerprint_files(scanned)?;

  // Skip files a previous run already organized.
  if let Some(ledger_path) = spindle::config::resolve_ledger_path(cli)
  {
    let ledger = Ledger::load(&ledger_path);
    let before = fingerprinted.len();
    fingerprinted.retain(|f| {
      let hex = spindle::ledger::hash_hex(&f.blake3_hash);
      !ledger.is_organized(&f.scanned.path, &hex)
    });
    let skipped = before - fingerprinted.len();
    if skipped > 0 {
      println!("  (skipping {skipped} already-organized files)");
    }
  }
  let exact_dupes = find_exact_duplicates(&fingerprinted);
  let near_dupes = spindle::fingerprint::find_near_duplicates(
    &fingerprinted,
    config.duplicates.near_duplicate_threshold,
  );
  let archive_matches =
    spindle::fingerprint::archive::find_archive_matches(
      &fingerprinted,
    );
  let text_matches =
    spindle::fingerprint::text::find_similar_text(&fingerprinted);
  let audio_matches =
    spindle::fingerprint::audio::find_similar_audio(&fingerprinted);
  let dupes: Vec<_> = exact_dupes
    .into_iter()
    .chain(near_dupes)
    .chain(archive_matches)
    .chain(text_matches)
    .chain(audio_matches)
    .collect();
  println!(
    "{} {} duplicate sets",
    progress::rainbow_bar(1.0, progress::BAR_WIDTH),
    dupes.len(),
  );

  if dupes.is_empty() {
    println!("\nNo duplicates found.");
    return Ok(());
  }

  let (groups, moves, dupe_types) =
    dupes_to_groups(&dupes, &fingerprinted);

  let picker = ratatui_image::picker::Picker::from_query_stdio().ok();
  let mut review_state = ReviewState::new(
    groups,
    moves,
    config.general.output_dir.clone(),
    picker,
    ReviewMode::Dupes,
  )
  .with_file_metadata(&fingerprinted);
  review_state.set_dupe_types(dupe_types);
  let mut session = TerminalSession::enter()?;
  let review = tui::run_review(review_state, &mut session.terminal);
  drop(session);
  let (action, review_state) = review?;

  match action {
    ReviewAction::Quit => {
      println!("Aborted by user.");
      return Ok(());
    }
    ReviewAction::Execute => {}
  }

  // Dedup-only runs don't organize into folders, so nothing is recorded.
  execute_review(cli, config, &review_state, None, None)
}

fn dupes_to_groups(
  dupes: &[spindle::model::DuplicateSet],
  fingerprinted: &[spindle::model::FingerprintedFile],
) -> (
  Vec<FileGroup>,
  Vec<FileMove>,
  Vec<spindle::model::DuplicateType>,
) {
  let mut groups = Vec::new();
  let mut moves = Vec::new();
  let mut dupe_types = Vec::new();

  for (i, set) in dupes.iter().enumerate() {
    let canonical = &fingerprinted[set.canonical];
    let canonical_name = canonical
      .scanned
      .path
      .file_name()
      .unwrap_or_default()
      .to_string_lossy();

    let mut members: Vec<usize> = vec![set.canonical];
    members.extend(&set.duplicates);

    let rationale = match set.duplicate_type {
      spindle::model::DuplicateType::Exact => {
        format!("Byte-identical — keep {}", canonical_name)
      }
      spindle::model::DuplicateType::NearDuplicate { distance } => {
        format!(
          "Perceptually similar (distance {}) — keep {}",
          distance, canonical_name
        )
      }
      spindle::model::DuplicateType::ArchiveMatch => {
        "Archive contents already extracted — keep the folder, \
         the archive is redundant"
          .to_string()
      }
      spindle::model::DuplicateType::SimilarText { distance } => {
        format!(
          "Text near-identical (simhash distance {}) — keep {}",
          distance, canonical_name
        )
      }
      spindle::model::DuplicateType::SimilarAudio { score } => {
        format!(
          "Same recording ({}% acoustic match) — keep {}",
          score, canonical_name
        )
      }
    };

    dupe_types.push(set.duplicate_type);

    groups.push(FileGroup {
      id: i,
      label: format!("Duplicate Set {}", i + 1),
      rationale,
      members: members.clone(),
      member_destinations: vec![],
      suggested_path: canonical
        .scanned
        .path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf(),
      member_notes: vec![],
    });

    moves.push(FileMove {
      from: canonical.scanned.path.clone(),
      to: canonical.scanned.path.clone(),
      group_id: i,
    });

    for &dup_idx in &set.duplicates {
      let dup = &fingerprinted[dup_idx];
      moves.push(FileMove {
        from: dup.scanned.path.clone(),
        to: dup.scanned.path.clone(),
        group_id: i,
      });
    }
  }

  (groups, moves, dupe_types)
}

/// Everything needed to append organized moves to the ledger after a
/// successful (non-dry-run) organize.
struct LedgerRecording<'a> {
  path: &'a Path,
  fingerprinted: &'a [FingerprintedFile],
  groups: &'a [FileGroup],
}

/// Append the completed moves to the persistent ledger so future runs skip
/// these files. A ledger write failure is logged, never fatal.
fn record_organized(
  recording: &LedgerRecording<'_>,
  moves: &[FileMove],
) {
  if moves.is_empty() {
    return;
  }
  let hash_by_path: HashMap<&Path, String> = recording
    .fingerprinted
    .iter()
    .map(|f| {
      (
        f.scanned.path.as_path(),
        spindle::ledger::hash_hex(&f.blake3_hash),
      )
    })
    .collect();
  let label_by_group: HashMap<usize, &str> = recording
    .groups
    .iter()
    .map(|g| (g.id, g.label.as_str()))
    .collect();

  let organized_at = chrono::Utc::now().to_rfc3339();
  let mut ledger = Ledger::load(recording.path);
  for mv in moves {
    let Some(hex) = hash_by_path.get(mv.from.as_path()) else {
      continue;
    };
    let label = label_by_group
      .get(&mv.group_id)
      .copied()
      .unwrap_or("ungrouped");
    ledger.record(LedgerEntry {
      source_path: mv.from.clone(),
      dest_path: mv.to.clone(),
      blake3_hex: hex.clone(),
      group_label: label.to_string(),
      organized_at: organized_at.clone(),
    });
  }
  if let Err(err) = ledger.save(recording.path) {
    tracing::warn!(
      path = %recording.path.display(),
      error = %err,
      "Failed to update organized ledger"
    );
  }
}

fn execute_review(
  cli: &CliArgs,
  config: &Config,
  review_state: &ReviewState,
  ledger: Option<LedgerRecording<'_>>,
  corrections: Option<CorrectionsRecording<'_>>,
) -> Result<()> {
  let exec_paths = ExecutorPaths::default_paths();

  match review_state.review_mode() {
    ReviewMode::Organize => {
      let approved_moves = review_state.approved_moves();
      let deletions = review_state.files_to_delete();
      if approved_moves.is_empty() && deletions.is_empty() {
        println!("No groups approved. Nothing to do.");
        return Ok(());
      }

      if cli.dry_run {
        println!("\n--- DRY RUN ---");
        for group in review_state.approved_groups() {
          println!(
            "  [{}] {} → {} ({} files)",
            group.id,
            group.label,
            group.suggested_path.display(),
            group.members.len()
          );
        }
        for m in &approved_moves {
          println!("  mv {} → {}", m.from.display(), m.to.display());
        }
        for path in &deletions {
          println!("  rm {}", path.display());
        }
        println!(
          "\n{} approved groups, {} moves, {} deletions (not executed).",
          review_state.approved_groups().len(),
          approved_moves.len(),
          deletions.len()
        );
        return Ok(());
      }

      let approved = ApprovedPlan {
        moves: approved_moves,
        deletions,
        skipped_files: vec![],
      };

      println!(
        "Executing plan ({} moves, {} deletions)...",
        approved.moves.len(),
        approved.deletions.len()
      );
      let report = execute_plan(
        &approved,
        &exec_paths,
        &config.general.output_dir,
      );

      println!(
        "\nDone! {} files moved, {} failed, {} staged to trash ({}).",
        report.moves_completed.len(),
        report.moves_failed.len(),
        report.deletions_staged.len(),
        format_bytes(report.bytes_staged),
      );

      if !report.moves_failed.is_empty() {
        for (mv, err) in &report.moves_failed {
          tracing::error!(from = %mv.from.display(), error = %err, "Move failed");
        }
      }

      print_undo_info(&report);

      if let Some(recording) = ledger {
        record_organized(&recording, &report.moves_completed);
      }
      if let Some(recording) = corrections {
        record_corrections(
          &recording,
          review_state,
          &report.moves_completed,
        );
      }
    }
    ReviewMode::Dupes => {
      let deletions = review_state.files_to_delete();
      if deletions.is_empty() {
        println!("No duplicates to delete.");
        return Ok(());
      }

      if cli.dry_run {
        println!("\n--- DRY RUN ---");
        for path in &deletions {
          println!("  rm {}", path.display());
        }
        println!(
          "\n{} duplicates to delete (not executed).",
          deletions.len()
        );
        return Ok(());
      }

      println!(
        "Staging {} duplicate files to trash...",
        deletions.len()
      );
      let plan = ApprovedPlan {
        moves: vec![],
        deletions,
        skipped_files: vec![],
      };
      let report =
        execute_plan(&plan, &exec_paths, &config.general.output_dir);

      println!(
        "\nDone! {} duplicates staged to trash ({} reclaimable \
         with --purge).",
        report.deletions_staged.len(),
        format_bytes(report.bytes_staged),
      );

      print_undo_info(&report);
    }
  }

  Ok(())
}

fn print_undo_info(report: &ExecutionReport) {
  if let Some(ref err) = report.journal_error {
    eprintln!(
      "WARNING: Failed to write the run journal: {}. \
       You will NOT be able to undo this operation.",
      err
    );
  } else if report.journal_path.is_some() {
    println!(
      "Undo with: spindle --undo   (this run: --undo-run {})",
      report.run_id
    );
  }
}

fn default_cache_dir() -> std::path::PathBuf {
  directories::BaseDirs::new()
    .map(|d| d.cache_dir().join("spindle"))
    .unwrap_or_else(|| std::path::PathBuf::from(".cache/spindle"))
}

fn format_bytes(bytes: u64) -> String {
  if bytes >= 1_000_000_000 {
    format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
  } else if bytes >= 1_000_000 {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
  } else if bytes >= 1_000 {
    format!("{:.1} KB", bytes as f64 / 1_000.0)
  } else {
    format!("{} B", bytes)
  }
}

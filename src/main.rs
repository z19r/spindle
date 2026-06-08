use anyhow::Result;
use clap::Parser;
use dotenv::dotenv;
use tracing_subscriber::EnvFilter;

use spindel::ai::ClaudeProvider;
use spindel::config::{CliArgs, Config};
use spindel::executor::execute_plan;
use spindel::fingerprint::{
  find_exact_duplicates, fingerprint_files,
};
use spindel::model::{
  ApprovedPlan, ExecutionReport, FileGroup, FileMove,
};
use spindel::pipeline::{self, PipelineConfig, PipelineEvent};
use spindel::progress::{self, PipelineProgress};
use spindel::scanner::{scan_directories_filtered, ScanOptions};
use spindel::tui::{self, ReviewAction, ReviewMode, ReviewState};

#[tokio::main]
async fn main() -> Result<()> {
  dotenv().ok();
  let cli = CliArgs::parse();

  let filter = match cli.verbose {
    0 => "spindel=info",
    1 => "spindel=debug",
    _ => "spindel=trace",
  };
  tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::new(filter))
    .init();

  let config = Config::load(&cli)?;
  tracing::debug!(?config, "Loaded configuration");

  if cli.undo {
    return run_undo(&cli, &config);
  }

  if cli.dupes_only {
    return run_dupes_only(&cli, &config);
  }

  let provider = ClaudeProvider::new("", config.ai.model.clone());

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
  };

  let (tx, mut rx) = tokio::sync::mpsc::channel::<PipelineEvent>(64);

  let event_handle = tokio::spawn(async move {
    let mut progress = PipelineProgress::new();
    while let Some(event) = rx.recv().await {
      progress.handle_event(&event);
    }
  });

  let result = pipeline::run(&provider, &pipeline_config, tx).await?;
  if let Err(e) = event_handle.await {
    tracing::error!(error = %e, "Event handler task panicked");
  }

  let plan = &result.plan;

  println!(
    "\nPlan: {} groups, {} moves, {} duplicates ({} reclaimable).",
    plan.stats.groups_created,
    plan.moves.len(),
    plan.stats.duplicates_found,
    format_bytes(plan.stats.space_to_reclaim),
  );

  if plan.groups.is_empty() {
    println!("Nothing to organize.");
    return Ok(());
  }

  let (dupes_groups, dupes_moves, dupe_types) =
    dupes_to_groups(&result.all_dupes, &result.fingerprinted);

  let picker = ratatui_image::picker::Picker::from_query_stdio().ok();

  let dupes_alt = if dupes_groups.is_empty() {
    None
  } else {
    Some((dupes_groups, dupes_moves, ReviewMode::Dupes, dupe_types))
  };

  let review_state = ReviewState::new(
    plan.groups.clone(),
    plan.moves.clone(),
    config.general.output_dir.clone(),
    picker,
    ReviewMode::Organize,
    dupes_alt,
  )
  .with_file_metadata(&result.fingerprinted);
  let (action, review_state) = tui::run_review(review_state)?;

  match action {
    ReviewAction::Quit => {
      println!("Aborted by user.");
      return Ok(());
    }
    ReviewAction::Execute => {}
  }

  execute_review(&cli, &config, &review_state)
}

fn run_undo(cli: &CliArgs, config: &Config) -> Result<()> {
  let undo_path = cli.undo_log.clone().unwrap_or_else(|| {
    config.general.output_dir.join(".spindel_undo.json")
  });
  println!("Undoing from: {}", undo_path.display());
  let restored = spindel::executor::undo(&undo_path)?;
  println!(
    "Restored {} files to their original locations.",
    restored.len()
  );
  for path in &restored {
    println!("  ← {}", path.display());
  }
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
  let fingerprinted = fingerprint_files(scanned)?;
  let exact_dupes = find_exact_duplicates(&fingerprinted);
  let near_dupes = spindel::fingerprint::find_near_duplicates(
    &fingerprinted,
    config.duplicates.near_duplicate_threshold,
  );
  let dupes: Vec<_> =
    exact_dupes.into_iter().chain(near_dupes).collect();
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
    None,
  )
  .with_file_metadata(&fingerprinted);
  review_state.set_dupe_types(dupe_types);
  let (action, review_state) = tui::run_review(review_state)?;

  match action {
    ReviewAction::Quit => {
      println!("Aborted by user.");
      return Ok(());
    }
    ReviewAction::Execute => {}
  }

  execute_review(cli, config, &review_state)
}

fn dupes_to_groups(
  dupes: &[spindel::model::DuplicateSet],
  fingerprinted: &[spindel::model::FingerprintedFile],
) -> (
  Vec<FileGroup>,
  Vec<FileMove>,
  Vec<spindel::model::DuplicateType>,
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
      spindel::model::DuplicateType::Exact => {
        format!("Byte-identical — keep {}", canonical_name)
      }
      spindel::model::DuplicateType::NearDuplicate { distance } => {
        format!(
          "Perceptually similar (distance {}) — keep {}",
          distance, canonical_name
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

fn execute_review(
  cli: &CliArgs,
  config: &Config,
  review_state: &ReviewState,
) -> Result<()> {
  let undo_log_path =
    config.general.output_dir.join(".spindel_undo.json");

  match review_state.review_mode() {
    ReviewMode::Organize => {
      let approved_moves = review_state.approved_moves();
      if approved_moves.is_empty() {
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
        println!(
          "\n{} approved groups, {} moves (not executed).",
          review_state.approved_groups().len(),
          approved_moves.len()
        );
        return Ok(());
      }

      let approved = ApprovedPlan {
        moves: approved_moves,
        deletions: vec![],
        skipped_files: vec![],
      };

      println!("Executing plan ({} moves)...", approved.moves.len());
      let report = execute_plan(
        &approved,
        &undo_log_path,
        &config.general.output_dir,
      );

      println!(
        "\nDone! {} files moved, {} failed.",
        report.moves_completed.len(),
        report.moves_failed.len()
      );

      if !report.moves_failed.is_empty() {
        for (mv, err) in &report.moves_failed {
          tracing::error!(from = %mv.from.display(), error = %err, "Move failed");
        }
      }

      print_undo_info(&report);
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

      println!("Deleting {} duplicate files...", deletions.len());
      let plan = ApprovedPlan {
        moves: vec![],
        deletions,
        skipped_files: vec![],
      };
      let report = execute_plan(
        &plan,
        &undo_log_path,
        &config.general.output_dir,
      );

      let reclaimed: u64 = report
        .deletions_completed
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();

      println!(
        "\nDone! {} duplicates deleted ({} reclaimed).",
        report.deletions_completed.len(),
        format_bytes(reclaimed),
      );

      print_undo_info(&report);
    }
  }

  Ok(())
}

fn print_undo_info(report: &ExecutionReport) {
  if let Some(ref err) = report.undo_log_error {
    eprintln!(
      "WARNING: Failed to write undo log: {}. \
       You will NOT be able to undo this operation.",
      err
    );
  } else {
    println!("Undo log saved to: {}", report.undo_log_path.display());
    println!("Run with --undo to reverse this operation.");
  }
}

fn default_cache_dir() -> std::path::PathBuf {
  directories::BaseDirs::new()
    .map(|d| d.cache_dir().join("spindel"))
    .unwrap_or_else(|| std::path::PathBuf::from(".cache/spindel"))
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

//! Grouping-quality eval against the real Claude API.
//!
//! Ignored by default. Run with `just eval` (sets `SPINDLE_EVAL=1`); it
//! follows your `ANTHROPIC_*` environment, proxy included. Needs
//! `ANTHROPIC_API_KEY`, or `ANTHROPIC_BASE_URL` pointing at a proxy that
//! injects the key. `just eval-direct` pins the real API instead. Per-file descriptions are cached under
//! `target/eval-cache`, so re-runs only pay for the grouping call.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use spindle::ai::ClaudeProvider;
use spindle::config::AiConfig;
use spindle::eval::{self, ExpectedSet, Placement};
use spindle::pipeline::{self, PipelineConfig, PipelineEvent};

/// Minimum composite score. Raised as grouping improves; a change
/// that drops below it is a regression.
const FLOOR: f64 = 0.85;
/// Floor for the large fixture; raised once a baseline is recorded.
const LARGE_FLOOR: f64 = 0.80;
/// Floors for the second-run fixture: composite, and the share of files
/// whose folder already existed that landed under exactly that label.
const SECOND_RUN_FLOOR: f64 = 0.85;
const REUSE_FLOOR: f64 = 0.75;

fn fixtures_root() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("tests")
    .join("fixtures")
}

fn provider() -> Option<ClaudeProvider> {
  let base_url = std::env::var("ANTHROPIC_BASE_URL").ok();
  let api_key = match std::env::var("ANTHROPIC_API_KEY") {
    Ok(key) => key,
    Err(_) if base_url.is_some() => "proxy".to_string(),
    Err(_) => return None,
  };
  let ai = AiConfig::default();
  let mut provider =
    ClaudeProvider::new(api_key, ai.model, ai.max_retries)
      .with_describe_model(ai.describe_model);
  if let Some(url) = base_url {
    provider = provider.with_base_url(url);
  }
  Some(provider)
}

fn print_groups(placements: &[Placement], expected: &ExpectedSet) {
  let mut by_label: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
  for p in placements {
    by_label.entry(&p.label).or_default().push(&p.path);
  }
  println!("\n=== produced groups ===");
  for (label, files) in &by_label {
    println!("{label}");
    for f in files {
      println!("    {f}");
    }
  }
  let placed: std::collections::HashSet<&str> =
    placements.iter().map(|p| p.path.as_str()).collect();
  let missing: Vec<&str> = expected
    .files
    .iter()
    .map(|e| e.path.as_str())
    .filter(|p| !placed.contains(p))
    .collect();
  if !missing.is_empty() {
    println!("\n=== expected files missing from the plan ===");
    for m in missing {
      println!("    {m}");
    }
  }
}

/// Copy the fixture's `Organized/` tree into `output` and write a ledger
/// that records every file in it under the folder's label, as a previous
/// run would have. Returns the ledger path.
fn seed_previous_run(fixture: &Path, output: &Path) -> PathBuf {
  use spindle::ledger::{Ledger, LedgerEntry};
  let organized = fixture.join("Organized");
  let mut ledger = Ledger::default();
  for entry in walkdir::WalkDir::new(&organized)
    .into_iter()
    .filter_map(Result::ok)
    .filter(|e| e.file_type().is_file())
  {
    let rel = entry
      .path()
      .strip_prefix(&organized)
      .expect("under Organized");
    let label = rel
      .parent()
      .expect("file inside a folder")
      .to_string_lossy()
      .replace('\\', "/");
    let dest = output.join(rel);
    std::fs::create_dir_all(dest.parent().unwrap()).expect("mkdir");
    std::fs::copy(entry.path(), &dest).expect("copy organized file");
    let bytes = std::fs::read(&dest).expect("read organized file");
    ledger.record(LedgerEntry {
      source_path: dest.clone(),
      dest_path: dest,
      blake3_hex: blake3::hash(&bytes).to_hex().to_string(),
      group_label: label,
      organized_at: "2026-09-01T00:00:00Z".to_string(),
    });
  }
  let path = output.join("ledger.json");
  ledger.save(&path).expect("save ledger");
  path
}

/// Run one fixture through the real pipeline and return its report.
async fn eval_fixture(name: &str) -> Option<eval::EvalReport> {
  eval_fixture_with(name, false).await
}

/// `second_run` seeds the output directory and ledger from the
/// fixture's `Organized/` tree first, so existing folders are offered
/// to the model for reuse.
async fn eval_fixture_with(
  name: &str,
  second_run: bool,
) -> Option<eval::EvalReport> {
  dotenvy::dotenv().ok();
  let _ = tracing_subscriber::fmt()
    .with_env_filter(
      tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "spindle=warn".into()),
    )
    .with_test_writer()
    .try_init();
  if std::env::var("SPINDLE_EVAL").as_deref() != Ok("1") {
    eprintln!("SPINDLE_EVAL != 1; skipping real-API eval");
    return None;
  }
  let Some(provider) = provider() else {
    panic!(
      "set ANTHROPIC_API_KEY or ANTHROPIC_BASE_URL to run the eval"
    );
  };

  let root = fixtures_root().join(name);
  let expected = eval::load_expected(&root.join("expected.toml"))
    .expect("expected.toml");
  let output = tempfile::tempdir().expect("tempdir");
  let cache_dir =
    Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eval-cache");
  let ledger_path =
    second_run.then(|| seed_previous_run(&root, output.path()));
  let ai = AiConfig::default();

  let config = PipelineConfig {
    target_dirs: vec![
      root.join("Desktop"),
      root.join("Documents"),
      root.join("Downloads"),
    ],
    output_dir: output.path().to_path_buf(),
    no_ai: false,
    max_files: 500,
    max_file_size_mb: 100,
    max_cost: None,
    near_duplicate_threshold: 8,
    cache_dir,
    max_concurrent: 5,
    include_trash: false,
    type_filter: vec![],
    use_batch_api: false,
    introspect_archives: true,
    max_archive_files: 20,
    max_archive_file_size_mb: 50,
    use_organized_context: second_run,
    ledger_path,
    model: ai.model,
    describe_model: ai.describe_model,
    taxonomy: spindle::model::default_areas(),
    corrections_path: None,
  };

  let (tx, mut rx) = tokio::sync::mpsc::channel::<PipelineEvent>(64);
  let drain = tokio::spawn(async move {
    while let Some(event) = rx.recv().await {
      match event {
        PipelineEvent::CostEstimated {
          estimated_usd,
          file_count,
        } => println!(
          "estimated ${estimated_usd:.4} for {file_count} uncached files"
        ),
        PipelineEvent::AnalysisComplete {
          succeeded,
          failed,
          failed_files,
        } => {
          println!("analysis: {succeeded} ok, {failed} failed");
          for (name, err) in failed_files {
            println!("    FAILED {name}: {err}");
          }
        }
        PipelineEvent::GroupingFailed { error } => {
          println!("GROUPING FAILED: {error}")
        }
        _ => {}
      }
    }
  });

  let result = pipeline::run(&provider, &config, tx)
    .await
    .expect("pipeline run");
  drain.await.expect("event drain");

  let placements = eval::placements_from_plan(
    &result.plan,
    &result.fingerprinted,
    &root,
  );
  let report = eval::score(&expected, &placements);

  println!("\n=== fixture: {name} ===");
  print_groups(&placements, &expected);
  println!("\n=== eval report ({name}) ===\n{report}");
  Some(report)
}

#[tokio::test]
#[ignore = "real API; run via `just eval`"]
async fn grouping_quality_meets_floor() {
  let Some(report) = eval_fixture("organize").await else {
    return;
  };
  assert!(
    report.composite() >= FLOOR,
    "composite {:.3} fell below floor {FLOOR:.3}",
    report.composite()
  );
}

/// ~140 files, 29 groups, ambiguous items, office/ebook files, exact
/// and near duplicates. Floor is recorded on #98 once a baseline exists.
#[tokio::test]
#[ignore = "real API; run via `just eval`"]
async fn large_fixture_quality_meets_floor() {
  let Some(report) = eval_fixture("organize-large").await else {
    return;
  };
  assert!(
    report.composite() >= LARGE_FLOOR,
    "composite {:.3} fell below floor {LARGE_FLOOR:.3}",
    report.composite()
  );
}

/// A dozen folders already exist from a previous run (seeded from the
/// fixture's `Organized/` tree plus a ledger). 27 incoming files belong
/// in them and 8 need new folders. Measures label reuse as well as the
/// usual composite.
#[tokio::test]
#[ignore = "real API; run via `just eval`"]
async fn second_run_reuses_existing_folders() {
  let Some(report) =
    eval_fixture_with("organize-second-run", true).await
  else {
    return;
  };
  assert!(
    report.composite() >= SECOND_RUN_FLOOR,
    "composite {:.3} fell below floor {SECOND_RUN_FLOOR:.3}",
    report.composite()
  );
  let reuse =
    report.reuse_rate().expect("fixture marks existing files");
  assert!(
    reuse >= REUSE_FLOOR,
    "label reuse {reuse:.3} fell below floor {REUSE_FLOOR:.3}"
  );
}

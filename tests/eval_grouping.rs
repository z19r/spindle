//! Grouping-quality eval against the real Claude API.
//!
//! Ignored by default. Run with `just eval` (sets `SPINDLE_EVAL=1` and
//! pins `ANTHROPIC_BASE_URL` to the real API). Needs `ANTHROPIC_API_KEY`,
//! or `ANTHROPIC_BASE_URL` pointing at a proxy that injects the key.
//! A rewriting proxy can garble the grouping request, so measure the
//! model directly and use `just eval-via-proxy` only to test the proxy. Per-file descriptions are cached under
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

fn fixture_root() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("tests")
    .join("fixtures")
    .join("organize")
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

#[tokio::test]
#[ignore = "real API; run via `just eval`"]
async fn grouping_quality_meets_floor() {
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
    return;
  }
  let Some(provider) = provider() else {
    panic!(
      "set ANTHROPIC_API_KEY or ANTHROPIC_BASE_URL to run the eval"
    );
  };

  let root = fixture_root();
  let expected = eval::load_expected(&root.join("expected.toml"))
    .expect("expected.toml");
  let output = tempfile::tempdir().expect("tempdir");
  let cache_dir =
    Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eval-cache");
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
    use_organized_context: false,
    ledger_path: None,
    model: ai.model,
    describe_model: ai.describe_model,
    taxonomy: spindle::model::default_areas(),
  };

  let (tx, mut rx) = tokio::sync::mpsc::channel::<PipelineEvent>(64);
  let drain = tokio::spawn(async move {
    while let Some(event) = rx.recv().await {
      match event {
        PipelineEvent::CostEstimated {
          estimated_usd,
          file_count,
        } => println!("estimated ${estimated_usd:.4} for {file_count} uncached files"),
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

  print_groups(&placements, &expected);
  println!("\n=== eval report ===\n{report}");

  assert!(
    report.composite() >= FLOOR,
    "composite {:.3} fell below floor {FLOOR:.3}",
    report.composite()
  );
}

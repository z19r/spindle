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

/// How far below a fixture's own ceiling a run may fall before it
/// counts as a regression.
///
/// Headroom, not an absolute composite. A fixture's ceiling is the
/// score its own `expected.toml` gets when every file lands exactly
/// where the answer key says, and that is not 1.000 for every
/// fixture: the granularity term reads the *shape* of the folders, so
/// a fixture built from pairs caps itself. `organize` tops out at
/// 0.944 and `organize-second-run` at 0.900, because their ground
/// truth holds groups of two and three. Comparing four fixtures with
/// four different ceilings against one absolute number compares
/// nothing, and the reading drifts silently every time a fixture
/// gains a file. These preserve the headroom the absolute floors
/// allowed — 0.90, 0.85, 0.85 against a ceiling then assumed to be
/// 1.000 — so a run that passed before still passes.
const SLACK: f64 = 0.10;
const LARGE_SLACK: f64 = 0.15;
const SECOND_RUN_SLACK: f64 = 0.15;
const GRANULARITY_SLACK: f64 = 0.15;
/// Share of files whose folder already existed and that landed under
/// exactly that label.
const REUSE_FLOOR: f64 = 0.75;
/// Share of files that landed in a folder holding at least
/// `MIN_GROUP_SIZE`. Absolute rather than headroom: the granularity
/// fixture's ground truth has no thin folder in it, so 1.000 really is
/// available. Provisional until #65 records a real baseline; set where
/// a run may strand roughly one cluster before it counts as a
/// regression.
const SHAPE_FLOOR: f64 = 0.70;

fn fixtures_root() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("tests")
    .join("fixtures")
}

/// The best composite this fixture's own answer key can score.
///
/// Computed rather than recorded, so editing a fixture moves its floor
/// with it instead of leaving a stale constant behind.
fn ceiling(name: &str) -> f64 {
  let root = fixtures_root().join(name);
  let expected = eval::load_expected(&root.join("expected.toml"))
    .unwrap_or_else(|e| panic!("{name}/expected.toml: {e}"));
  let perfect: Vec<Placement> = expected
    .files
    .iter()
    .map(|e| Placement {
      path: e.path.clone(),
      label: e.group.clone(),
    })
    .collect();
  eval::score(&expected, &perfect).composite()
}

/// Assert a run scored within `slack` of what the fixture allows.
fn assert_within_slack(
  name: &str,
  report: &eval::EvalReport,
  slack: f64,
) {
  let floor = ceiling(name) - slack;
  assert!(
    report.composite() >= floor,
    "{name}: composite {:.3} fell below {floor:.3} \
     ({:.3} achievable, {slack:.2} slack)",
    report.composite(),
    ceiling(name)
  );
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
  assert_within_slack("organize", &report, SLACK);
}

/// ~140 files, 29 groups, ambiguous items, office/ebook files, exact
/// and near duplicates. Floor is recorded on #98 once a baseline exists.
#[tokio::test]
#[ignore = "real API; run via `just eval`"]
async fn large_fixture_quality_meets_floor() {
  let Some(report) = eval_fixture("organize-large").await else {
    return;
  };
  assert_within_slack("organize-large", &report, LARGE_SLACK);
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
  assert_within_slack(
    "organize-second-run",
    &report,
    SECOND_RUN_SLACK,
  );
  let reuse =
    report.reuse_rate().expect("fixture marks existing files");
  assert!(
    reuse >= REUSE_FLOOR,
    "label reuse {reuse:.3} fell below floor {REUSE_FLOOR:.3}"
  );
}

/// 47 files in six clusters, every one of them a single thing whose
/// members are individually distinguishable: one renovation across
/// four trades, one job search across three companies, one trip across
/// two cities. Splitting a cluster along that inner seam yields
/// folders that are each defensible and a tree that is wrong.
///
/// The other fixtures cannot see that mistake. Their clusters are
/// small enough that a split barely moves pairwise F1, and a two-file
/// folder passes every hygiene rule — it is neither a singleton, nor
/// too deep, nor named for a file type. This one is built so the
/// tempting split is always available and always wrong, and so a
/// perfect run scores 1.000 rather than being capped by its own shape.
#[tokio::test]
#[ignore = "real API; run via `just eval`"]
async fn granularity_fixture_is_not_shredded_into_thin_folders() {
  let Some(report) = eval_fixture("organize-granularity").await
  else {
    return;
  };
  let shape = report.granularity.score();
  assert!(
    shape >= SHAPE_FLOOR,
    "granularity {shape:.3} fell below floor {SHAPE_FLOOR:.3}: \
     {} of {} groups hold fewer than {} files ({} files stranded)",
    report.granularity.thin_groups,
    report.granularity.total_groups,
    spindle::eval::MIN_GROUP_SIZE,
    report.granularity.files_in_thin_groups
  );
  assert_within_slack(
    "organize-granularity",
    &report,
    GRANULARITY_SLACK,
  );
}

/// Every fixture's ground truth is internally consistent: the files it
/// names exist, and filing them exactly as the answer key says is
/// perfect on every metric that judges the *model* — agreement,
/// routing, coverage, hygiene.
///
/// No API, so this runs on every `cargo test`. It is what keeps a
/// floor honest. A fixture whose own answer key cannot reach its floor
/// makes the floor unreachable, and the failure is otherwise invisible
/// until someone pays for a run to find it.
///
/// The composite is deliberately not asserted at 1.000 here, because
/// granularity judges the fixture rather than the model: `organize`
/// and `organize-second-run` are built from folders of two and three,
/// so their answer keys top out at 0.944 and 0.900. That is why the
/// floors are headroom below [`ceiling`] and not absolutes. The
/// granularity fixture is the one built to reach 1.000, and it is
/// asserted to.
#[test]
fn every_fixture_ground_truth_is_internally_consistent() {
  for name in [
    "organize",
    "organize-large",
    "organize-second-run",
    "organize-granularity",
  ] {
    let root = fixtures_root().join(name);
    let expected = eval::load_expected(&root.join("expected.toml"))
      .unwrap_or_else(|e| panic!("{name}/expected.toml: {e}"));
    assert!(
      !expected.files.is_empty(),
      "{name} has no expected files"
    );

    for e in &expected.files {
      assert!(
        root.join(&e.path).is_file(),
        "{name}: expected.toml names {}, which is not in the fixture",
        e.path
      );
    }

    let perfect: Vec<Placement> = expected
      .files
      .iter()
      .map(|e| Placement {
        path: e.path.clone(),
        label: e.group.clone(),
      })
      .collect();
    let r = eval::score(&expected, &perfect);
    println!("=== {name} ===\n{r}");
    for (metric, value) in [
      ("pairwise f1", r.pairwise_f1),
      ("top-level accuracy", r.top_level_accuracy),
      ("placed", r.placed_fraction()),
      ("hygiene", r.hygiene.score()),
    ] {
      assert!(
        (value - 1.0).abs() < 1e-9,
        "{name}: its own answer key scores {value:.4} on {metric}"
      );
    }
    assert!(
      ceiling(name) - GRANULARITY_SLACK.max(SLACK) > 0.0,
      "{name}: ceiling {:.3} leaves no room for a floor",
      ceiling(name)
    );
  }
}

/// The granularity fixture's promise: no expected folder is thin, so a
/// run that matches the answer key scores 1.000 outright. Every other
/// fixture caps itself below that, which is exactly why this one
/// exists.
#[test]
fn the_granularity_fixture_can_actually_be_scored_perfectly() {
  let c = ceiling("organize-granularity");
  assert!(
    (c - 1.0).abs() < 1e-9,
    "its own answer key scores {c:.4}; a thin expected group would \
     mean the fixture, not the model, is being measured"
  );
}

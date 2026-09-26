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
use spindle::group::validate::validate_groups;
use spindle::model::ProposedGroup;
use spindle::pipeline::{self, PipelineConfig, PipelineEvent};

/// How far below a fixture's own ceiling a run may fall before it
/// counts as a regression.
///
/// Headroom, not an absolute composite. A fixture's ceiling is the
/// score its own `expected.toml` gets when every file lands exactly
/// where the answer key says, and that is not 1.000 for every
/// fixture: the granularity term reads the *shape* of the folders, so
/// a fixture built from pairs caps itself, as `organize-large` does
/// deliberately. Comparing four fixtures with four different ceilings
/// against one absolute number compares nothing, and the reading
/// drifts silently every time a fixture gains a file. These preserve
/// the headroom the absolute floors allowed — 0.90, 0.85, 0.85
/// against a ceiling then assumed to be 1.000 — so a run that passed
/// before still passes. Each fixture's ceiling is written down in
/// [`MIN_CEILING`], which is what stops one drifting down unnoticed.
const SLACK: f64 = 0.10;
const LARGE_SLACK: f64 = 0.15;
const SECOND_RUN_SLACK: f64 = 0.15;
const GRANULARITY_SLACK: f64 = 0.15;
/// The lowest each fixture's own answer key may score against itself.
///
/// A fixture's ceiling is a property of the fixture, and it moves when
/// files are added to it. Recording it here makes that move deliberate:
/// splitting one expected folder into two thin ones drops the ceiling,
/// every floor derived from it drops in step, and nothing would
/// otherwise say the eval had gone quietly slacker.
///
/// Three of the four reach 1.000 — no expected folder in them holds
/// fewer than `MIN_GROUP_SIZE` files once the run is applied.
/// `organize-large` keeps five small folders on purpose, because a
/// large real tree has some, and pays 0.011 for them.
/// `organize-second-run` reaches it too, counting what is already
/// under its `Organized/` tree alongside what the run adds.
const MIN_CEILING: [(&str, f64); 4] = [
  ("organize", 1.000),
  ("organize-large", 0.985),
  ("organize-second-run", 1.000),
  ("organize-granularity", 1.000),
];

/// Share of files whose folder already existed and that landed under
/// exactly that label. A judgement call like [`SHAPE_FLOOR`], held
/// achievable by the same guard.
const REUSE_FLOOR: f64 = 0.75;
/// Share of files that landed in a folder holding at least
/// `MIN_GROUP_SIZE`.
///
/// Absolute rather than headroom, because the shape a fixture allows
/// is not the shape it caps: `organize-granularity`'s ground truth
/// has no thin folder in it, so 1.000 really is available and
/// subtracting a slack from it would only invent room to fail in.
///
/// A judgement call, set where a run may strand roughly one cluster
/// before it counts as a regression, and no issue is pending on it —
/// a number nobody has managed to fail is a number nobody can
/// calibrate. What keeps it honest is
/// [`every_fixture_ground_truth_is_internally_consistent`], which
/// checks that a run placing every file correctly would clear it.
/// That is the check [`REUSE_FLOOR`] needed and did not have: it
/// asserted 0.75 against an achievable 0.481 for as long as the size
/// floor ignored existing folders, and being API-gated, nothing said
/// so.
const SHAPE_FLOOR: f64 = 0.70;

/// Every fixture with an answer key, so a new one is checked by the
/// consistency tests the moment it is added.
const FIXTURES: [&str; 4] = [
  "organize",
  "organize-large",
  "organize-second-run",
  "organize-granularity",
];

fn fixtures_root() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("tests")
    .join("fixtures")
}

/// What the fixture's `Organized/` tree already holds, as the seeded
/// second run will find it. Empty for a fixture with no such tree.
fn existing_sizes(name: &str) -> eval::ExistingSizes {
  eval::ExistingSizes::from_tree(
    &fixtures_root().join(name).join("Organized"),
  )
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
  eval::score(&expected, &perfect, &existing_sizes(name)).composite()
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
  let report =
    eval::score(&expected, &placements, &existing_sizes(name));

  println!("\n=== fixture: {name} ===");
  print_groups(&placements, &expected);
  println!("\n=== eval report ({name}) ===\n{report}");

  // Reported, not asserted: #161 asks how good the tag-derived ALSO
  // FITS list is before #127 spends a model call on replacing it.
  // There is no baseline yet, so there is nothing to hold it to.
  let alts = eval::alternatives_coverage(
    &expected,
    &placements,
    &eval::descriptions_by_path(
      &result.fingerprinted,
      &result.descriptions,
      &root,
    ),
  );
  if let Some(rate) = alts.rate() {
    println!(
      "alternatives offered   {}/{} ({rate:.3}); {} got an empty list",
      alts.offered, alts.files, alts.empty
    );
  }
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
/// and near duplicates. Graded against its own ceiling like the rest,
/// by `LARGE_SLACK`.
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
  for name in FIXTURES {
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
    let r = eval::score(&expected, &perfect, &existing_sizes(name));
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
    let (_, floor) = MIN_CEILING
      .iter()
      .find(|(n, _)| *n == name)
      .unwrap_or_else(|| panic!("{name} has no recorded ceiling"));
    assert!(
      ceiling(name) >= *floor - 1e-9,
      "{name}: ceiling {:.3} has slipped below the recorded {floor:.3};        a run that gets everything right now scores less than it used to",
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

/// Folder labels a previous run left behind, as [`seed_previous_run`]
/// records them: one per directory under the fixture's `Organized/`.
/// Empty for a fixture that has no such tree.
fn existing_labels(name: &str) -> Vec<String> {
  let organized = fixtures_root().join(name).join("Organized");
  let mut labels: Vec<String> = walkdir::WalkDir::new(&organized)
    .into_iter()
    .filter_map(Result::ok)
    .filter(|e| e.file_type().is_file())
    .filter_map(|e| {
      let rel = e.path().strip_prefix(&organized).ok()?;
      Some(rel.parent()?.to_string_lossy().replace('\\', "/"))
    })
    .collect();
  labels.sort();
  labels.dedup();
  labels
}

/// A fixture's answer key, put through the validator the way a real
/// run's groups are: what the key asks for, and what the validator
/// makes of it.
///
/// This is the closest thing to a perfect run the eval can build
/// without paying for one, so it is what the guards below measure.
fn validated_answer_key(
  name: &str,
) -> (
  ExpectedSet,
  BTreeMap<String, Vec<usize>>,
  Vec<ProposedGroup>,
) {
  let root = fixtures_root().join(name);
  let expected = eval::load_expected(&root.join("expected.toml"))
    .unwrap_or_else(|e| panic!("{name}/expected.toml: {e}"));

  let mut by_label: BTreeMap<String, Vec<usize>> = BTreeMap::new();
  for (i, f) in expected.files.iter().enumerate() {
    by_label.entry(f.group.clone()).or_default().push(i);
  }
  let groups: Vec<ProposedGroup> = by_label
    .iter()
    .map(|(label, members)| ProposedGroup {
      label: label.clone(),
      rationale: String::new(),
      member_indices: members.clone(),
      member_destinations: vec![],
      member_notes: vec![],
    })
    .collect();

  let (out, _) = validate_groups(groups, &existing_labels(name));
  (expected, by_label, out)
}

/// Where a perfect run's files land once the validator has had its
/// say — what the scorer would see if the model got everything right.
fn perfect_placements(
  expected: &ExpectedSet,
  out: &[ProposedGroup],
) -> Vec<Placement> {
  out
    .iter()
    .flat_map(|g| {
      g.member_indices.iter().map(move |&i| Placement {
        path: expected.files[i].path.clone(),
        label: g.label.clone(),
      })
    })
    .collect()
}

/// No answer key may ask for two folders the validator will merge into
/// one.
///
/// A fixture is a claim about what the pipeline should produce, and the
/// validator is part of the pipeline. When it merges two expected
/// groups, the fixture is asking for something no run can deliver, and
/// the pairwise F1 — 45% of the composite — docks every run for it.
/// That is not a hard floor failing loudly; it is the ceiling dropping
/// quietly, which is the failure mode this test exists to prevent.
///
/// It caught a real one. Before the size floor learned about existing
/// folders, `organize-second-run` lost `Finance/Taxes/2023` into
/// `Finance/Taxes/2024` and `Personal/Pets/Biscuit` into
/// `Personal/Pets/Mochi`, capping a perfect run at F1 0.833.
///
/// A rename is a fault too, for a different reason. The scorer reads
/// groups rather than names, so `Work/Initech/Onboarding` folding up
/// to `Work/Initech` costs a run nothing directly — but the answer key
/// is then naming a folder the pipeline will not emit, and a fixture
/// that does that is not a specification of good output any more. It
/// is also how #156 hid: three folders too thin to survive, and the
/// only symptom was the composite ceiling sitting 0.023 low, which
/// took a throwaway probe script to trace back to them. This says
/// which label, and why.
///
/// Compared on [`validate::key`], the validator's own notion of label
/// identity, so `tidy_label` tidying punctuation or case — which a
/// real run reproduces — does not trip it. A segment disappearing
/// does.
#[test]
fn no_fixture_asks_for_folders_the_validator_would_merge() {
  use spindle::group::validate::key;

  for name in FIXTURES {
    let (_, by_label, out) = validated_answer_key(name);
    let wanted = by_label.len();

    // Which expected groups each surviving group drew its files from.
    let origin: BTreeMap<usize, &str> = by_label
      .iter()
      .flat_map(|(label, members)| {
        members.iter().map(move |&i| (i, label.as_str()))
      })
      .collect();
    for g in &out {
      let mut sources: Vec<&str> = g
        .member_indices
        .iter()
        .map(|i| origin[i])
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
      sources.sort_unstable();
      assert!(
        sources.len() <= 1,
        "{name}: the validator merges {sources:?} into one group \
         ({:?}), so no run can score them apart",
        g.label
      );
      let [source] = sources[..] else { continue };
      assert_eq!(
        key(&g.label),
        key(source),
        "{name}: the validator renames {source:?} to {:?}, so the \
         answer key is asking for a folder no run will produce",
        g.label
      );
    }
    assert_eq!(
      out.len(),
      wanted,
      "{name}: {wanted} expected groups came out as {}",
      out.len()
    );
  }
}

/// No hard floor may ask for more than a perfect run could give.
///
/// [`SHAPE_FLOOR`] and [`REUSE_FLOOR`] are the two absolute numbers
/// left in this file — judgement calls rather than a ceiling minus a
/// slack — and a fixture can quietly put either out of reach. One
/// thin folder added to an answer key caps the shape any run can
/// score; a folder the validator declines to keep caps the reuse.
///
/// That is not hypothetical. `REUSE_FLOOR` asserted 0.75 against an
/// achievable 0.481 for as long as the size floor ignored existing
/// folders, so `second_run_reuses_existing_folders` could not have
/// passed however well the model did — and being API-gated, nothing
/// said so until somebody paid for a run to find out.
#[test]
fn no_floor_asks_for_more_than_a_perfect_run_could_score() {
  for name in FIXTURES {
    let (expected, _, out) = validated_answer_key(name);
    let placements = perfect_placements(&expected, &out);
    let perfect =
      eval::score(&expected, &placements, &existing_sizes(name));

    let shape = perfect.granularity.score();
    assert!(
      shape >= SHAPE_FLOOR,
      "{name}: a run that placed every file correctly would score \
       {shape:.3} on folder shape, under the {SHAPE_FLOOR:.3} floor \
       the eval asserts; {} of {} folders hold fewer than {} files",
      perfect.granularity.thin_groups,
      perfect.granularity.total_groups,
      spindle::eval::MIN_GROUP_SIZE
    );

    if let Some(reuse) = perfect.reuse_rate() {
      assert!(
        reuse >= REUSE_FLOOR,
        "{name}: a run that placed every file correctly would reuse \
         {reuse:.3} of its existing folders, under the \
         {REUSE_FLOOR:.3} floor the eval asserts"
      );
    }
  }
}

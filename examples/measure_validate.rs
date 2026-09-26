//! Score a cached grouping response through `validate_groups`, so a
//! change to the validator can be measured against real data instead
//! of argued about. Usage:
//!
//! ```text
//! cargo run --example measure_validate -- \
//!   ~/.cache/spindle/groups.<hash>.v4.json
//! ```
//!
//! The cache identifies members by content hash; only their count
//! matters here, so they are renumbered as they are read.
use spindle::group::validate::validate_groups;
use spindle::model::ProposedGroup;

fn main() {
  let path = std::env::args()
    .nth(1)
    .expect("usage: measure_validate <groups cache json>");
  let text = std::fs::read_to_string(&path).expect("read cache");
  let v: serde_json::Value =
    serde_json::from_str(&text).expect("parse");
  let raw = v
    .get("groups")
    .unwrap_or(&v)
    .as_array()
    .expect("groups array");

  let mut next = 0usize;
  let groups: Vec<ProposedGroup> = raw
    .iter()
    .map(|g| {
      let n = g
        .get("members")
        .and_then(|m| m.as_array())
        .map_or(0, |m| m.len());
      let member_indices = (next..next + n).collect();
      next += n;
      ProposedGroup {
        label: g["label"].as_str().unwrap_or_default().to_string(),
        rationale: String::new(),
        member_indices,
        member_destinations: vec![],
        member_notes: vec![],
      }
    })
    .collect();

  let files: usize =
    groups.iter().map(|g| g.member_indices.len()).sum();
  // A cached grouping carries no record of what was already on disk,
  // so this measures a first run.
  let (out, n) = validate_groups(groups.clone(), &[]);
  report("before", &groups);
  report("after ", &out);
  println!("{files} files, {n:?}");

  // `--labels` prints the surviving tree, which is how you check that
  // a pass did the right thing rather than merely moved the counters.
  if std::env::args().any(|a| a == "--labels") {
    let mut after: Vec<&str> =
      out.iter().map(|g| g.label.as_str()).collect();
    after.sort_unstable();
    for l in after {
      println!("  {l}");
    }
  }
}

fn report(what: &str, groups: &[ProposedGroup]) {
  let mut sizes: Vec<usize> =
    groups.iter().map(|g| g.member_indices.len()).collect();
  sizes.sort_unstable();
  let median = sizes.get(sizes.len() / 2).copied().unwrap_or(0);
  let deep =
    groups.iter().filter(|g| segments(&g.label) >= 3).count();
  println!(
    "{what}: {:>3} groups  median {median}  <=3 files {:>3}  \
     <=4 files {:>3}  depth-3 {:>3}",
    groups.len(),
    sizes.iter().filter(|&&s| s <= 3).count(),
    sizes.iter().filter(|&&s| s <= 4).count(),
    deep,
  );
}

fn segments(label: &str) -> usize {
  label.split('/').filter(|s| !s.trim().is_empty()).count()
}

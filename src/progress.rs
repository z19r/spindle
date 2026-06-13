use std::{
  io::{self, Write},
  time::Duration,
};

fn progress_sleep(d: Duration) {
  tracing::debug!(
    sleep_ms = d.as_millis(),
    sleep_ns = d.as_nanos(),
    "progress progress_sleep"
  );
  #[cfg(not(test))]
  std::thread::sleep(d);
  #[cfg(test)]
  let _ = d;
}

use crossterm::style::{Color, ResetColor, SetForegroundColor};

use crate::pipeline::PipelineEvent;

pub const BAR_WIDTH: usize = 40;
const FILLED: char = '\u{2588}';
const EMPTY: char = '\u{2591}';

/// Total sleep budget for one `rainbow_bar` call: between 400 ms and 1.5 s
/// (exclusive), scaled by how many cells are filled vs bar width.
fn total_sleep_budget(filled: usize, width: usize) -> Duration {
  if filled == 0 || width == 0 {
    tracing::debug!(
      filled,
      width,
      budget_ms = 0u64,
      "progress total_sleep_budget (no sleep)"
    );
    return Duration::ZERO;
  }
  const MIN_MS: u64 = 401;
  const MAX_MS: u64 = 1499;
  let span = MAX_MS - MIN_MS;
  let numer = (filled - 1) as u64;
  let denom = width.saturating_sub(1).max(1) as u64;
  let ms = MIN_MS + span * numer / denom;
  let budget = Duration::from_millis(ms);
  tracing::debug!(
    filled,
    width,
    min_ms = MIN_MS,
    max_ms = MAX_MS,
    span,
    numer,
    denom,
    computed_ms = ms,
    budget_ms = budget.as_millis(),
    "progress total_sleep_budget"
  );
  budget
}

/// Splits `total` into `n` sleep durations, each longer than the previous.
fn monotonic_step_durations(
  n: usize,
  total: Duration,
) -> Vec<Duration> {
  if n == 0 {
    tracing::debug!(n, "progress monotonic_step_durations (empty)");
    return Vec::new();
  }
  let total_ns = total.as_nanos();
  let den = (n * (n + 1)) as u128;
  let mut prev_cum = 0u128;
  let mut steps = Vec::with_capacity(n);
  for k in 1..=n {
    let tri = (k as u128) * (k as u128 + 1);
    let cum = total_ns.saturating_mul(tri) / den;
    let step_ns = cum.saturating_sub(prev_cum);
    prev_cum = cum;
    let ns_u64 = u64::try_from(step_ns).unwrap_or(u64::MAX);
    steps.push(Duration::from_nanos(ns_u64));
  }
  let sum_ns: u128 = steps.iter().map(|d| d.as_nanos()).sum();
  let residual = total_ns.saturating_sub(sum_ns);
  if residual > 0 {
    if let Some(last) = steps.last_mut() {
      let add = u64::try_from(residual).unwrap_or(u64::MAX);
      *last += Duration::from_nanos(add);
    }
  }
  let steps_ms: Vec<u128> =
    steps.iter().map(|d| d.as_millis()).collect();
  let sum_ms: u128 = steps_ms.iter().sum();
  tracing::debug!(
    n,
    total_ms = total.as_millis(),
    residual_ns = residual,
    sum_ms_after_residual = sum_ms,
    ?steps_ms,
    "progress monotonic_step_durations"
  );
  steps
}

const GRADIENT: [(u8, u8, u8); 6] = [
  (0x9a, 0x34, 0x8e), // #9A348E
  (0xda, 0x62, 0x7d), // #DA627D
  (0xfc, 0xa1, 0x7d), // #FCA17D
  (0x86, 0xbb, 0xd8), // #86BBD8
  (0x06, 0x96, 0x9a), // #06969A
  (0x33, 0x65, 0x8a), // #33658A
];

fn gradient_color(t: f64) -> (u8, u8, u8) {
  let t = t.clamp(0.0, 1.0);
  let segments = GRADIENT.len() - 1;
  let scaled = t * segments as f64;
  let idx = (scaled as usize).min(segments - 1);
  let frac = scaled - idx as f64;
  let (r0, g0, b0) = GRADIENT[idx];
  let (r1, g1, b1) = GRADIENT[idx + 1];
  let lerp = |a: u8, b: u8, f: f64| -> u8 {
    (a as f64 + (b as f64 - a as f64) * f).round() as u8
  };
  (lerp(r0, r1, frac), lerp(g0, g1, frac), lerp(b0, b1, frac))
}

pub fn rainbow_bar(ratio: f64, width: usize) -> String {
  use std::fmt::Write as FmtWrite;

  let ratio = ratio.clamp(0.0, 1.0);
  let filled_count = (ratio * width as f64).round() as usize;
  let budget = total_sleep_budget(filled_count, width);
  let sleeps = monotonic_step_durations(filled_count, budget);
  tracing::debug!(
    ratio,
    width,
    filled_count,
    budget_ms = budget.as_millis(),
    sleep_steps = sleeps.len(),
    "progress rainbow_bar"
  );
  let mut sleep_iter = sleeps.into_iter();
  let mut buf = String::with_capacity(width * 20);

  for i in 0..width {
    let (r, g, b) = gradient_color(i as f64 / width as f64);
    if i < filled_count {
      let _ = write!(
        buf,
        "{}{}",
        SetForegroundColor(Color::Rgb { r, g, b }),
        FILLED
      );
      if let Some(d) = sleep_iter.next() {
        tracing::debug!(
          cell = i,
          filled_count,
          sleep_ms = d.as_millis(),
          "progress rainbow_bar filled cell sleep"
        );
        progress_sleep(d);
      }
    } else {
      let _ = write!(
        buf,
        "{}{}",
        SetForegroundColor(Color::Rgb {
          r: r / 3,
          g: g / 3,
          b: b / 3
        }),
        EMPTY
      );
    }
  }
  let _ = write!(buf, "{}", ResetColor);
  buf
}

#[derive(Default)]
pub struct PipelineProgress {
  analyzed: usize,
  total: usize,
}

impl PipelineProgress {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn handle_event(&mut self, event: &PipelineEvent) {
    match event {
      PipelineEvent::ScanComplete { file_count } => {
        let bar = rainbow_bar(1.0, BAR_WIDTH);
        println!("Found {} files to process.", file_count);
        println!("  {}", bar);
      }
      PipelineEvent::FingerprintComplete {
        exact_dupes,
        near_dupes,
        duplicate_details,
        ..
      } => {
        let total = exact_dupes + near_dupes;
        if total > 0 {
          println!(
            "Detected {} duplicate files ({} exact, {} near-duplicate).",
            total, exact_dupes, near_dupes,
          );
          for detail in duplicate_details {
            println!(
              "  {:>8}  {} \u{2194} {}",
              detail.kind, detail.canonical, detail.duplicate
            );
          }
        }
      }
      PipelineEvent::CostEstimated {
        estimated_usd,
        file_count,
      } => {
        println!(
          "Estimated cost: ${:.4} for {} files.",
          estimated_usd, file_count,
        );
      }
      PipelineEvent::AnalysisStarted { file_count, cached } => {
        self.total = *file_count;
        self.analyzed = 0;
        if *cached > 0 {
          println!(
            "Analyzing {} files with AI ({} from cache, {} sent)...",
            file_count,
            cached,
            file_count.saturating_sub(*cached),
          );
        } else {
          println!("Analyzing {} files with AI...", file_count);
        }
        let bar = rainbow_bar(0.0, BAR_WIDTH);
        print!("  {} 0/{}\r", bar, self.total);
        let _ = io::stdout().flush();
      }
      PipelineEvent::FileAnalyzed { filename } => {
        self.analyzed += 1;
        let ratio = if self.total > 0 {
          self.analyzed as f64 / self.total as f64
        } else {
          1.0
        };
        let bar = rainbow_bar(ratio, BAR_WIDTH);
        print!(
          "  {} {}/{} {}\r",
          bar, self.analyzed, self.total, filename
        );
        let _ = io::stdout().flush();
      }
      PipelineEvent::AnalysisComplete {
        succeeded,
        failed,
        failed_files,
      } => {
        let bar = rainbow_bar(1.0, BAR_WIDTH);
        println!(
          "  {} {}/{}                              ",
          bar, self.analyzed, self.total
        );
        println!(
          "AI analysis complete: {} succeeded, {} failed.",
          succeeded, failed,
        );
        for (name, err) in failed_files {
          println!("  \u{2717} {} \u{2014} {}", name, err);
        }
      }
      PipelineEvent::GroupingFailed { error } => {
        println!("  \u{26a0} Semantic grouping failed: {}", error,);
        println!("    Falling back to single group");
      }
      PipelineEvent::GroupingComplete { group_count } => {
        tracing::info!(group_count, "Semantic grouping complete");
      }
      PipelineEvent::PlanReady => {
        tracing::debug!("Plan ready for review");
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn gradient_color_endpoints() {
    let (r, g, b) = gradient_color(0.0);
    assert_eq!((r, g, b), (0x9a, 0x34, 0x8e));
    let (r, g, b) = gradient_color(1.0);
    assert_eq!((r, g, b), (0x33, 0x65, 0x8a));
  }

  #[test]
  fn gradient_color_clamps() {
    assert_eq!(gradient_color(-0.5), gradient_color(0.0));
    assert_eq!(gradient_color(1.5), gradient_color(1.0));
  }

  #[test]
  fn rainbow_bar_empty() {
    let bar = rainbow_bar(0.0, 10);
    assert!(!bar.contains(FILLED));
    assert!(bar.contains(EMPTY));
  }

  #[test]
  fn rainbow_bar_full() {
    let bar = rainbow_bar(1.0, 10);
    assert!(bar.contains(FILLED));
    assert!(!bar.contains(EMPTY));
  }

  #[test]
  fn rainbow_bar_half() {
    let bar = rainbow_bar(0.5, 10);
    let filled = bar.chars().filter(|&c| c == FILLED).count();
    let empty = bar.chars().filter(|&c| c == EMPTY).count();
    assert_eq!(filled, 5);
    assert_eq!(empty, 5);
  }

  #[test]
  fn rainbow_bar_clamps_over_one() {
    let bar = rainbow_bar(1.5, 10);
    let filled = bar.chars().filter(|&c| c == FILLED).count();
    assert_eq!(filled, 10);
  }

  #[test]
  fn rainbow_bar_clamps_negative() {
    let bar = rainbow_bar(-0.5, 10);
    let filled = bar.chars().filter(|&c| c == FILLED).count();
    assert_eq!(filled, 0);
  }

  #[test]
  fn total_sleep_budget_scales_and_is_in_open_band() {
    assert_eq!(total_sleep_budget(0, 10), Duration::ZERO);
    let low = total_sleep_budget(1, 40).as_millis();
    assert!((401..1500).contains(&low));
    let high = total_sleep_budget(40, 40).as_millis();
    assert!(high > 400 && high <= 1499);
    assert!(low < high);
  }

  #[test]
  fn monotonic_steps_sum_to_budget_and_strictly_increase() {
    let budget = total_sleep_budget(7, 20);
    let steps = monotonic_step_durations(7, budget);
    assert_eq!(steps.len(), 7);
    let sum: u128 = steps.iter().map(|d| d.as_nanos()).sum();
    assert_eq!(sum, budget.as_nanos());
    for w in steps.windows(2) {
      assert!(w[0] < w[1]);
    }
  }

  #[test]
  fn handle_event_scan_complete() {
    let mut progress = PipelineProgress::new();
    progress
      .handle_event(&PipelineEvent::ScanComplete { file_count: 42 });
  }

  #[test]
  fn handle_event_analysis_increments() {
    let mut progress = PipelineProgress::new();
    progress.handle_event(&PipelineEvent::AnalysisStarted {
      file_count: 3,
      cached: 0,
    });
    assert_eq!(progress.analyzed, 0);
    assert_eq!(progress.total, 3);

    progress.handle_event(&PipelineEvent::FileAnalyzed {
      filename: "a.png".to_string(),
    });
    assert_eq!(progress.analyzed, 1);

    progress.handle_event(&PipelineEvent::FileAnalyzed {
      filename: "b.png".to_string(),
    });
    assert_eq!(progress.analyzed, 2);
  }

  #[test]
  fn handle_event_analysis_complete_with_failures() {
    let mut progress = PipelineProgress::new();
    progress.total = 3;
    progress.analyzed = 3;
    progress.handle_event(&PipelineEvent::AnalysisComplete {
      succeeded: 2,
      failed: 1,
      failed_files: vec![(
        "broken.png".to_string(),
        "timeout".to_string(),
      )],
    });
  }

  #[test]
  fn handle_event_fingerprint_with_dupes() {
    let mut progress = PipelineProgress::new();
    progress.handle_event(&PipelineEvent::FingerprintComplete {
      file_count: 10,
      exact_dupes: 1,
      near_dupes: 1,
      duplicate_details: vec![
        crate::pipeline::DuplicateDetail {
          canonical: "orig.png".to_string(),
          duplicate: "copy.png".to_string(),
          kind: "exact".to_string(),
        },
        crate::pipeline::DuplicateDetail {
          canonical: "a.jpg".to_string(),
          duplicate: "b.jpg".to_string(),
          kind: "near(3)".to_string(),
        },
      ],
    });
  }
}

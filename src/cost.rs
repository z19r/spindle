const AVG_IMAGE_INPUT_TOKENS: u64 = 1600;
const AVG_TEXT_INPUT_TOKENS: u64 = 300;
const AVG_OUTPUT_TOKENS: u64 = 200;
const GROUP_PROMPT_TOKENS_PER_FILE: u64 = 150;
const GROUP_OUTPUT_TOKENS: u64 = 1000;

/// USD per million input/output tokens for the configured model family.
/// Falls back to Opus-tier pricing for unknown ids (the default model is
/// Opus-tier, and overestimating is safer than underestimating).
fn pricing_per_mtok(model: &str) -> (f64, f64) {
  if model.contains("haiku") {
    (1.0, 5.0)
  } else if model.contains("sonnet") {
    (3.0, 15.0)
  } else if model.contains("fable") || model.contains("mythos") {
    (10.0, 50.0)
  } else {
    // opus and unknown
    (5.0, 25.0)
  }
}

pub struct CostEstimate {
  pub describe_calls: usize,
  pub group_calls: usize,
  pub estimated_input_tokens: u64,
  pub estimated_output_tokens: u64,
  pub estimated_cost_usd: f64,
}

pub fn estimate_cost(file_count: usize, model: &str) -> CostEstimate {
  let (input_per_mtok, output_per_mtok) = pricing_per_mtok(model);

  let describe_input = file_count as u64
    * (AVG_IMAGE_INPUT_TOKENS + AVG_TEXT_INPUT_TOKENS);
  let describe_output = file_count as u64 * AVG_OUTPUT_TOKENS;

  let group_input = file_count as u64 * GROUP_PROMPT_TOKENS_PER_FILE
    + AVG_TEXT_INPUT_TOKENS;
  let group_output = GROUP_OUTPUT_TOKENS;

  let total_input = describe_input + group_input;
  let total_output = describe_output + group_output;

  let cost = (total_input as f64 / 1_000_000.0) * input_per_mtok
    + (total_output as f64 / 1_000_000.0) * output_per_mtok;

  CostEstimate {
    describe_calls: file_count,
    group_calls: 1,
    estimated_input_tokens: total_input,
    estimated_output_tokens: total_output,
    estimated_cost_usd: cost,
  }
}

impl std::fmt::Display for CostEstimate {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "Estimated cost: ${:.4} ({} describe calls + {} group call, ~{}k input + ~{}k output tokens)",
      self.estimated_cost_usd,
      self.describe_calls,
      self.group_calls,
      self.estimated_input_tokens / 1000,
      self.estimated_output_tokens / 1000,
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const OPUS: &str = "claude-opus-5";

  #[test]
  fn zero_files_minimal_cost() {
    let est = estimate_cost(0, OPUS);

    assert_eq!(est.describe_calls, 0);
    assert_eq!(est.group_calls, 1);
    assert!(est.estimated_cost_usd < 0.05);
  }

  #[test]
  fn cost_scales_with_file_count() {
    let est_10 = estimate_cost(10, OPUS);
    let est_100 = estimate_cost(100, OPUS);

    assert!(
      est_100.estimated_cost_usd > est_10.estimated_cost_usd * 5.0
    );
  }

  #[test]
  fn single_file_reasonable_cost() {
    let est = estimate_cost(1, OPUS);

    assert!(est.estimated_cost_usd > 0.0001);
    assert!(est.estimated_cost_usd < 0.10);
  }

  #[test]
  fn hundred_files_bounded_cost() {
    let est = estimate_cost(100, OPUS);

    assert!(est.estimated_cost_usd < 2.0);
  }

  #[test]
  fn pricing_varies_by_model_family() {
    let haiku = estimate_cost(100, "claude-haiku-4-5");
    let sonnet = estimate_cost(100, "claude-sonnet-5");
    let opus = estimate_cost(100, OPUS);
    let fable = estimate_cost(100, "claude-fable-5");

    assert!(haiku.estimated_cost_usd < sonnet.estimated_cost_usd);
    assert!(sonnet.estimated_cost_usd < opus.estimated_cost_usd);
    assert!(opus.estimated_cost_usd < fable.estimated_cost_usd);
  }

  #[test]
  fn unknown_model_uses_opus_pricing() {
    let unknown = estimate_cost(10, "some-future-model");
    let opus = estimate_cost(10, OPUS);

    assert_eq!(
      unknown.estimated_cost_usd.to_bits(),
      opus.estimated_cost_usd.to_bits()
    );
  }

  #[test]
  fn display_format_includes_dollar_sign() {
    let est = estimate_cost(5, OPUS);
    let display = format!("{}", est);

    assert!(display.starts_with("Estimated cost: $"));
    assert!(display.contains("describe calls"));
  }
}

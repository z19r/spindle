const INPUT_COST_PER_MTOK: f64 = 3.0;
const OUTPUT_COST_PER_MTOK: f64 = 15.0;

const AVG_IMAGE_INPUT_TOKENS: u64 = 1600;
const AVG_TEXT_INPUT_TOKENS: u64 = 300;
const AVG_OUTPUT_TOKENS: u64 = 200;
const GROUP_PROMPT_TOKENS_PER_FILE: u64 = 150;
const GROUP_OUTPUT_TOKENS: u64 = 1000;

pub struct CostEstimate {
  pub describe_calls: usize,
  pub group_calls: usize,
  pub estimated_input_tokens: u64,
  pub estimated_output_tokens: u64,
  pub estimated_cost_usd: f64,
}

pub fn estimate_cost(file_count: usize) -> CostEstimate {
  let describe_input = file_count as u64
    * (AVG_IMAGE_INPUT_TOKENS + AVG_TEXT_INPUT_TOKENS);
  let describe_output = file_count as u64 * AVG_OUTPUT_TOKENS;

  let group_input = file_count as u64 * GROUP_PROMPT_TOKENS_PER_FILE
    + AVG_TEXT_INPUT_TOKENS;
  let group_output = GROUP_OUTPUT_TOKENS;

  let total_input = describe_input + group_input;
  let total_output = describe_output + group_output;

  let cost = (total_input as f64 / 1_000_000.0) * INPUT_COST_PER_MTOK
    + (total_output as f64 / 1_000_000.0) * OUTPUT_COST_PER_MTOK;

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

  #[test]
  fn zero_files_minimal_cost() {
    let est = estimate_cost(0);

    assert_eq!(est.describe_calls, 0);
    assert_eq!(est.group_calls, 1);
    assert!(est.estimated_cost_usd < 0.02);
  }

  #[test]
  fn cost_scales_with_file_count() {
    let est_10 = estimate_cost(10);
    let est_100 = estimate_cost(100);

    assert!(
      est_100.estimated_cost_usd > est_10.estimated_cost_usd * 5.0
    );
  }

  #[test]
  fn single_file_reasonable_cost() {
    let est = estimate_cost(1);

    assert!(est.estimated_cost_usd > 0.0001);
    assert!(est.estimated_cost_usd < 0.10);
  }

  #[test]
  fn hundred_files_under_a_dollar() {
    let est = estimate_cost(100);

    assert!(est.estimated_cost_usd < 1.0);
  }

  #[test]
  fn display_format_includes_dollar_sign() {
    let est = estimate_cost(5);
    let display = format!("{}", est);

    assert!(display.starts_with("Estimated cost: $"));
    assert!(display.contains("describe calls"));
  }
}

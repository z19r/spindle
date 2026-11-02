use ratatui::{
  layout::Rect,
  style::{Color, Modifier, Style},
  text::{Line, Span},
  widgets::{Block, BorderType, Gauge, Padding, Paragraph},
  Frame,
};

use crate::pipeline::PipelineEvent;

// One palette with the review screen, so both read on light and dark
// terminals alike (body text is the terminal's default foreground).
use super::review::theme;

const SPINNER: [&str; 10] =
  ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Live state for the pipeline phase of the TUI (scan → fingerprint →
/// analyze → group), fed by `PipelineEvent`s.
#[derive(Default)]
pub struct PipelineTuiState {
  scanned: Option<usize>,
  exact_dupes: Option<usize>,
  near_dupes: Option<usize>,
  estimated_usd: Option<f64>,
  to_analyze: Option<usize>,
  cached: usize,
  analyzed: usize,
  failed: usize,
  current_file: Option<String>,
  analysis_done: bool,
  groups: Option<usize>,
  grouping_error: Option<String>,
  plan_ready: bool,
  spinner: usize,
}

impl PipelineTuiState {
  pub fn tick(&mut self) {
    self.spinner = (self.spinner + 1) % SPINNER.len();
  }

  pub fn is_done(&self) -> bool {
    self.plan_ready
  }

  pub fn handle_event(&mut self, event: &PipelineEvent) {
    match event {
      PipelineEvent::ScanComplete { file_count } => {
        self.scanned = Some(*file_count);
      }
      PipelineEvent::FingerprintComplete {
        exact_dupes,
        near_dupes,
        ..
      } => {
        self.exact_dupes = Some(*exact_dupes);
        self.near_dupes = Some(*near_dupes);
      }
      PipelineEvent::CostEstimated { estimated_usd, .. } => {
        self.estimated_usd = Some(*estimated_usd);
      }
      PipelineEvent::AnalysisStarted { file_count, cached } => {
        self.to_analyze = Some(*file_count);
        self.cached = *cached;
      }
      PipelineEvent::FileAnalyzed { filename } => {
        self.analyzed += 1;
        self.current_file = Some(filename.clone());
      }
      PipelineEvent::AnalysisComplete {
        succeeded, failed, ..
      } => {
        self.analyzed = succeeded + failed;
        self.failed = *failed;
        self.analysis_done = true;
        self.current_file = None;
      }
      PipelineEvent::GroupingComplete { group_count, .. } => {
        self.groups = Some(*group_count);
      }
      PipelineEvent::GroupingFailed { error } => {
        self.grouping_error = Some(error.clone());
      }
      PipelineEvent::GroupingIncomplete {
        reason,
        failed_calls,
        unplaced,
      } => {
        self.grouping_error = Some(match reason {
          Some(reason) => {
            format!("{reason} ({unplaced} files unplaced)")
          }
          None => format!(
            "{failed_calls} grouping call(s) failed \
             ({unplaced} files unplaced)"
          ),
        });
      }
      PipelineEvent::LabelsNormalised { .. }
      | PipelineEvent::RoutingComplete { .. } => {}
      PipelineEvent::PlanReady => {
        self.plan_ready = true;
      }
    }
  }

  fn spinner_frame(&self) -> &'static str {
    SPINNER[self.spinner]
  }
}

pub fn render_pipeline(frame: &mut Frame, state: &PipelineTuiState) {
  let area = frame.area();
  let width = (area.width * 70 / 100)
    .clamp(40, 76)
    .min(area.width.saturating_sub(2));
  let height = 14.min(area.height.saturating_sub(2));
  let x = (area.width.saturating_sub(width)) / 2;
  let y = (area.height.saturating_sub(height)) / 2;
  let panel = Rect::new(x, y, width, height);

  let mut lines: Vec<Line<'static>> = vec![Line::from("")];

  let spin = state.spinner_frame();
  let stage = |done: bool,
               active: bool,
               label: String,
               lines: &mut Vec<Line<'static>>| {
    let (icon, style) = if done {
      ("✓".to_string(), Style::default().fg(theme::ok()))
    } else if active {
      (spin.to_string(), Style::default().fg(theme::accent()))
    } else {
      ("·".to_string(), Style::default().fg(theme::subtle()))
    };
    let text_style = if done || active {
      Style::default().fg(theme::text())
    } else {
      Style::default().fg(theme::subtle())
    };
    lines.push(Line::from(vec![
      Span::styled(format!("  {icon} "), style),
      Span::styled(label, text_style),
    ]));
  };

  let scanned = state.scanned;
  stage(
    scanned.is_some(),
    scanned.is_none(),
    match scanned {
      Some(n) => format!("Scanned {n} files"),
      None => "Scanning…".to_string(),
    },
    &mut lines,
  );

  let fingerprinted = state.exact_dupes.is_some();
  stage(
    fingerprinted,
    scanned.is_some() && !fingerprinted,
    match (state.exact_dupes, state.near_dupes) {
      (Some(exact), Some(near)) => format!(
        "Fingerprinted — {exact} exact, {near} similar duplicates"
      ),
      _ => "Fingerprinting…".to_string(),
    },
    &mut lines,
  );

  // Analysis line with gauge.
  let analyzing = state.to_analyze.is_some() && !state.analysis_done;
  stage(
    state.analysis_done,
    analyzing,
    match (state.to_analyze, state.estimated_usd) {
      (Some(total), Some(usd)) => format!(
        "Analyzing content — {}/{} (est. ${usd:.2}, {} cached)",
        state.analyzed, total, state.cached
      ),
      (Some(total), None) => {
        format!("Analyzing content — {}/{}", state.analyzed, total)
      }
      _ => "Analyze content".to_string(),
    },
    &mut lines,
  );
  if let Some(file) = &state.current_file {
    lines.push(Line::from(Span::styled(
      format!("      {file}"),
      Style::default().fg(theme::subtle()),
    )));
  }
  if state.failed > 0 {
    lines.push(Line::from(Span::styled(
      format!("      {} files failed analysis", state.failed),
      Style::default().fg(theme::warn()),
    )));
  }

  stage(
    state.groups.is_some(),
    state.analysis_done && state.groups.is_none(),
    match (state.groups, &state.grouping_error) {
      (Some(n), _) => format!("Grouped into {n} folders"),
      (None, Some(_)) => {
        "Grouping failed — falling back to one group".to_string()
      }
      _ => "Group by topic".to_string(),
    },
    &mut lines,
  );

  lines.push(Line::from(""));

  let block = Block::bordered()
    .border_type(BorderType::Rounded)
    .title_top(Line::from(vec![Span::styled(
      " spindle ",
      Style::default()
        .fg(theme::text())
        .bg(theme::accent())
        .add_modifier(Modifier::BOLD),
    )]))
    .border_style(Style::default().fg(theme::accent()))
    .padding(Padding::new(1, 1, 0, 0));
  frame.render_widget(Paragraph::new(lines).block(block), panel);

  // Slim gauge under the panel while analysis runs.
  if let Some(total) = state.to_analyze {
    if !state.analysis_done && total > 0 && y + height < area.height {
      let gauge_area =
        Rect::new(x, y + height, width, 1.min(area.height));
      let ratio =
        (state.analyzed as f64 / total as f64).clamp(0.0, 1.0);
      let gauge = Gauge::default()
        .gauge_style(
          Style::default().fg(theme::ok()).bg(Color::Reset),
        )
        .ratio(ratio)
        .label("");
      frame.render_widget(gauge, gauge_area);
    }
  }
}

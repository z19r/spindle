use ratatui::{
  layout::{Constraint, Layout, Rect},
  style::{Color, Style},
  widgets::{Block, Borders, Gauge, Paragraph},
  Frame,
};

pub struct ProgressState {
  total_files: usize,
  completed: usize,
  current_file: Option<String>,
  stage: Stage,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stage {
  Scanning,
  Fingerprinting,
  Analyzing,
  Grouping,
}

impl std::fmt::Display for Stage {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Stage::Scanning => write!(f, "Scanning"),
      Stage::Fingerprinting => write!(f, "Fingerprinting"),
      Stage::Analyzing => write!(f, "Analyzing"),
      Stage::Grouping => write!(f, "Grouping"),
    }
  }
}

impl ProgressState {
  pub fn new(total_files: usize) -> Self {
    Self {
      total_files,
      completed: 0,
      current_file: None,
      stage: Stage::Scanning,
    }
  }

  pub fn total_files(&self) -> usize {
    self.total_files
  }

  pub fn completed(&self) -> usize {
    self.completed
  }

  pub fn increment(&mut self) {
    if self.total_files > 0 {
      self.completed = (self.completed + 1).min(self.total_files);
    }
  }

  pub fn set_current_file(&mut self, name: String) {
    self.current_file = Some(name);
  }

  pub fn set_stage(&mut self, stage: Stage) {
    self.stage = stage;
  }

  pub fn ratio(&self) -> f64 {
    if self.total_files == 0 {
      return 0.0;
    }
    self.completed as f64 / self.total_files as f64
  }
}

pub fn render(frame: &mut Frame, state: &ProgressState) {
  let area = frame.area();
  let chunks = Layout::vertical([
    Constraint::Length(3),
    Constraint::Length(3),
    Constraint::Length(3),
    Constraint::Min(0),
  ])
  .split(area);

  render_title(frame, chunks[0]);
  render_gauge(frame, chunks[1], state);
  render_status(frame, chunks[2], state);
}

fn render_title(frame: &mut Frame, area: Rect) {
  let title = Paragraph::new(" spindle ")
    .style(Style::default().fg(Color::Cyan))
    .block(Block::default().borders(Borders::BOTTOM));
  frame.render_widget(title, area);
}

fn render_gauge(
  frame: &mut Frame,
  area: Rect,
  state: &ProgressState,
) {
  let label = format!(
    "{} — {}/{}",
    state.stage, state.completed, state.total_files
  );
  let gauge = Gauge::default()
    .block(Block::default().borders(Borders::ALL).title("Progress"))
    .gauge_style(Style::default().fg(Color::Green))
    .ratio(state.ratio())
    .label(label);
  frame.render_widget(gauge, area);
}

fn render_status(
  frame: &mut Frame,
  area: Rect,
  state: &ProgressState,
) {
  let text = state.current_file.as_deref().unwrap_or("Waiting...");
  let status = Paragraph::new(text)
    .block(Block::default().borders(Borders::ALL).title("Current"));
  frame.render_widget(status, area);
}

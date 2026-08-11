mod progress;
mod review;

pub use progress::{PipelineTuiState, ProgressState, Stage};
pub use review::{Mode, ReviewAction, ReviewMode, ReviewState};

use anyhow::Result;
use crossterm::{
  event::{self, Event, KeyCode, KeyModifiers},
  terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen,
    LeaveAlternateScreen,
  },
  ExecutableCommand,
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::path::PathBuf;

pub enum Screen {
  Progress(ProgressState),
  Review(Box<ReviewState>),
  Done,
}

pub struct App {
  screen: Screen,
  should_quit: bool,
}

impl App {
  pub fn new_progress(total_files: usize) -> Self {
    Self {
      screen: Screen::Progress(ProgressState::new(total_files)),
      should_quit: false,
    }
  }

  pub fn new_review(state: ReviewState) -> Self {
    Self {
      screen: Screen::Review(Box::new(state)),
      should_quit: false,
    }
  }

  pub fn should_quit(&self) -> bool {
    self.should_quit
  }

  pub fn screen(&self) -> &Screen {
    &self.screen
  }

  pub fn set_screen(&mut self, screen: Screen) {
    self.screen = screen;
  }

  pub fn handle_key(
    &mut self,
    code: KeyCode,
    modifiers: KeyModifiers,
  ) {
    if code == KeyCode::Char('c')
      && modifiers.contains(KeyModifiers::CONTROL)
    {
      self.should_quit = true;
      return;
    }

    let in_text_mode = matches!(&self.screen, Screen::Review(s) if *s.mode() != Mode::Normal);
    if code == KeyCode::Char('q') && !in_text_mode {
      self.should_quit = true;
      return;
    }

    match &mut self.screen {
      Screen::Progress(_) => {}
      Screen::Review(state) => state.handle_key(code),
      Screen::Done => {
        self.should_quit = true;
      }
    }
  }
}

pub enum ProgressEvent {
  SetStage(Stage),
  SetTotal(usize),
  FileProcessed(String),
  Done,
}

pub fn run_progress(
  mut rx: tokio::sync::mpsc::Receiver<ProgressEvent>,
) -> Result<()> {
  enable_raw_mode()?;
  io::stdout().execute(EnterAlternateScreen)?;

  let backend = CrosstermBackend::new(io::stdout());
  let mut terminal = Terminal::new(backend)?;
  let mut state = ProgressState::new(0);

  loop {
    terminal.draw(|frame| progress::render(frame, &state))?;

    match rx.try_recv() {
      Ok(ProgressEvent::SetStage(stage)) => state.set_stage(stage),
      Ok(ProgressEvent::SetTotal(total)) => {
        state = ProgressState::new(total)
      }
      Ok(ProgressEvent::FileProcessed(name)) => {
        state.set_current_file(name);
        state.increment();
      }
      Ok(ProgressEvent::Done) => break,
      Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
        break
      }
      Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
    }

    if event::poll(std::time::Duration::from_millis(50))? {
      if let Event::Key(key) = event::read()? {
        if key.code == KeyCode::Char('q')
          || (key.code == KeyCode::Char('c')
            && key.modifiers.contains(KeyModifiers::CONTROL))
        {
          break;
        }
      }
    }
  }

  disable_raw_mode()?;
  io::stdout().execute(LeaveAlternateScreen)?;
  Ok(())
}

/// Drive the pipeline phase inside the TUI: a stage checklist with
/// spinner, live analysis gauge, and cost estimate, fed by
/// `PipelineEvent`s until the plan is ready (or the sender drops).
/// Ctrl-C aborts the whole process.
pub fn run_pipeline_progress(
  mut rx: tokio::sync::mpsc::Receiver<crate::pipeline::PipelineEvent>,
) -> Result<()> {
  enable_raw_mode()?;
  io::stdout().execute(EnterAlternateScreen)?;

  let backend = CrosstermBackend::new(io::stdout());
  let mut terminal = Terminal::new(backend)?;
  let mut state = PipelineTuiState::default();
  let mut disconnected = false;

  loop {
    state.tick();
    terminal
      .draw(|frame| progress::render_pipeline(frame, &state))?;

    loop {
      match rx.try_recv() {
        Ok(event) => state.handle_event(&event),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
          disconnected = true;
          break;
        }
      }
    }

    if state.is_done() || disconnected {
      // One last frame so the final ✓ is visible for a beat.
      terminal
        .draw(|frame| progress::render_pipeline(frame, &state))?;
      break;
    }

    if event::poll(std::time::Duration::from_millis(80))? {
      if let Event::Key(key) = event::read()? {
        if key.code == KeyCode::Char('c')
          && key.modifiers.contains(KeyModifiers::CONTROL)
        {
          disable_raw_mode()?;
          io::stdout().execute(LeaveAlternateScreen)?;
          std::process::exit(130);
        }
      }
    }
  }

  disable_raw_mode()?;
  io::stdout().execute(LeaveAlternateScreen)?;
  Ok(())
}

pub fn run_review(
  state: ReviewState,
) -> Result<(ReviewAction, ReviewState)> {
  enable_raw_mode()?;
  io::stdout().execute(EnterAlternateScreen)?;

  let backend = CrosstermBackend::new(io::stdout());
  let mut terminal = Terminal::new(backend)?;
  let mut app = App::new_review(state);

  let result = loop {
    if let Screen::Review(state) = &mut app.screen {
      state.poll_image_decode();
    }

    terminal.draw(|frame| match &mut app.screen {
      Screen::Review(state) => review::render(frame, state),
      Screen::Progress(state) => progress::render(frame, state),
      Screen::Done => {}
    })?;

    if event::poll(std::time::Duration::from_millis(50))? {
      if let Event::Key(key) = event::read()? {
        app.handle_key(key.code, key.modifiers);

        if app.should_quit() {
          break ReviewAction::Quit;
        }

        if let Screen::Review(state) = &app.screen {
          if let Some(action) = state.pending_action() {
            break action;
          }
        }
      }
    }
  };

  disable_raw_mode()?;
  io::stdout().execute(LeaveAlternateScreen)?;

  let review_state = match app.screen {
    Screen::Review(s) => *s,
    _ => ReviewState::new(
      vec![],
      vec![],
      PathBuf::new(),
      None,
      ReviewMode::Organize,
      None,
    ),
  };

  Ok((result, review_state))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::FileGroup;
  use std::path::PathBuf;

  fn make_review_state() -> ReviewState {
    ReviewState::new(
      vec![FileGroup {
        id: 0,
        label: "Test Group".to_string(),
        rationale: "Testing".to_string(),
        members: vec![0, 1],
        member_destinations: vec![],
        suggested_path: PathBuf::from("/out/test"),
      }],
      vec![],
      PathBuf::from("/out"),
      None,
      ReviewMode::Organize,
      None,
    )
  }

  #[test]
  fn app_starts_without_quitting() {
    let app = App::new_progress(10);

    assert!(!app.should_quit());
  }

  #[test]
  fn quit_on_q_key() {
    let mut app = App::new_review(make_review_state());

    app.handle_key(KeyCode::Char('q'), KeyModifiers::NONE);

    assert!(app.should_quit());
  }

  #[test]
  fn quit_on_ctrl_c() {
    let mut app = App::new_review(make_review_state());

    app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL);

    assert!(app.should_quit());
  }

  #[test]
  fn progress_state_tracks_total() {
    let state = ProgressState::new(42);

    assert_eq!(state.total_files(), 42);
    assert_eq!(state.completed(), 0);
  }

  #[test]
  fn progress_state_increments() {
    let mut state = ProgressState::new(10);

    state.increment();
    state.increment();

    assert_eq!(state.completed(), 2);
  }

  #[test]
  fn review_state_starts_at_first_group() {
    let state = make_review_state();

    assert_eq!(state.selected_index(), 0);
  }

  #[test]
  fn review_navigate_down_wraps() {
    let mut state = ReviewState::new(
      vec![
        FileGroup {
          id: 0,
          label: "A".to_string(),
          rationale: "".to_string(),
          members: vec![0],
          member_destinations: vec![],
          suggested_path: PathBuf::from("/a"),
        },
        FileGroup {
          id: 1,
          label: "B".to_string(),
          rationale: "".to_string(),
          members: vec![1],
          member_destinations: vec![],
          suggested_path: PathBuf::from("/b"),
        },
      ],
      vec![],
      PathBuf::from("/out"),
      None,
      ReviewMode::Organize,
      None,
    );

    state.handle_key(KeyCode::Char('j'));
    assert_eq!(state.selected_index(), 1);

    state.handle_key(KeyCode::Char('j'));
    assert_eq!(state.selected_index(), 0);
  }

  #[test]
  fn review_space_toggles_approval() {
    let mut state = make_review_state();

    assert!(state.is_approved(0));

    state.handle_key(KeyCode::Char(' '));

    assert!(!state.is_approved(0));

    state.handle_key(KeyCode::Char(' '));

    assert!(state.is_approved(0));
  }

  #[test]
  fn review_x_confirms_then_executes() {
    let mut state = make_review_state();

    state.handle_key(KeyCode::Char('x'));
    assert_eq!(state.pending_action(), None);

    state.handle_key(KeyCode::Enter);
    assert_eq!(state.pending_action(), Some(ReviewAction::Execute));
  }

  #[test]
  fn progress_set_current_file() {
    let mut state = ProgressState::new(5);

    state.set_current_file("photo.jpg".to_string());

    assert_eq!(state.completed(), 0);
  }

  #[test]
  fn progress_set_stage() {
    let mut state = ProgressState::new(5);

    state.set_stage(Stage::Analyzing);
    state.set_stage(Stage::Grouping);
    state.set_stage(Stage::Fingerprinting);
  }

  #[test]
  fn progress_ratio_calculates_correctly() {
    let mut state = ProgressState::new(4);
    assert!((state.ratio() - 0.0).abs() < f64::EPSILON);

    state.increment();
    assert!((state.ratio() - 0.25).abs() < f64::EPSILON);

    state.increment();
    state.increment();
    state.increment();
    assert!((state.ratio() - 1.0).abs() < f64::EPSILON);
  }

  #[test]
  fn progress_ratio_zero_total_returns_zero() {
    let state = ProgressState::new(0);

    assert!((state.ratio() - 0.0).abs() < f64::EPSILON);
  }

  #[test]
  fn progress_increment_clamps_at_total() {
    let mut state = ProgressState::new(2);

    state.increment();
    state.increment();
    state.increment();

    assert_eq!(state.completed(), 2);
  }

  #[test]
  fn progress_increment_zero_total_is_noop() {
    let mut state = ProgressState::new(0);

    state.increment();

    assert_eq!(state.completed(), 0);
  }

  #[test]
  fn stage_display() {
    assert_eq!(format!("{}", Stage::Scanning), "Scanning");
    assert_eq!(
      format!("{}", Stage::Fingerprinting),
      "Fingerprinting"
    );
    assert_eq!(format!("{}", Stage::Analyzing), "Analyzing");
    assert_eq!(format!("{}", Stage::Grouping), "Grouping");
  }

  #[test]
  fn review_navigate_up_wraps() {
    let mut state = ReviewState::new(
      vec![
        FileGroup {
          id: 0,
          label: "A".to_string(),
          rationale: "".to_string(),
          members: vec![0],
          member_destinations: vec![],
          suggested_path: PathBuf::from("/a"),
        },
        FileGroup {
          id: 1,
          label: "B".to_string(),
          rationale: "".to_string(),
          members: vec![1],
          member_destinations: vec![],
          suggested_path: PathBuf::from("/b"),
        },
      ],
      vec![],
      PathBuf::from("/out"),
      None,
      ReviewMode::Organize,
      None,
    );

    state.handle_key(KeyCode::Char('k'));
    assert_eq!(state.selected_index(), 1);

    state.handle_key(KeyCode::Char('k'));
    assert_eq!(state.selected_index(), 0);
  }

  #[test]
  fn review_approved_groups_returns_only_approved() {
    let mut state = ReviewState::new(
      vec![
        FileGroup {
          id: 0,
          label: "A".to_string(),
          rationale: "".to_string(),
          members: vec![0],
          member_destinations: vec![],
          suggested_path: PathBuf::from("/a"),
        },
        FileGroup {
          id: 1,
          label: "B".to_string(),
          rationale: "".to_string(),
          members: vec![1],
          member_destinations: vec![],
          suggested_path: PathBuf::from("/b"),
        },
      ],
      vec![],
      PathBuf::from("/out"),
      None,
      ReviewMode::Organize,
      None,
    );

    state.handle_key(KeyCode::Char(' '));

    let approved = state.approved_groups();
    assert_eq!(approved.len(), 1);
    assert_eq!(approved[0].label, "B");
  }

  #[test]
  fn app_done_screen_quits_on_any_key() {
    let mut app = App::new_progress(0);
    app.set_screen(Screen::Done);

    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);

    assert!(app.should_quit());
  }

  #[test]
  fn is_approved_out_of_bounds_returns_false() {
    let state = make_review_state();

    assert!(!state.is_approved(999));
  }

  #[test]
  fn review_arrow_keys_navigate() {
    let mut state = ReviewState::new(
      vec![
        FileGroup {
          id: 0,
          label: "A".to_string(),
          rationale: "".to_string(),
          members: vec![0],
          member_destinations: vec![],
          suggested_path: PathBuf::from("/a"),
        },
        FileGroup {
          id: 1,
          label: "B".to_string(),
          rationale: "".to_string(),
          members: vec![1],
          member_destinations: vec![],
          suggested_path: PathBuf::from("/b"),
        },
      ],
      vec![],
      PathBuf::from("/out"),
      None,
      ReviewMode::Organize,
      None,
    );

    state.handle_key(KeyCode::Down);
    assert_eq!(state.selected_index(), 1);

    state.handle_key(KeyCode::Up);
    assert_eq!(state.selected_index(), 0);
  }
}

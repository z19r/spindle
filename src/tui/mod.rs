mod progress;
mod review;

pub use progress::PipelineTuiState;
pub use review::theme;
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

/// The ratatui terminal type the review screens draw into.
pub type Term = Terminal<CrosstermBackend<io::Stdout>>;

/// Raw mode plus the alternate screen, held for the whole run so the
/// progress display flows straight into the review without the
/// terminal flashing back to the shell in between. Dropping it
/// restores the terminal, including on early error returns.
pub struct TerminalSession {
  pub terminal: Term,
}

impl TerminalSession {
  pub fn enter() -> Result<Self> {
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let terminal =
      Terminal::new(CrosstermBackend::new(io::stdout()))?;
    Ok(Self { terminal })
  }
}

impl Drop for TerminalSession {
  fn drop(&mut self) {
    let _ = disable_raw_mode();
    let _ = io::stdout().execute(LeaveAlternateScreen);
  }
}

/// Drive the pipeline phase inside the TUI: a stage checklist with
/// spinner, live analysis gauge, and cost estimate, fed by
/// `PipelineEvent`s until the plan is ready (or the sender drops).
/// Ctrl-C aborts the whole process.
pub fn run_pipeline_progress(
  mut rx: tokio::sync::mpsc::Receiver<crate::pipeline::PipelineEvent>,
  terminal: &mut Term,
) -> Result<()> {
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
          // Restore the terminal by hand: process::exit skips Drop.
          let _ = disable_raw_mode();
          let _ = io::stdout().execute(LeaveAlternateScreen);
          std::process::exit(130);
        }
      }
    }
  }

  Ok(())
}

/// Whether a key ends the review without executing: Ctrl-C always,
/// `q` only on the main screen. Inside a prompt `q` is typed, and in a
/// modal it closes the modal.
fn quits(
  state: &ReviewState,
  code: KeyCode,
  modifiers: KeyModifiers,
) -> bool {
  let ctrl_c = code == KeyCode::Char('c')
    && modifiers.contains(KeyModifiers::CONTROL);
  ctrl_c
    || (code == KeyCode::Char('q') && *state.mode() == Mode::Normal)
}

pub fn run_review(
  mut state: ReviewState,
  terminal: &mut Term,
) -> Result<(ReviewAction, ReviewState)> {
  let action = loop {
    state.poll_image_decode();
    // Before the draw, so a frame that came due during the last
    // `event::poll` is the one that goes up.
    state.advance_animation();

    terminal.draw(|frame| review::render(frame, &mut state))?;

    if !event::poll(std::time::Duration::from_millis(50))? {
      continue;
    }
    let Event::Key(key) = event::read()? else {
      continue;
    };
    if quits(&state, key.code, key.modifiers) {
      break ReviewAction::Quit;
    }
    state.handle_key(key.code);
    if let Some(action) = state.pending_action() {
      break action;
    }
  };
  Ok((action, state))
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
        member_notes: vec![],
      }],
      vec![],
      PathBuf::from("/out"),
      None,
      ReviewMode::Organize,
    )
  }

  #[test]
  fn q_and_ctrl_c_quit_from_the_main_screen() {
    let state = make_review_state();

    assert!(quits(&state, KeyCode::Char('q'), KeyModifiers::NONE));
    assert!(quits(&state, KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(!quits(&state, KeyCode::Char('c'), KeyModifiers::NONE));
  }

  /// A group named "Quarterly" must be typeable: inside a prompt `q`
  /// is a letter, and only Ctrl-C still leaves.
  #[test]
  fn q_is_typed_inside_a_prompt() {
    let mut state = make_review_state();
    state.handle_key(KeyCode::Char('r'));

    assert!(!quits(&state, KeyCode::Char('q'), KeyModifiers::NONE));
    assert!(quits(&state, KeyCode::Char('c'), KeyModifiers::CONTROL));
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
          member_notes: vec![],
        },
        FileGroup {
          id: 1,
          label: "B".to_string(),
          rationale: "".to_string(),
          members: vec![1],
          member_destinations: vec![],
          suggested_path: PathBuf::from("/b"),
          member_notes: vec![],
        },
      ],
      vec![],
      PathBuf::from("/out"),
      None,
      ReviewMode::Organize,
    );

    state.handle_key(KeyCode::Down);
    assert_eq!(state.selected_index(), 1);

    state.handle_key(KeyCode::Down);
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
          member_notes: vec![],
        },
        FileGroup {
          id: 1,
          label: "B".to_string(),
          rationale: "".to_string(),
          members: vec![1],
          member_destinations: vec![],
          suggested_path: PathBuf::from("/b"),
          member_notes: vec![],
        },
      ],
      vec![],
      PathBuf::from("/out"),
      None,
      ReviewMode::Organize,
    );

    state.handle_key(KeyCode::Up);
    assert_eq!(state.selected_index(), 1);

    state.handle_key(KeyCode::Up);
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
          member_notes: vec![],
        },
        FileGroup {
          id: 1,
          label: "B".to_string(),
          rationale: "".to_string(),
          members: vec![1],
          member_destinations: vec![],
          suggested_path: PathBuf::from("/b"),
          member_notes: vec![],
        },
      ],
      vec![],
      PathBuf::from("/out"),
      None,
      ReviewMode::Organize,
    );

    state.handle_key(KeyCode::Char(' '));

    let approved = state.approved_groups();
    assert_eq!(approved.len(), 1);
    assert_eq!(approved[0].label, "B");
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
          member_notes: vec![],
        },
        FileGroup {
          id: 1,
          label: "B".to_string(),
          rationale: "".to_string(),
          members: vec![1],
          member_destinations: vec![],
          suggested_path: PathBuf::from("/b"),
          member_notes: vec![],
        },
      ],
      vec![],
      PathBuf::from("/out"),
      None,
      ReviewMode::Organize,
    );

    state.handle_key(KeyCode::Down);
    assert_eq!(state.selected_index(), 1);

    state.handle_key(KeyCode::Up);
    assert_eq!(state.selected_index(), 0);
  }
}

use anyhow::Result;
use ccs_core::session::SessionFile;
use ccs_core::transcript;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::DefaultTerminal;

use crate::ui;
use crate::ui::picker::PickerState;
use crate::ui::transcript::{handle_key as handle_transcript_key, Action, TranscriptState};

/// Top-level screen the app is currently rendering.
///
/// `Picker` owns the picker's event-loop state directly; that state is
/// non-trivial (sessions, cursor, search buffer, metadata source) so we
/// keep it inside the enum rather than rebuilding it on every transition.
pub enum Screen {
    Picker(PickerState),
    Viewer {
        live: bool,
        session: Option<SessionFile>,
        state: Option<TranscriptState>,
    },
}

impl Screen {
    /// Eagerly load a session file into a [`TranscriptState`] when one is
    /// present. Called once on screen entry; failures are logged and fall
    /// through to an empty-state viewer so the user still has a way back
    /// to the picker.
    fn hydrate(&mut self) {
        if let Screen::Viewer {
            session: Some(session),
            state: state_slot @ None,
            ..
        } = self
        {
            match transcript::load_from_path(&session.path) {
                Ok(t) => {
                    *state_slot = Some(TranscriptState::new(t));
                }
                Err(err) => {
                    tracing::error!(
                        path = %session.path.display(),
                        error = %err,
                        "failed to load transcript from path",
                    );
                }
            }
        }
    }
}

pub struct App {
    screen: Screen,
    should_quit: bool,
}

impl App {
    pub fn new(mut screen: Screen) -> Self {
        screen.hydrate();
        Self {
            screen,
            should_quit: false,
        }
    }

    #[tracing::instrument(level = "debug", skip_all)]
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        tracing::info!("entering tui run loop");
        while !self.should_quit {
            terminal.draw(|frame| match &mut self.screen {
                Screen::Picker(state) => ui::picker::render(frame, state),
                Screen::Viewer { live, state, .. } => {
                    ui::viewer::render(frame, *live, state.as_mut())
                }
            })?;
            self.handle_event()?;
            self.process_screen_transitions();
        }
        Ok(())
    }

    fn handle_event(&mut self) -> Result<()> {
        let Event::Key(key) = event::read()? else {
            return Ok(());
        };
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        match &mut self.screen {
            Screen::Picker(state) => handle_picker_key(state, key.code, &mut self.should_quit),
            Screen::Viewer { state, .. } => {
                if let Some(state) = state {
                    if handle_transcript_key(state, key) == Action::Quit {
                        self.should_quit = true;
                    }
                } else {
                    // Placeholder viewer: only q/Esc to quit.
                    if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                        self.should_quit = true;
                    }
                }
            }
        }
        Ok(())
    }

    fn process_screen_transitions(&mut self) {
        if let Screen::Picker(state) = &mut self.screen {
            if let Some(session) = state.take_open_request() {
                let mut next = Screen::Viewer {
                    live: false,
                    session: Some(session),
                    state: None,
                };
                next.hydrate();
                self.screen = next;
            }
        }
    }
}

fn handle_picker_key(state: &mut PickerState, code: KeyCode, should_quit: &mut bool) {
    if state.search_mode {
        match code {
            KeyCode::Esc => {
                state.exit_search();
                state.clear_search();
            }
            KeyCode::Enter => state.exit_search(),
            KeyCode::Backspace => state.pop_search_char(),
            KeyCode::Char(c) => state.push_search_char(c),
            _ => {}
        }
        return;
    }

    match code {
        KeyCode::Char('q') => *should_quit = true,
        KeyCode::Esc => {
            if state.search_query().is_empty() {
                *should_quit = true;
            } else {
                state.clear_search();
            }
        }
        KeyCode::Char('j') | KeyCode::Down => state.move_down(),
        KeyCode::Char('k') | KeyCode::Up => state.move_up(),
        KeyCode::Char('g') | KeyCode::Home => state.jump_top(),
        KeyCode::Char('G') | KeyCode::End => state.jump_bottom(),
        KeyCode::Char('/') => state.enter_search(),
        KeyCode::Enter => state.request_open(),
        _ => {}
    }
}

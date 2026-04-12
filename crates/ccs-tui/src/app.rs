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
/// The `Viewer` payload is `Box`ed so the enum's size stays close to
/// `PickerState` and the `clippy::large_enum_variant` gate passes.
pub enum Screen {
    Picker(PickerState),
    Viewer(Box<ViewerScreen>),
}

/// Owned viewer-screen state. Heap-allocated so the `Screen` enum stays
/// compact: `TranscriptState` carries the whole transcript plus the
/// line cache, which is much larger than `PickerState`.
pub struct ViewerScreen {
    pub live: bool,
    pub session: Option<SessionFile>,
    pub state: Option<TranscriptState>,
}

impl Screen {
    /// Construct a fresh viewer screen, hydrating the transcript if a
    /// session is provided.
    pub fn viewer(live: bool, session: Option<SessionFile>) -> Self {
        let mut screen = Screen::Viewer(Box::new(ViewerScreen {
            live,
            session,
            state: None,
        }));
        screen.hydrate();
        screen
    }

    /// Eagerly load a session file into a [`TranscriptState`] when one is
    /// present. Called once on screen entry; failures are logged and fall
    /// through to an empty-state viewer so the user still has a way back
    /// to the picker.
    fn hydrate(&mut self) {
        if let Screen::Viewer(v) = self {
            if let (Some(session), None) = (&v.session, &v.state) {
                match transcript::load_from_path(&session.path) {
                    Ok(t) => {
                        v.state = Some(TranscriptState::new(t));
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
                Screen::Viewer(v) => ui::viewer::render(frame, v.live, v.state.as_mut()),
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
            Screen::Viewer(v) => {
                if let Some(state) = v.state.as_mut() {
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
                self.screen = Screen::viewer(false, Some(session));
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

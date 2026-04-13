use std::time::Duration;

use anyhow::Result;
use ccs_core::checkpoints;
use ccs_core::session::SessionFile;
use ccs_core::transcript;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::DefaultTerminal;

use crate::tail::LiveTail;
use crate::ui;
use crate::ui::picker::PickerState;
use crate::ui::transcript::{handle_key as handle_transcript_key, Action, TranscriptState};

/// How long the event loop blocks on a keypress before checking the
/// live-tail file for new data. 100 ms keeps end-to-end latency under
/// 200 ms (well below PJH-51's 500 ms target) while adding no
/// perceptible input lag for the non-live picker and viewer screens.
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(100);

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
    /// Live-tail driver, present only when `live` is true and a session
    /// path is known. Polled once per event-loop tick when no key
    /// events are waiting.
    pub tail: Option<LiveTail>,
}

impl Screen {
    /// Construct a fresh viewer screen, hydrating the transcript if a
    /// session is provided.
    pub fn viewer(live: bool, session: Option<SessionFile>) -> Self {
        let mut screen = Screen::Viewer(Box::new(ViewerScreen {
            live,
            session,
            state: None,
            tail: None,
        }));
        screen.hydrate();
        screen
    }

    /// Eagerly load a session file into a [`TranscriptState`] when one is
    /// present. Called once on screen entry; failures are logged and fall
    /// through to an empty-state viewer so the user still has a way back
    /// to the picker. When `live` is set and a session path is known,
    /// also opens a `LiveTail` driver seeded at the end-of-file offset
    /// from the initial load so the event loop can start polling for
    /// new events atomically (no lost writes, no replayed ones).
    fn hydrate(&mut self) {
        if let Screen::Viewer(v) = self {
            if let (Some(session), None) = (&v.session, &v.state) {
                match transcript::load_from_path_with_offset(&session.path) {
                    Ok((t, offset)) => {
                        let mut state = if v.live {
                            TranscriptState::new_live(t)
                        } else {
                            TranscriptState::new(t)
                        };
                        if let Some(path) = checkpoints::marks_path() {
                            state.attach_marks_file(path);
                        }
                        if v.live {
                            v.tail = Some(LiveTail::new_at(session.path.clone(), offset));
                            // Start the cursor at the bottom so the
                            // user sees the tail, not the top.
                            state.jump_bottom();
                        }
                        v.state = Some(state);
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
            // Poll the live-tail file first so any new events appear
            // in this frame rather than the next one.
            self.poll_live_tail();

            terminal.draw(|frame| match &mut self.screen {
                Screen::Picker(state) => ui::picker::render(frame, state),
                Screen::Viewer(v) => ui::viewer::render(frame, v.live, v.state.as_mut()),
            })?;

            // Block on a key for up to EVENT_POLL_INTERVAL. If nothing
            // arrives we fall out of `handle_event` as a no-op and
            // loop back into another `poll_live_tail` + redraw tick.
            self.handle_event()?;
            self.process_screen_transitions();
        }
        Ok(())
    }

    fn poll_live_tail(&mut self) {
        let Screen::Viewer(v) = &mut self.screen else {
            return;
        };
        let (Some(tail), Some(state)) = (v.tail.as_mut(), v.state.as_mut()) else {
            return;
        };
        match tail.poll() {
            Ok(update) if update.is_empty() => {}
            Ok(update) => {
                if update.errors_skipped > 0 {
                    state.set_flash(format!(
                        "live-tail skipped {} malformed line(s)",
                        update.errors_skipped
                    ));
                }
                if update.reset {
                    // File was rewritten under us. Reload from disk
                    // and replace the transcript wholesale; the live
                    // tail reader already seeked back to 0. If the
                    // reload fails we must NOT keep the old
                    // transcript — the tail reader will start
                    // streaming the new file on top of stale messages
                    // and mix two timelines. Fall back to an empty
                    // transcript so subsequent polls produce a clean
                    // rebuild.
                    if let Some(session) = v.session.as_ref() {
                        match transcript::load_from_path(&session.path) {
                            Ok(fresh) => state.reset_transcript(fresh),
                            Err(err) => {
                                tracing::error!(
                                    path = %session.path.display(),
                                    error = %err,
                                    "reload after tail reset failed — clearing transcript",
                                );
                                state.reset_transcript(ccs_core::transcript::Transcript::default());
                                state.set_flash("session compacted; reload failed");
                            }
                        }
                    }
                } else {
                    state.append_events(update.new_events);
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "live-tail poll failed");
            }
        }
    }

    fn handle_event(&mut self) -> Result<()> {
        if !event::poll(EVENT_POLL_INTERVAL)? {
            return Ok(());
        }
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

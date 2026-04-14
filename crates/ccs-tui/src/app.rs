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
    /// The picker state this viewer was launched from, parked here
    /// until the user presses `Esc` to return. `None` when the viewer
    /// was opened directly from the CLI (`ccs view <path>`, live-tail
    /// mode) without a picker behind it — in that case `Esc` falls
    /// through to `Quit` because there is nothing to return to.
    pub saved_picker: Option<PickerState>,
}

impl Screen {
    /// Construct a fresh viewer screen, hydrating the transcript if a
    /// session is provided. Used by CLI entry points (`ccs tail`,
    /// `ccs view`) — no picker is parked underneath, so `Esc` in the
    /// viewer falls through to `Quit`.
    pub fn viewer(live: bool, session: Option<SessionFile>) -> Self {
        Self::build_viewer(live, session, None)
    }

    /// Construct a viewer launched from an existing picker. The picker
    /// state is parked on the viewer so a later `Esc` can return the
    /// app to exactly the cursor / search / scroll position the user
    /// left behind.
    pub fn viewer_from_picker(
        live: bool,
        session: Option<SessionFile>,
        picker: PickerState,
    ) -> Self {
        Self::build_viewer(live, session, Some(picker))
    }

    fn build_viewer(
        live: bool,
        session: Option<SessionFile>,
        saved_picker: Option<PickerState>,
    ) -> Self {
        let mut screen = Screen::Viewer(Box::new(ViewerScreen {
            live,
            session,
            state: None,
            tail: None,
            saved_picker,
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
                            // Cursor-to-tail happens inside the first
                            // `relayout` via `needs_initial_bottom`,
                            // latched by `TranscriptState::new_live`.
                            // Calling `jump_bottom` here would no-op
                            // because `lines` is still empty until
                            // the first viewport is known.
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

        // Compute the viewer-side action (if any) before touching
        // `self.screen` again, so the subsequent transition code can
        // move the screen out without fighting the borrow checker.
        let viewer_action = match &mut self.screen {
            Screen::Picker(state) => {
                handle_picker_key(state, key.code, &mut self.should_quit);
                None
            }
            Screen::Viewer(v) => {
                if let Some(state) = v.state.as_mut() {
                    Some(handle_transcript_key(state, key))
                } else if matches!(key.code, KeyCode::Char('q')) {
                    Some(Action::Quit)
                } else if matches!(key.code, KeyCode::Esc) {
                    // Placeholder viewer (transcript failed to load):
                    // Esc still means "go back if you can".
                    Some(Action::BackToPicker)
                } else {
                    None
                }
            }
        };

        match viewer_action {
            None | Some(Action::None) => {}
            Some(Action::Quit) => self.should_quit = true,
            Some(Action::BackToPicker) => self.return_to_picker(),
        }
        Ok(())
    }

    /// Swap the viewer out for its parked picker state. When the
    /// viewer was opened without a saved picker (CLI entry point),
    /// fall through to `Quit` — there's nothing to return to.
    fn return_to_picker(&mut self) {
        // Placeholder used only to move the old `Screen::Viewer` out
        // of `self.screen` by value. It is never observed because we
        // reassign `self.screen` unconditionally below.
        let placeholder = Screen::Viewer(Box::new(ViewerScreen {
            live: false,
            session: None,
            state: None,
            tail: None,
            saved_picker: None,
        }));
        match std::mem::replace(&mut self.screen, placeholder) {
            Screen::Viewer(mut v) => match v.saved_picker.take() {
                Some(picker) => {
                    self.screen = Screen::Picker(picker);
                }
                None => {
                    // Viewer opened directly from the CLI — no picker
                    // behind it. Restore and quit.
                    self.screen = Screen::Viewer(v);
                    self.should_quit = true;
                }
            },
            // Unreachable: we only call this after matching Viewer.
            // Put it back if we somehow got here anyway.
            other => self.screen = other,
        }
    }

    fn process_screen_transitions(&mut self) {
        // Picker → Viewer: the user pressed Enter on a row. Move the
        // picker state into the new viewer so a later `Esc` can
        // return us to the same cursor / search / scroll position.
        let pending_open = if let Screen::Picker(state) = &mut self.screen {
            state.take_open_request()
        } else {
            None
        };
        let Some(session) = pending_open else {
            return;
        };
        // Move the PickerState out of `self.screen` by value. The
        // placeholder is only here to satisfy `mem::replace`; we
        // overwrite `self.screen` before anyone sees it.
        let placeholder = Screen::Viewer(Box::new(ViewerScreen {
            live: false,
            session: None,
            state: None,
            tail: None,
            saved_picker: None,
        }));
        let picker = match std::mem::replace(&mut self.screen, placeholder) {
            Screen::Picker(state) => state,
            // Unreachable because `pending_open` was `Some`.
            other => {
                self.screen = other;
                return;
            }
        };
        self.screen = Screen::viewer_from_picker(false, Some(session), picker);
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

#[cfg(test)]
mod tests {
    use super::*;
    use ccs_core::metadata::NullSource;
    use ccs_core::session::{SessionFile, SessionKind};
    use std::path::PathBuf;
    use std::time::{Duration, UNIX_EPOCH};

    fn fake_session(id: &str) -> SessionFile {
        SessionFile {
            session_id: id.into(),
            parent_session_id: None,
            kind: SessionKind::Primary,
            // Deliberately-nonexistent path so hydrate() fails cleanly
            // and the viewer lands in its placeholder state. We only
            // care about the transition, not the transcript itself.
            path: PathBuf::from(format!("/definitely/does/not/exist/{id}.jsonl")),
            project_cwd: PathBuf::from("/tmp"),
            modified: UNIX_EPOCH + Duration::from_secs(100),
            size: 0,
        }
    }

    fn picker_with(sessions: Vec<SessionFile>) -> PickerState {
        PickerState::new(sessions, Box::new(NullSource), None)
    }

    #[test]
    fn enter_moves_picker_state_into_viewer_saved_picker() {
        // Landing on the viewer via Enter must park the picker state
        // on `saved_picker` — if we drop it, there's nothing to return
        // to and `Esc` has to quit the whole process.
        let mut picker = picker_with(vec![fake_session("a"), fake_session("b")]);
        picker.move_down();
        picker.request_open();

        let mut app = App::new(Screen::Picker(picker));
        app.process_screen_transitions();

        match &app.screen {
            Screen::Viewer(v) => assert!(
                v.saved_picker.is_some(),
                "picker state must be parked on the viewer"
            ),
            Screen::Picker(_) => panic!("transition did not fire"),
        }
    }

    #[test]
    fn back_to_picker_restores_cursor_position() {
        // Round trip: picker cursor must survive the viewer detour.
        // This is the PJH-65 Step 3 acceptance check — "close the
        // session and return to the same row you opened from".
        let mut picker = picker_with(vec![
            fake_session("a"),
            fake_session("b"),
            fake_session("c"),
        ]);
        picker.move_down();
        picker.move_down();
        let cursor_before = picker.cursor();
        assert!(cursor_before > 0, "test precondition: cursor moved");
        picker.request_open();

        let mut app = App::new(Screen::Picker(picker));
        app.process_screen_transitions();
        assert!(matches!(app.screen, Screen::Viewer(_)));

        app.return_to_picker();

        match &app.screen {
            Screen::Picker(p) => {
                assert_eq!(p.cursor(), cursor_before, "picker cursor not preserved");
            }
            Screen::Viewer(_) => panic!("return_to_picker did not restore the picker"),
        }
        assert!(!app.should_quit);
    }

    #[test]
    fn back_to_picker_preserves_search_query() {
        // The picker's search query (and therefore its filtered row
        // list) must also survive the round trip. We pick a query
        // that still matches at least one row so `request_open` can
        // fire — otherwise filtered becomes empty and the transition
        // never happens.
        let mut picker = picker_with(vec![
            fake_session("alpha"),
            fake_session("beta"),
            fake_session("gamma"),
        ]);
        picker.enter_search();
        picker.push_search_char('a'); // matches alpha, beta, gamma
        picker.exit_search();
        let query_before = picker.search_query().to_string();
        assert_eq!(query_before, "a");
        picker.request_open();

        let mut app = App::new(Screen::Picker(picker));
        app.process_screen_transitions();
        app.return_to_picker();

        match &app.screen {
            Screen::Picker(p) => assert_eq!(p.search_query(), query_before),
            Screen::Viewer(_) => panic!("return_to_picker did not restore the picker"),
        }
    }

    #[test]
    fn back_to_picker_from_cli_viewer_quits() {
        // A viewer constructed without a saved picker (the CLI
        // `ccs view <path>` entry point) has nothing to return to —
        // BackToPicker must fall through to Quit.
        let mut app = App::new(Screen::viewer(false, None));
        app.return_to_picker();
        assert!(
            app.should_quit,
            "no-picker viewer must quit on BackToPicker"
        );
    }
}

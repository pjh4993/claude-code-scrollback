//! Pure key-handling for the transcript viewer.
//!
//! Isolated from the render layer so it can be unit-tested against a
//! hand-constructed [`TranscriptState`] without spinning up a terminal.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::state::TranscriptState;

/// High-level result of consuming one key event. `app.rs` maps this onto
/// `self.should_quit` / screen transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
}

/// Consume a `KeyEvent` and mutate `state` accordingly.
///
/// Vim keys implemented in PR 2:
/// * `j` / `k` / `↓` / `↑`       — line down / up
/// * `Ctrl-d` / `Ctrl-u`        — half-page down / up
/// * `g g` / `G` / `Home` / `End` — jump to top / bottom
/// * `q` / `Esc`                 — quit
pub fn handle_key(state: &mut TranscriptState, key: KeyEvent) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let pending_g = state.pending_g();
    // Every keypress clears the gg chord unless it's the second g.
    state.set_pending_g(false);

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,

        KeyCode::Char('j') | KeyCode::Down => {
            state.move_down(1);
            Action::None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            state.move_up(1);
            Action::None
        }

        KeyCode::Char('d') if ctrl => {
            state.half_page_down();
            Action::None
        }
        KeyCode::Char('u') if ctrl => {
            state.half_page_up();
            Action::None
        }

        KeyCode::Char('G') | KeyCode::End => {
            state.jump_bottom();
            Action::None
        }
        KeyCode::Home => {
            state.jump_top();
            Action::None
        }
        KeyCode::Char('g') => {
            if pending_g {
                state.jump_top();
            } else {
                state.set_pending_g(true);
            }
            Action::None
        }

        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccs_core::jsonl::parse_line;
    use ccs_core::transcript::{from_events, Transcript};
    use crossterm::event::KeyCode;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn state_with_n_messages(n: usize) -> TranscriptState {
        let lines: Vec<String> = (0..n)
            .map(|i| {
                format!(
                    r#"{{"type":"user","uuid":"u{i}","sessionId":"s1","timestamp":"t","message":{{"role":"user","content":"msg-{i}"}}}}"#
                )
            })
            .collect();
        let t: Transcript = from_events(lines.iter().map(|l| parse_line(l).unwrap()));
        let mut s = TranscriptState::new(t);
        s.set_viewport(80, 10);
        s
    }

    #[test]
    fn q_and_esc_both_quit() {
        let mut s = state_with_n_messages(1);
        assert_eq!(handle_key(&mut s, key(KeyCode::Char('q'))), Action::Quit);
        assert_eq!(handle_key(&mut s, key(KeyCode::Esc)), Action::Quit);
    }

    #[test]
    fn j_and_k_move_cursor() {
        let mut s = state_with_n_messages(20);
        let start = s.cursor();
        handle_key(&mut s, key(KeyCode::Char('j')));
        assert_eq!(s.cursor(), start + 1);
        handle_key(&mut s, key(KeyCode::Char('k')));
        assert_eq!(s.cursor(), start);
    }

    #[test]
    fn capital_g_jumps_to_last_line() {
        let mut s = state_with_n_messages(30);
        handle_key(&mut s, key(KeyCode::Char('G')));
        assert_eq!(s.cursor(), s.lines().len() - 1);
    }

    #[test]
    fn gg_chord_jumps_to_top() {
        let mut s = state_with_n_messages(30);
        handle_key(&mut s, key(KeyCode::Char('G'))); // bottom
        assert!(s.cursor() > 0);
        handle_key(&mut s, key(KeyCode::Char('g'))); // pending
        assert!(s.pending_g());
        handle_key(&mut s, key(KeyCode::Char('g'))); // fire
        assert_eq!(s.cursor(), 0);
        assert!(!s.pending_g());
    }

    #[test]
    fn unrelated_key_clears_gg_chord() {
        let mut s = state_with_n_messages(30);
        handle_key(&mut s, key(KeyCode::Char('g')));
        assert!(s.pending_g());
        handle_key(&mut s, key(KeyCode::Char('j')));
        assert!(!s.pending_g());
    }

    #[test]
    fn ctrl_d_and_ctrl_u_half_page_scroll() {
        let mut s = state_with_n_messages(100);
        let start = s.cursor();
        handle_key(&mut s, ctrl_key(KeyCode::Char('d')));
        assert!(s.cursor() > start, "ctrl-d did not advance cursor");
        let down_pos = s.cursor();
        handle_key(&mut s, ctrl_key(KeyCode::Char('u')));
        assert!(s.cursor() < down_pos, "ctrl-u did not rewind cursor");
    }

    #[test]
    fn cursor_clamps_at_bounds() {
        let mut s = state_with_n_messages(3);
        for _ in 0..50 {
            handle_key(&mut s, key(KeyCode::Char('j')));
        }
        assert_eq!(s.cursor(), s.lines().len() - 1);
        for _ in 0..50 {
            handle_key(&mut s, key(KeyCode::Char('k')));
        }
        assert_eq!(s.cursor(), 0);
    }
}

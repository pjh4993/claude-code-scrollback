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
/// * `j` / `k` / `↓` / `↑`        — line down / up
/// * `Ctrl-d` / `Ctrl-u`          — half-page down / up
/// * `g g` / `G` / `Home` / `End` — jump to top / bottom
/// * `{` / `}`                    — prev / next user turn
/// * `t`                          — toggle collapse on block under cursor
/// * `T`                          — cycle collapse-all (tools & thinking)
/// * `q` / `Esc`                  — quit
pub fn handle_key(state: &mut TranscriptState, key: KeyEvent) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let pending_g = state.pending_g();
    // Every keypress clears the gg chord unless it's the second g, and
    // clears the last flash so stale "not collapsible" messages don't
    // stick around forever.
    state.set_pending_g(false);
    state.clear_flash();

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

        KeyCode::Char('{') => {
            state.jump_prev_user_turn();
            Action::None
        }
        KeyCode::Char('}') => {
            state.jump_next_user_turn();
            Action::None
        }

        KeyCode::Char('t') => {
            state.toggle_current_block();
            Action::None
        }
        KeyCode::Char('T') => {
            state.toggle_collapse_all();
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

    fn state_with_tooling() -> TranscriptState {
        let lines = &[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"q"}}"#,
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"result"},{"type":"tool_use","id":"t1","name":"Read","input":{"path":"x"}}]}}"#,
            r#"{"type":"user","uuid":"u2","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"next"}}"#,
        ];
        let t: Transcript = from_events(lines.iter().map(|l| parse_line(l).unwrap()));
        let mut s = TranscriptState::new(t);
        s.set_viewport(80, 20);
        s
    }

    #[test]
    fn capital_t_cycles_collapse_all_and_sets_flash() {
        use crate::ui::transcript::state::{CollapseAll, LineKind};
        let mut s = state_with_tooling();
        handle_key(&mut s, key(KeyCode::Char('T')));
        assert_eq!(s.collapse_all(), CollapseAll::ToolsAndThinking);
        assert!(s.flash().is_some());
        let fold_count = s
            .lines()
            .iter()
            .filter(|l| l.kind == LineKind::Fold)
            .count();
        assert!(
            fold_count >= 2,
            "expected tool+thinking folds, got {fold_count}"
        );

        handle_key(&mut s, key(KeyCode::Char('T')));
        assert_eq!(s.collapse_all(), CollapseAll::Off);
        let fold_count = s
            .lines()
            .iter()
            .filter(|l| l.kind == LineKind::Fold)
            .count();
        assert_eq!(fold_count, 0);
    }

    #[test]
    fn t_on_text_block_sets_not_collapsible_flash() {
        let mut s = state_with_tooling();
        // Walk the cursor down until it lands on a Body line for a Text
        // block. This depends on the current layout emission order; if
        // `layout::build` changes where text bodies appear the loop will
        // need to be updated or replaced with a direct cursor set.
        for _ in 0..50 {
            if let Some(rl) = s.lines().get(s.cursor()) {
                use crate::ui::transcript::state::LineKind;
                if rl.kind == LineKind::Body {
                    if let Some(bi) = rl.block_index {
                        let msg = &s.transcript().messages[rl.msg_index];
                        if matches!(msg.blocks[bi], ccs_core::transcript::Block::Text(_)) {
                            break;
                        }
                    }
                }
            }
            handle_key(&mut s, key(KeyCode::Char('j')));
        }
        handle_key(&mut s, key(KeyCode::Char('t')));
        assert_eq!(s.flash(), Some("not collapsible"));
    }

    #[test]
    fn close_brace_and_open_brace_jump_between_user_turns() {
        let mut s = state_with_tooling();
        // Start at first user header (line 0).
        assert_eq!(s.cursor(), 0);
        handle_key(&mut s, key(KeyCode::Char('}')));
        let after_next = s.cursor();
        assert!(after_next > 0, "}} should advance cursor");
        // Line should be a user header at a different msg_index than the first.
        let row = &s.lines()[after_next];
        use crate::ui::transcript::state::LineKind;
        assert_eq!(row.kind, LineKind::Header);
        assert_ne!(row.msg_index, 0);
        handle_key(&mut s, key(KeyCode::Char('{')));
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn open_brace_at_top_flashes_no_previous() {
        let mut s = state_with_tooling();
        handle_key(&mut s, key(KeyCode::Char('{')));
        assert_eq!(s.flash(), Some("no previous user turns"));
    }

    #[test]
    fn flash_is_cleared_by_next_key() {
        let mut s = state_with_tooling();
        s.set_flash("probe");
        handle_key(&mut s, key(KeyCode::Char('j')));
        assert!(s.flash().is_none());
    }
}

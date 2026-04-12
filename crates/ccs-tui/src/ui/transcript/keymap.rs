//! Pure key-handling for the transcript viewer.
//!
//! Isolated from the render layer so it can be unit-tested against a
//! hand-constructed [`TranscriptState`] without spinning up a terminal.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::state::TranscriptState;
use super::yank;
use crate::clipboard;

/// High-level result of consuming one key event. `app.rs` maps this onto
/// `self.should_quit` / screen transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
}

/// Consume a `KeyEvent` and mutate `state` accordingly.
///
/// Normal-mode keys:
/// * `j` / `k` / `↓` / `↑`        — line down / up
/// * `Ctrl-d` / `Ctrl-u`          — half-page down / up
/// * `g g` / `G` / `Home` / `End` — jump to top / bottom
/// * `{` / `}`                    — prev / next user turn
/// * `t`                          — toggle collapse on block under cursor
/// * `T`                          — cycle collapse-all (tools & thinking)
/// * `/`                          — begin search
/// * `n` / `N`                    — next / prev search match (in active search)
/// * `y`                          — yank current message to clipboard
/// * `q` / `Esc`                  — quit
///
/// While search is typing: `Enter` commits, `Esc` cancels, `Backspace`
/// deletes one char, any other character extends the query.
pub fn handle_key(state: &mut TranscriptState, key: KeyEvent) -> Action {
    // Search-typing mode owns the whole keyboard; route first.
    if state.search_mode().is_typing() {
        return handle_search_typing(state, key);
    }

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

        KeyCode::Char('/') => {
            state.begin_search();
            Action::None
        }
        KeyCode::Char('n') => {
            state.next_match();
            Action::None
        }
        KeyCode::Char('N') => {
            state.prev_match();
            Action::None
        }

        KeyCode::Char('y') => {
            yank_current_message(state);
            Action::None
        }

        _ => Action::None,
    }
}

fn handle_search_typing(state: &mut TranscriptState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Esc => {
            state.cancel_search();
            state.set_flash("search cancelled");
        }
        KeyCode::Enter => {
            state.commit_search();
        }
        KeyCode::Backspace => {
            state.pop_search_char();
        }
        KeyCode::Char(ch) => {
            state.push_search_char(ch);
        }
        _ => {}
    }
    Action::None
}

fn yank_current_message(state: &mut TranscriptState) {
    let msg_idx = state.current_msg_index();
    let Some(message) = state.transcript().messages.get(msg_idx) else {
        state.set_flash("nothing to yank");
        return;
    };
    let text = yank::format_message(message);
    let n = text.len();
    match clipboard::copy(&text) {
        Ok(clipboard::CopyMethod::System) => {
            state.set_flash(format!("yanked {n} chars (system)"));
        }
        Ok(clipboard::CopyMethod::Osc52) => {
            state.set_flash(format!("yanked {n} chars (osc52)"));
        }
        Err(err) => {
            tracing::warn!(error = %err, "clipboard copy failed");
            state.set_flash("yank failed (see logs)");
        }
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
        let mut found_text_body = false;
        for _ in 0..50 {
            if let Some(rl) = s.lines().get(s.cursor()) {
                use crate::ui::transcript::state::LineKind;
                if rl.kind == LineKind::Body {
                    if let Some(bi) = rl.block_index {
                        let msg = &s.transcript().messages[rl.msg_index];
                        if matches!(msg.blocks[bi], ccs_core::transcript::Block::Text(_)) {
                            found_text_body = true;
                            break;
                        }
                    }
                }
            }
            handle_key(&mut s, key(KeyCode::Char('j')));
        }
        assert!(
            found_text_body,
            "failed to locate a Text Body line in the current layout"
        );
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
    fn close_brace_at_bottom_flashes_no_next() {
        let mut s = state_with_tooling();
        handle_key(&mut s, key(KeyCode::Char('G'))); // jump to EOF
        handle_key(&mut s, key(KeyCode::Char('}')));
        assert_eq!(s.flash(), Some("no more user turns"));
    }

    #[test]
    fn flash_is_cleared_by_next_key() {
        let mut s = state_with_tooling();
        s.set_flash("probe");
        handle_key(&mut s, key(KeyCode::Char('j')));
        assert!(s.flash().is_none());
    }
}

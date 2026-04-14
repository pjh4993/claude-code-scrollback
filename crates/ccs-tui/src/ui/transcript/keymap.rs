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
    /// Exit the process entirely. Bound to `q`.
    Quit,
    /// Return to the picker screen with its prior state preserved,
    /// falling through to `Quit` when the viewer was opened without a
    /// picker behind it (e.g. `ccs view <path>` from the CLI). Bound
    /// to `Esc`.
    BackToPicker,
}

/// Consume a `KeyEvent` and mutate `state` accordingly.
///
/// Normal-mode keys:
/// * `j` / `k` / `↓` / `↑`        — line down / up
/// * `Ctrl-d` / `Ctrl-u`          — half-page down / up
/// * `g g` / `G` / `Home` / `End` — jump to top / bottom
/// * `{` / `}`                    — prev / next user turn
/// * `[` / `]`                    — prev / next auto-checkpoint
/// * `c`                          — toggle checkpoint sidebar
/// * `m <letter>` / `' <letter>`  — set / jump to manual mark
/// * `t`                          — toggle collapse on block under cursor
/// * `T`                          — cycle collapse-all (tools & thinking)
/// * `/`                          — begin search
/// * `n` / `N`                    — next / prev search match (in active search)
/// * `y`                          — yank current message to clipboard
/// * `q`                          — quit the process
/// * `Esc`                        — return to picker (or quit if there is none)
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
    let pending_m = state.pending_m();
    let pending_quote = state.pending_quote();
    // Every keypress clears the gg / m / ' chord state unless it's the
    // second half of the chord, and clears the last flash so stale "not
    // collapsible" messages don't stick around forever.
    state.set_pending_g(false);
    state.set_pending_m(false);
    state.set_pending_quote(false);
    state.clear_flash();

    // Chord completions run before the normal-mode match so that the
    // second half of `m<letter>` / `'<letter>` doesn't accidentally hit
    // a normal-mode binding like `j` or `g`.
    if let KeyCode::Char(ch) = key.code {
        if !ctrl && pending_m {
            state.set_mark(ch);
            return Action::None;
        }
        if !ctrl && pending_quote {
            state.jump_to_mark(ch);
            return Action::None;
        }
    }

    match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Esc => Action::BackToPicker,

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

        KeyCode::Char('[') => {
            state.jump_prev_checkpoint();
            Action::None
        }
        KeyCode::Char(']') => {
            state.jump_next_checkpoint();
            Action::None
        }
        KeyCode::Char('c') => {
            state.toggle_sidebar();
            Action::None
        }

        KeyCode::Char('m') => {
            state.set_pending_m(true);
            Action::None
        }
        KeyCode::Char('\'') => {
            state.set_pending_quote(true);
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

        KeyCode::Char('F') => {
            state.toggle_follow();
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
    fn q_quits_and_esc_returns_to_picker() {
        // `q` still means "exit the process entirely". `Esc` now asks
        // the app layer to return to the picker — the app falls back
        // to `Quit` when there is no picker behind the viewer.
        let mut s = state_with_n_messages(1);
        assert_eq!(handle_key(&mut s, key(KeyCode::Char('q'))), Action::Quit);
        assert_eq!(handle_key(&mut s, key(KeyCode::Esc)), Action::BackToPicker,);
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

    #[test]
    fn m_letter_chord_sets_mark_and_quote_letter_jumps_back() {
        let mut s = state_with_n_messages(30);
        // Move down a few lines so the mark is not on line 0.
        for _ in 0..5 {
            handle_key(&mut s, key(KeyCode::Char('j')));
        }
        let marked_cursor = s.cursor();

        // `ma` — set mark `a` at current cursor.
        handle_key(&mut s, key(KeyCode::Char('m')));
        assert!(s.pending_m());
        handle_key(&mut s, key(KeyCode::Char('a')));
        assert!(!s.pending_m());
        assert_eq!(s.flash(), Some("mark a set"));
        assert!(s.marks().contains_key(&'a'));

        // Navigate away.
        handle_key(&mut s, key(KeyCode::Char('G')));
        assert_ne!(s.cursor(), marked_cursor);

        // `'a` — jump back.
        handle_key(&mut s, key(KeyCode::Char('\'')));
        assert!(s.pending_quote());
        handle_key(&mut s, key(KeyCode::Char('a')));
        assert!(!s.pending_quote());
        assert_eq!(s.cursor(), marked_cursor);
        assert_eq!(s.flash(), Some("'a"));
    }

    #[test]
    fn quote_letter_with_no_mark_flashes() {
        let mut s = state_with_n_messages(10);
        handle_key(&mut s, key(KeyCode::Char('\'')));
        handle_key(&mut s, key(KeyCode::Char('z')));
        assert_eq!(s.flash(), Some("no mark z"));
    }

    #[test]
    fn pending_m_consumes_the_next_key_even_if_it_is_a_normal_binding() {
        // After `m`, pressing `j` should set mark `j`, not move down.
        let mut s = state_with_n_messages(30);
        handle_key(&mut s, key(KeyCode::Char('j')));
        let after_j = s.cursor();
        handle_key(&mut s, key(KeyCode::Char('m')));
        handle_key(&mut s, key(KeyCode::Char('j')));
        assert_eq!(s.cursor(), after_j, "m<j> must not also move the cursor");
        assert!(s.marks().contains_key(&'j'));
    }

    #[test]
    fn uppercase_mark_letter_is_rejected() {
        let mut s = state_with_n_messages(10);
        handle_key(&mut s, key(KeyCode::Char('m')));
        handle_key(&mut s, key(KeyCode::Char('A')));
        assert!(!s.marks().contains_key(&'A'));
        assert_eq!(s.flash(), Some("bad mark 'A'"));
    }

    #[test]
    fn quote_with_invalid_letter_is_rejected() {
        // `'A` and `'1` must flash "bad mark" (same rule as `mA`), not
        // "no mark" — otherwise set and jump disagree on what's valid.
        let mut s = state_with_n_messages(10);
        handle_key(&mut s, key(KeyCode::Char('\'')));
        handle_key(&mut s, key(KeyCode::Char('A')));
        assert_eq!(s.flash(), Some("bad mark 'A'"));

        handle_key(&mut s, key(KeyCode::Char('\'')));
        handle_key(&mut s, key(KeyCode::Char('1')));
        assert_eq!(s.flash(), Some("bad mark '1'"));
    }

    #[test]
    fn same_session_save_merges_with_concurrent_writes() {
        // Two TranscriptStates opened on the same session file: each
        // sets a different letter. The second save must preserve the
        // first's letter rather than overwriting with its stale snapshot.
        let tmp = tempfile::tempdir().unwrap();
        let marks_path = tmp.path().join("marks.json");

        let mut s1 = state_with_n_messages(20);
        s1.attach_marks_file(marks_path.clone());
        let mut s2 = state_with_n_messages(20);
        s2.attach_marks_file(marks_path.clone());

        handle_key(&mut s1, key(KeyCode::Char('j')));
        handle_key(&mut s1, key(KeyCode::Char('m')));
        handle_key(&mut s1, key(KeyCode::Char('a')));

        handle_key(&mut s2, key(KeyCode::Char('j')));
        handle_key(&mut s2, key(KeyCode::Char('m')));
        handle_key(&mut s2, key(KeyCode::Char('b')));

        // Fresh attach — disk should carry both letters.
        let mut s3 = state_with_n_messages(20);
        s3.attach_marks_file(marks_path);
        assert!(s3.marks().contains_key(&'a'));
        assert!(s3.marks().contains_key(&'b'));
    }

    fn state_with_two_user_turns_and_a_stop() -> TranscriptState {
        let lines = &[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"first"}}"#,
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"answer 1"}]}}"#,
            r#"{"type":"system","uuid":"sys1","sessionId":"s1","timestamp":"t","stopReason":"end_turn"}"#,
            r#"{"type":"user","uuid":"u2","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"second"}}"#,
            r#"{"type":"assistant","uuid":"a2","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"answer 2"}]}}"#,
        ];
        let t: Transcript = from_events(lines.iter().map(|l| parse_line(l).unwrap()));
        let mut s = TranscriptState::new(t);
        s.set_viewport(80, 20);
        s
    }

    #[test]
    fn c_toggles_the_sidebar() {
        let mut s = state_with_two_user_turns_and_a_stop();
        assert!(!s.show_sidebar());
        handle_key(&mut s, key(KeyCode::Char('c')));
        assert!(s.show_sidebar());
        handle_key(&mut s, key(KeyCode::Char('c')));
        assert!(!s.show_sidebar());
    }

    #[test]
    fn close_bracket_advances_to_next_checkpoint() {
        let mut s = state_with_two_user_turns_and_a_stop();
        // There are 3 checkpoints: user1 (line 0), stop (mid), user2 (later).
        assert!(s.checkpoints().len() >= 3);
        assert_eq!(s.cursor(), 0);

        handle_key(&mut s, key(KeyCode::Char(']')));
        let after_one = s.cursor();
        assert!(
            after_one > 0,
            "] should advance cursor past first checkpoint"
        );
        // Landing line must be one of the checkpoint lines.
        assert!(s.checkpoints().iter().any(|c| c.line == after_one));

        handle_key(&mut s, key(KeyCode::Char(']')));
        let after_two = s.cursor();
        assert!(after_two > after_one);
    }

    #[test]
    fn open_bracket_steps_back_to_previous_checkpoint() {
        let mut s = state_with_two_user_turns_and_a_stop();
        // `G` lands past the final checkpoint; `[` must step back onto a
        // checkpoint line strictly above the current cursor.
        handle_key(&mut s, key(KeyCode::Char('G')));
        let after_g = s.cursor();

        handle_key(&mut s, key(KeyCode::Char('[')));
        assert!(s.cursor() < after_g, "[ should rewind the cursor");
        assert!(s.checkpoints().iter().any(|c| c.line == s.cursor()));

        let first_back = s.cursor();
        handle_key(&mut s, key(KeyCode::Char('[')));
        assert!(s.cursor() < first_back, "[ should rewind again");
    }

    #[test]
    fn close_bracket_at_end_flashes_no_more_checkpoints() {
        let mut s = state_with_two_user_turns_and_a_stop();
        handle_key(&mut s, key(KeyCode::Char('G'))); // bottom
        handle_key(&mut s, key(KeyCode::Char(']')));
        assert_eq!(s.flash(), Some("no more checkpoints"));
    }

    #[test]
    fn open_bracket_at_top_flashes_no_previous_checkpoints() {
        let mut s = state_with_two_user_turns_and_a_stop();
        assert_eq!(s.cursor(), 0);
        handle_key(&mut s, key(KeyCode::Char('[')));
        assert_eq!(s.flash(), Some("no previous checkpoints"));
    }

    #[test]
    fn active_checkpoint_index_follows_cursor() {
        let mut s = state_with_two_user_turns_and_a_stop();
        // At the top: active checkpoint is the first one (user1).
        assert_eq!(s.active_checkpoint_index(), Some(0));
        // After `]`: cursor moves to the second checkpoint.
        handle_key(&mut s, key(KeyCode::Char(']')));
        assert_eq!(s.active_checkpoint_index(), Some(1));
    }

    #[test]
    fn marks_persist_through_attach_marks_file() {
        // Setting a mark in one TranscriptState and then opening a fresh
        // state pointed at the same marks.json must reproduce the mark.
        let tmp = tempfile::tempdir().unwrap();
        let marks_path = tmp.path().join("marks.json");

        {
            let mut s = state_with_n_messages(20);
            s.attach_marks_file(marks_path.clone());
            for _ in 0..4 {
                handle_key(&mut s, key(KeyCode::Char('j')));
            }
            let saved_cursor = s.cursor();
            handle_key(&mut s, key(KeyCode::Char('m')));
            handle_key(&mut s, key(KeyCode::Char('q')));
            // NB: `q` here is the mark letter, consumed by the chord
            // handler — it does NOT quit, because pending_m routes first.
            assert!(s.marks().contains_key(&'q'));
            let _ = saved_cursor; // scoped: we re-check after reattach below
        }

        // Fresh state over the same transcript fixture, reattaching to
        // the same file — marks should load back in.
        let mut s2 = state_with_n_messages(20);
        assert!(s2.marks().is_empty());
        s2.attach_marks_file(marks_path);
        assert!(s2.marks().contains_key(&'q'));
    }
}

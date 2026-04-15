//! End-to-end first-run smoke tests for the `claude-code-scrollback` binary.
//!
//! Closes one of PJH-53's success criteria: "install →
//! `claude-code-scrollback` → picker appears with no prompts, no config
//! file creation, no permission errors". Spawns the release binary as
//! a subprocess (ratatui can't be exercised without a real PTY), so
//! these tests exercise the **non-interactive** code paths only:
//!
//! * `--version` and `--help` return cleanly under arbitrary `$HOME`.
//! * Bad session id / path bail with a clear shell error before any
//!   TUI initialization runs.
//! * Stdout-not-a-TTY is detected up-front instead of crashing inside
//!   ratatui's alternate-screen-buffer init.
//!
//! Cargo's test harness redirects stdout, so every subprocess we spawn
//! here naturally has stdout != tty — which is exactly the case we
//! want the CLI to handle gracefully.

use std::path::PathBuf;
use std::process::Command;

/// Locate the freshly-built `claude-code-scrollback` binary. Cargo
/// passes its path to integration tests via `CARGO_BIN_EXE_<name>`.
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_claude-code-scrollback"))
}

/// Spawn the binary with `args` and return (status, stdout, stderr).
/// `home` overrides `$HOME` to point at an empty temp dir so the test
/// can't accidentally see real session files.
fn run(args: &[&str], home: &std::path::Path) -> (std::process::ExitStatus, String, String) {
    let out = Command::new(binary())
        .args(args)
        .env("HOME", home)
        // Make the cache dir end up under HOME so we don't write to
        // the user's real ~/.cache.
        .env_remove("XDG_CACHE_HOME")
        .env_remove("RUST_LOG")
        .output()
        .expect("failed to spawn claude-code-scrollback");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (out.status, stdout, stderr)
}

#[test]
fn version_flag_works_with_empty_home() {
    let tmp = tempfile::tempdir().unwrap();
    let (status, stdout, stderr) = run(&["--version"], tmp.path());
    assert!(status.success(), "stderr: {stderr}");
    assert!(
        stdout.contains("claude-code-scrollback"),
        "expected version string, got: {stdout:?}"
    );
}

#[test]
fn help_flag_lists_the_documented_entry_points() {
    let tmp = tempfile::tempdir().unwrap();
    let (status, stdout, _stderr) = run(&["--help"], tmp.path());
    assert!(status.success());
    // Quick sanity on the four documented entry points so future flag
    // renames trip this test.
    assert!(stdout.contains("--live"));
    assert!(stdout.contains("--log-level"));
    assert!(stdout.contains("--log-format"));
    // The positional session arg is documented under the args list.
    assert!(stdout.to_lowercase().contains("session"));
}

#[test]
fn unknown_session_id_bails_before_tui_init() {
    // A made-up id can't possibly resolve under an empty HOME, so the
    // CLI must print a clear shell error and exit non-zero — not
    // launch the TUI and then crash on stdout-not-a-tty.
    let tmp = tempfile::tempdir().unwrap();
    let (status, _stdout, stderr) = run(&["nonexistent-session-id-xyz-999"], tmp.path());
    assert!(!status.success(), "expected non-zero exit");
    assert!(
        stderr.contains("unknown session id or path"),
        "expected clean error, got stderr: {stderr:?}"
    );
}

#[test]
fn live_with_no_active_session_bails_with_clear_error() {
    // Empty HOME → no project dir → no active session for `--live`.
    // Should hard-error with a path/cwd-aware message rather than
    // opening an empty live viewer.
    let tmp = tempfile::tempdir().unwrap();
    let (status, _stdout, stderr) = run(&["--live"], tmp.path());
    assert!(!status.success());
    assert!(
        stderr.contains("no active session"),
        "expected 'no active session' message, got: {stderr:?}"
    );
}

#[test]
fn no_args_with_non_tty_stdout_bails_cleanly() {
    // The most invasive case: with no args the CLI tries to enter the
    // picker, which calls into ratatui::init and would otherwise panic
    // when stdout isn't a real terminal. The TTY guard added in
    // PJH-53 should catch this before any escape sequences are
    // emitted.
    let tmp = tempfile::tempdir().unwrap();
    let (status, stdout, stderr) = run(&[], tmp.path());
    assert!(!status.success(), "expected non-zero exit");
    assert!(
        stderr.contains("requires an interactive terminal"),
        "expected TTY guard message, got stderr: {stderr:?}"
    );
    // No TUI escape sequences should have been written to stdout.
    assert!(
        !stdout.contains("\x1b["),
        "found terminal escape sequence in stdout: {stdout:?}"
    );
}

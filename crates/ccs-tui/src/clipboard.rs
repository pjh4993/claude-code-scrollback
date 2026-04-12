//! Terminal-friendly clipboard copy with OSC-52 fallback.
//!
//! Tries the system clipboard via [`arboard`] first (best UX on local
//! desktops); falls back to emitting an OSC-52 escape sequence directly
//! to stdout when `arboard` is unavailable (common over SSH, inside
//! tmux, or in headless containers). Callers receive a [`CopyMethod`]
//! indicating which path succeeded so the viewer can flash an
//! informative status line.

use std::io::{self, Write};

use anyhow::Result;

/// Which transport was used to copy the text. Returned to the viewer
/// so it can flash `yanked (osc52)` or `yanked (system)` — helpful when
/// debugging why a copy "didn't work" (tmux without `set-clipboard on`,
/// etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyMethod {
    System,
    Osc52,
}

/// Copy `text` to the user's clipboard. Attempts `arboard`, then falls
/// through to OSC-52 on error. Only the OSC-52 branch reaches stdout —
/// the arboard branch never writes anywhere, so the TUI's alternate
/// screen buffer is unaffected.
pub fn copy(text: &str) -> Result<CopyMethod> {
    match copy_via_arboard(text) {
        Ok(()) => Ok(CopyMethod::System),
        Err(err) => {
            tracing::debug!(
                error = %err,
                "arboard clipboard unavailable, falling back to OSC-52",
            );
            copy_via_osc52(text)?;
            Ok(CopyMethod::Osc52)
        }
    }
}

fn copy_via_arboard(text: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new()?;
    clipboard.set_text(text.to_string())?;
    Ok(())
}

/// Emit an OSC-52 set-clipboard escape sequence on stdout. Format:
/// `ESC ] 52 ; c ; <base64(text)> BEL`. Modern terminals (iTerm2,
/// kitty, alacritty, wezterm, tmux with `set-clipboard on`) honor this
/// sequence even inside an alternate screen buffer.
fn copy_via_osc52(text: &str) -> Result<()> {
    let encoded = base64_encode(text.as_bytes());
    let mut out = io::stdout().lock();
    // Using BEL (`\x07`) as the terminator rather than ST (`\x1b\\`)
    // because it's more widely supported and the only way to avoid
    // confusing tmux passthrough.
    write!(out, "\x1b]52;c;{encoded}\x07")?;
    out.flush()?;
    Ok(())
}

/// Tiny standalone base64 encoder (URL-unsafe, standard alphabet with
/// `=` padding). Avoids pulling in a `base64` crate dep for the ~30
/// lines we actually need. Well-tested against RFC 4648 vectors below.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let b0 = input[i] as u32;
        let b1 = input[i + 1] as u32;
        let b2 = input[i + 2] as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        out.push(ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
        out.push(ALPHABET[(triple & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        let b0 = input[i] as u32;
        let triple = b0 << 16;
        out.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let b0 = input[i] as u32;
        let b1 = input[i + 1] as u32;
        let triple = (b0 << 16) | (b1 << 8);
        out.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        out.push(ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
        out.push('=');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_encodes_rfc4648_vectors() {
        // Vectors straight from RFC 4648 §10.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_handles_utf8_payload() {
        let s = "héllo ✨";
        let enc = base64_encode(s.as_bytes());
        // Round-trip via a known-good decoder would require a base64
        // crate; instead sanity-check the alphabet is well-formed.
        for c in enc.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=',
                "unexpected base64 char: {c}"
            );
        }
        // Length must be a multiple of 4.
        assert_eq!(enc.len() % 4, 0);
    }
}

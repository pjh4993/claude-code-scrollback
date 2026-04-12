//! Transcript viewer: state, layout cache, keymap, and render.
//!
//! The viewer is built around a pre-rendered line cache (`Vec<RenderedLine>`)
//! owned by [`TranscriptState`]. Scrolling is a slice into this cache;
//! the cache is rebuilt only on transcript, width, or (future) collapse
//! changes. This keeps `j/k` and half-page scroll O(1).

pub mod keymap;
pub mod layout;
pub mod render;
pub mod state;

pub use keymap::{handle_key, Action};
pub use render::render;
pub use state::TranscriptState;

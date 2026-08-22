#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::missing_docs_in_private_items)]

//! # toptop
//!
//! A fast, memory-efficient, real-time Linux terminal system monitor and resource visualizer
//! written in Rust with Ratatui and Unicode Braille graphics.

/// Global memory allocator using `dlmalloc` to minimize heap metadata overhead.
#[global_allocator]
static ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

/// Application state, event loop, and input handling.
mod app;
/// Process telemetry and table sorting module.
mod process;
/// Point-in-time telemetry snapshots and history buffers.
mod snapshot;
/// System telemetry and hardware probe module.
mod system;
/// Theme, multi-stop linear gradient, and color calculation module.
mod theme;
/// User interface layout and Ratatui rendering engine.
mod ui;
/// Formatter utilities, fuzzy searching, and clipboard helpers.
mod utils;

use std::io;

use crossterm::{
    ExecutableCommand,
    event::{DisableMouseCapture, EnableMouseCapture},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::app::App;

/// Entry point for `toptop`. Initializes terminal raw mode, sets up panic hooks,
/// runs the application event loop, and cleanly restores terminal state on exit.
///
/// # Errors
/// Returns an `io::Result` if terminal initialization or restoration fails.
fn main() -> io::Result<()> {
    enable_raw_mode()?;
    io::stdout()
        .execute(EnterAlternateScreen)?
        .execute(EnableMouseCapture)?;

    let original_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(DisableMouseCapture);
        let _ = io::stdout().execute(LeaveAlternateScreen);
        original_panic(info);
    }));

    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let result = App::new().run(&mut terminal);

    disable_raw_mode()?;
    io::stdout()
        .execute(DisableMouseCapture)?
        .execute(LeaveAlternateScreen)?;

    result
}

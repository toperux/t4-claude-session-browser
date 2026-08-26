//! Browse, preview and delete Claude Code sessions.
//!
//! The crate is a library only so that the two binaries can share it: `csb` is a
//! console app carrying the CLI and TUI, and `csb-gui` is a Windows GUI-subsystem
//! app that exists purely so a Start Menu shortcut does not drag a console window
//! along behind it.

pub mod cli;
pub mod del;
pub mod gui;
pub mod index;
pub mod paths;
pub mod transcript;
pub mod tui;
pub mod update;

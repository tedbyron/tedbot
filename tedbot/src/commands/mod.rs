//! Slash commands. Each one is registered in `main.rs` (`FrameworkOptions.commands`).
//!
//! A command's `///` doc comment and its parameters' `#[description]`s are what Discord shows in
//! the command picker, so keep them short and user-facing.

pub mod channels;

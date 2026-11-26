//! Client adapter layer: terminal user interface and interactive shell for Gents.
//!
//! Presents sessions, transcripts, and slash commands over `ratatui`.

pub mod app;
pub mod cli;
pub mod commands;
pub mod draw;
pub mod filter;
pub mod jump;
pub mod keys;
pub mod layout;
pub mod sample;
pub mod source;
pub mod terminal;
#[cfg(test)]
mod testing;

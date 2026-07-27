//! Tauri-agnostic desktop bridge logic extracted from the Gents Desktop app.
//!
//! Phase 1 of the reusable-desktop-packages design: command implementations,
//! snapshot builders, view models, cascade/interrupt logic, and logging live
//! here. The `#[tauri::command]` wrappers and managed state remain in the app
//! until plugin-ization (phase 3).

pub mod cascade;
pub mod cause_derivation;
pub mod commands;
pub mod logging;
pub mod snapshot;
pub mod types;

#[cfg(test)]
mod tests;

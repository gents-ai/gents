//! Tauri-agnostic desktop bridge logic extracted from the Gents Desktop app.
//!
//! Phase 1 of the reusable-desktop-packages design: command implementations,
//! snapshot builders, view models, cascade/interrupt logic, and logging live
//! here. The `#[tauri::command]` wrappers and managed state remain in the app
//! until plugin-ization (phase 3).
//!
//! Phase 2 adds the contract fingerprint, `BridgeError` taxonomy, and the
//! type-generation spike (`ts-rs`) that phase 3 consumes.

pub mod cascade;
pub mod cause_derivation;
pub mod commands;
pub mod contract;
pub mod error;
pub mod logging;
pub mod snapshot;
pub mod types;

pub use contract::{current_contract, BridgeContract, CONTRACT_VERSION, PACKAGE_VERSION};
pub use error::{BridgeError, BridgeErrorCode};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod typegen;

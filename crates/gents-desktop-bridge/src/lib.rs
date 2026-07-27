//! Gents desktop bridge: Tauri plugin + view models + command logic.
//!
//! Hosts compose with:
//! ```ignore
//! gents_desktop_bridge::install_runtime();
//! tauri::Builder::default()
//!     .plugin(gents_desktop_bridge::init(BridgeConfig::default()))
//! ```
//!
//! Invoke paths: `plugin:gents-desktop-bridge|<command>`.

pub mod cascade;
pub mod cause_derivation;
pub mod commands;
pub mod config;
pub mod contract;
pub mod error;
pub mod logging;
pub mod plugin;
pub mod runtime_setup;
pub mod snapshot;
pub mod state;
pub mod tauri_commands;
pub mod types;

pub use config::{
    AgentHomePolicy, AppMeta, BootstrapPolicy, BridgeConfig, HomePolicy, TracingConfig,
};
pub use contract::{current_contract, BridgeContract, CONTRACT_VERSION, PACKAGE_VERSION};
pub use error::{BridgeError, BridgeErrorCode};
pub use plugin::init;
pub use runtime_setup::{init_tracing, install_runtime};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod typegen;

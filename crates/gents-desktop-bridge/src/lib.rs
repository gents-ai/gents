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

pub mod cause_derivation;
pub mod commands;
pub mod config;
pub mod contract;
pub mod error;
pub mod host_browser;
pub mod interrupt;
pub mod logging;
pub mod package_tools;
pub mod plugin;
pub mod provenance;
pub mod runtime_setup;
pub mod snapshot;
pub mod state;
pub mod tauri_commands;
pub mod types;

pub use config::{
    AgentHomePolicy, AppMeta, BootstrapPolicy, BridgeConfig, HomePolicy, ManagedServerPolicy,
    TracingConfig,
};
pub use error::{BridgeError, BridgeErrorCode};
pub use package_tools::{prefer_host_tools, prepare_host_command};
pub use plugin::init;
pub use runtime_setup::{init_tracing, install_runtime};
pub use snapshot::projection::SnapshotGrants;
pub use state::resolve_policy;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod typegen;

pub mod accounts;
#[cfg(feature = "tauri-host")]
mod app_exit;
pub mod approvals_watch;
pub mod cause_derivation;
pub mod client_runtime;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod client_test_support;
pub mod commands;
pub mod config;
pub mod contract;
pub mod error;
pub mod host;
pub mod host_browser;
pub mod inference_setup;
pub mod interrupt;
pub mod job_control;
pub mod job_view;
mod local_pairing;
pub mod logging;
#[cfg(feature = "tauri-host")]
pub mod navigation;
pub mod package_tools;
#[cfg(feature = "tauri-host")]
pub mod plugin;
pub mod plugin_access;
pub mod process_control;
pub mod provenance;
pub mod provider_recovery;
pub mod runtime_pack_watch;
#[cfg(feature = "tauri-host")]
pub mod runtime_setup;
pub mod snapshot;
#[cfg(feature = "tauri-host")]
pub mod state;
#[cfg(feature = "tauri-host")]
pub mod tauri_commands;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_support;
pub mod types;
#[cfg(feature = "tauri-host")]
mod ux_watch;

pub use config::{
    AppMeta, BootstrapPolicy, BridgeConfig, HomePolicy, ManagedServerPolicy, NodeHomePolicy,
    TracingConfig,
};
pub use gents_store::local::store_key::StoreKeyCustodyChoice;
pub use package_tools::{prefer_host_tools, prepare_host_command};
#[cfg(feature = "tauri-host")]
pub use plugin::init;
#[cfg(feature = "tauri-host")]
pub use runtime_setup::{init_tracing, install_runtime};
pub use snapshot::projection::SnapshotGrants;
#[cfg(feature = "tauri-host")]
pub use state::resolve_policy;

#[cfg(test)]
mod tests;
#[cfg(any(feature = "typegen", all(test, feature = "tauri-host")))]
pub mod typegen;

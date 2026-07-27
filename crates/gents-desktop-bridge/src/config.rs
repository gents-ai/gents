//! Host-supplied bridge configuration. Storage home is resolved once from
//! these policies at plugin init — the webview never supplies filesystem paths.

use std::path::PathBuf;

use crate::snapshot::projection::SnapshotGrants;

/// Configuration passed to [`crate::init`].
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Where the client's storage home comes from.
    pub home: HomePolicy,
    /// Host-side ceiling on local-runtime provisioning.
    pub bootstrap: BootstrapPolicy,
    /// Host identity metadata for logs/diagnostics (not payloads).
    pub app_meta: AppMeta,
    /// Process-wide snapshot section grants (v1: single profile per process).
    ///
    /// Hosts must set this to match the *maximum* capability profile they grant
    /// any webview. Fail-closed default is [`SnapshotGrants::core_only`] — Gents
    /// Desktop and other full hosts set [`SnapshotGrants::all`] explicitly.
    /// Per-caller capability introspection is deferred (see design doc).
    pub snapshot_grants: SnapshotGrants,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            home: HomePolicy::Default,
            bootstrap: BootstrapPolicy::LocalRuntimeAllowed {
                agent_home: AgentHomePolicy::Default,
            },
            app_meta: AppMeta {
                app_name: "gents-desktop".into(),
                app_version: env!("CARGO_PKG_VERSION").into(),
            },
            // Fail closed: third-party hosts that take Default must opt into
            // domain sections explicitly (Gents Desktop sets SnapshotGrants::all).
            snapshot_grants: SnapshotGrants::core_only(),
        }
    }
}

/// Where the Gents client store lives.
#[derive(Debug, Clone)]
pub enum HomePolicy {
    /// `GENTS_DESKTOP_HOME` env override, else platform data dir `/gents/desktop`.
    /// Delegation to `DesktopPaths::discover()` — not a re-specification.
    Default,
    /// Host app-data directory + subdirectory (sandbox-safe).
    AppDataDir { subdirectory: &'static str },
    /// Exact root (tests/fixtures).
    FixedRoot(PathBuf),
}

/// Whether the host permits local Gents runtime provisioning.
#[derive(Debug, Clone)]
pub enum BootstrapPolicy {
    /// `desktop_init_local_standard` may run; agent home from `agent_home`.
    LocalRuntimeAllowed { agent_home: AgentHomePolicy },
    /// Local runtime init fails with `BridgeErrorCode::Unsupported`.
    PairedRemoteOnly,
}

/// Where the local agent home lives when local runtime is allowed.
#[derive(Debug, Clone)]
pub enum AgentHomePolicy {
    /// Existing `~/.gents` conventions via `default_agent_home()`.
    Default,
    /// Exact agent home path.
    Fixed(PathBuf),
}

/// Host identity for logs/diagnostics and bootstrap summary.
#[derive(Debug, Clone)]
pub struct AppMeta {
    pub app_name: String,
    pub app_version: String,
}

/// Explicit tracing setup — the plugin never derives a log path itself.
#[derive(Debug, Clone)]
pub struct TracingConfig {
    pub log_path: PathBuf,
    pub filter: Option<String>,
    pub console: bool,
}

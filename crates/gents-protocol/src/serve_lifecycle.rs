//! Whether a serving runtime has finished starting, as `/status` reports it.
//!
//! Local discovery reads `runtime.json`, so a runtime is ready for discovery
//! and pairing only once it is written.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Top-level `/status` field carrying the [`ServeLifecycle`].
pub const STATUS_LIFECYCLE_FIELD: &str = "lifecycle";

/// Top-level `/status` field carrying the runtime's package version.
pub const STATUS_VERSION_FIELD: &str = "version";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServeLifecycle {
    /// Bound, but migrations, runtime readiness or the `runtime.json` write
    /// have not finished.
    Starting,
    /// `runtime.json` describes this process and it accepts work.
    Ready,
}

/// What a client concludes from one `/status` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservedServeLifecycle {
    Ready,
    /// Still starting. A lifecycle value this client does not know is also
    /// not ready: readiness is never inferred.
    Starting,
    /// The payload has no lifecycle field: the runtime predates it and never
    /// reports readiness this way. `version` is its reported package version.
    Outdated {
        version: Option<String>,
    },
}

impl ObservedServeLifecycle {
    pub fn observe(status: &Value) -> Self {
        let Some(lifecycle) = status.get(STATUS_LIFECYCLE_FIELD) else {
            return Self::Outdated {
                version: status
                    .get(STATUS_VERSION_FIELD)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|version| !version.is_empty())
                    .map(str::to_owned),
            };
        };
        match serde_json::from_value(lifecycle.clone()) {
            Ok(ServeLifecycle::Ready) => Self::Ready,
            Ok(ServeLifecycle::Starting) | Err(_) => Self::Starting,
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// Why a runtime that predates the lifecycle field cannot be used.
pub fn outdated_runtime_message(version: Option<&str>) -> String {
    let running = version.map_or_else(
        || "the running agent".to_string(),
        |version| format!("the running agent (v{})", version.trim_start_matches('v')),
    );
    format!(
        "{running} predates this app (v{}); restart it so it runs this version",
        env!("CARGO_PKG_VERSION")
    )
}
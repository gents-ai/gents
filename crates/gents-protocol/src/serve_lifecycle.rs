//! Whether a serving runtime has finished starting, as `/status` reports it.
//!
//! The HTTP surface binds before migrations run, before the runtime accepts
//! work and before `runtime.json` names the process. Local discovery reads that
//! file, so a runtime is ready for discovery and pairing only once it is written.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_explicit_ready_field_is_ready() {
        assert!(
            ObservedServeLifecycle::observe(&serde_json::json!({ "lifecycle": "ready" }))
                .is_ready()
        );
        for status in [
            serde_json::json!({ "lifecycle": "starting" }),
            serde_json::json!({ "lifecycle": "draining" }),
            serde_json::json!({ "lifecycle": null }),
        ] {
            assert_eq!(
                ObservedServeLifecycle::observe(&status),
                ObservedServeLifecycle::Starting
            );
        }
    }

    #[test]
    fn a_runtime_without_the_field_is_outdated_with_its_version() {
        assert_eq!(
            ObservedServeLifecycle::observe(
                &serde_json::json!({ "status": "ok", "version": "0.18.2" })
            ),
            ObservedServeLifecycle::Outdated {
                version: Some("0.18.2".to_string())
            }
        );
        assert_eq!(
            ObservedServeLifecycle::observe(&serde_json::json!({ "agent_did": "did:key:a" })),
            ObservedServeLifecycle::Outdated { version: None }
        );
        assert_eq!(
            outdated_runtime_message(Some("0.18.2")),
            format!(
                "the running agent (v0.18.2) predates this app (v{}); restart it so it runs this version",
                env!("CARGO_PKG_VERSION")
            )
        );
    }
}

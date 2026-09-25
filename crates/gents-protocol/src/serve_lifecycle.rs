//! Whether a serving runtime has finished starting, as `/status` reports it.
//!
//! The HTTP surface binds before migrations run, before the runtime accepts
//! work and before `runtime.json` names the process. Local discovery reads that
//! file, so a runtime is ready for discovery and pairing only once it is written.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Top-level `/status` field carrying the [`ServeLifecycle`].
pub const STATUS_LIFECYCLE_FIELD: &str = "lifecycle";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServeLifecycle {
    /// Bound, but migrations, runtime readiness or the `runtime.json` write
    /// have not finished.
    Starting,
    /// `runtime.json` describes this process and it accepts work.
    Ready,
}

impl ServeLifecycle {
    /// The lifecycle a `/status` payload reports. A payload without a
    /// recognized field is still starting: readiness is never inferred.
    pub fn observed(status: &Value) -> Self {
        status
            .get(STATUS_LIFECYCLE_FIELD)
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or(Self::Starting)
    }

    pub fn is_ready(self) -> bool {
        self == Self::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_explicit_ready_field_is_ready() {
        assert!(ServeLifecycle::observed(&serde_json::json!({ "lifecycle": "ready" })).is_ready());
        for status in [
            serde_json::json!({ "lifecycle": "starting" }),
            serde_json::json!({ "lifecycle": "serving" }),
            serde_json::json!({ "lifecycle": null }),
            serde_json::json!({ "status": "ok", "agent_did": "did:key:a" }),
        ] {
            assert_eq!(ServeLifecycle::observed(&status), ServeLifecycle::Starting);
        }
    }
}

//! Backend registry — DefraDB lookups and local concurrency tracking.
//!
//! The scheduler uses this to resolve a profile's backend, check health,
//! and enforce `max_concurrent` limits. Concurrency is tracked locally
//! in memory (sufficient for single agent-daemon instance). Acquired
//! capacity is represented by an owned permit so release happens on all
//! terminal paths, including panics.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use defra_node::EmbeddedNode;

use crate::graphql::escape_graphql_string;

/// An inference backend document from DefraDB.
#[derive(Debug, Clone)]
pub struct InferenceBackend {
    pub backend_id: String,
    pub name: String,
    pub endpoint: String,
    pub max_concurrent: i64,
    pub enabled: bool,
    pub probe_status: String,
}

impl InferenceBackend {
    /// Parse from a DefraDB JSON value.
    pub fn from_value(v: &serde_json::Value) -> Option<Self> {
        Some(Self {
            backend_id: v.get("backend_id")?.as_str()?.to_string(),
            name: v.get("name")?.as_str()?.to_string(),
            endpoint: v.get("endpoint")?.as_str()?.to_string(),
            max_concurrent: v.get("max_concurrent")?.as_i64()?,
            enabled: v.get("enabled")?.as_bool()?,
            probe_status: v
                .get("probe_status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
        })
    }

    /// Whether this backend is available for scheduling.
    pub fn is_available(&self) -> bool {
        self.enabled && self.probe_status == "healthy"
    }
}

/// Local concurrency tracker for backends.
///
/// Tracks how many agents are currently running against each backend.
/// All tracking is in-process memory — sufficient for a single daemon.
/// Uses `std::sync::Mutex` because the critical section is trivial
/// (no await inside the lock).
pub struct BackendTracker {
    running: Mutex<HashMap<String, i64>>,
}

/// Owned reservation of one backend execution slot.
///
/// Dropping the permit releases the slot back to the tracker.
pub struct BackendPermit {
    tracker: Arc<BackendTracker>,
    backend_id: String,
}

impl Drop for BackendPermit {
    fn drop(&mut self) {
        self.tracker.release(&self.backend_id);
    }
}

impl Default for BackendTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl BackendTracker {
    pub fn new() -> Self {
        Self {
            running: Mutex::new(HashMap::new()),
        }
    }

    /// Atomically check capacity and acquire a slot if available.
    /// Returns `true` if a slot was acquired, `false` if at capacity.
    ///
    /// This is the primary API for the scheduler — it avoids the
    /// TOCTOU race between separate `has_capacity` + `acquire` calls.
    pub fn try_acquire(&self, backend_id: &str, max_concurrent: i64) -> bool {
        let mut running = self.running.lock().expect("BackendTracker lock poisoned");
        let count = running.entry(backend_id.to_string()).or_insert(0);
        if *count < max_concurrent {
            *count += 1;
            true
        } else {
            false
        }
    }

    /// Acquire an owned permit for a backend if capacity is available.
    ///
    /// The returned permit releases its slot on drop, so scheduler code
    /// does not need to manually balance acquire/release across error paths.
    pub fn try_acquire_permit(
        self: &Arc<Self>,
        backend_id: impl Into<String>,
        max_concurrent: i64,
    ) -> Option<BackendPermit> {
        let backend_id = backend_id.into();
        self.try_acquire(&backend_id, max_concurrent)
            .then(|| BackendPermit {
                tracker: Arc::clone(self),
                backend_id,
            })
    }

    /// Decrement the running count for a backend.
    pub fn release(&self, backend_id: &str) {
        let mut running = self.running.lock().expect("BackendTracker lock poisoned");
        let count = running.entry(backend_id.to_string()).or_insert(0);
        *count = (*count - 1).max(0);
    }

    /// Current running count for a backend.
    pub fn running_count(&self, backend_id: &str) -> i64 {
        let running = self.running.lock().expect("BackendTracker lock poisoned");
        running.get(backend_id).copied().unwrap_or(0)
    }
}

/// Look up a backend by `backend_id` from DefraDB.
pub async fn lookup_backend(
    node: &EmbeddedNode,
    backend_id: &str,
) -> Result<Option<InferenceBackend>> {
    let escaped_id = escape_graphql_string(backend_id);
    let query = format!(
        r#"query {{ InferenceBackend(filter: {{backend_id: {{_eq: "{}"}}}}) {{ backend_id name endpoint max_concurrent enabled probe_status }} }}"#,
        escaped_id
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query InferenceBackend failed: {:?}", resp.errors);
    }

    let backend = resp
        .data
        .as_ref()
        .and_then(|d| d.get("InferenceBackend"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(InferenceBackend::from_value);

    Ok(backend)
}

/// Query all enabled backends from DefraDB.
pub async fn list_enabled_backends(node: &EmbeddedNode) -> Result<Vec<InferenceBackend>> {
    let query = r#"query { InferenceBackend(filter: {enabled: {_eq: true}}) { backend_id name endpoint max_concurrent enabled probe_status models last_probe } }"#;

    let resp = node.execute(query).await;
    if resp.has_errors() {
        anyhow::bail!("query InferenceBackend failed: {:?}", resp.errors);
    }

    let backends = resp
        .data
        .as_ref()
        .and_then(|d| d.get("InferenceBackend"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(InferenceBackend::from_value)
                .collect()
        })
        .unwrap_or_default();

    Ok(backends)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inference_backend_from_value_parses() {
        let json = serde_json::json!({
            "backend_id": "workstation-dual",
            "name": "Workstation Dual GPU",
            "endpoint": "http://100.73.235.38:8000",
            "max_concurrent": 4,
            "enabled": true,
            "probe_status": "healthy",
        });

        let backend = InferenceBackend::from_value(&json).expect("should parse");
        assert_eq!(backend.backend_id, "workstation-dual");
        assert_eq!(backend.endpoint, "http://100.73.235.38:8000");
        assert_eq!(backend.max_concurrent, 4);
        assert!(backend.enabled);
        assert_eq!(backend.probe_status, "healthy");
    }

    #[test]
    fn inference_backend_from_value_missing_probe_status_defaults() {
        let json = serde_json::json!({
            "backend_id": "test",
            "name": "Test",
            "endpoint": "http://localhost:8000",
            "max_concurrent": 1,
            "enabled": true,
        });

        let backend = InferenceBackend::from_value(&json).expect("should parse");
        assert_eq!(backend.probe_status, "unknown");
    }

    #[test]
    fn is_available_requires_enabled_and_healthy() {
        let healthy = InferenceBackend {
            backend_id: "test".into(),
            name: "Test".into(),
            endpoint: "http://localhost:8000".into(),
            max_concurrent: 1,
            enabled: true,
            probe_status: "healthy".into(),
        };
        assert!(healthy.is_available());

        let disabled = InferenceBackend {
            enabled: false,
            ..healthy.clone()
        };
        assert!(!disabled.is_available());

        let unhealthy = InferenceBackend {
            probe_status: "unhealthy".into(),
            ..healthy.clone()
        };
        assert!(!unhealthy.is_available());
    }

    #[test]
    fn try_acquire_respects_capacity() {
        let tracker = BackendTracker::new();

        assert_eq!(tracker.running_count("b1"), 0);
        assert!(tracker.try_acquire("b1", 2));
        assert_eq!(tracker.running_count("b1"), 1);

        assert!(tracker.try_acquire("b1", 2));
        assert_eq!(tracker.running_count("b1"), 2);

        // At capacity — should fail
        assert!(!tracker.try_acquire("b1", 2));
        assert_eq!(tracker.running_count("b1"), 2);

        tracker.release("b1");
        assert_eq!(tracker.running_count("b1"), 1);

        // Has capacity again
        assert!(tracker.try_acquire("b1", 2));
    }

    #[test]
    fn release_floors_at_zero() {
        let tracker = BackendTracker::new();
        tracker.release("nonexistent");
        assert_eq!(tracker.running_count("nonexistent"), 0);
    }

    #[test]
    fn backend_permit_releases_on_drop() {
        let tracker = Arc::new(BackendTracker::new());

        {
            let _permit = tracker
                .try_acquire_permit("b1", 1)
                .expect("permit should be acquired");
            assert_eq!(tracker.running_count("b1"), 1);
        }

        assert_eq!(tracker.running_count("b1"), 0);
    }
}

//! Backend registry — DefraDB lookups and local concurrency tracking.
//!
//! The scheduler uses this to resolve a behavior's backend, check health,
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
    /// OpenAI-compatible API base URL, including the `/v1` path segment.
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
    Ok(lookup_backend_record(node, backend_id)
        .await?
        .map(|(_, backend)| backend))
}

pub(crate) async fn lookup_backend_record(
    node: &EmbeddedNode,
    backend_id: &str,
) -> Result<Option<(String, InferenceBackend)>> {
    let escaped_id = escape_graphql_string(backend_id);
    let query = format!(
        r#"query {{ InferenceBackend(filter: {{backend_id: {{_eq: "{}"}}}}) {{ _docID backend_id name endpoint max_concurrent enabled probe_status }} }}"#,
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
        .and_then(|row| {
            Some((
                row.get("_docID")?.as_str()?.to_string(),
                InferenceBackend::from_value(row)?,
            ))
        });

    Ok(backend)
}

pub(crate) async fn lookup_backend_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, InferenceBackend)>> {
    let escaped_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"query {{ InferenceBackend(filter: {{_docID: {{_eq: "{}"}}}}, limit: 1) {{ _docID backend_id name endpoint max_concurrent enabled probe_status }} }}"#,
        escaped_id
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query InferenceBackend by _docID failed: {:?}", resp.errors);
    }

    let backend = resp
        .data
        .as_ref()
        .and_then(|d| d.get("InferenceBackend"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|row| {
            Some((
                row.get("_docID")?.as_str()?.to_string(),
                InferenceBackend::from_value(row)?,
            ))
        });

    Ok(backend)
}

pub(crate) async fn list_backend_records(
    node: &EmbeddedNode,
) -> Result<Vec<(String, InferenceBackend)>> {
    let query = r#"query {
        InferenceBackend(order: { backend_id: ASC }) {
            _docID
            backend_id
            name
            endpoint
            max_concurrent
            enabled
            probe_status
        }
    }"#;

    let resp = node.execute(query).await;
    if resp.has_errors() {
        anyhow::bail!("list InferenceBackend failed: {:?}", resp.errors);
    }

    let backends = resp
        .data
        .as_ref()
        .and_then(|d| d.get("InferenceBackend"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|row| {
                    Some((
                        row.get("_docID")?.as_str()?.to_string(),
                        InferenceBackend::from_value(row)?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(backends)
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
mod tests;

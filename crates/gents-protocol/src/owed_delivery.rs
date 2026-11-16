//! The `OwedDelivery` document (Lean `Hooks.OwedDelivery`).

use serde::{Deserialize, Serialize};

/// The physical subject a delivery is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwedSubject {
    Request,
    Session,
}

impl OwedSubject {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Session => "session",
        }
    }
}

/// Lean `Hooks.OwedDelivery.Record.settled`, with `Pending` for `none`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwedSettlement {
    Pending,
    Delivered,
    Dropped,
}

impl OwedSettlement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Delivered => "delivered",
            Self::Dropped => "dropped",
        }
    }
}

pub const OWED_DELIVERY_FIELDS: &str = "_docID delivery_key node_did node_doc_id parent_linked \
point plugin subject_kind subject_id subject_node_did actor_did runner_did terminal_state reason occurred_at \
halt_latch attempts last_error settlement created_at updated_at";

/// One owed delivery. `subject_node_did` and the event scalars rebuild the
/// point's event; no row of a plugin or of core is copied into it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwedDeliveryRow {
    #[serde(rename = "_docID", default, skip_serializing)]
    pub doc_id: Option<String>,
    pub delivery_key: String,
    pub node_did: String,
    #[serde(default)]
    pub node_doc_id: Option<String>,
    #[serde(default)]
    pub parent_linked: Option<bool>,
    pub point: String,
    pub plugin: String,
    pub subject_kind: OwedSubject,
    pub subject_id: String,
    pub subject_node_did: String,
    #[serde(default)]
    pub actor_did: Option<String>,
    pub runner_did: String,
    #[serde(default)]
    pub terminal_state: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    pub occurred_at: String,
    #[serde(default)]
    pub halt_latch: Option<String>,
    #[serde(default)]
    pub attempts: i64,
    #[serde(default)]
    pub last_error: Option<String>,
    pub settlement: OwedSettlement,
    pub created_at: String,
    pub updated_at: String,
}

/// The deterministic key: the point, the plugin and the physical subject. A
/// halt recurs on one session, so its subject includes the halt's own time; a
/// request becomes terminal once. Encoded as a JSON array so no identifier can
/// collide with a separator.
pub fn delivery_key(
    point: &str,
    plugin: &str,
    subject: OwedSubject,
    subject_id: &str,
    occurred_at: Option<&str>,
) -> String {
    serde_json::json!([point, plugin, subject.as_str(), subject_id, occurred_at]).to_string()
}

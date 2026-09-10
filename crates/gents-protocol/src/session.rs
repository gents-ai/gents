//! Canonical durable session document. Replaces AgentSession/AgentConversation row copies.
//! Schema, writers, readers and execution adapters migrate after Lean and conformance.

use serde::{Deserialize, Serialize};

/// One session, owned by a principal and derived from one behavior.
/// `_docID` and revision metadata belong to the database envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSession {
    pub session_id: String,
    pub agent_did: String,
    /// Exact requester scope; absence is not a wildcard or a grant of access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester_did: Option<String>,
    /// The only configuration selection. Context and inference resolve through it.
    pub behavior_id: String,
    /// Original session creation time, unchanged by resume or later requests.
    pub created_at: String,
    /// Explicit close time; absent for an open session. Reopening clears it.
    /// Authoritative request execution state remains on AgentRequest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<SessionTitle>,
    /// User-managed labels for filtering/grouping; never execution or provenance.
    #[serde(
        default,
        deserialize_with = "crate::row::deserialize_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
    /// Creation provenance for tracing/grouping, never another execution selector.
    #[serde(
        default,
        deserialize_with = "deserialize_provenance",
        skip_serializing_if = "provenance_is_empty"
    )]
    pub provenance: Option<SessionProvenance>,
    /// Compact presentation for index-only peers. Never admission or retry authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<SessionObservation>,
}

/// Existing session projection owner's materialized list view, kept in the same doc.
/// Advancing a request uses canonical request order; refreshing requires its exact
/// identity. Title/close/resume patches preserve request identity and preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionObservation {
    pub last_activity_at: String,
    /// Latest prompt snippet; existing whitespace normalization and 240-char bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// An unobserved referenced request must not fall back to an older local request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_request: Option<SessionRequestObservation>,
}

/// Identity and state observed together; actual lifecycle authority is AgentRequest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRequestObservation {
    pub request_doc_id: String,
    pub request_id: String,
    pub lifecycle_state: crate::request_lifecycle::RequestLifecycleState,
}

/// Keep text and its origin together for the existing title replacement policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionTitle {
    pub text: String,
    pub source: SessionTitleSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTitleSource {
    Placeholder,
    Generated,
    Task,
    User,
}

/// These origins can coexist: a graph stage invokes a task, and a subagent can
/// be spawned within that work. References record actual creation facts; they
/// neither select configuration nor confer graph membership or authorization.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionProvenance {
    /// Task invoked to create this session, scoped to the session's agent_did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Existing graph execution, not a second graph definition or stage config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_run_id: Option<String>,
    /// Exact causal request document for a spawned session, not its logical label.
    /// Further tool-call/trigger lineage remains on the existing request owners.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_request_doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<SessionFork>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionFork {
    pub source_session_id: String,
    /// Existing fork API's user-turn cut, including zero for an empty prefix.
    pub at_user_turn: u32,
}

fn provenance_is_empty(value: &Option<SessionProvenance>) -> bool {
    value.as_ref().is_none_or(|value| {
        value.task_id.is_none()
            && value.graph_run_id.is_none()
            && value.parent_request_doc_id.is_none()
            && value.fork.is_none()
    })
}

fn deserialize_provenance<'de, D>(deserializer: D) -> Result<Option<SessionProvenance>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<SessionProvenance>::deserialize(deserializer)?;
    Ok(if provenance_is_empty(&value) {
        None
    } else {
        value
    })
}

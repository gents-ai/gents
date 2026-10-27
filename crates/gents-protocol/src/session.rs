use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSession {
    pub session_id: String,
    pub node_did: String,
    /// Exact requester scope; absence is not a wildcard or a grant of access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester_did: Option<String>,
    /// The only configuration selection. Context and inference resolve through it.
    pub agent_id: String,
    pub created_at: String,
    /// Explicit close time; absent for an open session. Reopening clears it.
    /// Authoritative request execution state remains on AgentRequest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    /// Shared history visibility; independent of closing, stopping and request execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    /// Native transaction fence for archive/restore observations, including ABA changes.
    #[serde(
        default,
        deserialize_with = "crate::row::deserialize_null_default",
        skip_serializing_if = "archive_revision_is_zero"
    )]
    pub archive_revision: u64,
    /// Durable explicit-stop latch. Interrupts pause autonomous Goal work but
    /// leave this unset so subsequent ordinary messages remain admissible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<SessionTitle>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionObservation {
    pub last_activity_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_preview: Option<String>,
    /// An unobserved referenced request must not fall back to an older local request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_request: Option<SessionRequestObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRequestObservation {
    pub request_doc_id: String,
    pub request_id: String,
    pub lifecycle_state: crate::request_lifecycle::RequestLifecycleState,
}

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

/// These origins can coexist: a task runs, and that work can start another
/// session with `agent_new`. References record actual creation facts; they
/// neither select configuration nor confer authorization.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct SessionProvenance {
    /// Task invoked to create this session, scoped to the session's node_did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub task_id: Option<String>,
    /// Sessions stored before graph runs were removed may carry this key;
    /// it is accepted and ignored, and never written again.
    #[serde(default, rename = "graph_run_id", skip_serializing)]
    #[cfg_attr(feature = "typescript", ts(skip))]
    pub retired_graph_run_id: crate::row::RetiredKey,
    /// Exact causal request document for a session started by `agent_new`,
    /// copied from its first request's `caused_by_parent_request_doc_id`. It is
    /// provenance only: no hierarchy, cascade or authority follows from it.
    /// Further tool-call/trigger lineage remains on the existing request owners.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub parent_request_doc_id: Option<String>,
    /// The request whose claim or refusal created this session through a
    /// calling tool call, recorded once with `parent_request_doc_id` and
    /// never rewritten (Lean `AgentSession.Provenance.openingRequestDoc`).
    /// Readers open a caused session at it (Lean
    /// `DurableLineage.openingRequest`); a session created before it was
    /// recorded has none and falls back to `(created_at, _docID)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub opening_request_doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub fork: Option<SessionFork>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct SessionFork {
    pub source_session_id: String,
    pub at_user_turn: u32,
    /// Exact edit anchor committed with the child. Absence does not authorize
    /// replay of a caller-selected destination (Lean `SessionFork.decideEditFork`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub before_message_key: Option<String>,
    /// Native DID that authored the retained fork, independent of a shared
    /// runtime operator requester scope. This records creation, not a grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub edit_client_did: Option<String>,
}

fn provenance_is_empty(value: &Option<SessionProvenance>) -> bool {
    value.as_ref().is_none_or(|value| {
        value.task_id.is_none()
            && value.parent_request_doc_id.is_none()
            && value.opening_request_doc_id.is_none()
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

fn archive_revision_is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum SessionArchiveOutcome {
    Applied,
    Unchanged,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct SessionArchiveReceipt {
    pub node_did: String,
    pub session_id: String,
    pub archived_at: Option<String>,
    /// Decimal text preserves the database revision range in JavaScript clients.
    pub revision: String,
    pub outcome: SessionArchiveOutcome,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum SessionArchiveVisibility {
    #[default]
    Active,
    Archived,
    All,
}

impl SessionArchiveVisibility {
    pub fn includes(self, archived: bool) -> bool {
        match self {
            Self::Active => !archived,
            Self::Archived => archived,
            Self::All => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_0_21_provenance_with_graph_run_id_decodes_and_drops_it() {
        let stored: SessionProvenance = serde_json::from_value(serde_json::json!({
            "task_id": "task", "graph_run_id": "graph",
            "fork": {"source_session_id": "parent", "at_user_turn": 2}
        }))
        .expect("a stored provenance with the retired key decodes");
        assert_eq!(
            serde_json::to_value(&stored).unwrap(),
            serde_json::json!({"task_id": "task", "fork": {"source_session_id": "parent", "at_user_turn": 2}})
        );
    }
}

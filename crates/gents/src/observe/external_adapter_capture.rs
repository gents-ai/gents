use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use self::langgraph::import_langgraph_capture;
use self::multi_agent::import_multi_agent_capture;
use self::validation::validate_external_adapter_capture_mapping;
use crate::observe::adapter_projection::{AdapterProjectionEnvelope, AdapterProjectionKind};
use crate::observe::run_timeline::{TimelineRequestRow, TimelineSessionRow, TimelineToolCallRow};

pub(crate) mod langgraph;
mod multi_agent;
mod validation;

#[derive(Debug, Clone, Deserialize)]
pub struct ExternalAdapterCapture {
    pub source: ExternalAdapterSource,
    #[serde(default)]
    pub native: Value,
    #[serde(default)]
    pub mapping: Option<ExternalAdapterMapping>,
    #[serde(default)]
    pub envelope: Option<AdapterProjectionEnvelope>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExternalAdapterSource {
    pub system: String,
    #[serde(default)]
    pub package: Option<String>,
    #[serde(default)]
    pub package_version: Option<String>,
    #[serde(default)]
    pub generator: Option<String>,
    #[serde(default)]
    pub capture: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExternalAdapterMapping {
    pub projection: AdapterProjectionKind,
    #[serde(default)]
    pub scenario_id: Option<String>,
    pub request_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub node_did: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub actor_did: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub participants: Vec<ExternalParticipantMapping>,
    #[serde(default)]
    pub delegations: Vec<ExternalDelegationMapping>,
    #[serde(default)]
    pub tool_events: Vec<ExternalToolEventMapping>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExternalParticipantMapping {
    #[serde(default)]
    pub native_name: Option<String>,
    pub role: String,
    #[serde(default)]
    pub node_did: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExternalDelegationMapping {
    pub parent_request_id: String,
    pub child_request_id: String,
    #[serde(default)]
    pub parent_tool_call_id: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub node_did: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExternalToolEventMapping {
    pub id: String,
    pub request_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub child_request_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExternalAdapterImport {
    pub projection: AdapterProjectionKind,
    /// A derived import view, never a set of canonical DefraDB timeline rows.
    pub view: ExternalAdapterView,
    pub actor_did: Option<String>,
    pub source_system: String,
    pub scenario_id: String,
}

#[derive(Debug, Clone)]
pub struct ExternalAdapterView {
    pub request: TimelineRequestRow,
    pub session: Option<TimelineSessionRow>,
    pub requests: Vec<TimelineRequestRow>,
    pub messages: Vec<ExternalAdapterMessage>,
    pub tool_calls: Vec<TimelineToolCallRow>,
}

#[derive(Debug, Clone)]
pub struct ExternalAdapterMessage {
    /// Stable only within this imported capture; never a DefraDB document
    /// identifier.
    pub capture_message_label: String,
    /// External participant role retained for adapter export; it grants no
    /// native message role or request authority.
    pub source_role: String,
    pub session_id: String,
    pub request_id: Option<String>,
    pub sequence: i64,
    pub message: gents_protocol::message::Message,
    pub captured_at: Option<String>,
}

#[derive(Debug, Clone)]
struct ImportedMessage {
    role: String,
    content: String,
    request_id: Option<String>,
}

pub fn import_external_adapter_capture_to_derived_view(
    capture: &ExternalAdapterCapture,
) -> Result<ExternalAdapterImport> {
    let mapping = capture.mapping.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "external adapter capture {} is missing mapping metadata",
            capture.source.system
        )
    })?;
    validate_external_adapter_capture_mapping(capture, mapping)?;
    match mapping.projection {
        AdapterProjectionKind::MultiAgentTask => import_multi_agent_capture(capture, mapping),
        AdapterProjectionKind::LangGraphStateHistory => import_langgraph_capture(capture, mapping),
        AdapterProjectionKind::AtifTrajectory | AdapterProjectionKind::OpenAiCodexRunTrace => {
            bail!(
                "external adapter import for projection {} is not implemented",
                mapping.projection.id()
            )
        }
    }
}

/// An import fixture, not a persistence decoder: it never fabricates a
/// canonical transcript header or physical document identity.
fn imported_external_message(
    session_id: &str,
    request_id: &str,
    sequence: i64,
    role: &str,
    content: &str,
) -> ExternalAdapterMessage {
    let message = match role.to_ascii_lowercase().as_str() {
        "system" => gents_protocol::message::Message::system(content),
        "assistant" | "agent" | "ai" | "model" => {
            gents_protocol::message::Message::assistant(content)
        }
        _ => gents_protocol::message::Message::user(content),
    };
    let created_at = timestamp_for_index(sequence.max(0) as usize);
    ExternalAdapterMessage {
        capture_message_label: format!("capture:{session_id}:{sequence}"),
        source_role: role.to_owned(),
        session_id: session_id.to_string(),
        request_id: Some(request_id.to_string()),
        sequence,
        captured_at: Some(created_at),
        message,
    }
}

fn default_delegation_tool_call_id(delegation: &ExternalDelegationMapping) -> String {
    format!(
        "external:{}:{}",
        delegation.parent_request_id, delegation.child_request_id
    )
}

fn timestamp_for_index(index: usize) -> String {
    format!("2026-06-05T00:00:{:02}Z", index.min(59))
}

fn value_to_text(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn first_owned<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    values
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// A read-only projection of supplied capture identity, never a persisted doc.
fn imported_session(
    session_id: &str,
    node_did: Option<&str>,
    agent_id: Option<&str>,
    created_at: &str,
    title: String,
    preview: Option<&str>,
) -> Option<TimelineSessionRow> {
    use gents_protocol::session::{
        AgentSession, SessionObservation, SessionTitle, SessionTitleSource,
    };
    let node_did = node_did.filter(|value| !value.trim().is_empty())?;
    let agent_id = agent_id.filter(|value| !value.trim().is_empty())?;
    Some(TimelineSessionRow {
        doc_id: None,
        session: AgentSession {
            session_id: session_id.to_owned(),
            node_did: node_did.to_owned(),
            requester_did: None,
            agent_id: agent_id.to_owned(),
            created_at: created_at.to_owned(),
            closed_at: None,
            archived_at: None,
            archive_revision: 0,
            stopped_at: None,
            title: Some(SessionTitle {
                text: title,
                source: SessionTitleSource::Generated,
            }),
            tags: Vec::new(),
            provenance: None,
            observation: Some(SessionObservation {
                reply_preview: None,
                last_activity_at: created_at.to_owned(),
                preview: preview.map(crate::session::derive_session_preview),
                latest_request: None,
            }),
        },
    })
}

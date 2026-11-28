use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use self::langgraph::import_langgraph_capture;
use self::multi_agent::import_multi_agent_capture;
use self::validation::validate_external_adapter_capture_mapping;
use crate::observe::adapter_projection::AdapterProjectionEnvelope;
use crate::observe::run_timeline::rows::{
    TimelineRequestRow, TimelineSessionRow, TimelineToolCallRow,
};
use gents_protocol::adapter_projection_kind::AdapterProjectionKind;

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
    pub agent_did: Option<String>,
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
    pub agent_did: Option<String>,
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
    pub agent_did: Option<String>,
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
    agent_did: Option<&str>,
    created_at: &str,
    title: String,
    preview: Option<&str>,
) -> Option<TimelineSessionRow> {
    use gents_protocol::session::{
        AgentSession, SessionObservation, SessionTitle, SessionTitleSource,
    };
    let node_did = node_did.filter(|value| !value.trim().is_empty())?;
    let agent_did = agent_did.filter(|value| !value.trim().is_empty())?;
    Some(TimelineSessionRow {
        doc_id: None,
        session: AgentSession {
            session_id: session_id.to_owned(),
            node_did: node_did.to_owned(),
            requester_did: None,
            agent_did: agent_did.to_owned(),
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
                preview: preview.map(gents_session::session::derive_session_preview),
                latest_request: None,
            }),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::adapter_projection::{
        build_external_adapter_projection, validate_adapter_projection_contract, AdapterProjection,
        ProjectionContext, ProjectionRedactionMode, ProjectionSourceVersionStatus,
        EXTERNAL_ADAPTER_CAPTURE_PROJECTION_ID,
    };
    use serde_json::json;

    fn multi_agent_capture() -> Value {
        json!({
            "source": {"system":"autogen-agentchat","package":"autogen-agentchat","package_version":"0.7.0","generator":"unit-test"},
            "native": {"messages":[
                {"source":"planner","content":"Plan the work."},
                {"source":"researcher","content":"Research complete."}
            ]},
            "mapping": {
                "projection":"multi_agent_task","scenario_id":"unit-test","request_id":"req-root","session_id":"session-negative",
                "participants":[
                    {"role":"planner","node_did":"did:test:planner"},
                    {"role":"researcher","node_did":"did:test:researcher","request_id":"req-child"}
                ],
                "delegations":[{"parent_request_id":"req-root","child_request_id":"req-child","parent_tool_call_id":"tool-delegate","tool_name":"delegate_to_researcher","status":"completed"}],
                "tool_events":[{"id":"tool-delegate","request_id":"req-root","tool_name":"delegate_to_researcher","status":"completed","child_request_id":"req-child"}]
            }
        })
    }

    fn langgraph_capture() -> Value {
        json!({
            "source":{"system":"langgraph","package":"langgraph","package_version":"0.2.0"},
            "native":{"thread_id":"thread-missing-history"},
            "mapping":{"projection":"langgraph_state_history","scenario_id":"missing-history","request_id":"req-langgraph"}
        })
    }

    fn mutated_capture<F>(mutate: F) -> Value
    where
        F: FnOnce(&mut Value),
    {
        let mut capture = multi_agent_capture();
        mutate(&mut capture);
        capture
    }

    #[test]
    fn external_projection_preserves_mapped_children_without_forging_native_provenance(
    ) -> Result<()> {
        let capture: ExternalAdapterCapture = serde_json::from_value(multi_agent_capture())?;
        let imported = import_external_adapter_capture_to_derived_view(&capture)?;
        assert!(imported
            .view
            .requests
            .iter()
            .all(|r| r.doc_id.is_none() && r.caused_by_parent_request_doc_id.is_none()));
        let envelope = build_external_adapter_projection(&capture, &ProjectionContext::default())?;
        validate_adapter_projection_contract(&envelope)?;
        assert_eq!(
            envelope.provenance.source_projection_id,
            EXTERNAL_ADAPTER_CAPTURE_PROJECTION_ID
        );
        assert_eq!(
            envelope.provenance.source_version_status,
            ProjectionSourceVersionStatus::ExternalAdapterCapture
        );
        assert!(envelope.source_request_doc_id.is_none());
        let projection = serde_json::to_value(&envelope)?;
        assert_eq!(
            projection
                .pointer("/output/projection/messages")
                .and_then(Value::as_array)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            projection.pointer("/output/projection/messages/0/role"),
            Some(&json!("planner"))
        );
        assert_eq!(
            projection.pointer("/output/projection/messages/1/role"),
            Some(&json!("researcher"))
        );
        assert_eq!(
            projection.pointer("/output/projection/delegations/0/parent_tool_call_id"),
            Some(&json!("tool-delegate"))
        );
        for redaction_mode in [
            ProjectionRedactionMode::TrainingSafe,
            ProjectionRedactionMode::Public,
        ] {
            let envelope = build_external_adapter_projection(
                &capture,
                &ProjectionContext {
                    actor_did: None,
                    redaction_mode,
                },
            )?;
            validate_adapter_projection_contract(&envelope)?;
            let rendered = serde_json::to_string(&envelope.output)?;
            assert!(
                !rendered.contains("Research complete.")
                    && !rendered.contains("did:test:researcher")
            );
        }
        Ok(())
    }

    #[test]
    fn external_langgraph_projection_preserves_history_graph_tasks_and_redacts_values() -> Result<()>
    {
        let mut value = langgraph_capture();
        value["native"]["graph"] = json!({
            "nodes":["provider_model","research_subgraph"],
            "edges":[{"from":"langgraph:start","to":"langgraph:node:provider_model","kind":"flow"}],
            "subgraphs":{"research_subgraph":{"nodes":["search"]}}
        });
        value["native"]["history"] = json!([
            {"config":{"configurable":{"checkpoint_id":"checkpoint-current"}},"values":{"topic":"private-topic","status":"completed","child_request_id":"req-child"}},
            {"values":{"topic":"prior-topic"}}
        ]);
        let capture: ExternalAdapterCapture = serde_json::from_value(value)?;
        let envelope = build_external_adapter_projection(&capture, &ProjectionContext::default())?;
        validate_adapter_projection_contract(&envelope)?;
        let AdapterProjection::LangGraphStateHistory(projection) = &envelope.output else {
            panic!("expected LangGraph projection");
        };
        assert_eq!(projection.checkpoint_id, "checkpoint-current");
        assert_eq!(projection.values["history_checkpoint_count"], json!(2));
        assert_eq!(projection.values["topic"], json!("private-topic"));
        assert!(projection
            .nodes
            .iter()
            .any(|n| n.id == "langgraph:subgraph:research:search"));
        assert_eq!(projection.edges.len(), 1);
        assert!(projection.tasks.iter().any(|t| t.name == "search"));
        let public = build_external_adapter_projection(
            &capture,
            &ProjectionContext {
                actor_did: None,
                redaction_mode: ProjectionRedactionMode::Public,
            },
        )?;
        validate_adapter_projection_contract(&public)?;
        assert!(!serde_json::to_string(&public)?.contains("private-topic"));
        Ok(())
    }

    #[test]
    fn external_capture_rejects_malformed_json_and_bad_mappings() {
        let parse_err = serde_json::from_str::<ExternalAdapterCapture>(
            r#"{"source":{"system":"autogen-agentchat"},"mapping":"#,
        )
        .unwrap_err();
        assert!(parse_err.is_eof(), "{parse_err}");
        let wrong_envelope = json!({
            "projection_id":"langgraph_state_history","projection_version":"v1","source_request_id":"req-root","redaction_mode":"full",
            "provenance":{"runtime":"gents","source_projection_id":"run_timeline","source_projection_version":"v1","source_version_status":"current_state_captured_only"},
            "output":{"adapter":"langgraph_state_history","projection":{"checkpoint_id":"checkpoint-negative","root_request_id":"req-root","values":{"request_id":"req-root"},"nodes":[{"id":"langgraph:start","kind":"start"}],"edges":[],"tasks":[]}}
        });
        for (name, value, expected) in [
            ("missing participants", mutated_capture(|c| c["mapping"]["participants"] = json!([])), "must include at least one participant"),
            ("delegation absent child", mutated_capture(|c| c["mapping"]["delegations"][0]["child_request_id"] = json!("req-missing")), "child_request_id \"req-missing\" does not reference a declared child participant"),
            ("tool absent child", mutated_capture(|c| c["mapping"]["tool_events"][0]["child_request_id"] = json!("req-missing")), "child_request_id \"req-missing\" does not reference a declared child participant"),
            ("unknown framework", mutated_capture(|c| c["source"]["system"] = json!("unknown-agent-framework")), "is not supported for mapped import"),
            ("wrong envelope", mutated_capture(|c| c["envelope"] = wrong_envelope.clone()), "envelope projection langgraph_state_history does not match mapping projection multi_agent_task"),
            ("langgraph no history", langgraph_capture(), "requires non-empty native.history"),
        ] {
            let capture: ExternalAdapterCapture = serde_json::from_value(value).unwrap();
            let err = format!("{:#}", import_external_adapter_capture_to_derived_view(&capture).unwrap_err());
            assert!(err.contains(expected), "{name}: expected {expected:?}, got {err}");
        }
    }
}

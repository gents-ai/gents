use std::collections::BTreeSet;

use anyhow::{bail, Result};
use serde_json::Value;

use super::{ExternalAdapterCapture, ExternalAdapterMapping};
use crate::observe::adapter_projection::{
    validate_adapter_projection_contract, AdapterProjectionKind,
};

pub(super) fn validate_external_adapter_capture_mapping(
    capture: &ExternalAdapterCapture,
    mapping: &ExternalAdapterMapping,
) -> Result<()> {
    require_nonempty_field("source.system", &capture.source.system)?;
    require_nonempty_field("mapping.request_id", &mapping.request_id)?;
    if let Some(envelope) = capture.envelope.as_ref() {
        validate_adapter_projection_contract(envelope).map_err(|error| {
            anyhow::anyhow!("external adapter capture envelope is invalid: {error}")
        })?;
        if envelope.output.kind() != mapping.projection {
            bail!(
                "external adapter capture envelope projection {} does not match mapping projection {}",
                envelope.output.kind().id(),
                mapping.projection.id()
            );
        }
    }
    match mapping.projection {
        AdapterProjectionKind::MultiAgentTask => {
            validate_supported_source_system(
                capture,
                &["autogen-agentchat", "crewai", "microsoft-agent-framework"],
            )?;
            validate_multi_agent_mapping(mapping)
        }
        AdapterProjectionKind::LangGraphStateHistory => {
            validate_supported_source_system(capture, &["langgraph"])?;
            validate_langgraph_mapping(capture)
        }
        AdapterProjectionKind::AtifTrajectory | AdapterProjectionKind::OpenAiCodexRunTrace => {
            Ok(())
        }
    }
}

fn validate_supported_source_system(
    capture: &ExternalAdapterCapture,
    allowed: &[&str],
) -> Result<()> {
    if allowed.contains(&capture.source.system.as_str()) {
        return Ok(());
    }
    bail!(
        "external adapter capture source.system {:?} is not supported for mapped import; expected one of {}",
        capture.source.system,
        allowed.join(", ")
    );
}

fn validate_multi_agent_mapping(mapping: &ExternalAdapterMapping) -> Result<()> {
    if mapping.participants.is_empty() {
        bail!("multi-agent external adapter mapping must include at least one participant");
    }

    let mut request_ids = BTreeSet::from([mapping.request_id.as_str()]);
    let mut has_root_participant = false;
    for participant in &mapping.participants {
        require_nonempty_field("mapping.participants[].role", &participant.role)?;
        match participant.request_id.as_deref() {
            Some(request_id) => {
                require_nonempty_field("mapping.participants[].request_id", request_id)?;
                if request_id == mapping.request_id {
                    has_root_participant = true;
                }
                request_ids.insert(request_id);
            }
            None => has_root_participant = true,
        }
    }
    if !has_root_participant {
        bail!(
            "multi-agent external adapter mapping must include a root participant for request_id {:?}",
            mapping.request_id
        );
    }

    for delegation in &mapping.delegations {
        require_nonempty_field(
            "mapping.delegations[].parent_request_id",
            &delegation.parent_request_id,
        )?;
        require_nonempty_field(
            "mapping.delegations[].child_request_id",
            &delegation.child_request_id,
        )?;
        if !request_ids.contains(delegation.parent_request_id.as_str()) {
            bail!(
                "multi-agent delegation parent_request_id {:?} does not reference a declared participant/root request",
                delegation.parent_request_id
            );
        }
        if !request_ids.contains(delegation.child_request_id.as_str()) {
            bail!(
                "multi-agent delegation child_request_id {:?} does not reference a declared child participant",
                delegation.child_request_id
            );
        }
    }

    for event in &mapping.tool_events {
        require_nonempty_field("mapping.tool_events[].id", &event.id)?;
        require_nonempty_field("mapping.tool_events[].request_id", &event.request_id)?;
        require_nonempty_field("mapping.tool_events[].tool_name", &event.tool_name)?;
        if !request_ids.contains(event.request_id.as_str()) {
            bail!(
                "multi-agent tool event {:?} request_id {:?} does not reference a declared participant/root request",
                event.id,
                event.request_id
            );
        }
        if let Some(child_request_id) = event.child_request_id.as_deref() {
            require_nonempty_field("mapping.tool_events[].child_request_id", child_request_id)?;
            if !request_ids.contains(child_request_id) {
                bail!(
                    "multi-agent tool event {:?} child_request_id {:?} does not reference a declared child participant",
                    event.id,
                    child_request_id
                );
            }
        }
    }
    Ok(())
}

fn validate_langgraph_mapping(capture: &ExternalAdapterCapture) -> Result<()> {
    match capture.native.get("history").and_then(Value::as_array) {
        Some(history) if !history.is_empty() => Ok(()),
        _ => bail!("LangGraph external adapter mapping requires non-empty native.history"),
    }
}

fn require_nonempty_field(path: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{path} must not be empty");
    }
    Ok(())
}

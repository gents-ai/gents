mod messages;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use self::messages::native_messages;
use super::{
    default_delegation_tool_call_id, first_owned, imported_external_message, imported_session,
    timestamp_for_index, ExternalAdapterCapture, ExternalAdapterImport, ExternalAdapterMapping,
    ExternalAdapterView,
};
use crate::observe::run_timeline::{TimelineRequestRow, TimelineToolCallRow};

pub(super) fn import_multi_agent_capture(
    capture: &ExternalAdapterCapture,
    mapping: &ExternalAdapterMapping,
) -> Result<ExternalAdapterImport> {
    let session_id = mapping
        .session_id
        .clone()
        .unwrap_or_else(|| format!("external-session:{}", mapping.request_id));
    let status = mapping
        .status
        .clone()
        .unwrap_or_else(|| "completed".to_string());
    let lifecycle_state = gents_protocol::request_lifecycle::RequestLifecycleState::parse(&status)
        .with_context(|| format!("invalid mapped request lifecycle state {status:?}"))?;
    let started_at = "2026-06-05T00:00:00Z";
    let root_participant = mapping.participants.iter().find(|participant| {
        participant
            .request_id
            .as_deref()
            .is_none_or(|request_id| request_id == mapping.request_id)
    });
    let root_node_did = first_owned([
        mapping.node_did.as_deref(),
        root_participant.and_then(|participant| participant.node_did.as_deref()),
    ]);
    let root_agent_id = first_owned([
        mapping.agent_id.as_deref(),
        root_participant.and_then(|participant| participant.agent_id.as_deref()),
    ]);
    let participant_by_request = mapping
        .participants
        .iter()
        .filter_map(|participant| {
            let request_id = participant.request_id.as_ref()?;
            Some((request_id.as_str(), participant))
        })
        .collect::<BTreeMap<_, _>>();
    let delegation_by_child = mapping
        .delegations
        .iter()
        .map(|delegation| (delegation.child_request_id.as_str(), delegation))
        .collect::<BTreeMap<_, _>>();
    let child_request_ids = child_request_ids(mapping);
    let mut requests = Vec::new();
    requests.push(TimelineRequestRow {
        request_id: mapping.request_id.clone(),
        node_did: root_node_did.clone(),
        agent_id: root_agent_id.clone(),
        session_id: Some(session_id.clone()),
        content: external_task_text(&capture.native),
        lifecycle_state: Some(lifecycle_state),
        backend_id: capture.source.package.clone(),
        created_at: Some(started_at.to_string()),
        retry_count: Some(0),
        ..Default::default()
    });

    for child_request_id in child_request_ids {
        let participant = participant_by_request
            .get(child_request_id.as_str())
            .copied();
        let delegation = delegation_by_child.get(child_request_id.as_str()).copied();
        requests.push(TimelineRequestRow {
            request_id: child_request_id.clone(),
            node_did: first_owned([
                participant.and_then(|participant| participant.node_did.as_deref()),
                delegation.and_then(|delegation| delegation.node_did.as_deref()),
            ]),
            agent_id: first_owned([
                participant.and_then(|participant| participant.agent_id.as_deref()),
                delegation.and_then(|delegation| delegation.agent_id.as_deref()),
            ]),
            session_id: Some(session_id.clone()),
            content: participant
                .and_then(|participant| participant.native_name.as_deref())
                .map(|name| format!("Imported external participant {name}")),
            lifecycle_state: Some(lifecycle_state),
            backend_id: capture.source.package.clone(),
            created_at: Some(started_at.to_string()),
            retry_count: Some(0),
            caused_by_parent_request_id: delegation
                .map(|delegation| delegation.parent_request_id.clone()),
            caused_by_parent_tool_call_id: delegation.and_then(|delegation| {
                delegation
                    .parent_tool_call_id
                    .clone()
                    .or_else(|| Some(default_delegation_tool_call_id(delegation)))
            }),
            ..Default::default()
        });
    }

    let messages = native_messages(&capture.source.system, &capture.native, mapping)
        .into_iter()
        .enumerate()
        .map(|(index, message)| {
            imported_external_message(
                &session_id,
                &message
                    .request_id
                    .unwrap_or_else(|| mapping.request_id.clone()),
                (index as i64) + 1,
                &message.role,
                &message.content,
            )
        })
        .collect::<Vec<_>>();

    let mut seen_tool_call_ids = BTreeSet::new();
    let mut tool_calls = Vec::new();
    for (index, delegation) in mapping.delegations.iter().enumerate() {
        let tool_call_id = delegation
            .parent_tool_call_id
            .clone()
            .unwrap_or_else(|| default_delegation_tool_call_id(delegation));
        seen_tool_call_ids.insert(tool_call_id.clone());
        tool_calls.push(TimelineToolCallRow {
            request_id: Some(delegation.parent_request_id.clone()),
            session_id: session_id.clone(),
            message_sequence: Some((index as i64) + 1),
            tool_name: delegation
                .tool_name
                .clone()
                .unwrap_or_else(|| "handoff".to_string()),
            tool_call_id,
            args: json!({
                "source_system": capture.source.system,
                "child_request_id": delegation.child_request_id,
            })
            .to_string(),
            // Mapping metadata supplies no delivered tool-result payload.
            result: None,
            status: delegation
                .status
                .clone()
                .unwrap_or_else(|| "completed".to_string()),
            started_at: Some(timestamp_for_index(index + 1)),
            completed_at: Some(timestamp_for_index(index + 1)),
            child_request_id: Some(delegation.child_request_id.clone()),
            ..Default::default()
        });
    }
    for (index, event) in mapping.tool_events.iter().enumerate() {
        if !seen_tool_call_ids.insert(event.id.clone()) {
            continue;
        }
        tool_calls.push(TimelineToolCallRow {
            request_id: Some(event.request_id.clone()),
            session_id: session_id.clone(),
            message_sequence: Some((index as i64) + 1),
            tool_name: event.tool_name.clone(),
            tool_call_id: event.id.clone(),
            args: json!({ "source_system": capture.source.system }).to_string(),
            // Not evidence of a delivered result body.
            result: None,
            status: event
                .status
                .clone()
                .unwrap_or_else(|| "completed".to_string()),
            started_at: Some(timestamp_for_index(index + 1)),
            completed_at: Some(timestamp_for_index(index + 1)),
            child_request_id: event.child_request_id.clone(),
            ..Default::default()
        });
    }

    let root = requests
        .iter()
        .find(|request| request.request_id == mapping.request_id)
        .cloned()
        .context("imported rows missing root request")?;
    let scenario_id = mapping
        .scenario_id
        .clone()
        .unwrap_or_else(|| format!("{}:{}", capture.source.system, mapping.request_id));
    Ok(ExternalAdapterImport {
        projection: mapping.projection,
        actor_did: mapping.actor_did.clone(),
        source_system: capture.source.system.clone(),
        scenario_id,
        view: ExternalAdapterView {
            request: root,
            session: imported_session(
                &session_id,
                root_node_did.as_deref(),
                root_agent_id.as_deref(),
                started_at,
                format!("Imported {}", capture.source.system),
                external_task_text(&capture.native).as_deref(),
            ),
            requests,
            messages,
            tool_calls,
        },
    })
}

fn child_request_ids(mapping: &ExternalAdapterMapping) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for participant in &mapping.participants {
        if let Some(request_id) = participant.request_id.as_deref() {
            if request_id != mapping.request_id {
                ids.insert(request_id.to_string());
            }
        }
    }
    for delegation in &mapping.delegations {
        if delegation.child_request_id != mapping.request_id {
            ids.insert(delegation.child_request_id.clone());
        }
    }
    ids.into_iter().collect()
}

fn external_task_text(native: &Value) -> Option<String> {
    native
        .get("task")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

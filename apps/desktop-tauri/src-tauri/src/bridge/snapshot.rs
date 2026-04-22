use std::collections::HashMap;
use std::sync::Arc;

use defra_agent_desktop::client::{
    ClientCore, ClientPeerStatus, DesktopPaths, P2PHealth, PeerDirectory,
};
use defra_agent_desktop::local_runtime::default_agent_home;
use defra_agent_protocol::transcript::{normalize_markdown_text, present_persisted_message};
use serde_json::Value;

use super::types::{
    normalize_optional, turn_state_label, BehaviorView, ConversationSummary, DeploymentView,
    DesktopBootstrapSummary, DesktopClientSnapshot, DesktopRuntimeSnapshot, DesktopSessionSnapshot,
    MessageView, P2PHealthView, PendingTurnView, RenderedTimelineItem, RenderedToolCallView,
    ResponseView, RuntimeView, SavedPeerView, ToolCallView, ToolDetailFieldView,
    ToolDetailValueView, ToolResultView,
};

fn to_health_view(health: &P2PHealth) -> P2PHealthView {
    P2PHealthView {
        status: health.status_label().to_string(),
        connected_peer_count: health.connected_peer_count,
        replicator_count: health.replicator_count,
        consecutive_failures: health.consecutive_failures,
        last_error: health.last_error.clone(),
    }
}

pub(crate) async fn build_bootstrap_summary() -> Result<DesktopBootstrapSummary, String> {
    let agent_home = default_agent_home().map_err(|error| error.to_string())?;
    let desktop_paths = DesktopPaths::discover().map_err(|error| error.to_string())?;
    let peer_directory = PeerDirectory::load(desktop_paths.peer_directory_path())
        .await
        .map_err(|error| error.to_string())?;

    Ok(DesktopBootstrapSummary {
        default_agent_home: agent_home.display().to_string(),
        desktop_home: desktop_paths.root().display().to_string(),
        peer_directory_path: desktop_paths.peer_directory_path().display().to_string(),
        node_data_dir: desktop_paths.node_data_dir().display().to_string(),
        agent_home_exists: agent_home.exists(),
        desktop_home_exists: desktop_paths.root().exists(),
        peer_directory_exists: desktop_paths.peer_directory_path().exists(),
        saved_peers: peer_directory
            .records()
            .iter()
            .map(|peer| SavedPeerView {
                peer_id: peer.peer_id.clone(),
                label: peer.label.clone(),
                agent_did: peer.agent_did.clone(),
                addr: peer.addr.clone(),
                source: peer.source.clone(),
                graphql: peer.graphql.clone(),
            })
            .collect(),
    })
}

pub(crate) async fn build_runtime_snapshot(core: &ClientCore) -> DesktopRuntimeSnapshot {
    let store = core.store().snapshot();
    let peer_records = core.peer_records().await;
    let peer_statuses: HashMap<String, ClientPeerStatus> = core
        .peer_statuses()
        .into_iter()
        .map(|status| (status.agent_did.clone(), status))
        .collect();

    let mut deployments = peer_records
        .into_iter()
        .map(|peer| {
            let status = peer_statuses.get(&peer.agent_did);
            let default_behavior_id = store
                .default_behavior_id_for_agent(&peer.agent_did)
                .map(str::to_owned);
            let runtime = store
                .latest_runtime(&peer.agent_did)
                .map(|row| RuntimeView {
                    process_state: normalize_optional(row.process_state.as_deref()),
                    reconcile_phase: normalize_optional(row.reconcile_phase.as_deref()),
                    last_reconcile_result: normalize_optional(row.last_reconcile_result.as_deref()),
                    last_reconcile_error: normalize_optional(row.last_reconcile_error.as_deref()),
                    updated_at: normalize_optional(row.updated_at.as_deref()),
                });

            let mut behaviors = store
                .behavior_rows(&peer.agent_did)
                .into_iter()
                .map(|row| BehaviorView {
                    behavior_id: row.behavior_id.clone(),
                    display_name: normalize_optional(row.display_name.as_deref())
                        .unwrap_or_else(|| row.behavior_id.clone()),
                    model_name: normalize_optional(row.model_name.as_deref()),
                    enabled: row.enabled.unwrap_or(true),
                    is_default: default_behavior_id.as_deref() == Some(row.behavior_id.as_str()),
                })
                .collect::<Vec<_>>();
            behaviors.sort_by(|left, right| {
                right
                    .is_default
                    .cmp(&left.is_default)
                    .then_with(|| left.display_name.cmp(&right.display_name))
            });

            let mut conversations = store
                .conversation_rows(&peer.agent_did)
                .into_iter()
                .map(|row| {
                    let transcript = store.transcript(&row.session_id);
                    ConversationSummary {
                        session_id: row.session_id.clone(),
                        title: normalize_optional(row.title.as_deref()),
                        preview_text: normalize_optional(row.preview_text.as_deref()),
                        status: normalize_optional(row.status.as_deref()),
                        behavior_id: normalize_optional(row.behavior_id.as_deref()),
                        latest_request_id: store.latest_request_id_for_session(&row.session_id),
                        created_at: normalize_optional(row.created_at.as_deref()),
                        updated_at: normalize_optional(row.updated_at.as_deref()),
                        turn_state: store
                            .derive_turn(&row.session_id)
                            .map(turn_state_label)
                            .map(str::to_owned),
                        message_count: transcript.messages.len(),
                        tool_call_count: transcript.tool_calls.len(),
                    }
                })
                .collect::<Vec<_>>();
            conversations.sort_by(|left, right| {
                right
                    .updated_at
                    .cmp(&left.updated_at)
                    .then_with(|| right.created_at.cmp(&left.created_at))
            });

            DeploymentView {
                peer_id: peer.peer_id,
                label: peer.label,
                agent_did: peer.agent_did,
                addr: peer.addr,
                source: peer.source,
                graphql: peer.graphql,
                dial_succeeded: status.is_some_and(|status| status.dial_succeeded),
                last_error: status.and_then(|status| status.last_error.clone()),
                default_behavior_id,
                runtime,
                behaviors,
                conversations,
            }
        })
        .collect::<Vec<_>>();

    deployments.sort_by(|left, right| left.label.cmp(&right.label));

    DesktopRuntimeSnapshot {
        local_peer_id: core.local_peer_id().to_string(),
        listen_addresses: core.listen_addresses().to_vec(),
        p2p_health: to_health_view(&core.p2p_health()),
        bootstrap_errors: core.bootstrap_errors().to_vec(),
        last_mutation_error: core.last_mutation_error(),
        focused_request_id: core.store().focused_request_id(),
        configured_peer_count: core.configured_peer_count(),
        dialed_peer_count: core.dialed_peer_count(),
        peer_issue_count: core.peer_issue_count(),
        row_count: store.row_count(),
        approx_serialized_bytes: store.approx_serialized_bytes(),
        deployments,
    }
}

pub(crate) fn build_session_snapshot_from_store(
    store: &defra_agent_desktop::client::ClientStore,
    session_id: &str,
    preferred_request_id: Option<&str>,
) -> Option<DesktopSessionSnapshot> {
    let conversation = store
        .conversations
        .iter()
        .find(|row| row.session_id == session_id);
    let session_row = store
        .sessions
        .iter()
        .find(|row| row.session_id == session_id);
    let requests = store.requests_for_session(session_id);

    if conversation.is_none() && session_row.is_none() && requests.is_empty() {
        return None;
    }

    let transcript = store.transcript(session_id);
    let latest_request_id = preferred_request_id
        .filter(|request_id| requests.iter().any(|row| row.request_id == *request_id))
        .map(str::to_owned)
        .or_else(|| store.latest_request_id_for_session(session_id));
    let latest_request = latest_request_id
        .as_deref()
        .and_then(|request_id| {
            requests
                .iter()
                .find(|row| row.request_id == request_id)
                .copied()
        })
        .or_else(|| requests.last().copied());
    let turn_state = latest_request_id
        .as_deref()
        .and_then(|request_id| store.derive_turn_for_request(request_id))
        .or_else(|| store.derive_turn(session_id));
    let turn_state_label = turn_state.map(turn_state_label).map(str::to_owned);
    let latest_response = latest_request_id
        .as_deref()
        .and_then(|request_id| store.latest_response_for_request(request_id))
        .map(|row| ResponseView {
            status: normalize_optional(row.status.as_deref()),
            content: row
                .content
                .as_deref()
                .map(normalize_markdown_text)
                .filter(|value| !value.is_empty()),
            reasoning: row
                .reasoning
                .as_deref()
                .map(normalize_markdown_text)
                .filter(|value| !value.is_empty()),
            error_message: normalize_optional(row.error_message.as_deref()),
            token_count: row.token_count,
            materialized_message_sequence: row.materialized_message_sequence,
            materialized_at: normalize_optional(row.materialized_at.as_deref()),
            completed_at: normalize_optional(row.completed_at.as_deref()),
        });
    let active_response_overlay = latest_response.clone().filter(|response| {
        matches!(
            turn_state,
            Some(defra_agent_protocol::client_protocol::ClientTurnState::WaitingForClaim)
                | Some(defra_agent_protocol::client_protocol::ClientTurnState::Streaming)
        ) && response.materialized_message_sequence.is_none()
            && (response
                .content
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
                || response
                    .reasoning
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty()))
    });
    let pending_turn = latest_request_id
        .as_deref()
        .and_then(|request_id| build_pending_turn(store, session_id, request_id));
    let active_turn_index = latest_request_id
        .as_deref()
        .and_then(|request_id| logical_turn_index_for_request(store, session_id, request_id));

    let messages = transcript
        .messages
        .into_iter()
        .map(|row| {
            let role = normalize_optional(row.role.as_deref());
            let content = normalize_optional(row.content.as_deref());
            let presentation = role
                .as_deref()
                .zip(content.as_deref())
                .map(|(role, content)| present_persisted_message(role, content));

            MessageView {
                message_key: row.message_key.clone(),
                sequence: row.sequence,
                role,
                content,
                display_role: presentation
                    .as_ref()
                    .map(|presentation| presentation.role.label().to_ascii_lowercase()),
                display_content: presentation.as_ref().and_then(|presentation| {
                    normalize_optional(Some(presentation.body_markdown.as_str()))
                }),
                reasoning: presentation.as_ref().and_then(|presentation| {
                    presentation
                        .reasoning_markdown
                        .as_deref()
                        .and_then(|reasoning| normalize_optional(Some(reasoning)))
                }),
                has_tool_calls: presentation
                    .as_ref()
                    .is_some_and(|presentation| presentation.has_tool_calls),
                has_tool_results: presentation
                    .as_ref()
                    .is_some_and(|presentation| presentation.has_tool_results),
                timestamp: normalize_optional(row.timestamp.as_deref()),
            }
        })
        .collect::<Vec<_>>();

    let tool_calls = transcript
        .tool_calls
        .into_iter()
        .map(|row| ToolCallView {
            tool_call_key: row.tool_call_key.clone(),
            message_sequence: row.message_sequence,
            tool_name: normalize_optional(row.tool_name.as_deref()),
            tool_call_id: normalize_optional(row.tool_call_id.as_deref()),
            args: normalize_optional(row.args.as_deref()),
            result: normalize_optional(row.result.as_deref()),
            status: normalize_optional(row.status.as_deref()),
            started_at: normalize_optional(row.started_at.as_deref()),
            completed_at: normalize_optional(row.completed_at.as_deref()),
        })
        .collect::<Vec<_>>();

    let tool_results = transcript
        .tool_results
        .into_iter()
        .map(|row| ToolResultView {
            tool_name: normalize_optional(row.tool_name.as_deref()),
            tool_input: normalize_optional(row.tool_input.as_deref()),
            output_text: normalize_optional(row.output_text.as_deref()),
            truncated: row.truncated,
            created_at: normalize_optional(row.created_at.as_deref()),
        })
        .collect::<Vec<_>>();

    let timeline_items = build_rendered_timeline(
        &messages,
        &tool_calls,
        pending_turn.as_ref(),
        active_response_overlay.as_ref(),
        active_turn_index,
    );

    Some(DesktopSessionSnapshot {
        session_id: session_id.to_string(),
        agent_did: conversation
            .and_then(|row| normalize_optional(row.agent_did.as_deref()))
            .or_else(|| {
                latest_request.and_then(|row| normalize_optional(row.agent_did.as_deref()))
            }),
        behavior_id: conversation
            .and_then(|row| normalize_optional(row.behavior_id.as_deref()))
            .or_else(|| session_row.and_then(|row| normalize_optional(row.behavior_id.as_deref())))
            .or_else(|| {
                latest_request.and_then(|row| normalize_optional(row.behavior_id.as_deref()))
            }),
        title: conversation.and_then(|row| normalize_optional(row.title.as_deref())),
        preview_text: conversation.and_then(|row| normalize_optional(row.preview_text.as_deref())),
        status: conversation
            .and_then(|row| normalize_optional(row.status.as_deref()))
            .or_else(|| session_row.and_then(|row| normalize_optional(row.status.as_deref()))),
        turn_state: turn_state_label,
        latest_request_id,
        latest_response,
        active_response_overlay,
        pending_turn,
        timeline_items,
        messages,
        tool_calls,
        tool_results,
    })
}

fn normalize_timeline_text(value: Option<&str>) -> String {
    value.map(str::trim).unwrap_or_default().to_string()
}

fn render_json_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        _ => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    }
}

fn parse_tool_detail_value(value: Option<&str>) -> Option<ToolDetailValueView> {
    let raw_text = normalize_optional(value)?;
    let parsed = serde_json::from_str::<Value>(&raw_text).ok();
    let fields = match parsed {
        Some(Value::Object(map)) => map
            .into_iter()
            .map(|(key, value)| ToolDetailFieldView {
                key,
                value: render_json_value(&value),
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };

    Some(ToolDetailValueView { raw_text, fields })
}

fn tool_status_kind(status: Option<&str>) -> String {
    match status.unwrap_or_default().to_ascii_lowercase().as_str() {
        "completed" | "complete" | "success" => "success".to_string(),
        "failed" | "error" | "cancelled" => "error".to_string(),
        _ => "running".to_string(),
    }
}

fn render_tool_call(tool: ToolCallView) -> RenderedToolCallView {
    RenderedToolCallView {
        item_key: tool.tool_call_key.clone(),
        tool_name: tool.tool_name.clone().unwrap_or_else(|| "tool".to_string()),
        status_kind: tool_status_kind(tool.status.as_deref()),
        status: tool.status.clone(),
        args: parse_tool_detail_value(tool.args.as_deref()),
        result: parse_tool_detail_value(tool.result.as_deref()),
    }
}

fn live_overlay_suffix(
    committed_assistant_texts: &[String],
    overlay_text: Option<&str>,
) -> Option<String> {
    let mut remaining = normalize_timeline_text(overlay_text);
    if remaining.is_empty() {
        return None;
    }

    for committed_text in committed_assistant_texts {
        let normalized = committed_text.trim();
        if normalized.is_empty() {
            continue;
        }

        if remaining.starts_with(normalized) {
            remaining = remaining[normalized.len()..].trim_start().to_string();
        }
    }

    (!remaining.is_empty()).then_some(remaining)
}

fn request_turn_root_id(request: &defra_agent_protocol::row::AgentRequestRow) -> String {
    normalize_optional(request.retry_root_request.as_deref())
        .unwrap_or_else(|| request.request_id.clone())
}

fn logical_turn_roots_for_session(
    store: &defra_agent_desktop::client::ClientStore,
    session_id: &str,
) -> Vec<String> {
    let mut requests = store.requests_for_session(session_id);
    requests.sort_by(|left, right| {
        normalize_optional(left.created_at.as_deref())
            .cmp(&normalize_optional(right.created_at.as_deref()))
            .then_with(|| left.request_id.cmp(&right.request_id))
    });

    let mut roots = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for request in requests {
        let root_id = request_turn_root_id(request);
        if seen.insert(root_id.clone()) {
            roots.push(root_id);
        }
    }

    roots
}

fn logical_turn_index_for_request(
    store: &defra_agent_desktop::client::ClientStore,
    session_id: &str,
    request_id: &str,
) -> Option<usize> {
    let request = store.requests.iter().find(|row| {
        row.request_id == request_id && row.session_id.as_deref() == Some(session_id)
    })?;
    let root_id = request_turn_root_id(request);
    logical_turn_roots_for_session(store, session_id)
        .iter()
        .position(|candidate| candidate == &root_id)
}

fn materialized_user_turn_count(messages: &[MessageView]) -> usize {
    let mut ordered = messages.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|message| message.sequence.unwrap_or_default());
    ordered
        .into_iter()
        .filter(|message| {
            let role = message
                .display_role
                .as_deref()
                .or(message.role.as_deref())
                .unwrap_or_default();
            role.eq_ignore_ascii_case("user")
                && normalize_optional(message.display_content.as_deref()).is_some()
        })
        .count()
}

fn active_turn_committed_assistant_texts(
    messages: &[MessageView],
    active_turn_index: Option<usize>,
) -> Vec<String> {
    let Some(active_turn_index) = active_turn_index else {
        return Vec::new();
    };

    let mut ordered = messages.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|message| message.sequence.unwrap_or_default());

    let mut current_turn_index = None;
    let mut next_turn_index = 0usize;
    let mut assistant_texts = Vec::new();

    for message in ordered {
        let role = message
            .display_role
            .as_deref()
            .or(message.role.as_deref())
            .unwrap_or_default();
        let content = normalize_optional(message.display_content.as_deref());

        if role.eq_ignore_ascii_case("user") && content.is_some() {
            current_turn_index = Some(next_turn_index);
            next_turn_index += 1;
            continue;
        }

        if role.eq_ignore_ascii_case("assistant") && current_turn_index == Some(active_turn_index) {
            if let Some(content) = content {
                assistant_texts.push(content);
            }
        }
    }

    assistant_texts
}

fn build_rendered_timeline(
    messages: &[MessageView],
    tool_calls: &[ToolCallView],
    pending_turn: Option<&PendingTurnView>,
    active_response_overlay: Option<&ResponseView>,
    active_turn_index: Option<usize>,
) -> Vec<RenderedTimelineItem> {
    let mut timeline = Vec::new();
    let mut tool_groups: std::collections::BTreeMap<Option<i64>, Vec<ToolCallView>> =
        std::collections::BTreeMap::new();

    for tool in tool_calls.iter().cloned() {
        tool_groups
            .entry(tool.message_sequence)
            .or_default()
            .push(tool);
    }

    let mut used_group_keys = std::collections::BTreeSet::new();

    let mut timeline_messages = messages
        .iter()
        .filter(|message| {
            !message.has_tool_results
                && (!normalize_timeline_text(message.display_content.as_deref()).is_empty()
                    || !normalize_timeline_text(message.reasoning.as_deref()).is_empty()
                    || message.has_tool_calls)
        })
        .cloned()
        .collect::<Vec<_>>();
    timeline_messages.sort_by_key(|message| message.sequence.unwrap_or_default());

    for message in timeline_messages {
        let role = message
            .display_role
            .as_deref()
            .or(message.role.as_deref())
            .unwrap_or("assistant");
        let normalized_content = normalize_optional(message.display_content.as_deref());
        let normalized_reasoning = normalize_optional(message.reasoning.as_deref());

        match role {
            "user" => {
                if let Some(content) = normalized_content.clone() {
                    timeline.push(RenderedTimelineItem::UserMessage {
                        item_key: message.message_key.clone(),
                        sequence: message.sequence,
                        content,
                    });
                }
            }
            _ => {
                if normalized_content.is_some() || normalized_reasoning.is_some() {
                    timeline.push(RenderedTimelineItem::AssistantMessage {
                        item_key: message.message_key.clone(),
                        sequence: message.sequence,
                        content: normalized_content,
                        reasoning: normalized_reasoning,
                    });
                }
            }
        }

        let key = message.sequence;
        if let Some(grouped_tools) = tool_groups.get(&key).cloned() {
            used_group_keys.insert(key);
            timeline.push(RenderedTimelineItem::ToolGroup {
                item_key: format!("tools-{}", key.unwrap_or(-1)),
                message_sequence: key,
                tools: grouped_tools.into_iter().map(render_tool_call).collect(),
            });
        }
    }

    if let Some(pending_turn) = pending_turn {
        timeline.push(RenderedTimelineItem::PendingUserTurn {
            item_key: format!("pending-{}", pending_turn.request_id),
            request_id: pending_turn.request_id.clone(),
            content: pending_turn.content.clone(),
            lifecycle_state: pending_turn.lifecycle_state.clone(),
            created_at: pending_turn.created_at.clone(),
        });
    }

    for (key, grouped_tools) in tool_groups {
        if used_group_keys.contains(&key) {
            continue;
        }

        timeline.push(RenderedTimelineItem::ToolGroup {
            item_key: format!("tools-{}", key.unwrap_or(-1)),
            message_sequence: key,
            tools: grouped_tools.into_iter().map(render_tool_call).collect(),
        });
    }

    let committed_assistant_texts =
        active_turn_committed_assistant_texts(messages, active_turn_index);
    let overlay_content = live_overlay_suffix(
        &committed_assistant_texts,
        active_response_overlay.and_then(|overlay| overlay.content.as_deref()),
    );
    let overlay_reasoning = active_response_overlay
        .and_then(|overlay| normalize_optional(overlay.reasoning.as_deref()));
    if overlay_content.is_some() || overlay_reasoning.is_some() {
        timeline.push(RenderedTimelineItem::LiveAssistant {
            item_key: "live-assistant".to_string(),
            content: overlay_content,
            reasoning: overlay_reasoning,
        });
    }

    timeline
}

fn build_pending_turn(
    store: &defra_agent_desktop::client::ClientStore,
    session_id: &str,
    request_id: &str,
) -> Option<PendingTurnView> {
    let request = store.requests.iter().find(|row| {
        row.request_id == request_id && row.session_id.as_deref() == Some(session_id)
    })?;

    let lifecycle_state = normalize_optional(request.lifecycle_state.as_deref());
    let content = normalize_optional(request.content.as_deref())?;
    let active_turn_index = logical_turn_index_for_request(store, session_id, request_id)?;
    let transcript = store.transcript(session_id);
    let messages = transcript
        .messages
        .into_iter()
        .map(|row| {
            let role = normalize_optional(row.role.as_deref());
            let body = normalize_optional(row.content.as_deref());
            let presentation = role
                .as_deref()
                .zip(body.as_deref())
                .map(|(role, content)| present_persisted_message(role, content));

            MessageView {
                message_key: row.message_key.clone(),
                sequence: row.sequence,
                role,
                content: body,
                display_role: presentation
                    .as_ref()
                    .map(|presentation| presentation.role.label().to_ascii_lowercase()),
                display_content: presentation.as_ref().and_then(|presentation| {
                    normalize_optional(Some(presentation.body_markdown.as_str()))
                }),
                reasoning: None,
                has_tool_calls: false,
                has_tool_results: false,
                timestamp: normalize_optional(row.timestamp.as_deref()),
            }
        })
        .collect::<Vec<_>>();

    if materialized_user_turn_count(&messages) > active_turn_index {
        return None;
    }

    Some(PendingTurnView {
        request_id: request.request_id.clone(),
        content: content.to_string(),
        lifecycle_state,
        created_at: normalize_optional(request.created_at.as_deref()),
    })
}

pub(crate) async fn build_client_snapshot(
    core: Option<&Arc<ClientCore>>,
) -> Result<DesktopClientSnapshot, String> {
    let bootstrap = build_bootstrap_summary().await?;
    let client = match core {
        Some(core) => Some(build_runtime_snapshot(core.as_ref()).await),
        None => None,
    };
    Ok(DesktopClientSnapshot { bootstrap, client })
}

#[cfg(test)]
mod tests {
    use defra_agent_desktop::client::{ClientStore, ClientStoreRows};
    use defra_agent_protocol::row::{
        AgentConversationRow, AgentMessageRow, AgentRequestRow, AgentResponseRow, AgentSessionRow,
    };
    use rig::completion::message::{Message, Text, UserContent};
    use rig::one_or_many::OneOrMany;

    use super::{build_session_snapshot_from_store, RenderedTimelineItem};

    fn user_message_json(text: &str) -> String {
        serde_json::to_string(&Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: text.to_string(),
            })),
        })
        .expect("serialize user message")
    }

    #[test]
    fn session_snapshot_exposes_pending_turn_when_latest_request_is_not_materialized() {
        let store = ClientStore::from_rows(ClientStoreRows {
            conversations: vec![AgentConversationRow {
                session_id: "session-1".to_string(),
                agent_name: Some("Amy".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                title: Some("architecture-review".to_string()),
                title_source: Some("generated".to_string()),
                preview_text: Some("follow up question".to_string()),
                status: Some("active".to_string()),
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                updated_at: Some("2026-04-21T12:02:00Z".to_string()),
                latest_request_id: Some("req-2".to_string()),
            }],
            requests: vec![
                AgentRequestRow {
                    request_id: "req-1".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    session_id: Some("session-1".to_string()),
                    retry_parent_request: None,
                    retry_root_request: None,
                    superseded_by_request: None,
                    content: Some("first question".to_string()),
                    status: Some("completed".to_string()),
                    lifecycle_state: Some("completed".to_string()),
                    backend_id: None,
                    execution_origin: Some("interactive".to_string()),
                    failure_reason: None,
                    created_at: Some("2026-04-21T12:00:00Z".to_string()),
                    claimed_at: None,
                    deadline: None,
                    retry_count: Some(0),
                    max_retries: Some(3),
                    caused_by_trigger_id: None,
                    caused_by_trigger_kind: None,
                    interrupt_requested_at: None,
                    valid_until: None,
                },
                AgentRequestRow {
                    request_id: "req-2".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    session_id: Some("session-1".to_string()),
                    retry_parent_request: None,
                    retry_root_request: None,
                    superseded_by_request: None,
                    content: Some("follow up question".to_string()),
                    status: Some("processing".to_string()),
                    lifecycle_state: Some("processing".to_string()),
                    backend_id: None,
                    execution_origin: Some("interactive".to_string()),
                    failure_reason: None,
                    created_at: Some("2026-04-21T12:01:00Z".to_string()),
                    claimed_at: None,
                    deadline: None,
                    retry_count: Some(0),
                    max_retries: Some(3),
                    caused_by_trigger_id: None,
                    caused_by_trigger_kind: None,
                    interrupt_requested_at: None,
                    valid_until: None,
                },
            ],
            messages: vec![AgentMessageRow {
                message_key: "msg-1".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(1),
                role: Some("user".to_string()),
                content: Some(user_message_json("first question")),
                timestamp: Some("2026-04-21T12:00:00Z".to_string()),
            }],
            ..ClientStoreRows::default()
        });

        let snapshot =
            build_session_snapshot_from_store(&store, "session-1", None).expect("session snapshot");
        let pending = snapshot.pending_turn.expect("pending turn");
        assert_eq!(pending.request_id, "req-2");
        assert_eq!(pending.content, "follow up question");
        assert_eq!(pending.lifecycle_state.as_deref(), Some("processing"));
    }

    #[test]
    fn session_snapshot_hides_pending_turn_once_user_message_is_materialized() {
        let store = ClientStore::from_rows(ClientStoreRows {
            conversations: vec![AgentConversationRow {
                session_id: "session-1".to_string(),
                agent_name: Some("Amy".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                title: Some("architecture-review".to_string()),
                title_source: Some("generated".to_string()),
                preview_text: Some("follow up question".to_string()),
                status: Some("active".to_string()),
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                updated_at: Some("2026-04-21T12:02:00Z".to_string()),
                latest_request_id: Some("req-2".to_string()),
            }],
            requests: vec![AgentRequestRow {
                request_id: "req-2".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("follow up question".to_string()),
                status: Some("processing".to_string()),
                lifecycle_state: Some("processing".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:01:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            }],
            messages: vec![AgentMessageRow {
                message_key: "msg-2".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(2),
                role: Some("user".to_string()),
                content: Some(user_message_json("follow up question")),
                timestamp: Some("2026-04-21T12:01:01Z".to_string()),
            }],
            ..ClientStoreRows::default()
        });

        let snapshot =
            build_session_snapshot_from_store(&store, "session-1", None).expect("session snapshot");
        assert!(snapshot.pending_turn.is_none());
    }

    #[test]
    fn session_snapshot_keeps_pending_turn_for_repeated_prompt_until_second_user_message_materializes(
    ) {
        let store = ClientStore::from_rows(ClientStoreRows {
            conversations: vec![AgentConversationRow {
                session_id: "session-1".to_string(),
                agent_name: Some("Amy".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                title: Some("conversation".to_string()),
                title_source: Some("generated".to_string()),
                preview_text: Some("same prompt".to_string()),
                status: Some("active".to_string()),
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                updated_at: Some("2026-04-21T12:02:00Z".to_string()),
                latest_request_id: Some("req-2".to_string()),
            }],
            requests: vec![
                AgentRequestRow {
                    request_id: "req-1".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    session_id: Some("session-1".to_string()),
                    retry_parent_request: None,
                    retry_root_request: None,
                    superseded_by_request: None,
                    content: Some("same prompt".to_string()),
                    status: Some("completed".to_string()),
                    lifecycle_state: Some("completed".to_string()),
                    backend_id: None,
                    execution_origin: Some("interactive".to_string()),
                    failure_reason: None,
                    created_at: Some("2026-04-21T12:00:00Z".to_string()),
                    claimed_at: None,
                    deadline: None,
                    retry_count: Some(0),
                    max_retries: Some(3),
                    caused_by_trigger_id: None,
                    caused_by_trigger_kind: None,
                    interrupt_requested_at: None,
                    valid_until: None,
                },
                AgentRequestRow {
                    request_id: "req-2".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    session_id: Some("session-1".to_string()),
                    retry_parent_request: None,
                    retry_root_request: None,
                    superseded_by_request: None,
                    content: Some("same prompt".to_string()),
                    status: Some("processing".to_string()),
                    lifecycle_state: Some("processing".to_string()),
                    backend_id: None,
                    execution_origin: Some("interactive".to_string()),
                    failure_reason: None,
                    created_at: Some("2026-04-21T12:01:00Z".to_string()),
                    claimed_at: None,
                    deadline: None,
                    retry_count: Some(0),
                    max_retries: Some(3),
                    caused_by_trigger_id: None,
                    caused_by_trigger_kind: None,
                    interrupt_requested_at: None,
                    valid_until: None,
                },
            ],
            messages: vec![AgentMessageRow {
                message_key: "msg-1".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(1),
                role: Some("user".to_string()),
                content: Some(user_message_json("same prompt")),
                timestamp: Some("2026-04-21T12:00:00Z".to_string()),
            }],
            ..ClientStoreRows::default()
        });

        let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-2"))
            .expect("session snapshot");
        assert_eq!(
            snapshot
                .pending_turn
                .as_ref()
                .map(|turn| turn.request_id.as_str()),
            Some("req-2")
        );
    }

    #[test]
    fn session_snapshot_orders_pending_turn_before_orphan_tool_groups_and_live_overlay() {
        let store = ClientStore::from_rows(ClientStoreRows {
            conversations: vec![AgentConversationRow {
                session_id: "session-1".to_string(),
                agent_name: Some("Amy".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                title: Some("conversation".to_string()),
                title_source: Some("generated".to_string()),
                preview_text: Some("turn two".to_string()),
                status: Some("active".to_string()),
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                updated_at: Some("2026-04-21T12:02:00Z".to_string()),
                latest_request_id: Some("req-2".to_string()),
            }],
            requests: vec![
                AgentRequestRow {
                    request_id: "req-1".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    session_id: Some("session-1".to_string()),
                    retry_parent_request: None,
                    retry_root_request: None,
                    superseded_by_request: None,
                    content: Some("turn one".to_string()),
                    status: Some("completed".to_string()),
                    lifecycle_state: Some("completed".to_string()),
                    backend_id: None,
                    execution_origin: Some("interactive".to_string()),
                    failure_reason: None,
                    created_at: Some("2026-04-21T12:00:00Z".to_string()),
                    claimed_at: None,
                    deadline: None,
                    retry_count: Some(0),
                    max_retries: Some(3),
                    caused_by_trigger_id: None,
                    caused_by_trigger_kind: None,
                    interrupt_requested_at: None,
                    valid_until: None,
                },
                AgentRequestRow {
                    request_id: "req-2".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    session_id: Some("session-1".to_string()),
                    retry_parent_request: None,
                    retry_root_request: None,
                    superseded_by_request: None,
                    content: Some("turn two".to_string()),
                    status: Some("processing".to_string()),
                    lifecycle_state: Some("processing".to_string()),
                    backend_id: None,
                    execution_origin: Some("interactive".to_string()),
                    failure_reason: None,
                    created_at: Some("2026-04-21T12:01:00Z".to_string()),
                    claimed_at: None,
                    deadline: None,
                    retry_count: Some(0),
                    max_retries: Some(3),
                    caused_by_trigger_id: None,
                    caused_by_trigger_kind: None,
                    interrupt_requested_at: None,
                    valid_until: None,
                },
            ],
            messages: vec![AgentMessageRow {
                message_key: "msg-1".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(1),
                role: Some("user".to_string()),
                content: Some(user_message_json("turn one")),
                timestamp: Some("2026-04-21T12:00:00Z".to_string()),
            }],
            responses: vec![AgentResponseRow {
                response_key: "resp-2".to_string(),
                request_id: Some("req-2".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                content: Some("streaming reply".to_string()),
                reasoning: None,
                status: Some("streaming".to_string()),
                error_message: None,
                token_count: Some(12),
                progress_seq: Some(1),
                materialized_message_sequence: None,
                materialized_at: None,
                created_at: Some("2026-04-21T12:01:01Z".to_string()),
                completed_at: None,
                interrupted_at: None,
            }],
            tool_calls: vec![defra_agent_protocol::row::AgentToolCallRow {
                tool_call_key: "tool-1".to_string(),
                session_id: Some("session-1".to_string()),
                message_sequence: None,
                tool_name: Some("glob".to_string()),
                tool_call_id: Some("call-1".to_string()),
                args: Some("{\"pattern\":\"**/*.rs\"}".to_string()),
                result: None,
                status: Some("running".to_string()),
                started_at: Some("2026-04-21T12:01:02Z".to_string()),
                completed_at: None,
            }],
            ..ClientStoreRows::default()
        });

        let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-2"))
            .expect("session snapshot");
        let kinds = snapshot
            .timeline_items
            .iter()
            .map(|item| match item {
                RenderedTimelineItem::UserMessage { .. } => "user",
                RenderedTimelineItem::AssistantMessage { .. } => "assistant",
                RenderedTimelineItem::ToolGroup { .. } => "tools",
                RenderedTimelineItem::PendingUserTurn { .. } => "pending",
                RenderedTimelineItem::LiveAssistant { .. } => "live",
            })
            .collect::<Vec<_>>();
        assert_eq!(kinds, vec!["user", "pending", "tools", "live"]);
    }

    #[test]
    fn session_snapshot_keeps_full_live_overlay_when_only_prior_turn_shares_prefix() {
        let store = ClientStore::from_rows(ClientStoreRows {
            conversations: vec![AgentConversationRow {
                session_id: "session-1".to_string(),
                agent_name: Some("Amy".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                title: Some("conversation".to_string()),
                title_source: Some("generated".to_string()),
                preview_text: Some("turn two".to_string()),
                status: Some("active".to_string()),
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                updated_at: Some("2026-04-21T12:02:00Z".to_string()),
                latest_request_id: Some("req-2".to_string()),
            }],
            requests: vec![
                AgentRequestRow {
                    request_id: "req-1".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    session_id: Some("session-1".to_string()),
                    retry_parent_request: None,
                    retry_root_request: None,
                    superseded_by_request: None,
                    content: Some("turn one".to_string()),
                    status: Some("completed".to_string()),
                    lifecycle_state: Some("completed".to_string()),
                    backend_id: None,
                    execution_origin: Some("interactive".to_string()),
                    failure_reason: None,
                    created_at: Some("2026-04-21T12:00:00Z".to_string()),
                    claimed_at: None,
                    deadline: None,
                    retry_count: Some(0),
                    max_retries: Some(3),
                    caused_by_trigger_id: None,
                    caused_by_trigger_kind: None,
                    interrupt_requested_at: None,
                    valid_until: None,
                },
                AgentRequestRow {
                    request_id: "req-2".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    session_id: Some("session-1".to_string()),
                    retry_parent_request: None,
                    retry_root_request: None,
                    superseded_by_request: None,
                    content: Some("turn two".to_string()),
                    status: Some("processing".to_string()),
                    lifecycle_state: Some("processing".to_string()),
                    backend_id: None,
                    execution_origin: Some("interactive".to_string()),
                    failure_reason: None,
                    created_at: Some("2026-04-21T12:01:00Z".to_string()),
                    claimed_at: None,
                    deadline: None,
                    retry_count: Some(0),
                    max_retries: Some(3),
                    caused_by_trigger_id: None,
                    caused_by_trigger_kind: None,
                    interrupt_requested_at: None,
                    valid_until: None,
                },
            ],
            messages: vec![
                AgentMessageRow {
                    message_key: "msg-1".to_string(),
                    session_id: Some("session-1".to_string()),
                    sequence: Some(1),
                    role: Some("user".to_string()),
                    content: Some(user_message_json("turn one")),
                    timestamp: Some("2026-04-21T12:00:00Z".to_string()),
                },
                AgentMessageRow {
                    message_key: "msg-2".to_string(),
                    session_id: Some("session-1".to_string()),
                    sequence: Some(2),
                    role: Some("assistant".to_string()),
                    content: Some(
                        serde_json::to_string(&Message::assistant("I'll investigate"))
                            .expect("serialize assistant"),
                    ),
                    timestamp: Some("2026-04-21T12:00:01Z".to_string()),
                },
                AgentMessageRow {
                    message_key: "msg-3".to_string(),
                    session_id: Some("session-1".to_string()),
                    sequence: Some(3),
                    role: Some("user".to_string()),
                    content: Some(user_message_json("turn two")),
                    timestamp: Some("2026-04-21T12:01:00Z".to_string()),
                },
            ],
            responses: vec![AgentResponseRow {
                response_key: "resp-2".to_string(),
                request_id: Some("req-2".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                content: Some("I'll investigate further into p2p".to_string()),
                reasoning: None,
                status: Some("streaming".to_string()),
                error_message: None,
                token_count: Some(12),
                progress_seq: Some(1),
                materialized_message_sequence: None,
                materialized_at: None,
                created_at: Some("2026-04-21T12:01:01Z".to_string()),
                completed_at: None,
                interrupted_at: None,
            }],
            ..ClientStoreRows::default()
        });

        let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-2"))
            .expect("session snapshot");
        let live_content = snapshot.timeline_items.iter().find_map(|item| match item {
            RenderedTimelineItem::LiveAssistant { content, .. } => content.as_deref(),
            _ => None,
        });
        assert_eq!(live_content, Some("I'll investigate further into p2p"));
    }

    #[test]
    fn session_snapshot_renders_structured_tool_payloads_in_timeline() {
        let store = ClientStore::from_rows(ClientStoreRows {
            conversations: vec![AgentConversationRow {
                session_id: "session-1".to_string(),
                agent_name: Some("Amy".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                title: Some("conversation".to_string()),
                title_source: Some("generated".to_string()),
                preview_text: Some("turn one".to_string()),
                status: Some("active".to_string()),
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                updated_at: Some("2026-04-21T12:02:00Z".to_string()),
                latest_request_id: Some("req-1".to_string()),
            }],
            requests: vec![AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn one".to_string()),
                status: Some("processing".to_string()),
                lifecycle_state: Some("processing".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            }],
            messages: vec![AgentMessageRow {
                message_key: "msg-1".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(1),
                role: Some("user".to_string()),
                content: Some(user_message_json("turn one")),
                timestamp: Some("2026-04-21T12:00:00Z".to_string()),
            }],
            tool_calls: vec![defra_agent_protocol::row::AgentToolCallRow {
                tool_call_key: "tool-1".to_string(),
                session_id: Some("session-1".to_string()),
                message_sequence: Some(2),
                tool_name: Some("glob".to_string()),
                tool_call_id: Some("call-1".to_string()),
                args: Some("{\"pattern\":\"**/*.rs\",\"recursive\":true}".to_string()),
                result: Some("{\"matches\":12}".to_string()),
                status: Some("completed".to_string()),
                started_at: Some("2026-04-21T12:00:01Z".to_string()),
                completed_at: Some("2026-04-21T12:00:02Z".to_string()),
            }],
            ..ClientStoreRows::default()
        });

        let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-1"))
            .expect("session snapshot");
        let tools = snapshot
            .timeline_items
            .iter()
            .find_map(|item| match item {
                RenderedTimelineItem::ToolGroup { tools, .. } => Some(tools),
                _ => None,
            })
            .expect("tool group");

        assert_eq!(tools.len(), 1);
        let tool = &tools[0];
        assert_eq!(tool.tool_name, "glob");
        assert_eq!(tool.status_kind, "success");
        assert_eq!(
            tool.args.as_ref().map(|value| value
                .fields
                .iter()
                .map(|field| field.key.as_str())
                .collect::<Vec<_>>()),
            Some(vec!["pattern", "recursive"])
        );
        assert_eq!(
            tool.result
                .as_ref()
                .and_then(|value| value.fields.iter().find(|field| field.key == "matches"))
                .map(|field| field.value.as_str()),
            Some("12")
        );
    }

    #[test]
    fn session_snapshot_can_be_built_without_conversation_row_when_session_is_observed() {
        let store = ClientStore::from_rows(ClientStoreRows {
            sessions: vec![AgentSessionRow {
                session_id: "session-1".to_string(),
                agent_name: Some("Amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                started: Some("2026-04-21T12:00:00Z".to_string()),
                ended: None,
                status: Some("active".to_string()),
            }],
            requests: vec![AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("follow up question".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:01:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            }],
            responses: vec![AgentResponseRow {
                response_key: "resp-1".to_string(),
                request_id: Some("req-1".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                content: Some("done".to_string()),
                reasoning: None,
                status: Some("complete".to_string()),
                error_message: None,
                token_count: Some(12),
                progress_seq: Some(1),
                materialized_message_sequence: Some(2),
                materialized_at: Some("2026-04-21T12:01:05Z".to_string()),
                created_at: Some("2026-04-21T12:01:01Z".to_string()),
                completed_at: Some("2026-04-21T12:01:05Z".to_string()),
                interrupted_at: None,
            }],
            ..ClientStoreRows::default()
        });

        let snapshot =
            build_session_snapshot_from_store(&store, "session-1", None).expect("session snapshot");
        assert_eq!(snapshot.session_id, "session-1");
        assert_eq!(snapshot.agent_did.as_deref(), Some("did:defra:amy"));
        assert_eq!(snapshot.behavior_id.as_deref(), Some("amy-default"));
        assert_eq!(snapshot.status.as_deref(), Some("active"));
        assert_eq!(snapshot.turn_state.as_deref(), Some("completed"));
        assert_eq!(snapshot.latest_request_id.as_deref(), Some("req-1"));
    }

    #[test]
    fn session_snapshot_prefers_tracked_request_over_stale_conversation_latest_request() {
        let store = ClientStore::from_rows(ClientStoreRows {
            conversations: vec![AgentConversationRow {
                session_id: "session-1".to_string(),
                agent_name: Some("Amy".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                title: Some("conversation".to_string()),
                title_source: Some("generated".to_string()),
                preview_text: Some("turn two".to_string()),
                status: Some("active".to_string()),
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                updated_at: Some("2026-04-21T12:02:00Z".to_string()),
                latest_request_id: Some("req-1".to_string()),
            }],
            requests: vec![
                AgentRequestRow {
                    request_id: "req-1".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    session_id: Some("session-1".to_string()),
                    retry_parent_request: None,
                    retry_root_request: None,
                    superseded_by_request: None,
                    content: Some("turn one".to_string()),
                    status: Some("completed".to_string()),
                    lifecycle_state: Some("completed".to_string()),
                    backend_id: None,
                    execution_origin: Some("interactive".to_string()),
                    failure_reason: None,
                    created_at: Some("2026-04-21T12:00:00Z".to_string()),
                    claimed_at: None,
                    deadline: None,
                    retry_count: Some(0),
                    max_retries: Some(3),
                    caused_by_trigger_id: None,
                    caused_by_trigger_kind: None,
                    interrupt_requested_at: None,
                    valid_until: None,
                },
                AgentRequestRow {
                    request_id: "req-2".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    session_id: Some("session-1".to_string()),
                    retry_parent_request: None,
                    retry_root_request: None,
                    superseded_by_request: None,
                    content: Some("turn two".to_string()),
                    status: Some("processing".to_string()),
                    lifecycle_state: Some("processing".to_string()),
                    backend_id: None,
                    execution_origin: Some("interactive".to_string()),
                    failure_reason: None,
                    created_at: Some("2026-04-21T12:01:00Z".to_string()),
                    claimed_at: None,
                    deadline: None,
                    retry_count: Some(0),
                    max_retries: Some(3),
                    caused_by_trigger_id: None,
                    caused_by_trigger_kind: None,
                    interrupt_requested_at: None,
                    valid_until: None,
                },
            ],
            responses: vec![AgentResponseRow {
                response_key: "resp-2".to_string(),
                request_id: Some("req-2".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                content: Some("streaming reply".to_string()),
                reasoning: None,
                status: Some("streaming".to_string()),
                error_message: None,
                token_count: Some(12),
                progress_seq: Some(1),
                materialized_message_sequence: None,
                materialized_at: None,
                created_at: Some("2026-04-21T12:01:01Z".to_string()),
                completed_at: None,
                interrupted_at: None,
            }],
            messages: vec![AgentMessageRow {
                message_key: "msg-1".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(1),
                role: Some("user".to_string()),
                content: Some(user_message_json("turn one")),
                timestamp: Some("2026-04-21T12:00:00Z".to_string()),
            }],
            ..ClientStoreRows::default()
        });

        let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-2"))
            .expect("session snapshot");

        assert_eq!(snapshot.latest_request_id.as_deref(), Some("req-2"));
        assert_eq!(snapshot.turn_state.as_deref(), Some("streaming"));
        assert_eq!(
            snapshot
                .pending_turn
                .as_ref()
                .map(|turn| turn.request_id.as_str()),
            Some("req-2")
        );
        assert_eq!(
            snapshot
                .active_response_overlay
                .as_ref()
                .and_then(|response| response.content.as_deref()),
            Some("streaming reply")
        );
    }

    #[test]
    fn session_snapshot_stays_renderable_across_single_turn_observation_updates() {
        let submitted = ClientStore::from_rows(ClientStoreRows {
            conversations: vec![AgentConversationRow {
                session_id: "session-1".to_string(),
                agent_name: Some("Amy".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                title: Some("conversation".to_string()),
                title_source: Some("generated".to_string()),
                preview_text: Some("turn one".to_string()),
                status: Some("active".to_string()),
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                updated_at: Some("2026-04-21T12:00:01Z".to_string()),
                latest_request_id: None,
            }],
            requests: vec![AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn one".to_string()),
                status: Some("pending".to_string()),
                lifecycle_state: Some("pending".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            }],
            ..ClientStoreRows::default()
        });
        let submitted_snapshot =
            build_session_snapshot_from_store(&submitted, "session-1", Some("req-1"))
                .expect("submitted snapshot");
        assert_eq!(
            submitted_snapshot.latest_request_id.as_deref(),
            Some("req-1")
        );
        assert_eq!(
            submitted_snapshot.turn_state.as_deref(),
            Some("waitingForClaim")
        );
        assert_eq!(
            submitted_snapshot
                .pending_turn
                .as_ref()
                .map(|turn| turn.request_id.as_str()),
            Some("req-1")
        );

        let streaming = ClientStore::from_rows(ClientStoreRows {
            conversations: vec![AgentConversationRow {
                session_id: "session-1".to_string(),
                agent_name: Some("Amy".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                title: Some("conversation".to_string()),
                title_source: Some("generated".to_string()),
                preview_text: Some("turn one".to_string()),
                status: Some("active".to_string()),
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                updated_at: Some("2026-04-21T12:00:02Z".to_string()),
                latest_request_id: None,
            }],
            requests: vec![AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn one".to_string()),
                status: Some("processing".to_string()),
                lifecycle_state: Some("processing".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            }],
            responses: vec![AgentResponseRow {
                response_key: "resp-1".to_string(),
                request_id: Some("req-1".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                content: Some("streaming reply".to_string()),
                reasoning: None,
                status: Some("streaming".to_string()),
                error_message: None,
                token_count: Some(12),
                progress_seq: Some(1),
                materialized_message_sequence: None,
                materialized_at: None,
                created_at: Some("2026-04-21T12:00:01Z".to_string()),
                completed_at: None,
                interrupted_at: None,
            }],
            ..ClientStoreRows::default()
        });
        let streaming_snapshot =
            build_session_snapshot_from_store(&streaming, "session-1", Some("req-1"))
                .expect("streaming snapshot");
        assert_eq!(streaming_snapshot.turn_state.as_deref(), Some("streaming"));
        assert_eq!(
            streaming_snapshot
                .active_response_overlay
                .as_ref()
                .and_then(|response| response.content.as_deref()),
            Some("streaming reply")
        );

        let completed = ClientStore::from_rows(ClientStoreRows {
            conversations: vec![AgentConversationRow {
                session_id: "session-1".to_string(),
                agent_name: Some("Amy".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                title: Some("conversation".to_string()),
                title_source: Some("generated".to_string()),
                preview_text: Some("final answer".to_string()),
                status: Some("completed".to_string()),
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                updated_at: Some("2026-04-21T12:00:05Z".to_string()),
                latest_request_id: Some("req-1".to_string()),
            }],
            requests: vec![AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn one".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            }],
            responses: vec![AgentResponseRow {
                response_key: "resp-1".to_string(),
                request_id: Some("req-1".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                content: Some("final answer".to_string()),
                reasoning: None,
                status: Some("complete".to_string()),
                error_message: None,
                token_count: Some(34),
                progress_seq: Some(2),
                materialized_message_sequence: Some(2),
                materialized_at: Some("2026-04-21T12:00:05Z".to_string()),
                created_at: Some("2026-04-21T12:00:01Z".to_string()),
                completed_at: Some("2026-04-21T12:00:05Z".to_string()),
                interrupted_at: None,
            }],
            messages: vec![
                AgentMessageRow {
                    message_key: "msg-1".to_string(),
                    session_id: Some("session-1".to_string()),
                    sequence: Some(1),
                    role: Some("user".to_string()),
                    content: Some(user_message_json("turn one")),
                    timestamp: Some("2026-04-21T12:00:00Z".to_string()),
                },
                AgentMessageRow {
                    message_key: "msg-2".to_string(),
                    session_id: Some("session-1".to_string()),
                    sequence: Some(2),
                    role: Some("assistant".to_string()),
                    content: Some(
                        "{\"role\":\"assistant\",\"content\":[{\"text\":\"final answer\"}]}"
                            .to_string(),
                    ),
                    timestamp: Some("2026-04-21T12:00:05Z".to_string()),
                },
            ],
            ..ClientStoreRows::default()
        });
        let completed_snapshot =
            build_session_snapshot_from_store(&completed, "session-1", Some("req-1"))
                .expect("completed snapshot");
        assert_eq!(completed_snapshot.turn_state.as_deref(), Some("completed"));
        assert!(completed_snapshot.active_response_overlay.is_none());
        assert!(completed_snapshot.pending_turn.is_none());
    }

    #[test]
    fn session_snapshot_hides_live_overlay_once_turn_is_terminal_even_if_response_is_stale() {
        let store = ClientStore::from_rows(ClientStoreRows {
            conversations: vec![AgentConversationRow {
                session_id: "session-1".to_string(),
                agent_name: Some("Amy".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                title: Some("conversation".to_string()),
                title_source: Some("generated".to_string()),
                preview_text: Some("turn one".to_string()),
                status: Some("completed".to_string()),
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                updated_at: Some("2026-04-21T12:02:00Z".to_string()),
                latest_request_id: Some("req-1".to_string()),
            }],
            requests: vec![AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn one".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            }],
            messages: vec![
                AgentMessageRow {
                    message_key: "msg-1".to_string(),
                    session_id: Some("session-1".to_string()),
                    sequence: Some(1),
                    role: Some("user".to_string()),
                    content: Some(user_message_json("turn one")),
                    timestamp: Some("2026-04-21T12:00:00Z".to_string()),
                },
                AgentMessageRow {
                    message_key: "msg-2".to_string(),
                    session_id: Some("session-1".to_string()),
                    sequence: Some(2),
                    role: Some("assistant".to_string()),
                    content: Some(
                        serde_json::to_string(&Message::assistant("final answer"))
                            .expect("serialize assistant"),
                    ),
                    timestamp: Some("2026-04-21T12:00:01Z".to_string()),
                },
            ],
            responses: vec![AgentResponseRow {
                response_key: "resp-1".to_string(),
                request_id: Some("req-1".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                content: Some("final answer".to_string()),
                reasoning: None,
                status: Some("streaming".to_string()),
                error_message: None,
                token_count: Some(12),
                progress_seq: Some(1),
                materialized_message_sequence: None,
                materialized_at: None,
                created_at: Some("2026-04-21T12:00:01Z".to_string()),
                completed_at: None,
                interrupted_at: None,
            }],
            ..ClientStoreRows::default()
        });

        let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-1"))
            .expect("session snapshot");
        assert_eq!(snapshot.turn_state.as_deref(), Some("completed"));
        assert!(snapshot.active_response_overlay.is_none());
        assert!(!snapshot
            .timeline_items
            .iter()
            .any(|item| matches!(item, RenderedTimelineItem::LiveAssistant { .. })));
    }

    #[test]
    fn session_snapshot_stays_renderable_across_three_turns_with_stale_conversation_rows() {
        let store = ClientStore::from_rows(ClientStoreRows {
            conversations: vec![AgentConversationRow {
                session_id: "session-1".to_string(),
                agent_name: Some("Amy".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                title: Some("conversation".to_string()),
                title_source: Some("generated".to_string()),
                preview_text: Some("turn three".to_string()),
                status: Some("active".to_string()),
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                updated_at: Some("2026-04-21T12:03:00Z".to_string()),
                latest_request_id: Some("req-2".to_string()),
            }],
            requests: vec![
                AgentRequestRow {
                    request_id: "req-1".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    session_id: Some("session-1".to_string()),
                    retry_parent_request: None,
                    retry_root_request: None,
                    superseded_by_request: None,
                    content: Some("turn one".to_string()),
                    status: Some("completed".to_string()),
                    lifecycle_state: Some("completed".to_string()),
                    backend_id: None,
                    execution_origin: Some("interactive".to_string()),
                    failure_reason: None,
                    created_at: Some("2026-04-21T12:00:00Z".to_string()),
                    claimed_at: None,
                    deadline: None,
                    retry_count: Some(0),
                    max_retries: Some(3),
                    caused_by_trigger_id: None,
                    caused_by_trigger_kind: None,
                    interrupt_requested_at: None,
                    valid_until: None,
                },
                AgentRequestRow {
                    request_id: "req-2".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    session_id: Some("session-1".to_string()),
                    retry_parent_request: None,
                    retry_root_request: None,
                    superseded_by_request: None,
                    content: Some("turn two".to_string()),
                    status: Some("completed".to_string()),
                    lifecycle_state: Some("completed".to_string()),
                    backend_id: None,
                    execution_origin: Some("interactive".to_string()),
                    failure_reason: None,
                    created_at: Some("2026-04-21T12:01:00Z".to_string()),
                    claimed_at: None,
                    deadline: None,
                    retry_count: Some(0),
                    max_retries: Some(3),
                    caused_by_trigger_id: None,
                    caused_by_trigger_kind: None,
                    interrupt_requested_at: None,
                    valid_until: None,
                },
                AgentRequestRow {
                    request_id: "req-3".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    session_id: Some("session-1".to_string()),
                    retry_parent_request: None,
                    retry_root_request: None,
                    superseded_by_request: None,
                    content: Some("turn three".to_string()),
                    status: Some("processing".to_string()),
                    lifecycle_state: Some("processing".to_string()),
                    backend_id: None,
                    execution_origin: Some("interactive".to_string()),
                    failure_reason: None,
                    created_at: Some("2026-04-21T12:02:00Z".to_string()),
                    claimed_at: None,
                    deadline: None,
                    retry_count: Some(0),
                    max_retries: Some(3),
                    caused_by_trigger_id: None,
                    caused_by_trigger_kind: None,
                    interrupt_requested_at: None,
                    valid_until: None,
                },
            ],
            responses: vec![
                AgentResponseRow {
                    response_key: "resp-1".to_string(),
                    request_id: Some("req-1".to_string()),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    session_id: Some("session-1".to_string()),
                    content: Some("answer one".to_string()),
                    reasoning: None,
                    status: Some("complete".to_string()),
                    error_message: None,
                    token_count: Some(10),
                    progress_seq: Some(1),
                    materialized_message_sequence: Some(2),
                    materialized_at: Some("2026-04-21T12:00:05Z".to_string()),
                    created_at: Some("2026-04-21T12:00:01Z".to_string()),
                    completed_at: Some("2026-04-21T12:00:05Z".to_string()),
                interrupted_at: None,
                },
                AgentResponseRow {
                    response_key: "resp-2".to_string(),
                    request_id: Some("req-2".to_string()),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    session_id: Some("session-1".to_string()),
                    content: Some("answer two".to_string()),
                    reasoning: None,
                    status: Some("complete".to_string()),
                    error_message: None,
                    token_count: Some(10),
                    progress_seq: Some(1),
                    materialized_message_sequence: Some(4),
                    materialized_at: Some("2026-04-21T12:01:05Z".to_string()),
                    created_at: Some("2026-04-21T12:01:01Z".to_string()),
                    completed_at: Some("2026-04-21T12:01:05Z".to_string()),
                interrupted_at: None,
                },
                AgentResponseRow {
                    response_key: "resp-3".to_string(),
                    request_id: Some("req-3".to_string()),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    session_id: Some("session-1".to_string()),
                    content: Some("answer three in progress".to_string()),
                    reasoning: None,
                    status: Some("streaming".to_string()),
                    error_message: None,
                    token_count: Some(10),
                    progress_seq: Some(1),
                    materialized_message_sequence: None,
                    materialized_at: None,
                    created_at: Some("2026-04-21T12:02:01Z".to_string()),
                    completed_at: None,
                interrupted_at: None,
                },
            ],
            messages: vec![
                AgentMessageRow {
                    message_key: "msg-1".to_string(),
                    session_id: Some("session-1".to_string()),
                    sequence: Some(1),
                    role: Some("user".to_string()),
                    content: Some(user_message_json("turn one")),
                    timestamp: Some("2026-04-21T12:00:00Z".to_string()),
                },
                AgentMessageRow {
                    message_key: "msg-2".to_string(),
                    session_id: Some("session-1".to_string()),
                    sequence: Some(2),
                    role: Some("assistant".to_string()),
                    content: Some("{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"answer one\"}]}".to_string()),
                    timestamp: Some("2026-04-21T12:00:05Z".to_string()),
                },
                AgentMessageRow {
                    message_key: "msg-3".to_string(),
                    session_id: Some("session-1".to_string()),
                    sequence: Some(3),
                    role: Some("user".to_string()),
                    content: Some(user_message_json("turn two")),
                    timestamp: Some("2026-04-21T12:01:00Z".to_string()),
                },
                AgentMessageRow {
                    message_key: "msg-4".to_string(),
                    session_id: Some("session-1".to_string()),
                    sequence: Some(4),
                    role: Some("assistant".to_string()),
                    content: Some("{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"answer two\"}]}".to_string()),
                    timestamp: Some("2026-04-21T12:01:05Z".to_string()),
                },
            ],
            ..ClientStoreRows::default()
        });

        let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-3"))
            .expect("session snapshot");

        assert_eq!(snapshot.latest_request_id.as_deref(), Some("req-3"));
        assert_eq!(snapshot.turn_state.as_deref(), Some("streaming"));
        assert_eq!(snapshot.messages.len(), 4);
        assert_eq!(
            snapshot
                .pending_turn
                .as_ref()
                .map(|turn| turn.request_id.as_str()),
            Some("req-3")
        );
        assert_eq!(
            snapshot
                .active_response_overlay
                .as_ref()
                .and_then(|response| response.content.as_deref()),
            Some("answer three in progress")
        );
    }
}

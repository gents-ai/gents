use std::collections::HashMap;
use std::sync::Arc;

use defra_agent_desktop::client::{ClientCore, ClientPeerStatus, DesktopPaths, PeerDirectory, P2PHealth};
use defra_agent_protocol::transcript::present_persisted_message;
use defra_agent_desktop::local_runtime::default_agent_home;

use super::types::{
    normalize_optional, turn_state_label, BehaviorView, ConversationSummary, DeploymentView,
    DesktopBootstrapSummary, DesktopClientSnapshot, DesktopRuntimeSnapshot, DesktopSessionSnapshot,
    MessageView, P2PHealthView, ResponseView, RuntimeView, SavedPeerView, ToolCallView,
    ToolResultView,
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
            let default_behavior_id =
                store.default_behavior_id_for_agent(&peer.agent_did).map(str::to_owned);
            let runtime = store.latest_runtime(&peer.agent_did).map(|row| RuntimeView {
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
                        title: Some(
                            normalize_optional(row.title.as_deref())
                                .or_else(|| normalize_optional(row.preview_text.as_deref()))
                                .unwrap_or_else(|| "New Conversation".to_string()),
                        ),
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
) -> Option<DesktopSessionSnapshot> {
    let conversation = store
        .conversations
        .iter()
        .find(|row| row.session_id == session_id)?;
    let transcript = store.transcript(session_id);
    let latest_request_id = store.latest_request_id_for_session(session_id);
    let latest_response = latest_request_id
        .as_deref()
        .and_then(|request_id| store.latest_response_for_request(request_id))
        .map(|row| ResponseView {
            status: normalize_optional(row.status.as_deref()),
            content: normalize_optional(row.content.as_deref()),
            reasoning: normalize_optional(row.reasoning.as_deref()),
            error_message: normalize_optional(row.error_message.as_deref()),
            token_count: row.token_count,
            materialized_message_sequence: row.materialized_message_sequence,
            materialized_at: normalize_optional(row.materialized_at.as_deref()),
            completed_at: normalize_optional(row.completed_at.as_deref()),
        });
    let active_response_overlay = latest_response.clone().filter(|response| {
        response.materialized_message_sequence.is_none()
            && (response.content.as_deref().is_some_and(|value| !value.trim().is_empty())
                || response
                    .reasoning
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty()))
    });

    Some(DesktopSessionSnapshot {
        session_id: session_id.to_string(),
        agent_did: normalize_optional(conversation.agent_did.as_deref()),
        behavior_id: normalize_optional(conversation.behavior_id.as_deref()),
        title: normalize_optional(conversation.title.as_deref()),
        preview_text: normalize_optional(conversation.preview_text.as_deref()),
        status: normalize_optional(conversation.status.as_deref()),
        turn_state: store
            .derive_turn(session_id)
            .map(turn_state_label)
            .map(str::to_owned),
        latest_request_id,
        latest_response,
        active_response_overlay,
        messages: transcript
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
            .collect(),
        tool_calls: transcript
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
            .collect(),
        tool_results: transcript
            .tool_results
            .into_iter()
            .map(|row| ToolResultView {
                tool_name: normalize_optional(row.tool_name.as_deref()),
                tool_input: normalize_optional(row.tool_input.as_deref()),
                output_text: normalize_optional(row.output_text.as_deref()),
                truncated: row.truncated,
                created_at: normalize_optional(row.created_at.as_deref()),
            })
            .collect(),
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

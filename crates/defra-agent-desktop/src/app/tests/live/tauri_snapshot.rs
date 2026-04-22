use super::*;
use defra_agent_protocol::client_protocol::ClientTurnState;
use defra_agent_protocol::transcript::{normalize_markdown_text, present_persisted_message};

#[allow(dead_code)]
mod tauri_bridge {
    pub(crate) mod types {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../apps/desktop-tauri/src-tauri/src/bridge/types.rs"
        ));
    }
}
use tauri_bridge::types::{
    normalize_optional, turn_state_label, ChatSendRequest, ChatSendResult, DesktopSessionSnapshot,
    MessageView, PendingTurnView, RenderedTimelineItem, RenderedToolCallView, ResponseView,
    ToolCallView, ToolDetailFieldView, ToolDetailValueView, ToolResultView,
};

fn can_send_in_turn(state: ClientTurnState) -> bool {
    matches!(
        state,
        ClientTurnState::Completed
            | ClientTurnState::Failed
            | ClientTurnState::Superseded
            | ClientTurnState::Interrupted
    )
}

fn normalize_timeline_text(value: Option<&str>) -> String {
    value.map(str::trim).unwrap_or_default().to_string()
}

fn render_json_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        _ => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    }
}

fn parse_tool_detail_value(value: Option<&str>) -> Option<ToolDetailValueView> {
    let raw_text = normalize_optional(value)?;
    let parsed = serde_json::from_str::<serde_json::Value>(&raw_text).ok();
    let fields = match parsed {
        Some(serde_json::Value::Object(map)) => map
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
    store: &crate::client::ClientStore,
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
    store: &crate::client::ClientStore,
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

fn build_tauri_session_snapshot_from_store(
    store: &crate::client::ClientStore,
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
        response.materialized_message_sequence.is_none()
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
        turn_state: latest_request_id
            .as_deref()
            .and_then(|request_id| store.derive_turn_for_request(request_id))
            .or_else(|| store.derive_turn(session_id))
            .map(turn_state_label)
            .map(str::to_owned),
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

fn build_pending_turn(
    store: &crate::client::ClientStore,
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

fn run_live_tauri_session_snapshot(
    fixture_name: &str,
    backend: &AgentBackendConfig,
    prompts: &[&str],
) -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    init_test_tracing();

    let fixture = build_named_multi_agent_desktop_fixture_with_backend(
        fixture_name,
        &["alpha"],
        backend,
        global_log_store(),
    )?;

    let deployment = fixture
        .deployments
        .first()
        .ok_or_else(|| anyhow!("expected one live deployment"))?;
    let desktop_client = Arc::clone(
        fixture
            .driver
            .app
            .client
            .as_ref()
            .ok_or_else(|| anyhow!("desktop client missing"))?,
    );
    configure_live_repo_investigation_behavior(
        fixture.runtime.as_ref(),
        deployment.core.as_ref(),
        desktop_client.as_ref(),
        &deployment.label,
        &deployment.agent_did,
        &deployment.docs,
        &backend.model_name,
    )?;
    let mut session_id: Option<String> = None;
    let mut prior_tool_call_count = 0usize;

    for (index, prompt) in prompts.iter().enumerate() {
        let submitted = submit_live_tauri_chat_send(
            fixture.runtime.as_ref(),
            desktop_client.as_ref(),
            ChatSendRequest {
                agent_did: deployment.agent_did.clone(),
                behavior_id: Some(deployment.docs.behavior_id.clone()),
                session_id: session_id.clone(),
                content: (*prompt).to_string(),
            },
        )?;

        let current_session_id = session_id
            .clone()
            .unwrap_or_else(|| submitted.session_id.clone());
        if let Some(existing) = &session_id {
            assert_eq!(
                existing, &submitted.session_id,
                "expected all Tauri live turns to stay in the same session"
            );
        } else {
            session_id = Some(submitted.session_id.clone());
        }

        wait_for_value(
            &format!("request {} observed in desktop store", index + 1),
            Duration::from_secs(30),
            || {
                fixture
                    .runtime
                    .block_on(desktop_client.refresh_store())
                    .ok()?;
                let snapshot = desktop_client.store().snapshot();
                snapshot
                    .requests
                    .iter()
                    .find(|row| row.request_id == submitted.request_id)
                    .filter(|row| row.session_id.as_deref() == Some(current_session_id.as_str()))
                    .map(|_| ())
            },
        )?;

        let early_snapshot = wait_for_value(
            &format!("early tauri snapshot for turn {}", index + 1),
            Duration::from_secs(30),
            || {
                fixture
                    .runtime
                    .block_on(desktop_client.refresh_store())
                    .ok()?;
                let store = desktop_client.store().snapshot();
                let snapshot = build_tauri_session_snapshot_from_store(
                    store.as_ref(),
                    &current_session_id,
                    Some(submitted.request_id.as_str()),
                )?;
                (snapshot.latest_request_id.as_deref() == Some(submitted.request_id.as_str()))
                    .then_some(snapshot)
            },
        )?;
        assert_eq!(
            early_snapshot.latest_request_id.as_deref(),
            Some(submitted.request_id.as_str())
        );
        assert!(
            matches!(
                early_snapshot.turn_state.as_deref(),
                Some("waitingForClaim" | "streaming" | "completed")
            ),
            "unexpected early turn_state for turn {}: {:?}",
            index + 1,
            early_snapshot.turn_state
        );

        let completed_turn = wait_for_graphql_turn_completion(
            fixture.desktop_api.graphql_url(),
            &format!("turn {}", index + 1),
            &submitted.request_id,
        )?;

        let completed_snapshot = wait_for_value(
            &format!("completed tauri snapshot for turn {}", index + 1),
            Duration::from_secs(180),
            || {
                fixture
                    .runtime
                    .block_on(desktop_client.refresh_store())
                    .ok()?;
                let store = desktop_client.store().snapshot();
                let snapshot = build_tauri_session_snapshot_from_store(
                    store.as_ref(),
                    &current_session_id,
                    Some(completed_turn.effective_request_id.as_str()),
                )?;
                (snapshot.latest_request_id.as_deref()
                    == Some(completed_turn.effective_request_id.as_str())
                    && matches!(
                        snapshot.turn_state.as_deref(),
                        Some("completed" | "failed" | "superseded")
                    )
                    && snapshot.active_response_overlay.is_none()
                    && snapshot.pending_turn.is_none()
                    && snapshot.messages.iter().any(|message| {
                        message
                            .display_content
                            .as_deref()
                            .is_some_and(|content| content.contains(prompt))
                    }))
                .then_some(snapshot)
            },
        )?;

        assert_eq!(
            completed_snapshot.latest_request_id.as_deref(),
            Some(completed_turn.effective_request_id.as_str())
        );
        assert!(
            completed_snapshot.tool_calls.len() > prior_tool_call_count,
            "expected turn {} to add tool calls, but tool count stayed at {}",
            index + 1,
            prior_tool_call_count
        );
        prior_tool_call_count = completed_snapshot.tool_calls.len();
    }

    fixture.shutdown()
}

const TAURI_LIVE_REPO_INVESTIGATION_SYSTEM_PROMPT: &str =
    "You are a desktop repo investigation agent working inside a seeded workspace. This is a mechanics test. For every user turn, you must inspect the repository with tools before answering. Do not answer from memory. Start each turn by using rg or list_files to locate relevant code, then use read_file with one path at a time, and make multiple tool calls before you respond. If you have not used tools in the current turn, do not answer yet. Cite the file paths you inspected.";

fn configure_live_repo_investigation_behavior(
    runtime: &Runtime,
    remote_core: &ClientCore,
    desktop_client: &ClientCore,
    deployment_label: &str,
    agent_did: &str,
    docs: &LiveAgentDocs,
    model_name: &str,
) -> Result<()> {
    runtime.block_on(async {
        remote_core
            .save_behavior(&AgentBehaviorRow {
                behavior_id: docs.behavior_id.clone(),
                agent_did: Some(agent_did.to_string()),
                display_name: Some("Live Tauri Repo Investigation".to_string()),
                system_prompt: Some(TAURI_LIVE_REPO_INVESTIGATION_SYSTEM_PROMPT.to_string()),
                backend_id: Some(docs.backend_id.clone()),
                model_name: Some(model_name.to_string()),
                tool_selection_id: Some(docs.tool_selection_id.clone()),
                inference_profile_id: Some(docs.inference_profile_id.clone()),
                compaction_strategy: Some("StripThenSummarize".to_string()),
                compaction_threshold: Some(0.95),
                enabled: Some(true),
                created_at: Some(chrono::Utc::now().to_rfc3339()),
            })
            .await?;
        remote_core.refresh_store().await?;
        Ok::<(), anyhow::Error>(())
    })?;

    wait_for_value(
        &format!("repo investigation behavior replicated for {deployment_label}"),
        Duration::from_secs(60),
        || {
            runtime.block_on(desktop_client.refresh_store()).ok()?;
            let snapshot = desktop_client.store().snapshot();
            snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == docs.behavior_id)
                .filter(|row| {
                    row.agent_did.as_deref() == Some(agent_did)
                        && row.system_prompt.as_deref()
                            == Some(TAURI_LIVE_REPO_INVESTIGATION_SYSTEM_PROMPT)
                        && row.tool_selection_id.as_deref() == Some(docs.tool_selection_id.as_str())
                        && row.inference_profile_id.as_deref()
                            == Some(docs.inference_profile_id.as_str())
                        && row.model_name.as_deref() == Some(model_name)
                        && row.enabled == Some(true)
                })
                .map(|_| ())
        },
    )?;

    wait_for_stable_runtime_ready(
        runtime,
        remote_core,
        &format!("{deployment_label} repo investigation runtime"),
        agent_did,
        Duration::from_secs(2),
        Duration::from_secs(60),
    )?;
    wait_for_stable_runtime_ready(
        runtime,
        desktop_client,
        &format!("desktop mirror for {deployment_label} repo investigation runtime"),
        agent_did,
        Duration::from_secs(2),
        Duration::from_secs(60),
    )?;

    Ok(())
}

#[derive(Debug, Clone)]
struct TauriThreeNodeConversationCase {
    label: String,
    peer_id: String,
    agent_did: String,
    behavior_id: String,
    turn_one_prompt: String,
    turn_two_prompt: String,
    turn_one_tokens: Vec<String>,
    turn_two_token: String,
    session_id: Option<String>,
    latest_request_id: Option<String>,
    prior_tool_call_count: usize,
}

impl TauriThreeNodeConversationCase {
    fn expected_followup_tokens(&self) -> Vec<String> {
        let mut expected = self.turn_one_tokens.clone();
        expected.push(self.turn_two_token.clone());
        expected
    }
}

#[derive(Debug, Clone)]
struct SubmittedTauriTurn {
    case_index: usize,
    request_id: String,
    session_id: String,
}

fn refreshed_tauri_session_snapshot(
    runtime: &Runtime,
    desktop_client: &ClientCore,
    session_id: &str,
    request_id: &str,
) -> Option<DesktopSessionSnapshot> {
    runtime.block_on(desktop_client.refresh_store()).ok()?;
    let store = desktop_client.store().snapshot();
    build_tauri_session_snapshot_from_store(store.as_ref(), session_id, Some(request_id))
}

fn snapshot_contains_message_text(snapshot: &DesktopSessionSnapshot, needle: &str) -> bool {
    snapshot.messages.iter().any(|message| {
        message
            .display_content
            .as_deref()
            .or(message.content.as_deref())
            .is_some_and(|content| content.contains(needle))
    }) || snapshot
        .pending_turn
        .as_ref()
        .is_some_and(|pending| pending.content.contains(needle))
        || snapshot
            .active_response_overlay
            .as_ref()
            .and_then(|response| response.content.as_deref())
            .is_some_and(|content| content.contains(needle))
        || snapshot
            .latest_response
            .as_ref()
            .and_then(|response| response.content.as_deref())
            .is_some_and(|content| content.contains(needle))
}

fn assert_snapshot_prompt_isolation(
    label: &str,
    snapshot: &DesktopSessionSnapshot,
    expected_prompts: &[&str],
    forbidden_prompts: &[&str],
) -> Result<()> {
    for prompt in expected_prompts {
        if !snapshot_contains_message_text(snapshot, prompt) {
            anyhow::bail!(
                "{label} snapshot missing expected prompt {:?} in session {}",
                prompt,
                snapshot.session_id,
            );
        }
    }

    for prompt in forbidden_prompts {
        if snapshot_contains_message_text(snapshot, prompt) {
            anyhow::bail!(
                "{label} snapshot leaked prompt {:?} into session {}",
                prompt,
                snapshot.session_id,
            );
        }
    }

    Ok(())
}

fn wait_for_request_observed_in_desktop(
    runtime: &Runtime,
    desktop_client: &ClientCore,
    label: &str,
    request_id: &str,
    session_id: &str,
    agent_did: &str,
    behavior_id: &str,
) -> Result<()> {
    wait_for_value(label, Duration::from_secs(30), || {
        runtime.block_on(desktop_client.refresh_store()).ok()?;
        let snapshot = desktop_client.store().snapshot();
        snapshot
            .requests
            .iter()
            .find(|row| row.request_id == request_id)
            .filter(|row| {
                row.session_id.as_deref() == Some(session_id)
                    && row.agent_did.as_deref() == Some(agent_did)
                    && row.behavior_id.as_deref() == Some(behavior_id)
            })
            .map(|_| ())
    })
}

#[derive(Debug, Clone)]
struct CompletedTauriTurn {
    effective_request_id: String,
}

fn submit_live_tauri_chat_send(
    runtime: &Runtime,
    core: &ClientCore,
    request: ChatSendRequest,
) -> Result<ChatSendResult> {
    let agent_did = request.agent_did.trim().to_string();
    anyhow::ensure!(!agent_did.is_empty(), "agent_did is required");

    let content = request.content.trim().to_string();
    anyhow::ensure!(!content.is_empty(), "content is required");

    let behavior_id = request
        .behavior_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    runtime.block_on(async {
        let session_id = match request
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(session_id) => session_id.to_string(),
            None => {
                core.create_conversation(&agent_did, behavior_id.as_deref())
                    .await?
                    .session_id
            }
        };

        let store = core.store().snapshot();
        if let Some(turn_state) = store.derive_turn(&session_id) {
            anyhow::ensure!(
                can_send_in_turn(turn_state),
                "cannot send while current turn is {}",
                turn_state_label(turn_state),
            );
        }

        let submitted = core
            .submit_request(&session_id, &agent_did, &content, behavior_id.as_deref())
            .await?;

        Ok(ChatSendResult {
            session_id,
            request_id: submitted.request_id,
            agent_did: submitted.agent_did,
            behavior_id: submitted.behavior_id,
        })
    })
}

fn wait_for_graphql_turn_completion(
    graphql_url: &str,
    label: &str,
    request_id: &str,
) -> Result<CompletedTauriTurn> {
    let deadline = Instant::now() + Duration::from_secs(240);
    let mut current_request_id = request_id.to_string();
    let mut visited = std::collections::BTreeSet::from([current_request_id.clone()]);

    loop {
        let state = fetch_graphql_turn_state(graphql_url, &current_request_id)
            .with_context(|| format!("fetching desktop GraphQL turn state for {label}"))?;

        match state.derived_turn_state() {
            Some(ClientTurnState::Completed) => {
                if !state.response_is_durably_complete() {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }

                state.response.as_ref().ok_or_else(|| {
                    anyhow!(
                        "{label} request {current_request_id} derived Completed without AgentResponse"
                    )
                })?;
                return Ok(CompletedTauriTurn {
                    effective_request_id: current_request_id,
                });
            }
            Some(ClientTurnState::Superseded) => {
                if let Some(next_request_id) = state.successor_request_id() {
                    if visited.insert(next_request_id.clone()) {
                        current_request_id = next_request_id;
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                }

                anyhow::bail!(
                    "{label} request {current_request_id} was superseded without a successor: request={:?} response={:?}",
                    state.request,
                    state.response
                );
            }
            Some(ClientTurnState::Failed) | Some(ClientTurnState::Interrupted) => {
                anyhow::bail!(
                    "{label} request {current_request_id} failed before completion: request={:?} response={:?}",
                    state.request,
                    state.response
                );
            }
            Some(ClientTurnState::WaitingForClaim | ClientTurnState::Streaming) | None => {}
        }

        if Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for desktop GraphQL completion for {label}: request_id={} current_request_id={} request={:?} response={:?}",
                request_id,
                current_request_id,
                state.request,
                state.response
            );
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_completed_tauri_snapshot(
    runtime: &Runtime,
    desktop_client: &ClientCore,
    label: &str,
    session_id: &str,
    request_id: &str,
    prompt: &str,
) -> Result<DesktopSessionSnapshot> {
    wait_for_value(label, Duration::from_secs(60), || {
        let snapshot =
            refreshed_tauri_session_snapshot(runtime, desktop_client, session_id, request_id)?;
        (snapshot.latest_request_id.as_deref() == Some(request_id)
            && matches!(
                snapshot.turn_state.as_deref(),
                Some("completed" | "failed" | "superseded")
            )
            && snapshot.active_response_overlay.is_none()
            && snapshot.pending_turn.is_none()
            && snapshot_contains_message_text(&snapshot, prompt))
        .then_some(snapshot)
    })
}

fn wait_for_request_terminal_on_core(
    runtime: &Runtime,
    core: &ClientCore,
    label: &str,
    session_id: &str,
    request_id: &str,
    agent_did: &str,
) -> Result<()> {
    wait_for_value(label, Duration::from_secs(60), || {
        runtime.block_on(core.refresh_store()).ok()?;
        let snapshot = core.store().snapshot();
        let request = snapshot.requests.iter().find(|row| {
            row.request_id == request_id
                && row.session_id.as_deref() == Some(session_id)
                && row.agent_did.as_deref() == Some(agent_did)
        })?;
        let response = snapshot.latest_response_for_request(request_id);
        let turn_state = snapshot.derive_turn_for_request(request_id);

        (matches!(
            request.lifecycle_state.as_deref(),
            Some("completed" | "failed" | "dead" | "superseded")
        ) || (matches!(
            turn_state,
            Some(
                ClientTurnState::Completed | ClientTurnState::Failed | ClientTurnState::Superseded
            )
        ) && matches!(
            response.and_then(|row| row.status.as_deref()),
            Some("complete" | "completed")
        )))
        .then_some(())
    })
}

fn run_three_node_live_tauri_session_snapshot(
    fixture_name: &str,
    backend: &AgentBackendConfig,
) -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    init_test_tracing();

    let mut fixture = build_named_multi_agent_desktop_fixture_with_backend(
        fixture_name,
        &["alpha", "bravo", "charlie"],
        backend,
        global_log_store(),
    )?;
    assert_eq!(fixture.deployments.len(), 3);

    {
        let driver = &mut fixture.driver;
        driver.open_activity(Activity::Chat);
        for deployment in &fixture.deployments {
            driver.wait_for_target(
                &format!("chat deployment row for {}", deployment.label),
                Duration::from_secs(10),
                &audit::targets::chat_deployment(&deployment.peer_id),
            )?;
        }
    }

    let desktop_client = Arc::clone(
        fixture
            .driver
            .app
            .client
            .as_ref()
            .ok_or_else(|| anyhow!("desktop client missing"))?,
    );
    wait_for_value(
        "desktop store mirrors three live deployments",
        Duration::from_secs(30),
        || {
            fixture
                .runtime
                .block_on(desktop_client.refresh_store())
                .ok()?;
            let snapshot = desktop_client.store().snapshot();
            (fixture.deployments.iter().all(|deployment| {
                snapshot
                    .agent_principals
                    .iter()
                    .any(|row| row.agent_did == deployment.agent_did)
                    && snapshot
                        .latest_runtime(&deployment.agent_did)
                        .is_some_and(|row| row.runnable_behavior_count == Some(1))
            }) && desktop_client.peer_statuses().len() >= fixture.deployments.len())
            .then_some(())
        },
    )?;

    for deployment in &fixture.deployments {
        wait_for_stable_runtime_ready(
            fixture.runtime.as_ref(),
            deployment.core.as_ref(),
            &deployment.label,
            &deployment.agent_did,
            Duration::from_secs(2),
            Duration::from_secs(60),
        )?;
        wait_for_stable_runtime_ready(
            fixture.runtime.as_ref(),
            desktop_client.as_ref(),
            &format!("desktop mirror for {}", deployment.label),
            &deployment.agent_did,
            Duration::from_secs(2),
            Duration::from_secs(60),
        )?;
    }

    let mut cases = fixture
        .deployments
        .iter()
        .map(|deployment| {
            let slug = deployment.label.to_ascii_lowercase().replace(' ', "-");
            let turn_one_root = format!("tauri-live-three-node/{slug}/turn-one");
            let turn_one_paths = vec![
                format!("{turn_one_root}/alpha.txt"),
                format!("{turn_one_root}/beta.txt"),
                format!("{turn_one_root}/gamma.txt"),
            ];
            let turn_one_tokens = vec![
                uuid::Uuid::new_v4().simple().to_string(),
                uuid::Uuid::new_v4().simple().to_string(),
                uuid::Uuid::new_v4().simple().to_string(),
            ];
            for (path, token) in turn_one_paths.iter().zip(turn_one_tokens.iter()) {
                deployment.running_agent.write_tool_file(path, token)?;
            }

            let turn_two_path = format!("tauri-live-three-node/{slug}/turn-two/followup.txt");
            let turn_two_token = uuid::Uuid::new_v4().simple().to_string();
            deployment
                .running_agent
                .write_tool_file(&turn_two_path, &turn_two_token)?;

            Ok::<_, anyhow::Error>(TauriThreeNodeConversationCase {
                label: deployment.label.clone(),
                peer_id: deployment.peer_id.clone(),
                agent_did: deployment.agent_did.clone(),
                behavior_id: deployment.docs.behavior_id.clone(),
                turn_one_prompt: tool_loop_prompt(
                    &format!("{}-turn-1", slug),
                    &turn_one_root,
                    &turn_one_paths,
                ),
                turn_two_prompt: format!(
                    "Continue this same conversation for {}. Call read_file for {}. Reply with the previous three tokens from this conversation, followed by the exact token from {}, separated by single spaces.",
                    deployment.label, turn_two_path, turn_two_path
                ),
                turn_one_tokens,
                turn_two_token,
                session_id: None,
                latest_request_id: None,
                prior_tool_call_count: 0,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let mut first_turns = Vec::new();
    for (case_index, case) in cases.iter().enumerate() {
        let submitted = create_live_agent_request_via_graphql(
            fixture.desktop_api.graphql_url(),
            &case.agent_did,
            &case.turn_one_prompt,
            None,
            Some(&case.behavior_id),
        )?;
        first_turns.push(SubmittedTauriTurn {
            case_index,
            request_id: submitted.request_id,
            session_id: submitted.session_id,
        });
    }

    for submitted in &first_turns {
        let case = &mut cases[submitted.case_index];
        case.session_id = Some(submitted.session_id.clone());
        case.latest_request_id = Some(submitted.request_id.clone());
    }

    let distinct_sessions = first_turns
        .iter()
        .map(|turn| turn.session_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        distinct_sessions.len(),
        cases.len(),
        "expected one isolated first-turn session per live deployment"
    );

    for submitted in &first_turns {
        let case = &mut cases[submitted.case_index];
        let session_id = case
            .session_id
            .as_deref()
            .ok_or_else(|| anyhow!("missing first-turn session for {}", case.label))?;

        wait_for_request_observed_in_desktop(
            fixture.runtime.as_ref(),
            desktop_client.as_ref(),
            &format!("{} first-turn request observed", case.label),
            &submitted.request_id,
            session_id,
            &case.agent_did,
            &case.behavior_id,
        )?;

        let early_snapshot = wait_for_value(
            &format!("{} early Tauri snapshot", case.label),
            Duration::from_secs(30),
            || {
                let snapshot = refreshed_tauri_session_snapshot(
                    fixture.runtime.as_ref(),
                    desktop_client.as_ref(),
                    session_id,
                    &submitted.request_id,
                )?;
                (snapshot.latest_request_id.as_deref() == Some(submitted.request_id.as_str()))
                    .then_some(snapshot)
            },
        )?;
        assert_eq!(
            early_snapshot.latest_request_id.as_deref(),
            Some(submitted.request_id.as_str())
        );
        assert!(
            matches!(
                early_snapshot.turn_state.as_deref(),
                Some("waitingForClaim" | "streaming" | "completed")
            ),
            "unexpected early turn_state for {}: {:?}",
            case.label,
            early_snapshot.turn_state
        );

        let tool_call_count = wait_for_value(
            &format!("{} first-turn tool activity", case.label),
            Duration::from_secs(120),
            || {
                let snapshot = refreshed_tauri_session_snapshot(
                    fixture.runtime.as_ref(),
                    desktop_client.as_ref(),
                    session_id,
                    &submitted.request_id,
                )?;
                let tool_count = snapshot.tool_calls.len();
                (tool_count > case.prior_tool_call_count).then_some(tool_count)
            },
        )?;
        case.prior_tool_call_count = tool_call_count;

        let completed_snapshot = wait_for_completed_tauri_snapshot(
            fixture.runtime.as_ref(),
            desktop_client.as_ref(),
            &format!("{} completed first-turn Tauri snapshot", case.label),
            session_id,
            &submitted.request_id,
            &case.turn_one_prompt,
        )?;
        let response_content = completed_snapshot
            .latest_response
            .as_ref()
            .and_then(|response| response.content.as_deref())
            .ok_or_else(|| anyhow!("missing first-turn response content for {}", case.label))?;
        assert_response_contains_tokens(
            &format!("{} first-turn response", case.label),
            response_content,
            &case.turn_one_tokens,
        )?;
        assert_eq!(
            completed_snapshot.agent_did.as_deref(),
            Some(case.agent_did.as_str())
        );
        assert_eq!(
            completed_snapshot.behavior_id.as_deref(),
            Some(case.behavior_id.as_str())
        );
    }

    let mut second_turns = Vec::new();
    for (case_index, case) in cases.iter().enumerate() {
        let session_id = case
            .session_id
            .as_deref()
            .ok_or_else(|| anyhow!("missing first-turn session for {}", case.label))?;
        let submitted = create_live_agent_request_via_graphql(
            fixture.desktop_api.graphql_url(),
            &case.agent_did,
            &case.turn_two_prompt,
            Some(session_id),
            Some(&case.behavior_id),
        )?;
        assert_eq!(
            submitted.session_id, session_id,
            "expected {} follow-up turn to stay in the same desktop session",
            case.label
        );
        second_turns.push(SubmittedTauriTurn {
            case_index,
            request_id: submitted.request_id,
            session_id: submitted.session_id,
        });
    }

    for submitted in &second_turns {
        let case = &mut cases[submitted.case_index];
        case.latest_request_id = Some(submitted.request_id.clone());
    }

    for submitted in &second_turns {
        let case = &mut cases[submitted.case_index];
        let session_id = case
            .session_id
            .as_deref()
            .ok_or_else(|| anyhow!("missing follow-up session for {}", case.label))?;

        wait_for_request_observed_in_desktop(
            fixture.runtime.as_ref(),
            desktop_client.as_ref(),
            &format!("{} follow-up request observed", case.label),
            &submitted.request_id,
            session_id,
            &case.agent_did,
            &case.behavior_id,
        )?;

        let early_snapshot = wait_for_value(
            &format!("{} early follow-up Tauri snapshot", case.label),
            Duration::from_secs(30),
            || {
                let snapshot = refreshed_tauri_session_snapshot(
                    fixture.runtime.as_ref(),
                    desktop_client.as_ref(),
                    session_id,
                    &submitted.request_id,
                )?;
                (snapshot.latest_request_id.as_deref() == Some(submitted.request_id.as_str()))
                    .then_some(snapshot)
            },
        )?;
        assert_eq!(
            early_snapshot.latest_request_id.as_deref(),
            Some(submitted.request_id.as_str())
        );
        assert!(
            matches!(
                early_snapshot.turn_state.as_deref(),
                Some("waitingForClaim" | "streaming" | "completed")
            ),
            "unexpected early follow-up turn_state for {}: {:?}",
            case.label,
            early_snapshot.turn_state
        );

        let tool_call_count = wait_for_value(
            &format!("{} follow-up tool activity", case.label),
            Duration::from_secs(120),
            || {
                let snapshot = refreshed_tauri_session_snapshot(
                    fixture.runtime.as_ref(),
                    desktop_client.as_ref(),
                    session_id,
                    &submitted.request_id,
                )?;
                let tool_count = snapshot.tool_calls.len();
                (tool_count > case.prior_tool_call_count).then_some(tool_count)
            },
        )?;
        case.prior_tool_call_count = tool_call_count;

        let completed_snapshot = wait_for_completed_tauri_snapshot(
            fixture.runtime.as_ref(),
            desktop_client.as_ref(),
            &format!("{} completed follow-up Tauri snapshot", case.label),
            session_id,
            &submitted.request_id,
            &case.turn_two_prompt,
        )?;
        let expected_followup_tokens = case.expected_followup_tokens();
        let response_content = completed_snapshot
            .latest_response
            .as_ref()
            .and_then(|response| response.content.as_deref())
            .ok_or_else(|| anyhow!("missing follow-up response content for {}", case.label))?;
        assert_response_contains_tokens(
            &format!("{} follow-up response", case.label),
            response_content,
            &expected_followup_tokens,
        )?;
        assert_eq!(
            completed_snapshot.agent_did.as_deref(),
            Some(case.agent_did.as_str())
        );
        assert_eq!(
            completed_snapshot.behavior_id.as_deref(),
            Some(case.behavior_id.as_str())
        );
    }

    for case in &cases {
        let session_id = case
            .session_id
            .as_deref()
            .ok_or_else(|| anyhow!("missing final session for {}", case.label))?;
        let latest_request_id = case
            .latest_request_id
            .as_deref()
            .ok_or_else(|| anyhow!("missing final request for {}", case.label))?;

        fixture.runtime.block_on(desktop_client.refresh_store())?;
        let store = desktop_client.store().snapshot();
        let final_snapshot = build_tauri_session_snapshot_from_store(
            store.as_ref(),
            session_id,
            Some(latest_request_id),
        )
        .ok_or_else(|| anyhow!("missing final Tauri snapshot for {}", case.label))?;
        let request_count = store.requests_for_session(session_id).len();
        assert_eq!(
            request_count, 2,
            "expected exactly two desktop requests persisted for {}",
            case.label
        );
        assert_eq!(
            final_snapshot.latest_request_id.as_deref(),
            Some(latest_request_id)
        );
        assert_eq!(
            final_snapshot.agent_did.as_deref(),
            Some(case.agent_did.as_str())
        );
        assert_eq!(
            final_snapshot.behavior_id.as_deref(),
            Some(case.behavior_id.as_str())
        );
        assert!(matches!(
            final_snapshot.turn_state.as_deref(),
            Some("completed" | "failed" | "superseded")
        ));
        assert!(
            final_snapshot.tool_calls.len() >= case.prior_tool_call_count,
            "expected final snapshot to retain observed tool call history for {}",
            case.label
        );

        let own_prompts = vec![case.turn_one_prompt.as_str(), case.turn_two_prompt.as_str()];
        let forbidden_prompts = cases
            .iter()
            .filter(|other| other.peer_id != case.peer_id)
            .flat_map(|other| {
                [
                    other.turn_one_prompt.as_str(),
                    other.turn_two_prompt.as_str(),
                ]
            })
            .collect::<Vec<_>>();
        assert_snapshot_prompt_isolation(
            &case.label,
            &final_snapshot,
            &own_prompts,
            &forbidden_prompts,
        )?;

        let remote_core = fixture
            .deployments
            .iter()
            .find(|deployment| deployment.peer_id == case.peer_id)
            .map(|deployment| deployment.core.as_ref())
            .ok_or_else(|| anyhow!("missing remote core for {}", case.label))?;
        wait_for_request_terminal_on_core(
            fixture.runtime.as_ref(),
            remote_core,
            &format!("{} remote request terminal", case.label),
            session_id,
            latest_request_id,
            &case.agent_did,
        )?;
    }

    fixture.shutdown()
}

#[test]
#[ignore = "hits the fixed MiniMax live backend and asserts Tauri session snapshots through a real single-node three-turn repo investigation"]
fn tauri_live_session_snapshot_tracks_three_turn_repo_investigation() -> Result<()> {
    let prompts = [
        "Hey amy can you tell me about the p2p communcation between the agent and the desktop in this app and the docuemnt based request model?",
        "awesome breakdown, can you please tell me what you like about the architecture? please use details and point to files.",
        "can you please tell me what you don't like about the architecture? please use details and point to files.",
    ];
    run_live_tauri_session_snapshot(
        "tauri-live-session-snapshot",
        &explicit_soak_backend(),
        &prompts,
    )
}

#[test]
#[ignore = "hits the thinking-enabled live backend configured by DEFRA_AGENT_DESKTOP_THINKING_BACKEND_* and asserts Tauri session snapshots through a real single-node three-turn repo investigation"]
fn tauri_live_session_snapshot_tracks_three_turn_repo_investigation_with_thinking_backend(
) -> Result<()> {
    let prompts = [
        "Hey amy can you tell me about the p2p communcation between the agent and the desktop in this app and the docuemnt based request model?",
        "awesome breakdown, can you please tell me what you like about the architecture? please use details and point to files.",
        "can you please tell me what you don't like about the architecture? please use details and point to files.",
    ];
    let backend = AgentBackendConfig::live_from_env_prefix("DEFRA_AGENT_DESKTOP_THINKING_BACKEND")?;
    run_live_tauri_session_snapshot("tauri-live-session-snapshot-thinking", &backend, &prompts)
}

#[test]
#[ignore = "hits the fixed MiniMax live backend and asserts three registered live deployments keep separate multi-turn Tauri session snapshots through the desktop"]
fn tauri_live_session_snapshot_tracks_three_registered_multi_turn_conversations() -> Result<()> {
    run_three_node_live_tauri_session_snapshot(
        "tauri-live-three-node-session-snapshot",
        &explicit_soak_backend(),
    )
}

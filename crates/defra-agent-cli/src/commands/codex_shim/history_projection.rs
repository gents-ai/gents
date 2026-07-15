use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use codex_app_server_protocol as codex;
use codex_protocol::models::MessagePhase;
use defra_agent::graphql::escape_graphql_string;
use defra_agent_protocol::transcript::present_persisted_message;
use serde::Deserialize;
use serde_json::{json, Value};

use super::command_projection::{
    command_execution_item, file_change_item, tool_projection_status_with_settled,
    ToolProjectionStatus,
};
use super::compaction_projection::context_compaction_item;
use super::progress::{
    decode_defra_tool_call_progress, defra_tool_item, terminal_error_message, terminal_turn_status,
    DefraToolCallProgress,
};
use super::protocol::{absolute_path, agent_message_item_with_phase, turn_value};
use super::store::{hydrate_materialized_response_content, query_node_json};
use super::subagent_projection::{
    attach_subagent_link, collab_tool_item, load_authorized_subagent_threads_for_root,
};
use super::thread_projection::CodexThreadRecord;
use super::ShimState;

#[derive(Debug, Clone, Deserialize)]
struct RequestRow {
    request_id: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    lifecycle_state: String,
    #[serde(default)]
    failure_reason: String,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    metadata: String,
    #[serde(default)]
    execution_origin: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseRow {
    request_id: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    reasoning: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    error_message: Option<String>,
    #[serde(default)]
    materialized_message_sequence: Option<i64>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
    #[serde(default)]
    interrupted_at: Option<String>,
}

#[derive(Debug, Clone)]
struct ToolRow {
    request_id: String,
    message_sequence: i64,
    started_at: Option<String>,
    progress: DefraToolCallProgress,
}

#[derive(Debug, Clone, Deserialize)]
struct CompactionRow {
    request_id: String,
    call_id: String,
    #[serde(default)]
    call_state: String,
    #[serde(default)]
    call_seq: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct MessageRow {
    sequence: i64,
    #[serde(default)]
    role: String,
    #[serde(default)]
    content: String,
}

pub(super) async fn load_thread_turns(
    state: &ShimState,
    record: &CodexThreadRecord,
) -> Result<Vec<codex::Turn>> {
    let escaped_session_id = escape_graphql_string(&record.session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                request_id
                content
                status
                lifecycle_state
                failure_reason
                created_at
                metadata
                execution_origin
            }}
            AgentResponse(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                request_id
                session_id
                content
                reasoning
                status
                error_message
                materialized_message_sequence
                created_at
                completed_at
                interrupted_at
            }}
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ started_at: ASC }}
            ) {{
                tool_call_key
                request_id
                session_id
                message_sequence
                tool_name
                status
                lifecycle_state
                await_mode
                child_request_id
                args
                result
                started_at
                completed_at
            }}
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{
                sequence
                role
                content
            }}
        }}"#,
    );
    let response = query_node_json(&state.node, &query).await?;

    let requests = decode_rows::<RequestRow>(&response, "AgentRequest")
        .context("decoding AgentRequest history rows")?;
    let responses = decode_response_rows(state, &response).await?;
    let mut tools = decode_tool_rows(&response).context("decoding AgentToolCall history rows")?;
    let root_session_id = record
        .subagent
        .as_ref()
        .map(|link| link.root_session_id.as_str())
        .unwrap_or(record.session_id.as_str());
    let subagent_links = load_authorized_subagent_threads_for_root(state, root_session_id).await?;
    for tool in &mut tools {
        attach_subagent_link(&mut tool.progress, &subagent_links);
    }
    let messages = decode_rows::<MessageRow>(&response, "AgentMessage")
        .context("decoding AgentMessage rows")?;
    let compactions = load_completed_compactions(state, &requests).await?;

    let mut responses_by_request = BTreeMap::<String, ResponseRow>::new();
    for response in responses {
        responses_by_request.insert(response.request_id.clone(), response);
    }

    let mut tools_by_request = BTreeMap::<String, Vec<ToolRow>>::new();
    for tool in tools {
        let request_id = tool.request_id.clone();
        tools_by_request.entry(request_id).or_default().push(tool);
    }

    let mut compactions_by_request = BTreeMap::<String, Vec<CompactionRow>>::new();
    for compaction in compactions
        .into_iter()
        .filter(|row| row.call_state.trim() == "completed")
    {
        compactions_by_request
            .entry(compaction.request_id.clone())
            .or_default()
            .push(compaction);
    }
    for rows in compactions_by_request.values_mut() {
        rows.sort_by_key(|row| row.call_seq);
    }

    let messages_by_sequence = messages
        .iter()
        .cloned()
        .map(|message| (message.sequence, message))
        .collect::<BTreeMap<_, _>>();

    let turns = project_request_turns(
        record,
        requests,
        &responses_by_request,
        &tools_by_request,
        &compactions_by_request,
        &messages_by_sequence,
    )?;

    if turns.is_empty() && !messages.is_empty() {
        return Ok(project_message_turns(messages));
    }

    Ok(turns)
}

async fn load_completed_compactions(
    state: &ShimState,
    requests: &[RequestRow],
) -> Result<Vec<CompactionRow>> {
    let request_ids = requests
        .iter()
        .map(|request| request.request_id.trim())
        .filter(|request_id| !request_id.is_empty())
        .collect::<BTreeSet<_>>();
    if request_ids.is_empty() {
        return Ok(Vec::new());
    }
    let id_list = request_ids
        .into_iter()
        .map(|request_id| format!(r#""{}""#, escape_graphql_string(request_id)))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{
                    request_id: {{ _in: [{id_list}] }},
                    call_kind: {{ _eq: "compaction" }},
                    call_state: {{ _eq: "completed" }}
                }},
                order: {{ call_seq: ASC }}
            ) {{
                request_id
                call_id
                call_state
                call_seq
            }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    decode_rows::<CompactionRow>(&response, "InferenceCall")
        .context("decoding completed InferenceCall compaction history rows")
}

fn project_message_turns(messages: Vec<MessageRow>) -> Vec<codex::Turn> {
    let mut turns = Vec::new();
    let mut current_id = None::<String>;
    let mut current_items = Vec::<codex::ThreadItem>::new();
    let mut saw_assistant = false;

    for message in messages {
        let role = message.role.trim();
        if role.eq_ignore_ascii_case("user") {
            finish_message_turn(
                &mut turns,
                current_id.take(),
                std::mem::take(&mut current_items),
                saw_assistant,
            );
            saw_assistant = false;
            current_id = Some(format!("defra-message-turn-{}", message.sequence));
            let presentation = present_persisted_message(&message.role, &message.content);
            if !presentation.body_markdown.trim().is_empty() {
                current_items.push(codex::ThreadItem::UserMessage {
                    id: format!("defra-user-message-{}", message.sequence),
                    content: vec![codex::UserInput::Text {
                        text: presentation.body_markdown,
                        text_elements: Vec::new(),
                    }],
                });
            }
        } else if role.eq_ignore_ascii_case("assistant") {
            if current_id.is_none() {
                current_id = Some(format!("defra-message-turn-{}", message.sequence));
            }
            saw_assistant |= append_assistant_message_items(
                &mut current_items,
                message.sequence,
                &message,
                true,
            );
        }
    }

    finish_message_turn(&mut turns, current_id.take(), current_items, saw_assistant);
    turns
}

fn finish_message_turn(
    turns: &mut Vec<codex::Turn>,
    turn_id: Option<String>,
    items: Vec<codex::ThreadItem>,
    saw_assistant: bool,
) {
    if let Some(turn_id) = turn_id.filter(|_| !items.is_empty()) {
        let status = if saw_assistant {
            codex::TurnStatus::Completed
        } else {
            codex::TurnStatus::Interrupted
        };
        turns.push(turn_value(&turn_id, status, items, None));
    }
}

fn project_request_turns(
    record: &CodexThreadRecord,
    requests: Vec<RequestRow>,
    responses_by_request: &BTreeMap<String, ResponseRow>,
    tools_by_request: &BTreeMap<String, Vec<ToolRow>>,
    compactions_by_request: &BTreeMap<String, Vec<CompactionRow>>,
    messages_by_sequence: &BTreeMap<i64, MessageRow>,
) -> Result<Vec<codex::Turn>> {
    let requests = requests
        .into_iter()
        .filter(|request| record.is_subagent() || is_codex_visible_request(request))
        .collect::<Vec<_>>();
    let requests_by_id = requests
        .iter()
        .map(|request| (request.request_id.as_str(), request))
        .collect::<BTreeMap<_, _>>();

    let mut root_order = Vec::<String>::new();
    let mut grouped = BTreeMap::<String, Vec<RequestRow>>::new();
    for request in &requests {
        let root_id = steering_root_id(request, &requests_by_id)?;
        if !grouped.contains_key(&root_id) {
            root_order.push(root_id.clone());
        }
        grouped.entry(root_id).or_default().push(request.clone());
    }

    let mut turns = Vec::with_capacity(grouped.len());
    for root_id in root_order {
        let Some(group) = grouped.remove(&root_id) else {
            continue;
        };
        turns.push(project_turn_group(
            record,
            &root_id,
            &group,
            responses_by_request,
            tools_by_request,
            compactions_by_request,
            messages_by_sequence,
        ));
    }
    Ok(turns)
}

fn steering_root_id(
    request: &RequestRow,
    requests_by_id: &BTreeMap<&str, &RequestRow>,
) -> Result<String> {
    let mut current = request;
    let mut seen = BTreeSet::<String>::new();
    loop {
        if !seen.insert(current.request_id.clone()) {
            anyhow::bail!(
                "cycle in Codex steering history ancestry at request {}",
                current.request_id
            );
        }

        let Some(parent_id) = steering_parent_id(current) else {
            return Ok(current.request_id.clone());
        };
        let Some(parent) = requests_by_id.get(parent_id.as_str()).copied() else {
            return Ok(parent_id);
        };
        current = parent;
    }
}

fn steering_parent_id(request: &RequestRow) -> Option<String> {
    let metadata = request.metadata.trim();
    if metadata.is_empty() {
        return None;
    }
    let value = serde_json::from_str::<Value>(metadata).ok()?;
    let queue = value.get("queue")?;
    let source = queue.get("source").and_then(Value::as_str)?;
    if source != "steering" {
        return None;
    }
    queue
        .get("queued_after_request_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|request_id| !request_id.is_empty())
        .map(ToOwned::to_owned)
}

fn is_codex_visible_request(request: &RequestRow) -> bool {
    request.metadata.contains("\"codex_shim\"")
        || request
            .execution_origin
            .trim()
            .eq_ignore_ascii_case("interactive")
}

pub(super) fn thread_turns_list_response(
    mut turns: Vec<codex::Turn>,
    cursor: Option<String>,
    limit: Option<u32>,
    sort_direction: Option<codex::SortDirection>,
    items_view: Option<codex::TurnItemsView>,
) -> codex::ThreadTurnsListResponse {
    if sort_direction.unwrap_or(codex::SortDirection::Desc) == codex::SortDirection::Desc {
        turns.reverse();
    }

    let items_view = items_view.unwrap_or(codex::TurnItemsView::Summary);
    for turn in &mut turns {
        apply_items_view(turn, items_view);
    }

    let page = paginate_by_id(turns, cursor.as_deref(), limit, |turn| &turn.id);
    codex::ThreadTurnsListResponse {
        data: page.items,
        next_cursor: page.next_cursor,
        backwards_cursor: page.backwards_cursor,
    }
}

pub(super) fn thread_turn_items_list_response(
    turns: Vec<codex::Turn>,
    turn_id: &str,
    cursor: Option<String>,
    limit: Option<u32>,
    sort_direction: Option<codex::SortDirection>,
) -> Option<codex::ThreadTurnsItemsListResponse> {
    let mut items = turns.into_iter().find(|turn| turn.id == turn_id)?.items;
    if sort_direction.unwrap_or(codex::SortDirection::Asc) == codex::SortDirection::Desc {
        items.reverse();
    }
    let page = paginate_by_id(items, cursor.as_deref(), limit, |item| item.id());
    Some(codex::ThreadTurnsItemsListResponse {
        data: page.items,
        next_cursor: page.next_cursor,
        backwards_cursor: page.backwards_cursor,
    })
}

pub(super) fn conversation_summary_json(state: &ShimState, record: &CodexThreadRecord) -> Value {
    let conversation = record.conversation.as_ref();
    let preview = conversation
        .and_then(|conversation| {
            let preview = conversation.preview_text.trim();
            (!preview.is_empty()).then_some(preview)
        })
        .or_else(|| {
            conversation.and_then(|conversation| {
                let title = conversation.title.trim();
                (!title.is_empty()).then_some(title)
            })
        })
        .unwrap_or("");
    json!({
        "summary": {
            "conversationId": record.session_id,
            "path": absolute_path(&state.codex_home.join("defra-backed").join(&record.session_id)),
            "preview": preview,
            "timestamp": conversation.and_then(|conversation| conversation.created_at.clone()),
            "updatedAt": conversation.and_then(|conversation| conversation.updated_at.clone()),
            "modelProvider": "defra",
            "cwd": absolute_path(&record.cwd),
            "cliVersion": env!("CARGO_PKG_VERSION"),
            "source": "cli",
            "gitInfo": conversation_summary_git_info(&record.git_info),
        }
    })
}

/// Reshape the derived v2 thread git metadata (`{sha, branch, originUrl}`) into
/// the v1 `ConversationGitInfo` shape (`{sha, branch, origin_url}`) used by
/// `getConversationSummary`. Without this, the typed round-trip in
/// `send_typed_json_result` drops the camelCase `originUrl` and loses the remote.
fn conversation_summary_git_info(git_info: &Option<Value>) -> Option<Value> {
    let object = git_info.as_ref()?.as_object()?;
    let field = |name: &str| object.get(name).and_then(Value::as_str);
    Some(json!({
        "sha": field("sha"),
        "branch": field("branch"),
        "origin_url": field("originUrl").or_else(|| field("origin_url")),
    }))
}

fn project_turn_group(
    record: &CodexThreadRecord,
    turn_id: &str,
    requests: &[RequestRow],
    responses_by_request: &BTreeMap<String, ResponseRow>,
    tools_by_request: &BTreeMap<String, Vec<ToolRow>>,
    compactions_by_request: &BTreeMap<String, Vec<CompactionRow>>,
    messages_by_sequence: &BTreeMap<i64, MessageRow>,
) -> codex::Turn {
    let Some(first_request) = requests.first() else {
        return turn_value(turn_id, codex::TurnStatus::Completed, Vec::new(), None);
    };
    let tail_request = requests.last().unwrap_or(first_request);
    let tail_response = responses_by_request.get(&tail_request.request_id);

    let mut items = Vec::new();
    for request in requests {
        let response = responses_by_request.get(&request.request_id);
        let tools = tools_by_request
            .get(&request.request_id)
            .cloned()
            .unwrap_or_default();
        let compactions = compactions_by_request
            .get(&request.request_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        append_request_items(
            record,
            &mut items,
            request,
            response,
            tools,
            compactions,
            messages_by_sequence,
        );
    }

    let status = turn_status(tail_request, tail_response);
    let error = (status == codex::TurnStatus::Failed)
        .then(|| turn_error(tail_request, tail_response))
        .flatten();
    let started_at = first_request
        .created_at
        .as_deref()
        .and_then(parse_timestamp_seconds);
    let completed_at = turn_completed_timestamp(tail_response);
    let duration_ms = started_at
        .zip(completed_at)
        .map(|(started, completed)| (completed - started).max(0) * 1000);

    codex::Turn {
        id: turn_id.to_string(),
        items,
        items_view: codex::TurnItemsView::Full,
        status,
        error,
        started_at,
        completed_at,
        duration_ms,
    }
}

fn append_request_items(
    record: &CodexThreadRecord,
    items: &mut Vec<codex::ThreadItem>,
    request: &RequestRow,
    response: Option<&ResponseRow>,
    mut tools: Vec<ToolRow>,
    compactions: &[CompactionRow],
    messages_by_sequence: &BTreeMap<i64, MessageRow>,
) {
    let projection_settled = matches!(
        request.lifecycle_state.trim(),
        "completed" | "failed" | "dead" | "interrupted" | "superseded"
    ) || response.is_some_and(|response| {
        matches!(response.status.trim(), "complete" | "completed" | "error")
    });
    tools.sort_by(|left, right| {
        left.message_sequence
            .cmp(&right.message_sequence)
            .then_with(|| left.started_at.cmp(&right.started_at))
    });

    if !request.content.trim().is_empty() {
        items.push(codex::ThreadItem::UserMessage {
            id: format!("defra-user-{}", request.request_id),
            content: vec![codex::UserInput::Text {
                text: request.content.clone(),
                text_elements: Vec::new(),
            }],
        });
    }

    items.extend(
        compactions
            .iter()
            .map(|compaction| context_compaction_item(&compaction.call_id)),
    );

    let mut rendered_assistant_sequences = BTreeSet::<i64>::new();
    for tool in tools {
        if let Some(message) = messages_by_sequence.get(&tool.message_sequence) {
            if !rendered_assistant_sequences.contains(&tool.message_sequence)
                && append_assistant_message_items(items, tool.message_sequence, message, false)
            {
                rendered_assistant_sequences.insert(tool.message_sequence);
            }
        }
        if let Some(item) = project_tool(record, &tool.progress, projection_settled) {
            items.push(item);
        }
    }

    if let Some(response) = response {
        let rendered_materialized = response
            .materialized_message_sequence
            .and_then(|sequence| {
                if rendered_assistant_sequences.contains(&sequence) {
                    return Some(true);
                }
                let message = messages_by_sequence.get(&sequence)?;
                append_assistant_message_items(items, sequence, message, true).then_some(true)
            })
            .unwrap_or(false);

        if !rendered_materialized && !response.reasoning.trim().is_empty() {
            items.push(codex::ThreadItem::Reasoning {
                id: format!("defra-reasoning-{}", request.request_id),
                summary: Vec::new(),
                content: vec![response.reasoning.clone()],
            });
        }
        if !rendered_materialized && !response.content.trim().is_empty() {
            items.push(agent_message_item_with_phase(
                &format!("defra-agent-{}", request.request_id),
                &response.content,
                Some(MessagePhase::FinalAnswer),
            ));
        }
    }
}

fn append_assistant_message_items(
    items: &mut Vec<codex::ThreadItem>,
    sequence: i64,
    message: &MessageRow,
    final_answer: bool,
) -> bool {
    if !message.role.trim().eq_ignore_ascii_case("assistant") {
        return false;
    }
    let presentation = present_persisted_message(&message.role, &message.content);
    let mut appended = false;
    if let Some(reasoning) = presentation
        .reasoning_markdown
        .filter(|value| !value.trim().is_empty())
    {
        items.push(codex::ThreadItem::Reasoning {
            id: format!("defra-reasoning-message-{sequence}"),
            summary: Vec::new(),
            content: vec![reasoning],
        });
        appended = true;
    }
    if !presentation.body_markdown.trim().is_empty() {
        let phase = if final_answer && !presentation.has_tool_calls {
            MessagePhase::FinalAnswer
        } else {
            MessagePhase::Commentary
        };
        items.push(agent_message_item_with_phase(
            &format!("defra-agent-message-{sequence}"),
            &presentation.body_markdown,
            Some(phase),
        ));
        appended = true;
    }
    appended
}

fn project_tool(
    record: &CodexThreadRecord,
    tool: &DefraToolCallProgress,
    projection_settled: bool,
) -> Option<codex::ThreadItem> {
    match tool_projection_status_with_settled(tool, projection_settled) {
        ToolProjectionStatus::Mcp(status) => Some(defra_tool_item(tool, status)),
        ToolProjectionStatus::Command(status) => {
            Some(command_execution_item(&record.cwd, tool, status))
        }
        ToolProjectionStatus::Collab(projection) => {
            Some(collab_tool_item(&record.session_id, tool, &projection))
        }
        ToolProjectionStatus::DeferredCollab => None,
        ToolProjectionStatus::DeferredFileChange => None,
        ToolProjectionStatus::FileChange(status) => file_change_item(tool, status),
    }
}

fn turn_status(request: &RequestRow, response: Option<&ResponseRow>) -> codex::TurnStatus {
    let lifecycle_state = normalized_nonempty(&request.lifecycle_state)
        .or_else(|| normalized_nonempty(&request.status))
        .unwrap_or_else(|| "pending".to_string());
    let response_status = response
        .and_then(|response| normalized_nonempty(&response.status))
        .unwrap_or_default();

    if response_status == "interrupted" {
        codex::TurnStatus::Interrupted
    } else if response_status == "complete"
        || response_status == "completed"
        || response_status == "error"
        || matches!(
            lifecycle_state.as_str(),
            "completed" | "failed" | "dead" | "interrupted" | "superseded"
        )
    {
        terminal_turn_status(&lifecycle_state, &response_status)
    } else {
        codex::TurnStatus::InProgress
    }
}

fn turn_error(request: &RequestRow, response: Option<&ResponseRow>) -> Option<codex::TurnError> {
    let response_status = response.map(|row| row.status.as_str()).unwrap_or_default();
    let response_error = response.and_then(|row| row.error_message.as_deref());
    let lifecycle_state = request.lifecycle_state.as_str();
    terminal_error_message(
        response_status,
        response_error,
        lifecycle_state,
        &request.failure_reason,
    )
    .map(|message| codex::TurnError {
        message,
        codex_error_info: None,
        additional_details: None,
    })
}

fn turn_completed_timestamp(response: Option<&ResponseRow>) -> Option<i64> {
    response.and_then(|response| {
        response
            .completed_at
            .as_deref()
            .or(response.interrupted_at.as_deref())
            .or(response.created_at.as_deref())
            .and_then(parse_timestamp_seconds)
    })
}

async fn decode_response_rows(state: &ShimState, response: &Value) -> Result<Vec<ResponseRow>> {
    let mut rows = Vec::new();
    for mut row in raw_rows(response, "AgentResponse") {
        hydrate_materialized_response_content(&state.node, &mut row).await?;
        rows.push(serde_json::from_value(row).context("decoding AgentResponse history row")?);
    }
    Ok(rows)
}

fn decode_tool_rows(response: &Value) -> Result<Vec<ToolRow>> {
    raw_rows(response, "AgentToolCall")
        .into_iter()
        .map(|row| {
            let message_sequence = row
                .get("message_sequence")
                .and_then(|value| value.as_i64().or_else(|| value.as_u64().map(|v| v as i64)))
                .unwrap_or(0);
            let request_id = row
                .get("request_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let started_at = row
                .get("started_at")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let progress = decode_defra_tool_call_progress(&row)
                .with_context(|| format!("decoding AgentToolCall progress row: {row}"))?;
            Ok(ToolRow {
                request_id,
                message_sequence,
                started_at,
                progress,
            })
        })
        .collect()
}

fn decode_rows<T>(response: &Value, collection: &str) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    raw_rows(response, collection)
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("decoding {collection} rows"))
}

fn raw_rows(response: &Value, collection: &str) -> Vec<Value> {
    response
        .pointer(&format!("/data/{collection}"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn apply_items_view(turn: &mut codex::Turn, view: codex::TurnItemsView) {
    match view {
        codex::TurnItemsView::NotLoaded => {
            turn.items.clear();
            turn.items_view = codex::TurnItemsView::NotLoaded;
        }
        codex::TurnItemsView::Summary => {
            let first_user_message = turn
                .items
                .iter()
                .find(|item| matches!(item, codex::ThreadItem::UserMessage { .. }))
                .cloned();
            let final_agent_message = turn
                .items
                .iter()
                .rev()
                .find(|item| matches!(item, codex::ThreadItem::AgentMessage { .. }))
                .cloned();
            turn.items = match (first_user_message, final_agent_message) {
                (Some(user), Some(agent)) if user.id() != agent.id() => vec![user, agent],
                (Some(user), _) => vec![user],
                (None, Some(agent)) => vec![agent],
                (None, None) => Vec::new(),
            };
            turn.items_view = codex::TurnItemsView::Summary;
        }
        codex::TurnItemsView::Full => {
            turn.items_view = codex::TurnItemsView::Full;
        }
    }
}

struct Page<T> {
    items: Vec<T>,
    next_cursor: Option<String>,
    backwards_cursor: Option<String>,
}

fn paginate_by_id<T>(
    items: Vec<T>,
    cursor: Option<&str>,
    limit: Option<u32>,
    id: impl Fn(&T) -> &str,
) -> Page<T> {
    let start = cursor
        .and_then(|cursor| items.iter().position(|item| id(item) == cursor))
        .map(|position| position + 1)
        .unwrap_or(0);
    let limit = limit.map(|limit| limit as usize).unwrap_or(items.len());
    let end = start.saturating_add(limit).min(items.len());
    let backwards_cursor = items.get(start).map(|item| id(item).to_string());
    let next_cursor = (end < items.len())
        .then(|| {
            items
                .get(end.saturating_sub(1))
                .map(|item| id(item).to_string())
        })
        .flatten();
    Page {
        items: items.into_iter().skip(start).take(end - start).collect(),
        next_cursor,
        backwards_cursor,
    }
}

fn normalized_nonempty(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    (!value.is_empty()).then_some(value)
}

fn parse_timestamp_seconds(raw: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|timestamp| timestamp.timestamp())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn completed_request_overrides_error_response_status() {
        let request = RequestRow {
            request_id: "request-1".to_string(),
            content: "Inspect the repo".to_string(),
            status: "completed".to_string(),
            lifecycle_state: "completed".to_string(),
            failure_reason: String::new(),
            created_at: None,
            metadata: r#"{"codex_shim":{}}"#.to_string(),
            execution_origin: "interactive".to_string(),
        };
        let response = ResponseRow {
            request_id: request.request_id.clone(),
            content: "Done".to_string(),
            reasoning: String::new(),
            status: "error".to_string(),
            error_message: Some("stale response error".to_string()),
            materialized_message_sequence: None,
            created_at: None,
            completed_at: None,
            interrupted_at: None,
        };

        assert_eq!(
            turn_status(&request, Some(&response)),
            codex::TurnStatus::Completed
        );
    }

    #[test]
    fn append_request_items_replays_assistant_text_around_tool_calls() {
        let record = CodexThreadRecord {
            session_id: "thread-1".to_string(),
            cwd: PathBuf::from("/tmp/project"),
            archived: false,
            loaded: true,
            memory_mode: "enabled".to_string(),
            name: String::new(),
            settings_json: "{}".to_string(),
            git_info: None,
            projection_started: None,
            conversation: None,
            subagent: None,
        };
        let request = RequestRow {
            request_id: "request-1".to_string(),
            content: "Inspect the repo".to_string(),
            status: "completed".to_string(),
            lifecycle_state: "completed".to_string(),
            failure_reason: String::new(),
            created_at: None,
            metadata: r#"{"codex_shim":{}}"#.to_string(),
            execution_origin: "interactive".to_string(),
        };
        let response = ResponseRow {
            request_id: request.request_id.clone(),
            content: String::new(),
            reasoning: String::new(),
            status: "complete".to_string(),
            error_message: None,
            materialized_message_sequence: Some(4),
            created_at: None,
            completed_at: None,
            interrupted_at: None,
        };
        let first_tool = ToolRow {
            request_id: request.request_id.clone(),
            message_sequence: 2,
            started_at: None,
            progress: DefraToolCallProgress {
                tool_call_key: "thread-1:call-1".to_string(),
                tool_name: "list_files".to_string(),
                status: "completed".to_string(),
                lifecycle_state: Some("completed".to_string()),
                await_mode: None,
                child_request_id: None,
                args: r#"{"path":"."}"#.to_string(),
                result: "Cargo.toml\nsrc".to_string(),
                subagent_link: None,
            },
        };
        let second_tool = ToolRow {
            request_id: request.request_id.clone(),
            message_sequence: 2,
            started_at: Some("2026-06-02T00:00:01Z".to_string()),
            progress: DefraToolCallProgress {
                tool_call_key: "thread-1:call-2".to_string(),
                tool_name: "read_file".to_string(),
                status: "completed".to_string(),
                lifecycle_state: Some("completed".to_string()),
                await_mode: None,
                child_request_id: None,
                args: r#"{"path":"Cargo.toml"}"#.to_string(),
                result: "[package]".to_string(),
                subagent_link: None,
            },
        };
        let messages_by_sequence = BTreeMap::from([
            (
                2,
                MessageRow {
                    sequence: 2,
                    role: "assistant".to_string(),
                    content: r#"{"role":"assistant","id":null,"content":[{"id":"call-1","call_id":null,"function":{"name":"list_files","arguments":{"path":"."}},"signature":null,"additional_params":null},{"text":"Before the tool call."}]}"#.to_string(),
                },
            ),
            (
                4,
                MessageRow {
                    sequence: 4,
                    role: "assistant".to_string(),
                    content: r#"{"role":"assistant","id":null,"content":[{"text":"Final answer after tools."}]}"#.to_string(),
                },
            ),
        ]);

        let mut items = Vec::new();
        append_request_items(
            &record,
            &mut items,
            &request,
            Some(&response),
            vec![first_tool, second_tool],
            &[],
            &messages_by_sequence,
        );

        let agent_messages = items
            .iter()
            .filter_map(|item| match item {
                codex::ThreadItem::AgentMessage { text, phase, .. } => {
                    Some((text.as_str(), phase.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            agent_messages
                .iter()
                .any(|(text, _)| text.contains("Before the tool call.")),
            "intermediate assistant text should be replayed; items={items:?}"
        );
        let intermediate_count = agent_messages
            .iter()
            .filter(|(text, _)| text.contains("Before the tool call."))
            .count();
        assert_eq!(
            intermediate_count, 1,
            "assistant text before sibling tool calls should only be replayed once; items={items:?}"
        );
        assert!(
            agent_messages
                .iter()
                .any(|(text, phase)| text.contains("Before the tool call.")
                    && *phase == Some(MessagePhase::Commentary)),
            "intermediate assistant text should replay as commentary; items={items:?}"
        );
        assert!(
            agent_messages
                .iter()
                .any(|(text, phase)| text.contains("Final answer after tools.")
                    && *phase == Some(MessagePhase::FinalAnswer)),
            "final materialized assistant text should be replayed; items={items:?}"
        );
    }

    #[test]
    fn append_request_items_replays_only_successful_context_compactions() {
        let record = CodexThreadRecord {
            session_id: "thread-1".to_string(),
            cwd: PathBuf::from("/tmp/project"),
            archived: false,
            loaded: true,
            memory_mode: "disabled".to_string(),
            name: String::new(),
            settings_json: String::new(),
            git_info: None,
            projection_started: None,
            conversation: None,
            subagent: None,
        };
        let request = RequestRow {
            request_id: "request-1".to_string(),
            content: "Continue".to_string(),
            status: "completed".to_string(),
            lifecycle_state: "completed".to_string(),
            failure_reason: String::new(),
            created_at: None,
            metadata: r#"{"codex_shim":{}}"#.to_string(),
            execution_origin: "interactive".to_string(),
        };
        let compactions = vec![CompactionRow {
            request_id: request.request_id.clone(),
            call_id: "compact-1".to_string(),
            call_state: "completed".to_string(),
            call_seq: 1,
        }];
        let mut items = Vec::new();
        append_request_items(
            &record,
            &mut items,
            &request,
            None,
            Vec::new(),
            &compactions,
            &BTreeMap::new(),
        );

        assert!(items.iter().any(|item| matches!(
            item,
            codex::ThreadItem::ContextCompaction { id } if id == "compact-1"
        )));
    }

    #[test]
    fn project_message_turns_renders_structured_persisted_messages() {
        let turns = project_message_turns(vec![
            MessageRow {
                sequence: 1,
                role: "user".to_string(),
                content: r#"{"role":"user","content":[{"type":"text","text":"Hello from stored user JSON."}]}"#
                    .to_string(),
            },
            MessageRow {
                sequence: 2,
                role: "assistant".to_string(),
                content: r#"{"role":"assistant","id":null,"content":[{"text":"Hello from stored assistant JSON."}]}"#
                    .to_string(),
            },
        ]);

        assert_eq!(turns.len(), 1);
        let turn = &turns[0];
        assert!(turn.items.iter().any(|item| matches!(
            item,
            codex::ThreadItem::UserMessage { content, .. }
                if content.iter().any(|input| matches!(
                    input,
                    codex::UserInput::Text { text, .. }
                        if text == "Hello from stored user JSON."
                ))
        )));
        assert!(turn.items.iter().any(|item| matches!(
            item,
            codex::ThreadItem::AgentMessage { text, phase, .. }
                if text == "Hello from stored assistant JSON."
                    && *phase == Some(MessagePhase::FinalAnswer)
        )));
    }
}

use super::*;

#[derive(Debug)]
pub(super) struct TurnCapture {
    pub(super) text: String,
    pub(super) turn: codex::Turn,
    pub(super) started_tools: Vec<String>,
    pub(super) completed_tool_ids: Vec<String>,
    pub(super) completed_tools: Vec<String>,
    pub(super) completed_collab_items: Vec<CompletedCollabItem>,
    pub(super) turn_completed_tool_ids: Vec<String>,
    pub(super) event_order: Vec<TurnStreamEvent>,
    pub(super) token_usage: Option<codex::ThreadTokenUsage>,
}

#[derive(Debug)]
pub(super) struct CompletedCollabItem {
    pub(super) tool: codex::CollabAgentTool,
    pub(super) status: codex::CollabAgentToolCallStatus,
    pub(super) receiver_thread_ids: Vec<String>,
    pub(super) model: Option<String>,
    pub(super) child_status: Option<codex::CollabAgentStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TurnStreamEvent {
    AgentDelta,
    ToolStarted,
    ToolCompleted,
}

pub(super) async fn read_turn_to_completion(
    ws: &mut ShimWebSocket,
) -> Result<(String, codex::Turn)> {
    let capture = read_turn_capture(ws).await?;
    Ok((capture.text, capture.turn))
}

pub(super) async fn read_turn_capture(ws: &mut ShimWebSocket) -> Result<TurnCapture> {
    let mut text = String::new();
    let mut started_tools = Vec::new();
    let mut completed_tool_ids = Vec::new();
    let mut completed_tools = Vec::new();
    let mut completed_collab_items = Vec::new();
    let mut event_order = Vec::new();
    let mut token_usage = None;
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                match server_notification_from_jsonrpc(notification)? {
                    codex::ServerNotification::ThreadTokenUsageUpdated(update) => {
                        token_usage = Some(update.token_usage);
                    }
                    codex::ServerNotification::AgentMessageDelta(delta) => {
                        if !delta.delta.is_empty() {
                            event_order.push(TurnStreamEvent::AgentDelta);
                        }
                        text.push_str(&delta.delta);
                    }
                    codex::ServerNotification::ItemStarted(started) => match started.item {
                        codex::ThreadItem::McpToolCall { tool, .. } => {
                            event_order.push(TurnStreamEvent::ToolStarted);
                            started_tools.push(tool);
                        }
                        codex::ThreadItem::CommandExecution { command, .. } => {
                            event_order.push(TurnStreamEvent::ToolStarted);
                            started_tools.push(command);
                        }
                        _ => {}
                    },
                    codex::ServerNotification::ItemCompleted(completed) => match completed.item {
                        codex::ThreadItem::McpToolCall { id, tool, .. } => {
                            event_order.push(TurnStreamEvent::ToolCompleted);
                            completed_tool_ids.push(id);
                            completed_tools.push(tool);
                        }
                        codex::ThreadItem::CommandExecution { id, command, .. } => {
                            event_order.push(TurnStreamEvent::ToolCompleted);
                            completed_tool_ids.push(id);
                            completed_tools.push(command);
                        }
                        codex::ThreadItem::CollabAgentToolCall {
                            tool,
                            status,
                            receiver_thread_ids,
                            model,
                            agents_states,
                            ..
                        } => {
                            event_order.push(TurnStreamEvent::ToolCompleted);
                            let child_status = receiver_thread_ids
                                .first()
                                .and_then(|thread_id| agents_states.get(thread_id))
                                .map(|state| state.status.clone());
                            completed_collab_items.push(CompletedCollabItem {
                                tool,
                                status,
                                receiver_thread_ids,
                                model,
                                child_status,
                            });
                        }
                        _ => {}
                    },
                    codex::ServerNotification::TurnCompleted(completed) => {
                        let turn_completed_tool_ids = mcp_tool_ids(&completed.turn);
                        return Ok(TurnCapture {
                            text,
                            turn: completed.turn,
                            started_tools,
                            completed_tool_ids,
                            completed_tools,
                            completed_collab_items,
                            turn_completed_tool_ids,
                            event_order,
                            token_usage,
                        });
                    }
                    _ => {}
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

pub(super) fn assert_text_contains_all_case_insensitive(text: &str, label: &str, needles: &[&str]) {
    let lower = text.to_ascii_lowercase();
    for needle in needles {
        assert!(
            lower.contains(&needle.to_ascii_lowercase()),
            "{label} response did not contain {needle:?}:\n{text}"
        );
    }
}

pub(super) fn turn_had_tool_before_later_agent_text(capture: &TurnCapture) -> bool {
    let mut saw_tool = false;
    for event in &capture.event_order {
        match event {
            TurnStreamEvent::AgentDelta if saw_tool => return true,
            TurnStreamEvent::ToolStarted | TurnStreamEvent::ToolCompleted => saw_tool = true,
            TurnStreamEvent::AgentDelta => {}
        }
    }
    false
}

pub(super) fn turn_had_tool_after_final_agent_text(capture: &TurnCapture) -> bool {
    let Some(final_agent_index) = capture
        .event_order
        .iter()
        .rposition(|event| *event == TurnStreamEvent::AgentDelta)
    else {
        return false;
    };
    capture.event_order[final_agent_index + 1..]
        .iter()
        .any(|event| {
            matches!(
                event,
                TurnStreamEvent::ToolStarted | TurnStreamEvent::ToolCompleted
            )
        })
}

fn mcp_tool_ids(turn: &codex::Turn) -> Vec<String> {
    turn.items
        .iter()
        .filter_map(|item| match item {
            codex::ThreadItem::McpToolCall { id, .. } => Some(id.clone()),
            codex::ThreadItem::CommandExecution { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect()
}

pub(super) fn assert_turn_has_user_text(turn: &codex::Turn, expected: &str) {
    assert!(
        turn.items.iter().any(|item| match item {
            codex::ThreadItem::UserMessage { content, .. } => {
                content.iter().any(|input| match input {
                    codex::UserInput::Text { text, .. } => text.contains(expected),
                    _ => false,
                })
            }
            _ => false,
        }),
        "turn {} did not include user text {expected:?}: {:?}",
        turn.id,
        turn.items
    );
}

pub(super) fn assert_turn_has_agent_text(turn: &codex::Turn, expected: &str) {
    assert!(
        turn.items.iter().any(|item| match item {
            codex::ThreadItem::AgentMessage { text, .. } => text.contains(expected),
            _ => false,
        }),
        "turn {} did not include agent text {expected:?}: {:?}",
        turn.id,
        turn.items
    );
}

pub(super) async fn wait_for_request_metadata(
    graphql: &str,
    agent_did: &str,
    content: &str,
) -> Result<(String, String, Value)> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        agent_did: {{ _eq: "{}" }},
                        content: {{ _eq: "{}" }}
                    }},
                    order: {{ created_at: DESC }},
                    limit: 1
                ) {{
                    request_id
                    session_id
                    metadata
                }}
            }}"#,
            escape_graphql_string(agent_did),
            escape_graphql_string(content),
        );
        let response = graphql_query(graphql, &query).await?;
        if let Ok(row) = first_graphql_row(&response, "AgentRequest") {
            let request_id = row
                .get("request_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("AgentRequest row missing request_id: {row}"))?;
            let session_id = row
                .get("session_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("AgentRequest row missing session_id: {row}"))?;
            let metadata_raw = row
                .get("metadata")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("AgentRequest row missing metadata: {row}"))?;
            let metadata = serde_json::from_str::<Value>(metadata_raw)
                .with_context(|| format!("decoding AgentRequest metadata: {metadata_raw}"))?;
            return Ok((request_id.to_string(), session_id.to_string(), metadata));
        }

        if std::time::Instant::now() >= deadline {
            bail!("timed out waiting for AgentRequest metadata for {agent_did}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub(super) async fn read_jsonrpc(ws: &mut ShimWebSocket) -> Result<codex::JSONRPCMessage> {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(60), ws.next())
            .await
            .context("timed out waiting for Codex shim websocket message")?
            .ok_or_else(|| anyhow!("Codex shim websocket closed"))?
            .context("reading Codex shim websocket message")?;
        let text = match frame {
            WsMessage::Text(text) => text,
            WsMessage::Binary(bytes) => String::from_utf8(bytes.to_vec())
                .context("decoding binary websocket payload as UTF-8")?
                .into(),
            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
            WsMessage::Close(close) => bail!("Codex shim websocket closed: {close:?}"),
            WsMessage::Frame(_) => bail!("unexpected raw websocket frame"),
        };
        return serde_json::from_str(&text)
            .with_context(|| format!("decoding JSON-RPC message: {text}"));
    }
}

pub(super) fn server_notification_from_jsonrpc(
    notification: codex::JSONRPCNotification,
) -> Result<codex::ServerNotification> {
    serde_json::from_value(serde_json::to_value(notification)?)
        .context("decoding Codex server notification")
}

pub(super) async fn read_token_usage_notification(
    ws: &mut ShimWebSocket,
) -> Result<codex::ThreadTokenUsage> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::ThreadTokenUsageUpdated(update) =
                    server_notification_from_jsonrpc(notification)?
                {
                    return Ok(update.token_usage);
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

use super::*;
use crate::support::TestResult;

#[derive(Debug)]
pub(super) struct TurnCapture {
    pub(super) text: String,
    pub(super) completed_user_inputs: Vec<Vec<codex::UserInput>>,
    pub(super) turn: codex::Turn,
    pub(super) started_tools: Vec<String>,
    pub(super) completed_tool_ids: Vec<String>,
    pub(super) completed_tools: Vec<String>,
    pub(super) turn_completed_tool_ids: Vec<String>,
    pub(super) event_order: Vec<TurnStreamEvent>,
    pub(super) token_usage: Option<codex::ThreadTokenUsage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TurnStreamEvent {
    AgentDelta,
    ToolStarted,
    ToolCompleted,
}

pub(super) async fn read_turn_to_completion(
    ws: &mut ShimWebSocket,
) -> TestResult<(String, codex::Turn)> {
    let capture = read_turn_capture(ws).await?;
    Ok((capture.text, capture.turn))
}

pub(super) async fn read_turn_capture(ws: &mut ShimWebSocket) -> TestResult<TurnCapture> {
    let mut text = String::new();
    let mut completed_user_inputs = Vec::new();
    let mut started_tools = Vec::new();
    let mut completed_tool_ids = Vec::new();
    let mut completed_tools = Vec::new();
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
                        codex::ThreadItem::UserMessage { content, .. } => {
                            completed_user_inputs.push(content)
                        }
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
                        _ => {}
                    },
                    codex::ServerNotification::TurnCompleted(completed) => {
                        let turn_completed_tool_ids = mcp_tool_ids(&completed.turn);
                        return Ok(TurnCapture {
                            text,
                            completed_user_inputs,
                            turn: completed.turn,
                            started_tools,
                            completed_tool_ids,
                            completed_tools,
                            turn_completed_tool_ids,
                            event_order,
                            token_usage,
                        });
                    }
                    _ => {}
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                return Err(
                    format!("Codex shim emitted JSON-RPC error: {}", error.error.message).into(),
                );
            }
            codex::JSONRPCMessage::Request(request) => {
                return Err(
                    format!("Codex shim sent unexpected server request: {request:?}").into(),
                );
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

pub(super) async fn wait_for_request_input(
    graphql: &str,
    node_did: &str,
    content: &str,
) -> TestResult<(String, String, gents_protocol::request_input::RequestInput)> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{ {} }},
                    order: {{ created_at: DESC }},
                    limit: 1
                ) {{
                    request_id
                    session_id
                    input
                }}
            }}"#,
            gents::session::public_request_filter(&format!(
                r#"node_did: {{ _eq: "{}" }}, content: {{ _eq: "{}" }}"#,
                escape_graphql_string(node_did),
                escape_graphql_string(content),
            )),
        );
        let response = graphql_query(graphql, &query).await?;
        if let Ok(row) = first_graphql_row(&response, "AgentRequest") {
            let request_id = row
                .get("request_id")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("AgentRequest row missing request_id: {row}"))?;
            let session_id = row
                .get("session_id")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("AgentRequest row missing session_id: {row}"))?;
            let input = serde_json::from_value::<gents_protocol::request_input::RequestInput>(
                row.get("input")
                    .cloned()
                    .ok_or_else(|| format!("AgentRequest row missing input: {row}"))?,
            )
            .map_err(|error| format!("decoding canonical AgentRequest input: {error:#}"))?;
            return Ok((request_id.to_string(), session_id.to_string(), input));
        }

        if std::time::Instant::now() >= deadline {
            return Err(format!("timed out waiting for AgentRequest input for {node_did}").into());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub(super) async fn read_jsonrpc(ws: &mut ShimWebSocket) -> TestResult<codex::JSONRPCMessage> {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(60), ws.next())
            .await
            .map_err(|error| {
                format!("timed out waiting for Codex shim websocket message: {error:#}")
            })?
            .ok_or_else(|| "Codex shim websocket closed".to_string())?
            .map_err(|error| format!("reading Codex shim websocket message: {error:#}"))?;
        let text = match frame {
            WsMessage::Text(text) => text,
            WsMessage::Binary(bytes) => String::from_utf8(bytes.to_vec())
                .map_err(|error| format!("decoding binary websocket payload as UTF-8: {error:#}"))?
                .into(),
            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
            WsMessage::Close(close) => {
                return Err(format!("Codex shim websocket closed: {close:?}").into())
            }
            WsMessage::Frame(_) => return Err("unexpected raw websocket frame".to_string().into()),
        };
        return serde_json::from_str(&text)
            .map_err(|error| format!("decoding JSON-RPC message: {text}: {error:#}").into());
    }
}

pub(super) fn server_notification_from_jsonrpc(
    notification: codex::JSONRPCNotification,
) -> TestResult<codex::ServerNotification> {
    serde_json::from_value(serde_json::to_value(notification)?)
        .map_err(|error| format!("decoding Codex server notification: {error:#}").into())
}

pub(super) async fn read_token_usage_notification(
    ws: &mut ShimWebSocket,
) -> TestResult<codex::ThreadTokenUsage> {
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
                return Err(
                    format!("Codex shim emitted JSON-RPC error: {}", error.error.message).into(),
                );
            }
            codex::JSONRPCMessage::Request(request) => {
                return Err(
                    format!("Codex shim sent unexpected server request: {request:?}").into(),
                );
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

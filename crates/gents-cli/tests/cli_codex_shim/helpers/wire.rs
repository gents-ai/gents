use super::*;

pub(super) fn request_id(value: i64) -> codex::RequestId {
    codex::RequestId::Integer(value)
}

pub(super) async fn send_client_request(
    ws: &mut ShimWebSocket,
    request: codex::ClientRequest,
) -> Result<()> {
    let value = serde_json::to_value(request).context("serializing Codex client request")?;
    let request: codex::JSONRPCRequest =
        serde_json::from_value(value).context("building JSON-RPC request")?;
    write_jsonrpc(ws, codex::JSONRPCMessage::Request(request)).await
}

pub(super) async fn send_raw_client_request(
    ws: &mut ShimWebSocket,
    request_id: codex::RequestId,
    method: &str,
    params: Value,
) -> Result<()> {
    let request: codex::JSONRPCRequest = serde_json::from_value(json!({
        "id": request_id,
        "method": method,
        "params": params,
    }))
    .with_context(|| format!("building raw JSON-RPC request for {method}"))?;
    write_jsonrpc(ws, codex::JSONRPCMessage::Request(request)).await
}

pub(super) async fn send_client_notification(
    ws: &mut ShimWebSocket,
    notification: codex::ClientNotification,
) -> Result<()> {
    let value =
        serde_json::to_value(notification).context("serializing Codex client notification")?;
    let notification: codex::JSONRPCNotification =
        serde_json::from_value(value).context("building JSON-RPC notification")?;
    write_jsonrpc(ws, codex::JSONRPCMessage::Notification(notification)).await
}

async fn write_jsonrpc(ws: &mut ShimWebSocket, message: codex::JSONRPCMessage) -> Result<()> {
    let text = serde_json::to_string(&message).context("encoding JSON-RPC message")?;
    ws.send(WsMessage::Text(text.into()))
        .await
        .context("sending JSON-RPC websocket message")
}

pub(super) async fn read_typed_response<T>(
    ws: &mut ShimWebSocket,
    expected_id: codex::RequestId,
) -> Result<T>
where
    T: DeserializeOwned,
{
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Response(response) if response.id == expected_id => {
                return serde_json::from_value(response.result)
                    .context("decoding typed Codex response");
            }
            codex::JSONRPCMessage::Error(error) if error.id == expected_id => {
                bail!(
                    "Codex shim returned error for request {}: {}",
                    expected_id,
                    error.error.message
                );
            }
            codex::JSONRPCMessage::Notification(_) => {}
            other => {
                bail!("unexpected JSON-RPC message while waiting for {expected_id}: {other:?}")
            }
        }
    }
}

pub(super) async fn read_error_response(
    ws: &mut ShimWebSocket,
    expected_id: codex::RequestId,
) -> Result<codex::JSONRPCErrorError> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Error(error) if error.id == expected_id => {
                return Ok(error.error);
            }
            codex::JSONRPCMessage::Response(response) if response.id == expected_id => {
                bail!("expected JSON-RPC error for {expected_id}, got response {response:?}");
            }
            codex::JSONRPCMessage::Notification(_) => {}
            other => {
                bail!(
                    "unexpected JSON-RPC message while waiting for error {expected_id}: {other:?}"
                )
            }
        }
    }
}

pub(super) async fn read_turn_started(
    ws: &mut ShimWebSocket,
) -> Result<codex::TurnStartedNotification> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::TurnStarted(started) =
                    server_notification_from_jsonrpc(notification)?
                {
                    return Ok(started);
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

pub(super) async fn read_thread_status_changed(
    ws: &mut ShimWebSocket,
    expected_thread_id: &str,
) -> Result<codex::ThreadStatus> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::ThreadStatusChanged(changed) =
                    server_notification_from_jsonrpc(notification)?
                {
                    if changed.thread_id == expected_thread_id {
                        return Ok(changed.status);
                    }
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

pub(super) async fn read_background_command_started(
    ws: &mut ShimWebSocket,
    expected_tool_call_key: &str,
) -> Result<String> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::ItemStarted(started) =
                    server_notification_from_jsonrpc(notification)?
                {
                    if let codex::ThreadItem::CommandExecution { id, process_id, .. } = started.item
                    {
                        if id == expected_tool_call_key
                            && process_id.as_deref() == Some(expected_tool_call_key)
                        {
                            return Ok(id);
                        }
                    }
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

pub(super) async fn read_collab_agent_status(
    ws: &mut ShimWebSocket,
    expected_tool_call_key: &str,
    child_thread_id: &str,
) -> Result<(codex::CollabAgentStatus, Option<String>, bool)> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::ItemCompleted(completed) =
                    server_notification_from_jsonrpc(notification)?
                {
                    if let codex::ThreadItem::CollabAgentToolCall {
                        id,
                        model,
                        reasoning_effort,
                        agents_states,
                        ..
                    } = completed.item
                    {
                        if id == expected_tool_call_key {
                            let status = agents_states
                                .get(child_thread_id)
                                .map(|state| state.status.clone())
                                .with_context(|| {
                                    format!(
                                        "collab item {id} missing child state for {child_thread_id}"
                                    )
                                })?;
                            return Ok((status, model, reasoning_effort.is_none()));
                        }
                    }
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

pub(super) async fn read_mcp_tool_completion(
    ws: &mut ShimWebSocket,
    expected_tool_call_key: &str,
    expected_completed_at_ms: i64,
) -> Result<()> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::ItemCompleted(completed) =
                    server_notification_from_jsonrpc(notification)?
                {
                    if let codex::ThreadItem::McpToolCall {
                        id,
                        server,
                        tool,
                        status,
                        duration_ms,
                        ..
                    } = completed.item
                    {
                        if id == expected_tool_call_key {
                            assert_eq!(status, codex::McpToolCallStatus::Completed);
                            assert_eq!(server, "runtime-subagents");
                            assert_eq!(tool, "spawn");
                            assert_eq!(duration_ms, Some(23));
                            assert_eq!(completed.completed_at_ms, expected_completed_at_ms);
                            return Ok(());
                        }
                    }
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

pub(super) async fn read_child_agent_and_reasoning_deltas(
    ws: &mut ShimWebSocket,
    child_thread_id: &str,
    child_turn_id: &str,
) -> Result<(String, String, i64)> {
    let mut agent_delta = None;
    let mut reasoning_delta = None;
    let mut reasoning_started_at_ms = None;
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                match server_notification_from_jsonrpc(notification)? {
                    codex::ServerNotification::ItemStarted(started)
                        if started.thread_id == child_thread_id
                            && started.turn_id == child_turn_id
                            && matches!(started.item, codex::ThreadItem::Reasoning { .. }) =>
                    {
                        reasoning_started_at_ms = Some(started.started_at_ms);
                    }
                    codex::ServerNotification::AgentMessageDelta(delta)
                        if delta.thread_id == child_thread_id && delta.turn_id == child_turn_id =>
                    {
                        agent_delta = Some(delta.delta);
                    }
                    codex::ServerNotification::ReasoningTextDelta(delta)
                        if delta.thread_id == child_thread_id && delta.turn_id == child_turn_id =>
                    {
                        assert_eq!(delta.content_index, 0);
                        reasoning_delta = Some(delta.delta);
                    }
                    _ => {}
                }
                if reasoning_started_at_ms.is_some()
                    && agent_delta.is_some()
                    && reasoning_delta.is_some()
                {
                    return Ok((
                        agent_delta.take().expect("checked agent delta"),
                        reasoning_delta.take().expect("checked reasoning delta"),
                        reasoning_started_at_ms.expect("checked reasoning start"),
                    ));
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

pub(super) async fn read_child_reasoning_delta(
    ws: &mut ShimWebSocket,
    child_thread_id: &str,
    child_turn_id: &str,
) -> Result<String> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::ReasoningTextDelta(delta) =
                    server_notification_from_jsonrpc(notification)?
                {
                    if delta.thread_id == child_thread_id && delta.turn_id == child_turn_id {
                        assert_eq!(delta.content_index, 0);
                        return Ok(delta.delta);
                    }
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

pub(super) async fn read_child_reasoning_completion(
    ws: &mut ShimWebSocket,
    child_thread_id: &str,
    child_turn_id: &str,
) -> Result<(String, i64)> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::ItemCompleted(completed) =
                    server_notification_from_jsonrpc(notification)?
                {
                    if completed.thread_id == child_thread_id && completed.turn_id == child_turn_id
                    {
                        if let codex::ThreadItem::Reasoning {
                            content, summary, ..
                        } = completed.item
                        {
                            assert!(summary.is_empty());
                            return Ok((content.concat(), completed.completed_at_ms));
                        }
                    }
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

pub(super) async fn read_terminal_child_without_reasoning_replay(
    ws: &mut ShimWebSocket,
    child_thread_id: &str,
    child_turn_id: &str,
    expected_tool_call_key: &str,
) -> Result<(codex::CollabAgentStatus, codex::ThreadStatus, i64)> {
    let mut child_status = None;
    let mut thread_status = None;
    let mut agent_completed_at_ms = None;
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                match server_notification_from_jsonrpc(notification)? {
                    codex::ServerNotification::ReasoningTextDelta(delta)
                        if delta.thread_id == child_thread_id && delta.turn_id == child_turn_id =>
                    {
                        bail!(
                            "terminal durable reasoning replayed after reset-tail completion: {}",
                            delta.delta
                        );
                    }
                    codex::ServerNotification::ItemStarted(started)
                        if started.thread_id == child_thread_id
                            && started.turn_id == child_turn_id
                            && matches!(started.item, codex::ThreadItem::Reasoning { .. }) =>
                    {
                        bail!(
                            "terminal durable reasoning opened a duplicate item after reset-tail"
                        );
                    }
                    codex::ServerNotification::ItemCompleted(completed) => match completed.item {
                        codex::ThreadItem::Reasoning { .. }
                            if completed.thread_id == child_thread_id
                                && completed.turn_id == child_turn_id =>
                        {
                            bail!(
                                "terminal durable reasoning completed a duplicate item after reset-tail"
                            );
                        }
                        codex::ThreadItem::AgentMessage { .. }
                            if completed.thread_id == child_thread_id
                                && completed.turn_id == child_turn_id =>
                        {
                            agent_completed_at_ms = Some(completed.completed_at_ms);
                        }
                        codex::ThreadItem::CollabAgentToolCall {
                            id, agents_states, ..
                        } if id == expected_tool_call_key => {
                            child_status = agents_states
                                .get(child_thread_id)
                                .map(|state| state.status.clone());
                        }
                        _ => {}
                    },
                    codex::ServerNotification::ThreadStatusChanged(changed)
                        if changed.thread_id == child_thread_id =>
                    {
                        thread_status = Some(changed.status);
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

        // A CollabAgentToolCall update can still carry the child's
        // intermediate Running status after its thread has gone Idle; keep
        // reading until the collab projection reports a settled (non-Running)
        // status instead of asserting the first snapshot observed.
        let child_settled = child_status
            .as_ref()
            .is_some_and(|status| *status != codex::CollabAgentStatus::Running);
        if child_settled && thread_status.is_some() && agent_completed_at_ms.is_some() {
            return Ok((
                child_status.take().expect("checked child status"),
                thread_status.take().expect("checked thread status"),
                agent_completed_at_ms
                    .take()
                    .expect("checked agent completion timestamp"),
            ));
        }
    }
}

pub(super) async fn read_interrupt_response_and_completed_turn(
    ws: &mut ShimWebSocket,
    expected_id: codex::RequestId,
) -> Result<codex::Turn> {
    let mut saw_interrupt_response = false;
    let mut completed_turn = None;
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Response(response) if response.id == expected_id => {
                let _: codex::TurnInterruptResponse = serde_json::from_value(response.result)
                    .context("decoding interrupt response")?;
                saw_interrupt_response = true;
            }
            codex::JSONRPCMessage::Error(error) if error.id == expected_id => {
                bail!(
                    "Codex shim returned error for interrupt {}: {}",
                    expected_id,
                    error.error.message
                );
            }
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::TurnCompleted(completed) =
                    server_notification_from_jsonrpc(notification)?
                {
                    completed_turn = Some(completed.turn);
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(response) => {
                bail!(
                    "unexpected JSON-RPC response while waiting for interrupt {expected_id}: {response:?}"
                );
            }
        }

        if saw_interrupt_response {
            if let Some(turn) = completed_turn.take() {
                return Ok(turn);
            }
        }
    }
}

pub(super) async fn read_fuzzy_file_search_update(
    ws: &mut ShimWebSocket,
) -> Result<codex::FuzzyFileSearchSessionUpdatedNotification> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::FuzzyFileSearchSessionUpdated(update) =
                    server_notification_from_jsonrpc(notification)?
                {
                    return Ok(update);
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

pub(super) async fn read_fuzzy_file_search_completed(
    ws: &mut ShimWebSocket,
) -> Result<codex::FuzzyFileSearchSessionCompletedNotification> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::FuzzyFileSearchSessionCompleted(completed) =
                    server_notification_from_jsonrpc(notification)?
                {
                    return Ok(completed);
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

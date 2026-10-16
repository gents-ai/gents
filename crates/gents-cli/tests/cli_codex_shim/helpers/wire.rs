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

pub(super) async fn read_fuzzy_file_search_exchange(
    ws: &mut ShimWebSocket,
    expected_id: codex::RequestId,
    session_id: &str,
    query: &str,
) -> Result<codex::FuzzyFileSearchSessionUpdatedNotification> {
    let mut exchange = FuzzySearchExchange::default();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            exchange.observe(read_jsonrpc(ws).await?, &expected_id, session_id, query)?;
            if exchange.response && exchange.completed && exchange.update.is_some() {
                return Ok(exchange.update.take().unwrap());
            }
        }
    })
    .await
    .with_context(|| {
        format!(
            "fuzzy search exchange did not finish: response={}, update={}, completed={}",
            exchange.response,
            exchange.update.is_some(),
            exchange.completed
        )
    })?
}

#[derive(Default)]
struct FuzzySearchExchange {
    response: bool,
    update: Option<codex::FuzzyFileSearchSessionUpdatedNotification>,
    completed: bool,
}

impl FuzzySearchExchange {
    fn observe(
        &mut self,
        message: codex::JSONRPCMessage,
        expected_id: &codex::RequestId,
        session_id: &str,
        query: &str,
    ) -> Result<()> {
        match message {
            codex::JSONRPCMessage::Response(response) if &response.id == expected_id => {
                let _: codex::FuzzyFileSearchSessionUpdateResponse =
                    serde_json::from_value(response.result)?;
                self.response = true;
            }
            codex::JSONRPCMessage::Notification(notification) => {
                match server_notification_from_jsonrpc(notification)? {
                    codex::ServerNotification::FuzzyFileSearchSessionUpdated(update)
                        if update.session_id == session_id && update.query == query =>
                    {
                        self.update = Some(update);
                    }
                    codex::ServerNotification::FuzzyFileSearchSessionCompleted(completed)
                        if completed.session_id == session_id =>
                    {
                        self.completed = true;
                    }
                    _ => {}
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            other => bail!("unexpected fuzzy search exchange message: {other:?}"),
        }
        Ok(())
    }
}

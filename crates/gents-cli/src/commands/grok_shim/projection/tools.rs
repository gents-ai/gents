use rapidhash::RapidHashMap;
use std::{char::REPLACEMENT_CHARACTER, sync::Arc};

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents::config_client::ConfigAccess;
use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use gents::tools::call::lifecycle::{load_tool_call_presentation, CanonicalToolCallPresentation};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::nonempty;

mod background;

pub(crate) const JSONRPC_METHOD_NOT_SUPPORTED: i64 = -32601;

pub(crate) const PAGER_WAIT_FOR_EXIT_MESSAGE: &str = "pager does not handle WaitForTerminalExit";

pub(super) const TOOL_META_KEY: &str = "x.ai/tool";

pub(super) const TOOL_META_VERSION: u64 = 1;

pub(super) const TOOL_META_KIND_ACTIVE_AGENT_MESSAGE: &str = "ActiveAgentMessage";

pub(super) const SEND_ACTIVE_AGENT_MESSAGE_TITLE: &str = "send_subagent_message";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolCallKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    Other,
}

impl ToolCallKind {
    pub(super) fn wire_name(self) -> &'static str {
        match self {
            ToolCallKind::Read => "read",
            ToolCallKind::Edit => "edit",
            ToolCallKind::Delete => "delete",
            ToolCallKind::Move => "move",
            ToolCallKind::Search => "search",
            ToolCallKind::Execute => "execute",
            ToolCallKind::Think => "think",
            ToolCallKind::Fetch => "fetch",
            ToolCallKind::Other => "other",
        }
    }

    pub(super) fn from_tool_name(tool_name: &str) -> Self {
        match tool_name {
            "read_file" | gents::toolset::AGENT_SESSION_READ_TOOL_NAME => ToolCallKind::Read,
            "edit_file" | "write_file" | "apply_patch" | "create_file" => ToolCallKind::Edit,
            "delete_file" | "remove_file" => ToolCallKind::Delete,
            "move_file" | "rename_file" => ToolCallKind::Move,
            "grep"
            | "glob"
            | "list_files"
            | "search"
            | gents::toolset::AGENT_SESSION_LIST_TOOL_NAME => ToolCallKind::Search,
            "bash"
            | "bash_unrestricted"
            | "shell"
            | "execute"
            | "execute_command"
            | "run_command"
            | "run_terminal_command"
            | "run_terminal_cmd"
            | "terminal" => ToolCallKind::Execute,
            "think" | "reasoning" => ToolCallKind::Think,
            "fetch" | "web_fetch" | "web_search" => ToolCallKind::Fetch,
            _ => ToolCallKind::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl ToolCallStatus {
    pub(super) fn wire_name(self) -> &'static str {
        match self {
            ToolCallStatus::Pending => "pending",
            ToolCallStatus::InProgress => "in_progress",
            ToolCallStatus::Completed => "completed",
            ToolCallStatus::Failed => "failed",
        }
    }

    pub(super) fn is_completed(self) -> bool {
        matches!(self, ToolCallStatus::Completed | ToolCallStatus::Failed)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct ToolCallRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    tool_call_key: String,
    #[serde(default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    selected_tool_name: Option<String>,
    #[serde(default)]
    tool_failure_class: Option<String>,
    #[serde(default)]
    await_mode: Option<String>,
    #[serde(default)]
    message_sequence: Option<i64>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
}

const TOOL_CALL_FIELDS: &str = r#"
    node_did requester_did session_id request_id request_doc_id
    _docID
    tool_call_key
    tool_call_id
    tool_name
    status
    lifecycle_state
    selected_tool_name
    tool_failure_class
    await_mode
    message_sequence
    started_at
    completed_at
"#;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ToolProjection {
    pub updates: Vec<ToolUpdate>,
    pub chronology: Vec<Option<i64>>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum ToolUpdate {
    ToolCall(ToolCallUpdate),
    ToolCallUpdate(ToolCallFieldsUpdate),
    AvailableCommands(AvailableCommandsUpdate),
    BackgroundTask(background::BackgroundTaskUpdate),
}

impl ToolUpdate {
    #[cfg(test)]
    pub fn session_update_kind(&self) -> &'static str {
        match self {
            ToolUpdate::ToolCall(_) => "tool_call",
            ToolUpdate::ToolCallUpdate(_) => "tool_call_update",
            ToolUpdate::AvailableCommands(_) => "available_commands_update",
            ToolUpdate::BackgroundTask(update) => update.kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ToolCallUpdate {
    pub tool_call_id: String,
    pub title: String,
    pub kind: ToolCallKind,
    pub status: ToolCallStatus,
    pub content: Vec<Value>,
    pub raw_input: Option<Value>,
    pub raw_output: Option<Value>,
    pub meta: Option<Value>,
}

impl ToolCallUpdate {
    #[cfg(test)]
    pub fn is_active_agent_message(&self) -> bool {
        is_active_agent_message_meta(self.meta.as_ref())
            || self.title == SEND_ACTIVE_AGENT_MESSAGE_TITLE
    }

    pub fn to_payload(&self) -> Value {
        let mut payload = json!({
            "sessionUpdate": "tool_call",
            "toolCallId": self.tool_call_id,
            "title": self.title,
            "kind": self.kind.wire_name(),
            "status": self.status.wire_name(),
        });
        let object = payload
            .as_object_mut()
            .expect("tool_call payload is a JSON object");
        if !self.content.is_empty() {
            object.insert("content".to_string(), Value::Array(self.content.clone()));
        }
        if let Some(raw_input) = self.raw_input.as_ref() {
            object.insert("rawInput".to_string(), raw_input.clone());
        }
        if let Some(raw_output) = self.raw_output.as_ref() {
            object.insert("rawOutput".to_string(), raw_output.clone());
        }
        if let Some(meta) = self.meta.as_ref() {
            object.insert("_meta".to_string(), meta.clone());
        }
        payload
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ToolCallFieldsUpdate {
    pub tool_call_id: String,
    pub fields: Value,
}

impl ToolCallFieldsUpdate {
    #[cfg(test)]
    pub fn to_payload(&self) -> Value {
        tool_call_update_payload(&self.tool_call_id, &self.fields)
    }
}

pub(super) fn tool_call_update_payload(tool_call_id: &str, fields: &Value) -> Value {
    let mut payload = json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": tool_call_id,
    });
    if let (Some(payload), Some(fields)) = (payload.as_object_mut(), fields.as_object()) {
        payload.extend(fields.clone());
    }
    payload
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct AvailableCommandsUpdate {
    pub tools: Vec<String>,
}

impl AvailableCommandsUpdate {
    pub fn to_payload(&self) -> Value {
        json!({
            "sessionUpdate": "available_commands_update",
            "availableCommands": [],
            "_meta": {
                "tools": self.tools,
            },
        })
    }
}

pub(super) fn suppressed_tool_family(tool_name: &str) -> Option<&'static str> {
    match tool_name {
        "todo" | "todos" => Some("todo"),
        "bg-plumbing" | "background_plumbing" => Some("bg-plumbing"),
        "goal" | "goals" => Some("goal"),
        "scheduler" => Some("scheduler"),
        "workflow" | "workflows" => Some("workflow"),
        _ => None,
    }
}

pub(super) fn is_task_family(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "task" | "Task" | gents::toolset::AGENT_NEW_TOOL_NAME
    )
}

pub(super) fn is_active_agent_message_meta(meta: Option<&Value>) -> bool {
    let Some(meta) = meta else {
        return false;
    };
    let Some(tool_meta) = meta.get(TOOL_META_KEY) else {
        return false;
    };
    let Some(object) = tool_meta.as_object() else {
        return false;
    };
    object.get("version").and_then(Value::as_u64) == Some(TOOL_META_VERSION)
        && object.get("kind").and_then(Value::as_str) == Some(TOOL_META_KIND_ACTIVE_AGENT_MESSAGE)
}

pub(super) async fn project_tools(
    node: &Arc<EmbeddedNode>,
    request: &gents_protocol::row::AgentRequestRow,
) -> Result<ToolProjection> {
    let node_did = request
        .node_did
        .as_deref()
        .context("tool request node missing")?;
    let session_id = request
        .session_id
        .as_deref()
        .context("tool request session missing")?;
    let physical = request
        .doc_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .context("tool request physical identity missing")?;
    let tool_response = graphql_with_transaction_retry(
        node,
        &tool_calls_query(request)?,
        "grok shim tool call query",
    )
    .await?;
    let values = tool_response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(Value::as_array)
        .context("missing tool call rows")?;
    let mut identities = rapidhash::RapidHashSet::default();
    for value in values {
        anyhow::ensure!(
            value["request_doc_id"].as_str() == Some(physical)
                && value["request_id"].as_str() == Some(request.request_id.as_str())
                && value["node_did"].as_str() == Some(node_did)
                && value["session_id"].as_str() == Some(session_id)
                && value.get("requester_did") == Some(&json!(request.requester_did)),
            "tool row has wrong request scope"
        );
        let id = value["_docID"]
            .as_str()
            .filter(|id| !id.trim().is_empty())
            .context("tool row lacks physical identity")?;
        anyhow::ensure!(identities.insert(id), "duplicate tool physical identity");
    }
    let rows = decode_tool_call_rows(&tool_response)?;
    let access = ConfigAccess::local(node.clone());
    let mut presentations = rapidhash::RapidHashMap::default();
    for row in &rows {
        let presentation = load_tool_call_presentation(
            &access,
            &row.doc_id,
            node_did,
            session_id,
            request.requester_did.as_deref(),
        )
        .await
        .with_context(|| format!("canonical tool presentation for {}", row.doc_id))?;
        presentations.insert(row.doc_id.clone(), presentation);
    }

    let projection = project_tool_rows(&rows, &presentations);
    Ok(projection)
}

fn tool_call_row_sort_key(row: &ToolCallRow) -> (i64, String) {
    (
        row.message_sequence.unwrap_or(i64::MAX),
        row.stable_identity().to_string(),
    )
}

fn sort_tool_call_rows(rows: &mut [ToolCallRow]) {
    rows.sort_by_key(tool_call_row_sort_key);
}

pub(super) fn project_tool_rows(
    rows: &[ToolCallRow],
    presentations: &RapidHashMap<String, CanonicalToolCallPresentation>,
) -> ToolProjection {
    let mut updates: Vec<ToolUpdate> = Vec::new();
    let mut chronology: Vec<Option<i64>> = Vec::new();

    let mut ordered = rows.to_vec();
    sort_tool_call_rows(&mut ordered);

    let mut live_windows: Vec<String> = Vec::new();

    for row in &ordered {
        let Some(tool_call_id) = row.tool_call_key_tool_call_id() else {
            continue;
        };
        let presentation = presentations.get(&row.doc_id);
        let tool_name = row
            .tool_name
            .as_deref()
            .and_then(nonempty)
            .unwrap_or_default();
        let args = presentation.map_or("", |p| p.arguments.as_str());
        let durable_result = presentation.and_then(|p| p.result.as_deref());
        let result_text: &str = if durable_result.is_none() {
            if let Some(window) = live_output_window(presentation, row) {
                live_windows.push(window);
                live_windows.last().expect("just pushed").as_str()
            } else {
                ""
            }
        } else {
            durable_result.unwrap_or_default()
        };
        let meta = tool_meta_envelope(tool_name, args, row.await_mode.as_deref());

        if suppressed_tool_family(tool_name).is_some() {
            continue;
        }

        let status = observed_status(row);
        let kind = ToolCallKind::from_tool_name(tool_name);
        let title = tool_title(row, &kind, presentations);
        let content = tool_content(result_text);
        let mut raw_input = if tool_name == gents::toolset::AGENT_MESSAGE_TOOL_NAME {
            agent_message_raw_input(args)
        } else {
            raw_input_value(args, meta.as_ref())
        };
        let background = background::project(row, &tool_call_id, result_text, args);
        if background.is_some() {
            if let Some(input) = raw_input.as_mut().and_then(Value::as_object_mut) {
                input.insert("is_background".to_string(), Value::Bool(true));
            }
        }
        let raw_output = if background.is_some() && !result_text.is_empty() {
            Some(background::raw_output(result_text))
        } else if durable_result.is_some() {
            Some(raw_output_value(result_text).unwrap_or_else(|| json!({ "output": "" })))
        } else {
            raw_output_value(result_text)
        };

        updates.push(ToolUpdate::ToolCall(ToolCallUpdate {
            tool_call_id: tool_call_id.clone(),
            title,
            kind,
            status,
            content,
            raw_input,
            raw_output: raw_output.clone(),
            meta: meta.clone(),
        }));
        chronology.push(row.message_sequence);

        if let Some((started, _)) = &background {
            updates.push(ToolUpdate::BackgroundTask(started.clone()));
            chronology.push(row.message_sequence);
            if let Some(raw) = raw_output {
                updates.push(ToolUpdate::BackgroundTask(background::BackgroundTaskUpdate {
                    method: "session/update", kind: "tool_call_update", key: tool_call_id.clone(),
                    payload: json!({"sessionUpdate":"tool_call_update","toolCallId":tool_call_id,"rawOutput":raw}),
                    output_start: serde_json::from_str::<Value>(result_text).ok()
                        .and_then(|value| value["_gents_output_start"].as_u64())
                        .or_else(|| durable_result.is_some().then_some(0)),
                }));
                chronology.push(row.message_sequence);
            }
        }

        if status.is_completed() {
            updates.push(ToolUpdate::ToolCallUpdate(ToolCallFieldsUpdate {
                tool_call_id,
                fields: json!({
                    "status": status.wire_name(),
                }),
            }));
            chronology.push(row.message_sequence);
        }
        if let Some((_, Some(completed))) = background {
            updates.push(ToolUpdate::BackgroundTask(completed));
            chronology.push(row.message_sequence);
        }
    }

    if !updates.is_empty() {
        updates.push(ToolUpdate::AvailableCommands(AvailableCommandsUpdate {
            tools: available_commands(rows),
        }));
        chronology.push(None);
    }

    ToolProjection {
        updates,
        chronology,
    }
}

impl ToolCallRow {
    fn tool_call_key_tool_call_id(&self) -> Option<String> {
        if let Some(id) = self.tool_call_id.as_deref().and_then(nonempty) {
            return Some(id.to_string());
        }
        nonempty(&self.tool_call_key).map(|key| match key.split_once(':') {
            Some((_, tail)) if !tail.is_empty() => tail.to_string(),
            _ => key.to_string(),
        })
    }

    fn stable_identity(&self) -> &str {
        self.tool_call_id
            .as_deref()
            .and_then(nonempty)
            .or_else(|| nonempty(&self.tool_call_key))
            .or_else(|| nonempty(&self.doc_id))
            .unwrap_or("")
    }
}

fn live_output_window(
    presentation: Option<&CanonicalToolCallPresentation>,
    row: &ToolCallRow,
) -> Option<String> {
    let preview = presentation.and_then(|p| p.live_output.as_deref())?;
    if preview.trim().is_empty() {
        return None;
    }
    if observed_status(row).is_completed() {
        return Some(preview.to_string());
    }
    let mut decoded = preview.to_string();
    if decoded.ends_with(REPLACEMENT_CHARACTER) {
        decoded.pop();
    }
    if decoded.trim().is_empty() {
        return None;
    }
    Some(decoded)
}

fn observed_status(row: &ToolCallRow) -> ToolCallStatus {
    if row
        .tool_failure_class
        .as_deref()
        .and_then(nonempty)
        .is_some()
    {
        return ToolCallStatus::Failed;
    }
    if let Some(lifecycle) = row.lifecycle_state.as_deref().and_then(nonempty) {
        return match lifecycle {
            "pending" => ToolCallStatus::Pending,
            "running" => ToolCallStatus::InProgress,
            "completed" => ToolCallStatus::Completed,
            "failed" | "timedOut" | "cancelled" => ToolCallStatus::Failed,
            _ => ToolCallStatus::InProgress,
        };
    }
    match row.status.as_deref().and_then(nonempty) {
        Some(status) => match status.to_ascii_lowercase().as_str() {
            "cancelled" | "dead" | "error" | "failed" | "failure" | "timedout" => {
                ToolCallStatus::Failed
            }
            "completed" | "complete" | "success" | "succeeded" => ToolCallStatus::Completed,
            "pending" => ToolCallStatus::Pending,
            _ => ToolCallStatus::InProgress,
        },
        None => ToolCallStatus::InProgress,
    }
}

fn tool_title(
    row: &ToolCallRow,
    kind: &ToolCallKind,
    presentations: &RapidHashMap<String, CanonicalToolCallPresentation>,
) -> String {
    let tool_name = row
        .tool_name
        .as_deref()
        .and_then(nonempty)
        .unwrap_or_default();
    if is_active_agent_message_meta(
        tool_meta_from_args(
            presentations
                .get(&row.doc_id)
                .map_or("", |p| p.arguments.as_str()),
        )
        .as_ref(),
    ) || tool_name == SEND_ACTIVE_AGENT_MESSAGE_TITLE
        || tool_name == gents::toolset::AGENT_MESSAGE_TOOL_NAME
    {
        return SEND_ACTIVE_AGENT_MESSAGE_TITLE.to_string();
    }
    if matches!(kind, ToolCallKind::Execute) {
        if let Some(command) = shell_command_from_args(
            presentations
                .get(&row.doc_id)
                .map_or("", |p| p.arguments.as_str()),
        ) {
            return command;
        }
    }
    if let Some(selected) = row.selected_tool_name.as_deref().and_then(nonempty) {
        return selected.to_string();
    }
    if tool_name.is_empty() {
        "tool".to_string()
    } else {
        tool_name.to_string()
    }
}

fn shell_command_from_args(args: &str) -> Option<String> {
    let object = serde_json::from_str::<Value>(args).ok()?;
    object
        .get("command")
        .and_then(Value::as_str)
        .and_then(nonempty)
        .map(ToOwned::to_owned)
}

fn tool_content(result_text: &str) -> Vec<Value> {
    let trimmed = result_text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    vec![json!({
        "type": "content",
        "content": {"type": "text", "text": trimmed},
    })]
}

fn agent_message_raw_input(args: &str) -> Option<Value> {
    let object = serde_json::from_str::<Value>(args.trim()).ok()?;
    let mut shaped = Map::new();
    if let Some(session_id) = object.get("session_id") {
        shaped.insert("subagent_id".to_string(), session_id.clone());
    }
    if let Some(message) = object.get("message") {
        shaped.insert("text".to_string(), message.clone());
    }
    Some(Value::Object(shaped))
}

fn raw_input_value(args: &str, meta: Option<&Value>) -> Option<Value> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return None;
    }
    if is_active_agent_message_meta(meta) {
        if let Ok(object) = serde_json::from_str::<Value>(trimmed) {
            let mut shaped = Map::new();
            for key in ["subagent_id", "subagentId", "text"] {
                if let Some(value) = object.get(key) {
                    shaped.insert(key.to_string(), value.clone());
                }
            }
            return Some(Value::Object(shaped));
        }
    }
    if let Ok(object) = serde_json::from_str::<Value>(trimmed) {
        return Some(object);
    }
    Some(json!({ "input": trimmed }))
}

fn raw_output_value(result_text: &str) -> Option<Value> {
    let trimmed = result_text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(object) = serde_json::from_str::<Value>(trimmed) {
        return Some(object);
    }
    Some(json!({ "output": trimmed }))
}

fn tool_meta_envelope(tool_name: &str, args: &str, await_mode: Option<&str>) -> Option<Value> {
    let tool_meta = tool_meta_from_args(args);
    let background = matches!(await_mode, Some("background"));
    if is_task_family(tool_name) {
        let mut envelope = Map::new();
        if let Some(tool_meta) = tool_meta {
            envelope.insert(TOOL_META_KEY.to_string(), tool_meta);
        }
        envelope.insert("subagentBackground".to_string(), Value::Bool(background));
        return Some(Value::Object(envelope));
    }
    match (tool_meta, background) {
        (None, false) => None,
        (Some(tool_meta), false) => Some(json!({
            TOOL_META_KEY: tool_meta,
        })),
        (tool_meta, true) => {
            let mut envelope = Map::new();
            if let Some(tool_meta) = tool_meta {
                envelope.insert(TOOL_META_KEY.to_string(), tool_meta);
            }
            envelope.insert("subagentBackground".to_string(), Value::Bool(true));
            Some(Value::Object(envelope))
        }
    }
}

fn tool_meta_from_args(args: &str) -> Option<Value> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return None;
    }
    let object = serde_json::from_str::<Value>(trimmed).ok()?;
    object.get(TOOL_META_KEY).cloned()
}

fn available_commands(rows: &[ToolCallRow]) -> Vec<String> {
    let mut tools: Vec<String> = rows
        .iter()
        .filter_map(|row| row.tool_name.as_deref().and_then(nonempty))
        .filter(|name| suppressed_tool_family(name).is_none())
        .map(ToOwned::to_owned)
        .collect();
    tools.sort();
    tools.dedup();
    tools
}

pub(crate) fn wait_for_exit_not_supported() -> Value {
    json!({
        "code": JSONRPC_METHOD_NOT_SUPPORTED,
        "message": PAGER_WAIT_FOR_EXIT_MESSAGE,
    })
}

pub(crate) fn terminal_not_supported_error(method: &str) -> Value {
    json!({
        "code": JSONRPC_METHOD_NOT_SUPPORTED,
        "message": format!(
            "{method}: terminal methods are not supported by the Gents Grok shim; the \
             connection registers clientTerminal=false and shell work runs agent-side"
        ),
    })
}

pub(crate) fn handle_terminal_client_method(method: &str) -> std::result::Result<(), Value> {
    match method {
        "terminal/wait_for_exit" => Err(wait_for_exit_not_supported()),
        "terminal/create" | "terminal/output" | "terminal/kill" | "terminal/release" => {
            Err(terminal_not_supported_error(method))
        }
        other => Err(json!({
            "code": JSONRPC_METHOD_NOT_SUPPORTED,
            "message": format!(
                "{other}: unknown terminal method; the Grok shim supports only \
                 terminal/create, terminal/output, terminal/wait_for_exit, \
                 terminal/kill, and terminal/release as shaped unsupported results"
            ),
        })),
    }
}

fn tool_calls_query(request: &gents_protocol::row::AgentRequestRow) -> Result<String> {
    let scope = gents::session::session_scope_filter(
        request.node_did.as_deref().context("tool node missing")?,
        request
            .session_id
            .as_deref()
            .context("tool session missing")?,
        request.requester_did.as_deref(),
    );
    let doc = request
        .doc_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .context("tool request physical identity missing")?;
    Ok(format!(
        r#"{{ AgentToolCall(filter: {{ {scope}, request_id: {{_eq: "{}"}}, request_doc_id: {{_eq: "{}"}} }}, order: {{started_at: ASC}}) {{ {TOOL_CALL_FIELDS} }} }}"#,
        escape_graphql_string(&request.request_id),
        escape_graphql_string(doc)
    ))
}

fn decode_tool_call_rows(response: &defra_node::QueryResponse) -> Result<Vec<ToolCallRow>> {
    serde_json::from_value(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .cloned()
            .context("missing AgentToolCall rows")?,
    )
    .context("invalid AgentToolCall row")
}

#[cfg(test)]
mod tests {
    use gents::graphql::ensure_no_errors;
    fn request_fixture(
        id: &str,
        session: &str,
        physical: &str,
    ) -> gents_protocol::row::AgentRequestRow {
        serde_json::from_value(serde_json::json!({"_docID":physical,"request_id":id,"session_id":session,"node_did":"did:test:grok-shim","requester_did":"did:test:grok-shim"})).unwrap()
    }
    async fn seed_projection_request(
        node: &EmbeddedNode,
        id: &str,
        session: &str,
    ) -> gents_protocol::row::AgentRequestRow {
        let response = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{purpose: "normal", request_id: "{}", session_id: "{}", node_did: "did:test:grok-shim", requester_did: "did:test:grok-shim", agent_id: "test", content: "test", lifecycle_state: "pending"}}) {{ _docID }} }}"#, escape_graphql_string(id), escape_graphql_string(session))).await;
        ensure_no_errors(&response, "seed projection request").unwrap();
        let physical = gents_protocol::graphql::extract_mutation_doc_id(
            &json!({"data":response.data}),
            "AgentRequest",
        )
        .unwrap();
        request_fixture(id, session, &physical)
    }
    use super::*;

    fn tool_row(tool_name: &str, lifecycle_state: Option<&str>) -> ToolCallRow {
        ToolCallRow {
            doc_id: format!("doc-{tool_name}"),
            tool_call_key: format!("session-1:call-{tool_name}"),
            tool_call_id: Some(format!("call-{tool_name}")),
            tool_name: Some(tool_name.to_string()),
            status: None,
            lifecycle_state: lifecycle_state.map(ToOwned::to_owned),
            selected_tool_name: None,
            tool_failure_class: None,
            await_mode: None,
            message_sequence: None,
            started_at: None,
            completed_at: None,
        }
    }

    fn presentation(
        doc_id: &str,
        arguments: &str,
        result: Option<&str>,
    ) -> (String, CanonicalToolCallPresentation) {
        (
            doc_id.to_string(),
            CanonicalToolCallPresentation {
                arguments: arguments.to_string(),
                result: result.map(ToOwned::to_owned),
                live_output: None,
            },
        )
    }

    fn no_presentations() -> RapidHashMap<String, CanonicalToolCallPresentation> {
        RapidHashMap::default()
    }

    fn args_result(
        doc_id: &str,
        arguments: &str,
        result: Option<&str>,
    ) -> RapidHashMap<String, CanonicalToolCallPresentation> {
        RapidHashMap::from_iter([presentation(doc_id, arguments, result)])
    }

    fn live_presentation(
        doc_id: &str,
        arguments: &str,
        live_output: Option<&str>,
    ) -> RapidHashMap<String, CanonicalToolCallPresentation> {
        RapidHashMap::from_iter([CanonicalToolCallPresentation {
            arguments: arguments.to_string(),
            result: None,
            live_output: live_output.map(ToOwned::to_owned),
        }
        .keyed(doc_id)])
    }

    trait PresentationKeyed {
        fn keyed(self, doc_id: &str) -> (String, CanonicalToolCallPresentation);
    }
    impl PresentationKeyed for CanonicalToolCallPresentation {
        fn keyed(self, doc_id: &str) -> (String, CanonicalToolCallPresentation) {
            (doc_id.to_string(), self)
        }
    }

    #[test]
    fn native_task_output_follows_registration_and_keeps_large_snapshot() {
        let mut row = tool_row("bash", Some("running"));
        row.await_mode = Some("background".into());
        row.started_at = Some("2026-01-01T00:00:00Z".into());
        let text = format!("PREFIX{}SUFFIX", "0123456789".repeat(10_000));
        let presentations = RapidHashMap::from_iter([presentation(
            "doc-bash",
            r#"{"command":"produce output"}"#,
            Some(&json!({"stdout":text,"stdout_truncation":{"truncated":true}}).to_string()),
        )]);
        let projected = project_tool_rows(&[row], &presentations);
        let start = projected
            .updates
            .iter()
            .position(
                |u| matches!(u, ToolUpdate::BackgroundTask(v) if v.kind == "task_backgrounded"),
            )
            .unwrap();
        let output = projected
            .updates
            .iter()
            .position(
                |u| matches!(u, ToolUpdate::BackgroundTask(v) if v.kind == "tool_call_update"),
            )
            .unwrap();
        assert!(start < output);
        let ToolUpdate::BackgroundTask(update) = &projected.updates[output] else {
            unreachable!()
        };
        assert_eq!(update.payload["rawOutput"]["output_for_prompt"], text);
        assert_eq!(update.payload["rawOutput"]["truncated"], true);
    }

    #[test]
    fn completed_tool_call_payload_matches_grok_wire_shape() {
        let row = tool_row("bash", Some("completed"));
        let presentations = args_result(
            "doc-bash",
            r#"{"command":"echo gents-subprocess-probe"}"#,
            Some("gents-subprocess-probe"),
        );
        let projection = project_tool_rows(&[row], &presentations);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert_eq!(call.tool_call_id, "call-bash");
        assert_eq!(call.kind, ToolCallKind::Execute);
        assert_eq!(call.status, ToolCallStatus::Completed);
        assert_eq!(call.title, "echo gents-subprocess-probe");
        assert_eq!(
            call.raw_input
                .as_ref()
                .and_then(|input| input.get("command"))
                .and_then(Value::as_str),
            Some("echo gents-subprocess-probe")
        );
        assert_eq!(
            call.raw_output
                .as_ref()
                .and_then(|output| output.get("output"))
                .and_then(Value::as_str),
            Some("gents-subprocess-probe")
        );
        let payload = call.to_payload();
        assert_eq!(payload["sessionUpdate"], "tool_call");
        assert_eq!(payload["toolCallId"], "call-bash");
        assert_eq!(payload["kind"], "execute");
        assert_eq!(payload["status"], "completed");
        assert_eq!(payload["title"], "echo gents-subprocess-probe");
    }

    #[test]
    fn terminal_status_emits_tool_call_update() {
        let row = tool_row("bash", Some("completed"));
        let projection = project_tool_rows(&[row], &no_presentations());
        assert!(projection.updates.len() >= 2);
        let ToolUpdate::ToolCallUpdate(update) = &projection.updates[1] else {
            panic!("second update should be a tool_call_update");
        };
        assert_eq!(update.tool_call_id, "call-bash");
        assert_eq!(update.fields["status"], "completed");
        let payload = update.to_payload();
        assert_eq!(payload["sessionUpdate"], "tool_call_update");
        assert_eq!(payload["toolCallId"], "call-bash");
        assert_eq!(payload["status"], "completed");
        assert!(payload.get("fields").is_none());
    }

    #[test]
    fn running_tool_call_has_no_update_and_stays_in_progress() {
        let row = tool_row("bash", Some("running"));
        let projection = project_tool_rows(&[row], &no_presentations());
        assert_eq!(projection.updates.len(), 2);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert_eq!(call.status, ToolCallStatus::InProgress);
    }

    #[test]
    fn task_family_rows_render_ordinary_tool_calls_and_terminal_updates() {
        for name in ["task", "Task", "agent_new"] {
            let row = tool_row(name, Some("completed"));
            let presentations =
                args_result("doc-task", r#"{"description":"scout the repo"}"#, None);
            let projection = project_tool_rows(&[row], &presentations);
            let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
                panic!("{name} must render an ordinary tool_call");
            };
            assert_eq!(call.tool_call_id, format!("call-{name}"), "{name}");
            assert_eq!(call.title, name, "{name} keeps its durable tool name");
            assert_eq!(call.kind, ToolCallKind::Other, "{name}");
            assert_eq!(call.status, ToolCallStatus::Completed, "{name}");
            let ToolUpdate::ToolCallUpdate(update) = &projection.updates[1] else {
                panic!("{name} must emit a same-id terminal tool_call_update");
            };
            assert_eq!(update.tool_call_id, format!("call-{name}"), "{name}");
            assert_eq!(update.fields["status"], "completed", "{name}");
        }
    }

    #[test]
    fn task_family_rows_always_carry_explicit_background_meta_flag() {
        for name in ["task", "Task", "agent_new"] {
            for (await_mode, expected) in [
                (None, false),
                (Some("foreground"), false),
                (Some("Background"), false),
                (Some("background "), false),
                (Some("unknown-mode"), false),
                (Some("background"), true),
            ] {
                let mut row = tool_row(name, Some("running"));
                let presentations =
                    args_result("doc-task", r#"{"description":"scout the repo"}"#, None);
                row.await_mode = await_mode.map(ToOwned::to_owned);
                let projection = project_tool_rows(&[row], &presentations);
                let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
                    panic!("{name} must render a tool_call");
                };
                let meta = call
                    .meta
                    .as_ref()
                    .unwrap_or_else(|| panic!("{name} must always carry an object meta"));
                assert!(meta.is_object(), "{name} meta must be an object: {meta}");
                assert_eq!(
                    meta.get("subagentBackground"),
                    Some(&Value::Bool(expected)),
                    "{name} with await_mode {await_mode:?} must carry subagentBackground \
                     == {expected} explicitly"
                );
                let payload = call.to_payload();
                assert_eq!(
                    payload["_meta"]["subagentBackground"], expected,
                    "{name} payload must carry the explicit boolean"
                );
            }
        }
    }

    #[test]
    fn task_family_canonical_meta_envelope_survives_with_boolean_beside_it() {
        let tool_meta = json!({
            "version": TOOL_META_VERSION,
            "kind": "SubagentSpawn",
        });
        for (await_mode, expected) in [(Some("foreground"), false), (Some("background"), true)] {
            let presentations = args_result(
                "doc-Task",
                &format!(r#"{{"description":"scout the repo","{TOOL_META_KEY}":{tool_meta}}}"#),
                None,
            );
            let mut row = tool_row("Task", Some("running"));
            row.await_mode = await_mode.map(ToOwned::to_owned);
            let projection = project_tool_rows(&[row], &presentations);
            let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
                panic!("Task must render a tool_call");
            };
            let meta = call.meta.as_ref().expect("canonical meta row carries meta");
            assert_eq!(
                meta[TOOL_META_KEY], tool_meta,
                "the full x.ai/tool envelope must survive verbatim"
            );
            assert_eq!(meta["subagentBackground"], expected);
            let payload = call.to_payload();
            assert_eq!(payload["_meta"][TOOL_META_KEY], tool_meta);
            assert_eq!(payload["_meta"]["subagentBackground"], expected);
        }
    }

    #[test]
    fn background_await_mode_merges_background_true_into_meta() {
        let presentations = args_result("doc-Task", r#"{"description":"scout the repo"}"#, None);
        let mut row = tool_row("Task", Some("running"));
        row.await_mode = Some("background".to_string());
        let projection = project_tool_rows(&[row], &presentations);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("Task must render a tool_call");
        };
        let meta = call.meta.as_ref().expect("background row carries meta");
        assert_eq!(meta["subagentBackground"], true);
        let payload = call.to_payload();
        assert_eq!(payload["_meta"]["subagentBackground"], true);

        let mut foreground = tool_row("Task", Some("running"));
        foreground.await_mode = Some("foreground".to_string());
        let projection = project_tool_rows(&[foreground], &presentations);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("foreground Task must render a tool_call");
        };
        let meta = call
            .meta
            .as_ref()
            .expect("task-family rows always carry meta");
        assert_eq!(meta["subagentBackground"], false);
    }

    #[test]
    fn other_suppressed_families_render_nothing() {
        for name in ["todo", "bg-plumbing", "goal", "scheduler", "workflow"] {
            let row = tool_row(name, Some("running"));
            let projection = project_tool_rows(&[row], &no_presentations());
            assert!(
                projection
                    .updates
                    .iter()
                    .all(|update| update.session_update_kind() != "tool_call"),
                "{name} must not render"
            );
        }
    }

    #[test]
    fn active_agent_message_is_recognized_by_canonical_meta() {
        let tool_meta = json!({
            "version": TOOL_META_VERSION,
            "kind": TOOL_META_KIND_ACTIVE_AGENT_MESSAGE,
        });
        let envelope = json!({ TOOL_META_KEY: tool_meta });
        assert!(is_active_agent_message_meta(Some(&envelope)));
        assert!(!is_active_agent_message_meta(None));
        assert!(!is_active_agent_message_meta(Some(&json!({"version": 2}))));
        assert!(!is_active_agent_message_meta(Some(&json!({
            TOOL_META_KEY: {"version": 2},
        }))));
        let presentations = args_result(
            "doc-send_subagent_message",
            &format!(r#"{{"subagent_id":"sub-1","text":"hi","{TOOL_META_KEY}":{tool_meta}}}"#),
            None,
        );
        let row = tool_row("send_subagent_message", Some("running"));
        let projection = project_tool_rows(&[row], &presentations);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert!(call.is_active_agent_message());
        assert_eq!(call.title, SEND_ACTIVE_AGENT_MESSAGE_TITLE);
        assert_eq!(
            call.raw_input
                .as_ref()
                .and_then(|input| input.get("subagent_id"))
                .and_then(Value::as_str),
            Some("sub-1")
        );
    }

    #[test]
    fn agent_message_projects_as_an_active_agent_message() {
        let presentations = args_result(
            "doc-agent_message",
            r#"{"session_id":"child","message":"continue","interrupt":false}"#,
            None,
        );
        let row = tool_row("agent_message", Some("running"));
        let projection = project_tool_rows(&[row], &presentations);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert!(call.is_active_agent_message());
        assert_eq!(
            call.raw_input,
            Some(json!({"subagent_id": "child", "text": "continue"}))
        );
    }

    #[test]
    fn active_agent_message_is_recognized_by_title_fallback() {
        let presentations = args_result(
            "doc-send_subagent_message",
            r#"{"subagent_id":"sub-1","text":"hi"}"#,
            None,
        );
        let row = tool_row("send_subagent_message", Some("running"));
        let projection = project_tool_rows(&[row], &presentations);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert!(call.is_active_agent_message());
        assert_eq!(call.title, SEND_ACTIVE_AGENT_MESSAGE_TITLE);
    }

    #[test]
    fn available_commands_update_carries_visible_tools_meta() {
        let rows = vec![
            tool_row("bash", Some("completed")),
            tool_row("read_file", Some("completed")),
            tool_row("todo", Some("completed")),
        ];
        let projection = project_tool_rows(&rows, &no_presentations());
        let ToolUpdate::AvailableCommands(update) = projection
            .updates
            .iter()
            .find(|update| update.session_update_kind() == "available_commands_update")
            .cloned()
            .expect("available_commands_update should be emitted")
        else {
            unreachable!("already checked the kind");
        };
        assert_eq!(update.tools, vec!["bash", "read_file"]);
        let payload = update.to_payload();
        assert_eq!(payload["sessionUpdate"], "available_commands_update");
        assert_eq!(payload["availableCommands"], json!([]));
        assert_eq!(payload["_meta"]["tools"], json!(["bash", "read_file"]));
    }

    #[test]
    fn blank_lifecycle_falls_back_to_status_vocabulary() {
        let mut row = tool_row("bash", None);
        row.status = Some("completed".to_string());
        let projection = project_tool_rows(&[row], &no_presentations());
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert_eq!(call.status, ToolCallStatus::Completed);
    }

    #[test]
    fn failure_class_is_always_failed() {
        let mut row = tool_row("bash", Some("running"));
        row.tool_failure_class = Some("transport".to_string());
        let projection = project_tool_rows(&[row], &no_presentations());
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert_eq!(call.status, ToolCallStatus::Failed);
    }

    #[test]
    fn status_authority_holds_even_when_a_delivered_result_exists() {
        let mut row = tool_row("bash", Some("running"));
        row.status = Some("success".to_string());
        let presentations = args_result(
            "doc-bash",
            r#"{"command":"echo hi"}"#,
            Some("delivered output"),
        );
        let projection = project_tool_rows(&[row], &presentations);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert_eq!(call.status, ToolCallStatus::InProgress);
        assert_eq!(
            call.raw_output
                .as_ref()
                .and_then(|output| output.get("output"))
                .and_then(Value::as_str),
            Some("delivered output")
        );
    }

    #[test]
    fn presentation_join_is_by_physical_doc_id_not_tool_name() {
        let first = tool_row("bash", Some("completed"));
        let mut second = tool_row("bash", Some("completed"));
        second.doc_id = "doc-bash-second".to_string();
        second.tool_call_key = "session-1:call-bash-second".to_string();
        second.tool_call_id = Some("call-bash-second".to_string());

        let presentations = RapidHashMap::from_iter([
            presentation(
                "doc-bash-second",
                r#"{"command":"true"}"#,
                Some("second delivered output"),
            ),
            presentation(
                "doc-bash",
                r#"{"command":"true"}"#,
                Some("first delivered output"),
            ),
        ]);

        let projection = project_tool_rows(&[first, second], &presentations);
        let output_for = |tool_call_id: &str| {
            projection
                .updates
                .iter()
                .find_map(|update| match update {
                    ToolUpdate::ToolCall(call) if call.tool_call_id == tool_call_id => call
                        .raw_output
                        .as_ref()
                        .and_then(|output| output.get("output"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    _ => None,
                })
                .unwrap_or_default()
        };
        assert_eq!(output_for("call-bash"), "first delivered output");
        assert_eq!(output_for("call-bash-second"), "second delivered output");

        let mut orphan = tool_row("bash", Some("completed"));
        orphan.doc_id = "doc-bash-orphan".to_string();
        orphan.tool_call_key = "session-1:call-bash-orphan".to_string();
        orphan.tool_call_id = Some("call-bash-orphan".to_string());
        let projection = project_tool_rows(&[orphan], &presentations);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert_eq!(call.raw_output, None);
    }

    #[test]
    fn absent_result_omits_content_and_raw_output() {
        let row = tool_row("bash", Some("completed"));
        let projection = project_tool_rows(&[row], &no_presentations());
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        let payload = call.to_payload();
        assert!(payload.get("content").is_none());
        assert!(payload.get("rawOutput").is_none());
    }

    #[test]
    fn delivered_empty_result_projects_empty_raw_output() {
        let presentations = args_result("doc-bash", r#"{"command":"true"}"#, Some(""));
        let projection = project_tool_rows(&[tool_row("bash", Some("completed"))], &presentations);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        let payload = call.to_payload();
        assert!(payload.get("rawOutput").is_some());
    }

    #[test]
    fn tool_call_id_falls_back_to_key_without_session_prefix() {
        let mut row = tool_row("bash", Some("completed"));
        row.tool_call_id = None;
        let projection = project_tool_rows(&[row], &no_presentations());
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert_eq!(call.tool_call_id, "call-bash");
    }

    #[test]
    fn running_row_with_no_durable_result_streams_live_output_window() {
        let presentations = live_presentation(
            "doc-bash",
            r#"{"command":"echo gents-subprocess-probe"}"#,
            Some("streaming line one\n"),
        );
        let projection = project_tool_rows(&[tool_row("bash", Some("running"))], &presentations);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert_eq!(call.status, ToolCallStatus::InProgress);
        assert_eq!(
            call.raw_output
                .as_ref()
                .and_then(|output| output.get("output"))
                .and_then(Value::as_str),
            Some("streaming line one"),
            "the live window streams as rawOutput while the call runs (payload rendering trims, as for a durable result)"
        );
        assert_eq!(
            call.content
                .first()
                .and_then(|block| block.pointer("/content/text"))
                .and_then(Value::as_str),
            Some("streaming line one"),
            "the live window is also the call's content block"
        );
    }

    #[test]
    fn running_row_streams_incremental_growth_not_replay() {
        let row = tool_row("bash", Some("running"));
        let first = project_tool_rows(
            std::slice::from_ref(&row),
            &live_presentation("doc-bash", "{}", Some("first")),
        );
        let second = project_tool_rows(
            &[row],
            &live_presentation("doc-bash", "{}", Some("first second")),
        );
        let window = |projection: &ToolProjection| {
            projection
                .updates
                .iter()
                .find_map(|update| match update {
                    ToolUpdate::ToolCall(call) => call.raw_output.as_ref().map(|output| {
                        output
                            .get("output")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    }),
                    _ => None,
                })
                .expect("a running row with a live preview projects rawOutput")
        };
        assert_eq!(window(&first), "first");
        assert_eq!(window(&second), "first second");
    }

    #[test]
    fn running_row_holds_back_a_trailing_incomplete_utf8_sequence() {
        let row = tool_row("bash", Some("running"));
        let mut split_tail = "counter: 3 ".to_string();
        split_tail.push(REPLACEMENT_CHARACTER);
        let held = project_tool_rows(
            std::slice::from_ref(&row),
            &live_presentation("doc-bash", "{}", Some(&split_tail)),
        );
        let ToolUpdate::ToolCall(call) = &held.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert_eq!(
            call.raw_output
                .as_ref()
                .and_then(|output| output.get("output"))
                .and_then(Value::as_str),
            Some("counter: 3"),
            "incremental decoding streams the longest valid prefix and holds back only the pending sequence"
        );

        let complete = project_tool_rows(
            &[row],
            &live_presentation("doc-bash", "{}", Some("counter: 3 ✅")),
        );
        let ToolUpdate::ToolCall(call) = &complete.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert_eq!(
            call.raw_output
                .as_ref()
                .and_then(|output| output.get("output"))
                .and_then(Value::as_str),
            Some("counter: 3 ✅"),
            "the completed sequence streams whole once flushed"
        );
    }

    #[test]
    fn terminal_row_keeps_a_trailing_replacement_character() {
        let mut tail = "done ".to_string();
        tail.push(REPLACEMENT_CHARACTER);
        let presentations = live_presentation("doc-bash", r#"{"command":"true"}"#, Some(&tail));
        let projection = project_tool_rows(&[tool_row("bash", Some("completed"))], &presentations);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        let mut expected = "done ".to_string();
        expected.push(REPLACEMENT_CHARACTER);
        assert_eq!(
            call.raw_output
                .as_ref()
                .and_then(|output| output.get("output"))
                .and_then(Value::as_str),
            Some(expected.as_str()),
            "a terminal row keeps its genuine lossy decoding"
        );
    }

    #[test]
    fn durable_result_always_beats_the_live_window() {
        let presentations = RapidHashMap::from_iter([CanonicalToolCallPresentation {
            arguments: r#"{"command":"echo gents-subprocess-probe"}"#.to_string(),
            result: Some("gents-subprocess-probe".to_string()),
            live_output: Some("stale streaming preview".to_string()),
        }
        .keyed("doc-bash")]);
        let projection = project_tool_rows(&[tool_row("bash", Some("completed"))], &presentations);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert_eq!(
            call.raw_output
                .as_ref()
                .and_then(|output| output.get("output"))
                .and_then(Value::as_str),
            Some("gents-subprocess-probe"),
            "the durable result wins"
        );

        let presentations = RapidHashMap::from_iter([CanonicalToolCallPresentation {
            arguments: r#"{"command":"echo gents-subprocess-probe"}"#.to_string(),
            result: Some("delivered while running".to_string()),
            live_output: Some("stale streaming preview".to_string()),
        }
        .keyed("doc-bash")]);
        let projection = project_tool_rows(&[tool_row("bash", Some("running"))], &presentations);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert_eq!(call.status, ToolCallStatus::InProgress);
        assert_eq!(
            call.raw_output
                .as_ref()
                .and_then(|output| output.get("output"))
                .and_then(Value::as_str),
            Some("delivered while running"),
            "the delivered result beats the stale live preview on a running row too"
        );
    }

    #[test]
    fn queries_escape_interpolated_values() {
        let query = tool_calls_query(&request_fixture(
            r#"request-"quoted\"-id"#,
            "session",
            "physical",
        ))
        .unwrap();
        assert!(
            !query.contains(r#""request-"quoted\"-id""#),
            "raw value must not appear unescaped: {query}"
        );
        assert!(query.contains("AgentToolCall"));
        assert!(query.contains("_docID"), "{query}");
    }

    #[test]
    fn wait_for_exit_answers_the_pagers_exact_method_not_found_error() {
        let error = handle_terminal_client_method("terminal/wait_for_exit")
            .expect_err("wait_for_exit must be unsupported");
        assert_eq!(error["code"], JSONRPC_METHOD_NOT_SUPPORTED);
        assert_eq!(
            error["message"],
            "pager does not handle WaitForTerminalExit"
        );
        assert_eq!(error["message"], PAGER_WAIT_FOR_EXIT_MESSAGE);
        assert!(!error["message"]
            .as_str()
            .expect("message")
            .contains("is not supported by the Gents Grok shim"));
    }

    #[test]
    fn other_terminal_methods_answer_shaped_method_not_found() {
        for method in [
            "terminal/create",
            "terminal/output",
            "terminal/kill",
            "terminal/release",
        ] {
            let error = handle_terminal_client_method(method)
                .expect_err("terminal methods must be unsupported");
            assert_eq!(error["code"], JSONRPC_METHOD_NOT_SUPPORTED);
            let message = error["message"].as_str().expect("message");
            assert!(
                message.contains(&format!("{method}: ")),
                "{method} must be named in the shaped error: {message}"
            );
            assert!(
                message.contains("clientTerminal=false"),
                "{method} must explain the agent-side terminal routing: {message}"
            );
        }
        let unknown = handle_terminal_client_method("terminal/invent")
            .expect_err("unknown terminal methods are also rejected");
        assert_eq!(unknown["code"], JSONRPC_METHOD_NOT_SUPPORTED);
    }

    fn sequenced_tool_row(
        tool_call_id: &str,
        tool_name: &str,
        message_sequence: Option<i64>,
    ) -> ToolCallRow {
        let mut row = tool_row(tool_name, Some("completed"));
        row.tool_call_id = Some(tool_call_id.to_string());
        row.tool_call_key = format!("session-1:{tool_call_id}");
        row.message_sequence = message_sequence;
        row
    }

    fn projected_call_ids(projection: &ToolProjection) -> Vec<String> {
        projection
            .updates
            .iter()
            .filter_map(|update| match update {
                ToolUpdate::ToolCall(call) => Some(call.tool_call_id.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn decoded_rows_sort_by_sequence_then_stable_identity_regardless_of_input_order() {
        let rows = vec![
            sequenced_tool_row("call-z-late", "bash", Some(5)),
            sequenced_tool_row("call-a-first", "grep", Some(5)),
            sequenced_tool_row("call-m-later", "read_file", Some(9)),
            sequenced_tool_row("call-b-early", "edit", Some(2)),
            sequenced_tool_row("call-x-unsequenced", "fetch", None),
        ];
        let projection = project_tool_rows(&rows, &no_presentations());
        assert_eq!(
            projected_call_ids(&projection),
            vec![
                "call-b-early".to_string(),
                "call-a-first".to_string(),
                "call-z-late".to_string(),
                "call-m-later".to_string(),
                "call-x-unsequenced".to_string(),
            ],
            "wire order must follow (sequence, stable identity), not input order"
        );

        let mut reversed = rows.clone();
        reversed.reverse();
        assert_eq!(
            projected_call_ids(&project_tool_rows(&reversed, &no_presentations())),
            projected_call_ids(&projection),
            "reversing the decoded input order must not change the wire order"
        );
    }

    #[test]
    fn stable_identity_falls_back_to_key_then_doc_id() {
        let mut by_key = sequenced_tool_row("call-z", "bash", Some(1));
        by_key.tool_call_id = None;
        by_key.tool_call_key = "session-1:call-z".to_string();
        let mut by_doc = sequenced_tool_row("call-y", "bash", Some(1));
        by_doc.tool_call_id = None;
        by_doc.tool_call_key = String::new();
        by_doc.doc_id = "doc-call-y".to_string();
        assert_eq!(by_key.stable_identity(), "session-1:call-z");
        assert_eq!(by_doc.stable_identity(), "doc-call-y");
        assert_eq!(
            tool_call_row_sort_key(&by_key),
            (1, "session-1:call-z".to_string())
        );
        assert_eq!(
            tool_call_row_sort_key(&by_doc),
            (1, "doc-call-y".to_string())
        );
    }

    #[test]
    fn equal_sequence_rows_keep_base_and_update_semantics_and_result_join() {
        let mut call_a = sequenced_tool_row("call-a", "bash", Some(4));
        call_a.doc_id = "doc-a".to_string();
        let mut call_b = sequenced_tool_row("call-b", "bash", Some(4));
        call_b.doc_id = "doc-b".to_string();
        let presentations = RapidHashMap::from_iter([
            presentation("doc-b", r#"{"command":"true"}"#, Some("output for b")),
            presentation("doc-a", r#"{"command":"true"}"#, Some("output for a")),
        ]);
        let projection = project_tool_rows(&[call_b, call_a], &presentations);
        let pairs: Vec<(String, &str)> = projection
            .updates
            .iter()
            .filter_map(|update| match update {
                ToolUpdate::ToolCall(call) => Some((
                    call.tool_call_id.clone(),
                    call.raw_output.as_ref()?.get("output")?.as_str()?,
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("call-a".to_string(), "output for a"),
                ("call-b".to_string(), "output for b"),
            ],
            "each same-sequence call joins its own spilled result, in stable order"
        );
    }

    async fn embedded_node() -> (tempfile::TempDir, Arc<EmbeddedNode>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let node = Arc::new(
            crate::shared::test_support::node_builder()
                .data_path(dir.path().join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
                .build()
                .await
                .expect("embedded node"),
        );
        gents::store::schema::ensure_runtime_schemas(node.as_ref())
            .await
            .expect("runtime schemas");
        (dir, node)
    }

    use gents::graphql::single_mutation_document;
    use gents::session::canonical_rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
        OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload, ToolResultPart, TranscriptMessage,
    };

    async fn insert_segment(node: &EmbeddedNode, segment: &OutputSegment) -> String {
        let response = node
            .execute_request(
                gents::defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                    .with_variables(output_segment_create_variables(segment).unwrap()),
            )
            .await;
        assert!(
            !response.has_errors(),
            "segment seed: {:?}",
            response.errors
        );
        single_mutation_document(&response, "create_AgentOutputSegment")
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_string()
    }

    async fn insert_header(node: &EmbeddedNode, header: &TranscriptMessage) {
        let response = node
            .execute_request(
                gents::defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                    .with_variables(transcript_message_create_variables(header).unwrap()),
            )
            .await;
        assert!(
            !response.has_errors(),
            "message seed: {:?}",
            response.errors
        );
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "test seed helper takes the seeded row's fields"
    )]
    async fn seed_tool_delivery(
        node: &EmbeddedNode,
        request_doc_id: &str,
        request_id: &str,
        session_id: &str,
        tool_call_id: &str,
        tool_doc_id: &str,
        _sequence: u32,
        created_at: &str,
        result: &str,
        existing_prefix: Option<&str>,
    ) {
        let prefix = existing_prefix.unwrap_or_default();
        assert!(result.starts_with(prefix));
        let appended = &result[prefix.len()..];
        let ordinal = u32::from(existing_prefix.is_some());
        let segments = u32::from(existing_prefix.is_some()) + 1;
        let result_segment = OutputSegment {
            node_did: "did:test:grok-shim".into(),
            requester_did: Some("did:test:grok-shim".into()),
            session_id: session_id.into(),
            request_doc_id: request_doc_id.into(),
            source: OutputSource::ToolCall {
                tool_call_doc_id: tool_doc_id.into(),
            },
            writer: OutputWriter::ToolExecution {
                tool_call_doc_id: tool_doc_id.into(),
            },
            ordinal: Some(ordinal),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: appended.len() as u32,
                declaration: existing_prefix.is_none().then_some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::ToolOutput,
                }),
            }],
            payload: appended.into(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments,
                stream_bytes: vec![result.len() as u64],
            }),
            created_at: created_at.into(),
        };
        let result_close_doc_id = insert_segment(node, &result_segment).await;
        let sequence =
            crate::commands::grok_shim::test_fixtures::next_request_sequence(node, request_doc_id)
                .await;
        insert_header(
            node,
            &TranscriptMessage {
                message_key: format!("delivery:{request_id}:{tool_call_id}"),
                session_id: session_id.into(),
                node_did: "did:test:grok-shim".into(),
                requester_did: Some("did:test:grok-shim".into()),
                request_doc_id: Some(request_doc_id.into()),
                publication: MessagePublication::ToolDelivery {
                    tool_call_doc_id: tool_doc_id.into(),
                },
                outcome: OutputOutcome::Complete,
                sequence,
                role: MessageRole::User,
                native_id: None,
                blocks: vec![MessageBlock::ToolResult {
                    tool_call_doc_id: tool_doc_id.into(),
                    id: tool_call_id.into(),
                    call_id: None,
                    parts: vec![ToolResultPart::Text {
                        text: PresentedPayload {
                            output: PayloadRef {
                                close_doc_id: result_close_doc_id,
                                stream: 0,
                            },
                            presentation: PayloadPresentation::Full,
                        },
                    }],
                }],
                created_at: created_at.into(),
            },
        )
        .await;
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "test seed helper takes the seeded row's fields"
    )]
    async fn seed_canonical_tool_call(
        node: &Arc<EmbeddedNode>,
        request_doc_id: &str,
        request_id: &str,
        session_id: &str,
        _key_suffix: &str,
        tool_call_id: &str,
        tool_name: &str,
        message_sequence: u32,
        created_at: &str,
        arguments: &str,
        spawned_by: Option<&str>,
        result: Option<&str>,
    ) -> String {
        let request: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(serde_json::json!({
                "_docID": request_doc_id,
                "request_id": request_id,
                "node_did": "did:test:grok-shim",
                "requester_did": "did:test:grok-shim",
                "session_id": session_id
            }))
            .unwrap();
        return crate::commands::grok_shim::test_fixtures::seed_canonical_tool_call(
            node,
            &request,
            tool_call_id,
            tool_name,
            "running",
            arguments,
            result,
            spawned_by,
            Some(message_sequence),
            Some(created_at),
        )
        .await;
    }

    #[tokio::test]
    async fn embedded_exact_presentation_association_survives_query_deserialize_projection() {
        let (_dir, node) = embedded_node().await;
        let session_id = "s-embedded-spill";
        let request_id = "req-embedded-spill";
        let request = seed_projection_request(&node, request_id, session_id).await;
        let request_doc_id = request.doc_id.as_deref().unwrap().to_string();

        let doc_second = seed_canonical_tool_call(
            &node,
            &request_doc_id,
            request_id,
            session_id,
            "call-spill-second",
            "call-spill-second",
            "bash",
            5,
            "2026-08-31T22:46:45Z",
            r#"{"command":"true"}"#,
            None,
            Some("durable output for the second call"),
        )
        .await;
        let doc_first = seed_canonical_tool_call(
            &node,
            &request_doc_id,
            request_id,
            session_id,
            "call-spill-first",
            "call-spill-first",
            "bash",
            4,
            "2026-08-31T22:46:46Z",
            r#"{"command":"true"}"#,
            None,
            Some("durable output for the first call"),
        )
        .await;
        assert_ne!(doc_first, doc_second, "the two calls are distinct docs");

        let projection = project_tools(&node, &request)
            .await
            .expect("tool projection");
        let output_for = |tool_call_id: &str| -> Option<String> {
            projection.updates.iter().find_map(|update| match update {
                ToolUpdate::ToolCall(call) if call.tool_call_id == tool_call_id => call
                    .raw_output
                    .as_ref()
                    .and_then(|output| output.get("output"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                _ => None,
            })
        };
        assert_eq!(
            output_for("call-spill-first").as_deref(),
            Some("durable output for the first call"),
            "the first pager toolCallId must receive exactly its own durable result"
        );
        assert_eq!(
            output_for("call-spill-second").as_deref(),
            Some("durable output for the second call"),
            "the second pager toolCallId must receive exactly its own durable result"
        );
        let call_ids = projected_call_ids(&projection);
        assert_eq!(
            call_ids,
            vec![
                "call-spill-first".to_string(),
                "call-spill-second".to_string()
            ],
            "canonical message chronology, not creation order, owns emission; the pure projection regression separately pins equal-sequence identity ties"
        );
    }

    fn call_for<'a>(projection: &'a ToolProjection, tool_call_id: &str) -> &'a ToolCallUpdate {
        projection
            .updates
            .iter()
            .find_map(|update| match update {
                ToolUpdate::ToolCall(call) if call.tool_call_id == tool_call_id => Some(call),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no tool_call registration for {tool_call_id}"))
    }

    #[tokio::test]
    async fn embedded_task_family_meta_mapping_and_terminal_update_keep_identity() {
        let (_dir, node) = embedded_node().await;
        let session_id = "s-embedded-task";
        let request_id = "req-embedded-task";
        let request = seed_projection_request(&node, request_id, session_id).await;
        let request_doc_id = request.doc_id.as_deref().unwrap().to_string();

        let _doc_fg = seed_canonical_tool_call(
            &node,
            &request_doc_id,
            request_id,
            session_id,
            "call-task-fg",
            "call-task-fg",
            "task",
            1,
            "2026-08-31T23:00:01Z",
            r#"{"description":"foreground scout"}"#,
            None,
            None,
        )
        .await;
        let _doc_bg = seed_canonical_tool_call(
            &node,
            &request_doc_id,
            request_id,
            session_id,
            "call-task-bg",
            "call-task-bg",
            "Task",
            2,
            "2026-08-31T23:00:02Z",
            r#"{"description":"background scout","x.ai/tool":{"version":1,"kind":"SubagentSpawn"}}"#,
            None,
            None,
        )
        .await;
        let doc_absent = seed_canonical_tool_call(
            &node,
            &request_doc_id,
            request_id,
            session_id,
            "call-tasks-absent",
            "call-tasks-absent",
            "tasks",
            3,
            "2026-08-31T23:00:03Z",
            r#"{"description":"absent mode scout"}"#,
            None,
            None,
        )
        .await;
        let _doc_unknown = seed_canonical_tool_call(
            &node,
            &request_doc_id,
            request_id,
            session_id,
            "call-spawn-unknown",
            "call-spawn-unknown",
            "task",
            4,
            "2026-08-31T23:00:04Z",
            r#"{"description":"unknown mode scout"}"#,
            None,
            None,
        )
        .await;
        let await_modes = r#"mutation {
            fg: update_AgentToolCall(
                filter: { tool_call_id: { _eq: "call-task-fg" } },
                input: { await_mode: "foreground" }
            ) { _docID }
            bg: update_AgentToolCall(
                filter: { tool_call_id: { _eq: "call-task-bg" } },
                input: { await_mode: "background" }
            ) { _docID }
            unknown: update_AgentToolCall(
                filter: { tool_call_id: { _eq: "call-spawn-unknown" } },
                input: { await_mode: "detached" }
            ) { _docID }
        }"#;
        let response = node.execute(await_modes).await;
        assert!(
            !response.has_errors(),
            "await_mode stamp failed: {:?}",
            response.errors
        );

        let projection = project_tools(&node, &request)
            .await
            .expect("tool projection");

        assert_eq!(
            projected_call_ids(&projection),
            vec![
                "call-task-fg".to_string(),
                "call-task-bg".to_string(),
                "call-tasks-absent".to_string(),
                "call-spawn-unknown".to_string(),
            ],
            "task/spawn rows and ordinary unknown rows must render"
        );

        for (id, expected) in [
            ("call-task-fg", false),
            ("call-task-bg", true),
            ("call-spawn-unknown", false),
        ] {
            let call = call_for(&projection, id);
            let meta = call
                .meta
                .as_ref()
                .unwrap_or_else(|| panic!("{id} must always carry an object meta"));
            assert!(meta.is_object(), "{id} meta must be an object: {meta}");
            assert_eq!(
                meta.get("subagentBackground"),
                Some(&Value::Bool(expected)),
                "{id} must carry subagentBackground == {expected} explicitly"
            );
        }

        let plural = call_for(&projection, "call-tasks-absent");
        assert!(
            plural.meta.is_none(),
            "plural tasks is not a Grok task alias and must not receive task-only metadata"
        );

        let bg = call_for(&projection, "call-task-bg");
        let meta = bg.meta.as_ref().expect("background row carries meta");
        assert_eq!(
            meta.get(TOOL_META_KEY),
            Some(&json!({
                "version": TOOL_META_VERSION,
                "kind": "SubagentSpawn",
            })),
            "the full x.ai/tool envelope must survive the projection"
        );
        assert_eq!(meta["subagentBackground"], true);

        seed_tool_delivery(
            &node,
            &request_doc_id,
            request_id,
            session_id,
            "call-tasks-absent",
            &doc_absent,
            4,
            "2026-08-31T23:00:05Z",
            "scout finished",
            None,
        )
        .await;
        let escaped_id = escape_graphql_string("call-tasks-absent");
        let escaped_state = escape_graphql_string("completed");
        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ tool_call_id: {{ _eq: "{escaped_id}" }} }},
                    input: {{
                        lifecycle_state: "{escaped_state}"
                        status: "{escaped_state}"
                    }}
                ) {{ _docID }}
            }}"#
        );
        let response = node.execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "terminal update failed: {:?}",
            response.errors
        );

        let reprojection = project_tools(&node, &request)
            .await
            .expect("tool reprojection");
        let call = call_for(&reprojection, "call-tasks-absent");
        assert_eq!(call.status, ToolCallStatus::Completed);
        assert_eq!(
            call.raw_output
                .as_ref()
                .and_then(|output| output.get("output"))
                .and_then(Value::as_str),
            Some("scout finished")
        );
        assert!(
            call.meta.is_none(),
            "plural tasks remains an ordinary tool after terminal update"
        );
        let update = reprojection
            .updates
            .iter()
            .find_map(|update| match update {
                ToolUpdate::ToolCallUpdate(update)
                    if update.tool_call_id == "call-tasks-absent" =>
                {
                    Some(update)
                }
                _ => None,
            })
            .expect("terminal call emits a same-id tool_call_update");
        assert_eq!(update.fields["status"], "completed");
    }

    #[tokio::test]
    async fn embedded_live_output_window_streams_then_durable_result_replaces_it() {
        let (_dir, node) = embedded_node().await;
        let session_id = "s-embedded-live";
        let request_id = "req-embedded-live";
        let request = seed_projection_request(&node, request_id, session_id).await;
        let request_doc_id = request.doc_id.as_deref().unwrap().to_string();

        let parent_doc_id = seed_canonical_tool_call(
            &node,
            &request_doc_id,
            request_id,
            session_id,
            "call-parent",
            "call-parent",
            "spawn_process",
            1,
            "2026-08-31T23:10:01Z",
            r#"{"tool_name":"bash","args":{"command":"echo gents-subprocess-probe"}}"#,
            None,
            None,
        )
        .await;

        let child_doc_id = seed_canonical_tool_call(
            &node,
            &request_doc_id,
            request_id,
            session_id,
            "call-live",
            "call-live",
            "bash",
            1,
            "2026-08-31T23:10:02Z",
            r#"{"command":"echo gents-subprocess-probe"}"#,
            Some(&parent_doc_id),
            None,
        )
        .await;

        let live_segment = OutputSegment {
            node_did: "did:test:grok-shim".into(),
            requester_did: Some("did:test:grok-shim".into()),
            session_id: session_id.into(),
            request_doc_id: request_doc_id.clone(),
            source: OutputSource::ToolCall {
                tool_call_doc_id: child_doc_id.clone(),
            },
            writer: OutputWriter::ToolExecution {
                tool_call_doc_id: child_doc_id.clone(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: "gents-".len() as u32,
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::ToolOutput,
                }),
            }],
            payload: "gents-".into(),
            close: None,
            created_at: "2026-08-31T23:10:03Z".into(),
        };
        insert_segment(&node, &live_segment).await;

        let projection = project_tools(&node, &request)
            .await
            .expect("live projection");
        let live = call_for(&projection, "call-live");
        assert_eq!(live.status, ToolCallStatus::InProgress);
        assert_eq!(
            live.raw_output
                .as_ref()
                .and_then(|output| output.get("output"))
                .and_then(Value::as_str),
            Some("gents-"),
            "the live window streams through rawOutput while the call runs"
        );

        insert_segment(
            &node,
            &OutputSegment {
                node_did: "did:test:grok-shim".into(),
                requester_did: Some("did:test:grok-shim".into()),
                session_id: session_id.into(),
                request_doc_id: request_doc_id.clone(),
                source: OutputSource::ToolCall {
                    tool_call_doc_id: child_doc_id.clone(),
                },
                writer: OutputWriter::ToolExecution {
                    tool_call_doc_id: child_doc_id.clone(),
                },
                ordinal: Some(1),
                runs: vec![SegmentRun {
                    stream: 0,
                    bytes: "subprocess-probe".len() as u32,
                    declaration: None,
                }],
                payload: "subprocess-probe".into(),
                close: Some(SourceClose::Closed {
                    outcome: OutputOutcome::Complete,
                    segments: 2,
                    stream_bytes: vec!["gents-subprocess-probe".len() as u64],
                }),
                created_at: "2026-08-31T23:10:04Z".into(),
            },
        )
        .await;
        let escaped_id = escape_graphql_string("call-live");
        let escaped_state = escape_graphql_string("completed");
        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ tool_call_id: {{ _eq: "{escaped_id}" }} }},
                    input: {{
                        lifecycle_state: "{escaped_state}"
                        status: "{escaped_state}"
                    }}
                ) {{ _docID }}
            }}"#
        );
        let response = node.execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "terminal update failed: {:?}",
            response.errors
        );

        let reprojection = project_tools(&node, &request)
            .await
            .expect("terminal projection");
        let terminal = call_for(&reprojection, "call-live");
        assert_eq!(terminal.status, ToolCallStatus::Completed);
        assert_eq!(
            terminal
                .raw_output
                .as_ref()
                .and_then(|output| output.get("output"))
                .and_then(Value::as_str),
            Some("gents-subprocess-probe"),
            "the durable result replaces the live window at terminalization"
        );
    }
}

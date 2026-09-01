//! Grok shim tool projection.
//!
//! Durable `AgentToolCall` rows are the authoritative source for the Grok
//! pager's `tool_call` / `tool_call_update` `session/update` notifications.
//! This leaf projects those rows, request-id scoped and ordered by
//! `started_at`, into fresh Grok notification payloads: tracker updates
//! (`tool_call` then `tool_call_update` when the observed lifecycle status
//! changes), command titles/status/content, available-command updates, and
//! the execute-kind subprocess lifecycle the pager renders for shell work.
//!
//! Ordering rules mirrored from `xai-grok-pager/src/acp/tracker.rs`:
//!
//! - suppressed tool families (`todo`, `bg-plumbing`, `task`, `goal`,
//!   `scheduler`, `workflow`) are never rendered as scrollback blocks;
//! - however, family suppression is applied **after** blocking-wait
//!   registration. A `task` tool whose meta does not carry
//!   `subagentBackground: true` registers a blocking subagent wait instead
//!   of a scrollback block; the same is true for any spawn row that
//!   recorded a `child_request_id`. Suppression removes the *rendered*
//!   block, never the *wait*.
//! - `send_subagent_message` is recognized by canonical tool meta
//!   (`{"version": <TOOL_META_VERSION>, "kind": "ActiveAgentMessage"}`) or
//!   the title `send_subagent_message`, with rawInput
//!   `{"subagent_id", "text"}`;
//! - `available_commands_update` carries meta `{"tools": [...]}`;
//! - orphan `tool_call_update` values arriving before their `tool_call` are
//!   merged into the pending base by `toolCallId` on arrival.
//!
//! Terminal ACP client methods `terminal/create`, `terminal/output`,
//! `terminal/wait_for_exit`, `terminal/kill`, and `terminal/release` remain
//! explicit shaped unsupported results: the shim registers
//! `clientTerminal: false`, so shell work runs agent-side and reaches the
//! client purely as execute-kind tool_call events. The pager-style
//! not-supported error is reproduced verbatim. No permission document is
//! ever created by this leaf.
//!
//! All queries go through the in-process embedded node (`node.execute`) with
//! every interpolated value passed through `escape_graphql_string`; no HTTP
//! GraphQL helper is used. Projection is bounded and request-id-scoped: one
//! `AgentToolCall` query and one `AgentToolResult` query per request id,
//! with no graph walks beyond the rows of the request being projected.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;
use gents::graphql::{ensure_no_errors, escape_graphql_string};
use serde::Deserialize;
use serde_json::{json, Map, Value};

/// Wire name of the `session/update` notification carrying a tool update.
pub(super) const TOOL_UPDATE_METHOD: &str = "session/update";

/// Pager-style not-supported stub for terminal ACP client methods. The
/// reference pager answers `terminal/wait_for_exit` with exactly this
/// not-supported error; the shim reproduces it for every terminal method.
pub(super) const TERMINAL_NOT_SUPPORTED_MESSAGE: &str =
    "terminal is not supported by this client (pager)";

/// JSON-RPC error code the pager uses for a client method the client does
/// not implement (method not supported by the connection).
pub(super) const JSONRPC_METHOD_NOT_SUPPORTED: i64 = -32601;

/// Canonical tool meta key the pager recognizes for
/// `send_subagent_message`.
pub(super) const TOOL_META_KEY: &str = "x/grok tool meta";

/// Canonical tool meta version marker.
pub(super) const TOOL_META_VERSION: u64 = 1;

/// Canonical tool meta kind for an active agent message
/// (`send_subagent_message`).
pub(super) const TOOL_META_KIND_ACTIVE_AGENT_MESSAGE: &str = "ActiveAgentMessage";

/// Title the pager falls back to when recognizing
/// `send_subagent_message` without canonical meta.
pub(super) const SEND_SUBAGENT_MESSAGE_TITLE: &str = "send_subagent_message";

/// Grok pager tool-call kinds, mapped from durable tool names.
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
    /// The `kind` wire string for this tool call.
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

    /// Map a durable Gents tool name onto the pager kind vocabulary.
    pub(super) fn from_tool_name(tool_name: &str) -> Self {
        match tool_name {
            "read_file" => ToolCallKind::Read,
            "edit_file" | "write_file" | "apply_patch" | "create_file" => ToolCallKind::Edit,
            "delete_file" | "remove_file" => ToolCallKind::Delete,
            "move_file" | "rename_file" => ToolCallKind::Move,
            "grep" | "glob" | "list_files" | "search" => ToolCallKind::Search,
            "bash" | "shell" | "execute_command" | "run_command" => ToolCallKind::Execute,
            "think" | "reasoning" => ToolCallKind::Think,
            "fetch" | "web_fetch" | "web_search" => ToolCallKind::Fetch,
            _ => ToolCallKind::Other,
        }
    }
}

/// Grok pager tool-call statuses, mapped from the authoritative durable
/// lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl ToolCallStatus {
    /// The `status` wire string for this tool call.
    pub(super) fn wire_name(self) -> &'static str {
        match self {
            ToolCallStatus::Pending => "pending",
            ToolCallStatus::InProgress => "in_progress",
            ToolCallStatus::Completed => "completed",
            ToolCallStatus::Failed => "failed",
        }
    }

    /// True when the pager treats the tool call as settled
    /// (`Completed | Failed`); a settled call emits no further updates.
    pub(super) fn is_completed(self) -> bool {
        matches!(self, ToolCallStatus::Completed | ToolCallStatus::Failed)
    }
}

/// One durable `AgentToolCall` row scoped to the projected request.
#[derive(Clone, Debug, Deserialize)]
struct ToolCallRow {
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
    child_request_id: Option<String>,
    #[serde(default)]
    args: Option<String>,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    selected_tool_name: Option<String>,
    #[serde(default)]
    tool_failure_class: Option<String>,
}

/// One durable `AgentToolResult` conversation audit row for the projected
/// request, when the runtime wrote one. The schema keys the audit row by
/// `tool_call_doc_id` and carries `output_text`; oversized outputs spill
/// here from their `AgentToolCall`.
#[derive(Clone, Debug, Deserialize)]
struct ToolResultRow {
    tool_call_doc_id: String,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    output_text: Option<String>,
}

const TOOL_CALL_FIELDS: &str = r#"
    tool_call_key
    tool_call_id
    tool_name
    status
    lifecycle_state
    child_request_id
    args
    result
    selected_tool_name
    tool_failure_class
"#;

const TOOL_RESULT_FIELDS: &str = r#"
    tool_call_doc_id
    tool_name
    output_text
"#;

/// The full set of projection events for one request id, ordered so a client
/// can replay each tool call's lifecycle exactly once.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ToolProjection {
    /// Tracker-shaped `tool_call` / `tool_call_update` notifications in
    /// emission order.
    pub updates: Vec<ToolUpdate>,
    /// Blocking subagent waits keyed by the wait's `toolCallId`. These are
    /// the spawn rows the pager must block on (a suppressed-family `task`
    /// tool without `subagentBackground: true`, or any spawn row that
    /// recorded a `child_request_id`) — they register **before** family
    /// suppression drops the rendered block.
    pub subagent_waits: BTreeMap<String, SubagentWait>,
}

/// A blocking subagent wait registered by a spawn tool call. The pager
/// renders a blocking Subagent wait for these rows instead of a scrollback
/// block, and clears them on turn finalization.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SubagentWait {
    /// The `toolCallId` the pager keys the wait by.
    pub tool_call_id: String,
    /// The child `AgentRequest` id the spawn row recorded, when present.
    pub child_request_id: Option<String>,
    /// The durable tool name of the spawn row.
    pub tool_name: String,
}

/// A single projected tool update, already split by kind so the caller (the
/// projection engine) only needs to stamp `_meta` and wrap it in a
/// `session/update` notification.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ToolUpdate {
    /// A full `tool_call` tracker registration.
    ToolCall(ToolCallUpdate),
    /// A `tool_call_update` merging fields into the pending base by
    /// `toolCallId`.
    ToolCallUpdate(ToolCallFieldsUpdate),
    /// An `available_commands_update` carrying the visible tool list.
    AvailableCommands(AvailableCommandsUpdate),
}

impl ToolUpdate {
    /// The `sessionUpdate` discriminator for this update.
    pub fn session_update_kind(&self) -> &'static str {
        match self {
            ToolUpdate::ToolCall(_) => "tool_call",
            ToolUpdate::ToolCallUpdate(_) => "tool_call_update",
            ToolUpdate::AvailableCommands(_) => "available_commands_update",
        }
    }

    /// The `toolCallId` this update belongs to, when it carries one.
    pub fn tool_call_id(&self) -> Option<&str> {
        match self {
            ToolUpdate::ToolCall(update) => Some(&update.tool_call_id),
            ToolUpdate::ToolCallUpdate(update) => Some(&update.tool_call_id),
            ToolUpdate::AvailableCommands(_) => None,
        }
    }

    /// Render the Grok pager payload for this update. Field names match
    /// `xai-grok-pager/src/acp/tracker.rs` exactly.
    pub fn to_payload(&self) -> Value {
        match self {
            ToolUpdate::ToolCall(update) => update.to_payload(),
            ToolUpdate::ToolCallUpdate(update) => update.to_payload(),
            ToolUpdate::AvailableCommands(update) => update.to_payload(),
        }
    }
}

/// A full `tool_call` tracker registration payload.
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
    /// True when canonical tool meta recognizes this call as an active agent
    /// message (`send_subagent_message`).
    pub fn is_active_agent_message(&self) -> bool {
        is_active_agent_message_meta(self.meta.as_ref())
            || self.title == SEND_SUBAGENT_MESSAGE_TITLE
    }

    /// Render the `tool_call` payload. Optional absent objects
    /// (`rawInput`/`rawOutput`/`meta`/`content`) are omitted entirely rather
    /// than sent as nulls, matching the pager decoder.
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
            object.insert("meta".to_string(), meta.clone());
        }
        payload
    }
}

/// A `tool_call_update` payload: the changed fields merged into the pending
/// base by `toolCallId`.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ToolCallFieldsUpdate {
    pub tool_call_id: String,
    pub fields: Value,
}

impl ToolCallFieldsUpdate {
    pub fn to_payload(&self) -> Value {
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": self.tool_call_id,
            "fields": self.fields,
        })
    }
}

/// An `available_commands_update` payload.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct AvailableCommandsUpdate {
    pub tools: Vec<String>,
}

impl AvailableCommandsUpdate {
    pub fn to_payload(&self) -> Value {
        json!({
            "sessionUpdate": "available_commands_update",
            "meta": {
                "tools": self.tools,
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Suppressed tool families and canonical meta
// ---------------------------------------------------------------------------

/// The tool families the pager never renders as scrollback blocks. Family
/// suppression is applied *after* blocking-wait registration: a suppressed
/// `task` tool without `subagentBackground: true` still registers its
/// subagent wait.
pub(super) fn suppressed_tool_family(tool_name: &str) -> Option<&'static str> {
    match tool_name {
        "todo" | "todos" => Some("todo"),
        "bg-plumbing" | "background_plumbing" => Some("bg-plumbing"),
        "task" | "tasks" => Some("task"),
        "goal" | "goals" => Some("goal"),
        "scheduler" => Some("scheduler"),
        "workflow" | "workflows" => Some("workflow"),
        _ => None,
    }
}

/// True when canonical tool meta recognizes the call as an active agent
/// message (`send_subagent_message`): a JSON object carrying
/// `version == TOOL_META_VERSION` and `kind == ActiveAgentMessage`.
pub(super) fn is_active_agent_message_meta(meta: Option<&Value>) -> bool {
    let Some(meta) = meta else {
        return false;
    };
    let Some(object) = meta.as_object() else {
        return false;
    };
    object.get("version").and_then(Value::as_u64) == Some(TOOL_META_VERSION)
        && object.get("kind").and_then(Value::as_str)
            == Some(TOOL_META_KIND_ACTIVE_AGENT_MESSAGE)
}

/// True when the row's recorded meta carries `subagentBackground: true`.
fn is_subagent_background(meta: Option<&Value>) -> bool {
    meta.and_then(|meta| meta.get("subagentBackground"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// True when the row is a spawn row: a durable `child_request_id` was
/// recorded, or the tool name is a recognized spawn verb.
fn is_spawn_row(row: &ToolCallRow) -> bool {
    row.child_request_id
        .as_deref()
        .and_then(nonempty)
        .is_some()
        || matches!(
            row.tool_name.as_deref().and_then(nonempty),
            Some("spawn_subagent") | Some("launch_subagent") | Some("task")
        )
}

/// The blocking subagent wait a spawn row registers, when it should. A
/// `task` tool whose meta does not carry `subagentBackground: true`
/// registers a blocking wait; so does any spawn row that recorded a
/// `child_request_id`. Registration happens before family suppression so a
/// suppressed `task` tool still blocks.
fn subagent_wait_for(row: &ToolCallRow, tool_call_id: &str, meta: Option<&Value>) -> Option<SubagentWait> {
    if !is_spawn_row(row) {
        return None;
    }
    let tool_name = row
        .tool_name
        .as_deref()
        .and_then(nonempty)
        .unwrap_or_default()
        .to_string();
    // A background subagent (`subagentBackground: true`) is tracked by the
    // subagent projection as a spawned/progress/finished lifecycle instead
    // of a blocking wait, so it does not register here.
    if is_subagent_background(meta) {
        return None;
    }
    Some(SubagentWait {
        tool_call_id: tool_call_id.to_string(),
        child_request_id: row
            .child_request_id
            .as_deref()
            .and_then(nonempty)
            .map(ToOwned::to_owned),
        tool_name,
    })
}

// ---------------------------------------------------------------------------
// Projection entry point
// ---------------------------------------------------------------------------

/// Project the tool-call lifecycle for one request id.
///
/// Bounded and request-id-scoped: the query set is exactly
/// 1. one `AgentToolCall` query for the rows of this request id, and
/// 2. one `AgentToolResult` query for the same session id (the audit
///    collection is keyed by `tool_call_doc_id`/`session_id`, so the
///    in-memory cross-check below keeps the observation scoped to this
///    request's call rows).
///
/// The projection is read-only: it never replays the session, never
/// duplicates durable materialization, and never writes a document.
pub(super) async fn project_tools(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
) -> Result<ToolProjection> {
    let tool_response = node.execute(&tool_calls_query(request_id)).await;
    ensure_no_errors(&tool_response, "grok shim tool call query")?;
    let rows = decode_tool_call_rows(&tool_response);

    let result_response = node.execute(&tool_results_query(session_id)).await;
    ensure_no_errors(&result_response, "grok shim tool result query")?;
    let results = decode_tool_result_rows(&result_response);

    let projection = project_tool_rows(&rows, &results);
    Ok(projection)
}

/// Pure projection over decoded rows; unit-testable without a node.
pub(super) fn project_tool_rows(rows: &[ToolCallRow], results: &[ToolResultRow]) -> ToolProjection {
    let mut updates: Vec<ToolUpdate> = Vec::new();
    let mut subagent_waits: BTreeMap<String, SubagentWait> = BTreeMap::new();

    for row in rows {
        let Some(tool_call_id) = row.tool_call_key_tool_call_id() else {
            continue;
        };
        let tool_name = row
            .tool_name
            .as_deref()
            .and_then(nonempty)
            .unwrap_or_default();
        let args = row.args.as_deref().and_then(nonempty).unwrap_or("");
        let result_text = effective_result_text(row, results);
        let meta = tool_meta_from_args(args);
        // `subagentBackground` lives in the tool's recorded args object
        // (the Gents analogue of the pager's tool meta), not in the
        // canonical `x/grok tool meta` key.
        let args_object = serde_json::from_str::<Value>(args).ok();

        // 1. Blocking subagent waits register BEFORE family suppression. A
        //    canonical `task` tool (suppressed family) without
        //    `subagentBackground: true`, or any spawn row that recorded a
        //    `child_request_id`, must still appear in `subagent_waits` — the
        //    pager blocks on it instead of rendering a scrollback block.
        if let Some(wait) =
            subagent_wait_for(row, &tool_call_id, args_object.as_ref())
        {
            subagent_waits.insert(tool_call_id.clone(), wait);
        }

        // 2. Family suppression only drops the *rendered* block; the wait
        //    registered above survives.
        if suppressed_tool_family(tool_name).is_some() {
            continue;
        }

        let status = observed_status(row);
        let kind = ToolCallKind::from_tool_name(tool_name);
        let title = tool_title(row, &kind);
        let content = tool_content(result_text);
        let raw_input = raw_input_value(args, meta.as_ref());
        let raw_output = raw_output_value(result_text);

        updates.push(ToolUpdate::ToolCall(ToolCallUpdate {
            tool_call_id: tool_call_id.clone(),
            title,
            kind,
            status,
            content,
            raw_input,
            raw_output,
            meta: meta.clone(),
        }));

        // A terminal first observation still emits the base `tool_call`
        // (a fast call may first be observed already completed); a later
        // lifecycle change emits `tool_call_update`.
        if status.is_completed() {
            updates.push(ToolUpdate::ToolCallUpdate(ToolCallFieldsUpdate {
                tool_call_id,
                fields: json!({
                    "status": status.wire_name(),
                }),
            }));
        }
    }

    if !updates.is_empty() {
        updates.push(ToolUpdate::AvailableCommands(AvailableCommandsUpdate {
            tools: available_commands(&rows),
        }));
    }

    ToolProjection {
        updates,
        subagent_waits,
    }
}

impl ToolCallRow {
    /// The pager-visible `toolCallId`: the durable `tool_call_id` when
    /// recorded, otherwise the `tool_call_key` (which the runtime shapes as
    /// `<session_id>:<tool_call_id>`).
    fn tool_call_key_tool_call_id(&self) -> Option<String> {
        if let Some(id) = self.tool_call_id.as_deref().and_then(nonempty) {
            return Some(id.to_string());
        }
        nonempty(&self.tool_call_key).map(|key| {
            // Strip a `<session>:` prefix when the key carries one so the
            // pager sees the bare call id it correlates updates by.
            match key.split_once(':') {
                Some((_, tail)) if !tail.is_empty() => tail.to_string(),
                _ => key.to_string(),
            }
        })
    }
}

/// The effective result text for one tool row: the call row's own `result`
/// when present, otherwise the spilled `output_text` of the audit row
/// recorded for the same session and tool name (the audit collection is
/// conversation-scoped, so the match is narrowed by tool name and only used
/// as an output source, never as a status override).
fn effective_result_text<'a>(row: &'a ToolCallRow, results: &'a [ToolResultRow]) -> &'a str {
    if let Some(result) = row.result.as_deref().and_then(nonempty) {
        return result;
    }
    let tool_name = row.tool_name.as_deref().and_then(nonempty);
    results
        .iter()
        .find(|result| {
            tool_name.is_some_and(|name| result.tool_name.as_deref() == Some(name))
                && result
                    .output_text
                    .as_deref()
                    .and_then(nonempty)
                    .is_some()
        })
        .and_then(|result| result.output_text.as_deref())
        .and_then(nonempty)
        .unwrap_or("")
}

/// The authoritative observed lifecycle status of one tool row. The durable
/// `lifecycle_state` wins; a persisted failure class is always `failed`; a
/// blank row falls back to the legacy `status` vocabulary; anything else is
/// still in progress. (Mirrors the codex shim's `observed_tool_status`
/// without importing it.) The `AgentToolResult` audit collection is
/// conversation-scoped (`tool_call_doc_id`/`session_id`, no request id), so
/// it never overrides the call row's authoritative lifecycle.
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
            "pending" | "awaitingApproval" => ToolCallStatus::Pending,
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

/// The pager title for a tool call. `send_subagent_message` keeps its
/// recognized title; shell tools surface their command; other tools surface
/// their durable name.
fn tool_title(row: &ToolCallRow, kind: &ToolCallKind) -> String {
    let tool_name = row
        .tool_name
        .as_deref()
        .and_then(nonempty)
        .unwrap_or_default();
    if is_active_agent_message_meta(tool_meta_from_args(row.args.as_deref().unwrap_or("")).as_ref())
        || tool_name == SEND_SUBAGENT_MESSAGE_TITLE
    {
        return SEND_SUBAGENT_MESSAGE_TITLE.to_string();
    }
    if matches!(kind, ToolCallKind::Execute) {
        if let Some(command) = shell_command_from_args(row.args.as_deref().unwrap_or("")) {
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

/// Extract a shell command from JSON args (`command` key).
fn shell_command_from_args(args: &str) -> Option<String> {
    let object = serde_json::from_str::<Value>(args).ok()?;
    object
        .get("command")
        .and_then(Value::as_str)
        .and_then(nonempty)
        .map(ToOwned::to_owned)
}

/// The `content` blocks for a tool call, derived from the recorded result
/// text. Absent results produce no blocks (the field is omitted on the
/// wire).
fn tool_content(result_text: &str) -> Vec<Value> {
    let trimmed = result_text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    vec![json!({
        "type": "text",
        "text": trimmed,
    })]
}

/// The structured `rawInput` for a tool call. JSON args are passed through
/// as the object; `send_subagent_message` is shaped to
/// `{"subagent_id", "text"}`; non-JSON args are wrapped as a single
/// `input` string.
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

/// The structured `rawOutput` for a tool call. JSON results pass through;
/// shell results surface their recorded exit code when present; plain text
/// is wrapped as a single `output` string.
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

/// Decode the canonical tool meta recorded in the row's args, when present.
fn tool_meta_from_args(args: &str) -> Option<Value> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return None;
    }
    let object = serde_json::from_str::<Value>(trimmed).ok()?;
    object.get(TOOL_META_KEY).cloned()
}

/// The visible tool list for `available_commands_update`, derived from the
/// projected request's non-suppressed durable tool names, deduplicated and
/// ordered.
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

fn nonempty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

// ---------------------------------------------------------------------------
// Terminal ACP client method stubs
// ---------------------------------------------------------------------------

/// The pager-style not-supported JSON-RPC error value for a terminal ACP
/// client method. The pager answers `terminal/wait_for_exit` with
/// `wait_for_exit_not_supported("pager")`; the shim reproduces that shape
/// for every terminal method because it registers `clientTerminal: false`
/// and never routes terminal work to the client.
pub(super) fn terminal_not_supported_error(method: &str) -> Value {
    json!({
        "code": JSONRPC_METHOD_NOT_SUPPORTED,
        "message": format!("{method}: {TERMINAL_NOT_SUPPORTED_MESSAGE}"),
    })
}

/// Route one terminal ACP client method to its shaped unsupported result.
///
/// Returns `Err(not_supported_error)` for the five known terminal methods
/// (`terminal/create`, `terminal/output`, `terminal/wait_for_exit`,
/// `terminal/kill`, `terminal/release`) so the ACP service can surface the
/// pager-style not-supported error, and `Ok(())` never: the shim does not
/// implement a client terminal, does not synthesize terminal documents, and
/// does not create permission documents. Unknown methods are routed through
/// the caller's generic method-not-found handling.
pub(super) fn handle_terminal_client_method(method: &str) -> std::result::Result<(), Value> {
    match method {
        "terminal/create" | "terminal/output" | "terminal/wait_for_exit" | "terminal/kill"
        | "terminal/release" => Err(terminal_not_supported_error(method)),
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

// ---------------------------------------------------------------------------
// Queries and decoding
// ---------------------------------------------------------------------------

fn tool_calls_query(request_id: &str) -> String {
    format!(
        r#"{{
            AgentToolCall(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                order: {{ started_at: ASC }}
            ) {{ {TOOL_CALL_FIELDS} }}
        }}"#,
        request_id = escape_graphql_string(request_id),
    )
}

fn tool_results_query(session_id: &str) -> String {
    format!(
        r#"{{
            AgentToolResult(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ created_at: ASC }}
            ) {{ {TOOL_RESULT_FIELDS} }}
        }}"#,
        session_id = escape_graphql_string(session_id),
    )
}

fn decode_tool_call_rows(response: &defra_node::QueryResponse) -> Vec<ToolCallRow> {
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| match serde_json::from_value::<ToolCallRow>(row) {
            Ok(row) => Some(row),
            Err(error) => {
                tracing::debug!(
                    %error,
                    "grok shim skipped an undecodable AgentToolCall row"
                );
                None
            }
        })
        .collect()
}

fn decode_tool_result_rows(response: &defra_node::QueryResponse) -> Vec<ToolResultRow> {
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolResult"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| match serde_json::from_value::<ToolResultRow>(row) {
            Ok(row) => Some(row),
            Err(error) => {
                tracing::debug!(
                    %error,
                    "grok shim skipped an undecodable AgentToolResult row"
                );
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_row(tool_name: &str, lifecycle_state: Option<&str>) -> ToolCallRow {
        ToolCallRow {
            tool_call_key: format!("session-1:call-{tool_name}"),
            tool_call_id: Some(format!("call-{tool_name}")),
            tool_name: Some(tool_name.to_string()),
            status: None,
            lifecycle_state: lifecycle_state.map(ToOwned::to_owned),
            child_request_id: None,
            args: Some(r#"{"command":"echo gents-subprocess-probe"}"#.to_string()),
            result: Some("gents-subprocess-probe".to_string()),
            selected_tool_name: None,
            tool_failure_class: None,
        }
    }

    #[test]
    fn kind_wire_names_match_grok_pager() {
        assert_eq!(ToolCallKind::Read.wire_name(), "read");
        assert_eq!(ToolCallKind::Edit.wire_name(), "edit");
        assert_eq!(ToolCallKind::Delete.wire_name(), "delete");
        assert_eq!(ToolCallKind::Move.wire_name(), "move");
        assert_eq!(ToolCallKind::Search.wire_name(), "search");
        assert_eq!(ToolCallKind::Execute.wire_name(), "execute");
        assert_eq!(ToolCallKind::Think.wire_name(), "think");
        assert_eq!(ToolCallKind::Fetch.wire_name(), "fetch");
        assert_eq!(ToolCallKind::Other.wire_name(), "other");
    }

    #[test]
    fn kind_mapping_covers_durable_tool_names() {
        assert_eq!(ToolCallKind::from_tool_name("read_file"), ToolCallKind::Read);
        assert_eq!(ToolCallKind::from_tool_name("bash"), ToolCallKind::Execute);
        assert_eq!(ToolCallKind::from_tool_name("grep"), ToolCallKind::Search);
        assert_eq!(ToolCallKind::from_tool_name("unknown_tool"), ToolCallKind::Other);
    }

    #[test]
    fn status_wire_names_match_grok_pager() {
        assert_eq!(ToolCallStatus::Pending.wire_name(), "pending");
        assert_eq!(ToolCallStatus::InProgress.wire_name(), "in_progress");
        assert_eq!(ToolCallStatus::Completed.wire_name(), "completed");
        assert_eq!(ToolCallStatus::Failed.wire_name(), "failed");
    }

    #[test]
    fn completed_tool_call_payload_matches_grok_wire_shape() {
        let row = tool_row("bash", Some("completed"));
        let projection = project_tool_rows(&[row], &[]);
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
        let projection = project_tool_rows(&[row], &[]);
        assert!(projection.updates.len() >= 2);
        let ToolUpdate::ToolCallUpdate(update) = &projection.updates[1] else {
            panic!("second update should be a tool_call_update");
        };
        assert_eq!(update.tool_call_id, "call-bash");
        assert_eq!(update.fields["status"], "completed");
        let payload = update.to_payload();
        assert_eq!(payload["sessionUpdate"], "tool_call_update");
        assert_eq!(payload["toolCallId"], "call-bash");
        assert_eq!(payload["fields"]["status"], "completed");
    }

    #[test]
    fn running_tool_call_has_no_update_and_stays_in_progress() {
        let row = tool_row("bash", Some("running"));
        let projection = project_tool_rows(&[row], &[]);
        assert_eq!(projection.updates.len(), 2); // tool_call + available_commands
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert_eq!(call.status, ToolCallStatus::InProgress);
    }

    #[test]
    fn suppressed_families_never_render_but_task_registers_blocking_wait() {
        // THE attempt-1 defect: a canonical `task` tool (suppressed family)
        // with meta lacking `subagentBackground: true` must register a
        // blocking subagent wait BEFORE family suppression drops the
        // rendered block.
        let mut row = tool_row("task", Some("running"));
        row.args = Some(r#"{"description":"scout the repo"}"#.to_string());
        row.result = None;
        let projection = project_tool_rows(&[row], &[]);
        assert!(
            projection
                .updates
                .iter()
                .all(|update| update.session_update_kind() != "tool_call"),
            "suppressed task family must not render a scrollback block"
        );
        let wait = projection
            .subagent_waits
            .get("call-task")
            .expect("task tool without subagentBackground must register a blocking wait");
        assert_eq!(wait.tool_name, "task");
    }

    #[test]
    fn task_with_subagent_background_true_does_not_block() {
        let mut row = tool_row("task", Some("running"));
        row.args = Some(r#"{"subagentBackground":true}"#.to_string());
        row.result = None;
        let projection = project_tool_rows(&[row], &[]);
        assert!(projection.subagent_waits.is_empty());
    }

    #[test]
    fn spawn_row_with_child_request_id_registers_wait_even_when_named_run_subagent() {
        let mut row = tool_row("run_subagent", Some("running"));
        row.child_request_id = Some("child-request-1".to_string());
        row.result = None;
        let projection = project_tool_rows(&[row], &[]);
        let wait = projection
            .subagent_waits
            .get("call-run_subagent")
            .expect("spawn row with child_request_id must register a wait");
        assert_eq!(wait.child_request_id.as_deref(), Some("child-request-1"));
    }

    #[test]
    fn spawn_subagent_named_row_registers_wait() {
        let mut row = tool_row("spawn_subagent", Some("running"));
        row.result = None;
        let projection = project_tool_rows(&[row], &[]);
        assert!(projection.subagent_waits.contains_key("call-spawn_subagent"));
    }

    #[test]
    fn non_spawn_rows_register_no_wait() {
        let row = tool_row("read_file", Some("running"));
        let projection = project_tool_rows(&[row], &[]);
        assert!(projection.subagent_waits.is_empty());
    }

    #[test]
    fn other_suppressed_families_render_nothing() {
        for name in ["todo", "bg-plumbing", "goal", "scheduler", "workflow"] {
            let row = tool_row(name, Some("running"));
            let projection = project_tool_rows(&[row], &[]);
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
    fn send_subagent_message_is_recognized_by_canonical_meta() {
        let meta = json!({
            "version": TOOL_META_VERSION,
            "kind": TOOL_META_KIND_ACTIVE_AGENT_MESSAGE,
        });
        assert!(is_active_agent_message_meta(Some(&meta)));
        assert!(!is_active_agent_message_meta(None));
        assert!(!is_active_agent_message_meta(Some(&json!({"version": 2}))));
        let mut row = tool_row("send_subagent_message", Some("running"));
        row.args = Some(
            format!(
                r#"{{"subagent_id":"sub-1","text":"hi","{TOOL_META_KEY}":{meta}}}"#
            ),
        );
        row.result = None;
        let projection = project_tool_rows(&[row], &[]);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert!(call.is_active_agent_message());
        assert_eq!(call.title, SEND_SUBAGENT_MESSAGE_TITLE);
        assert_eq!(
            call.raw_input
                .as_ref()
                .and_then(|input| input.get("subagent_id"))
                .and_then(Value::as_str),
            Some("sub-1")
        );
    }

    #[test]
    fn send_subagent_message_is_recognized_by_title_fallback() {
        let mut row = tool_row("send_subagent_message", Some("running"));
        row.args = Some(r#"{"subagent_id":"sub-1","text":"hi"}"#.to_string());
        row.result = None;
        let projection = project_tool_rows(&[row], &[]);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert!(call.is_active_agent_message());
        assert_eq!(call.title, SEND_SUBAGENT_MESSAGE_TITLE);
    }

    #[test]
    fn available_commands_update_carries_visible_tools_meta() {
        let rows = vec![
            tool_row("bash", Some("completed")),
            tool_row("read_file", Some("completed")),
            tool_row("todo", Some("completed")),
        ];
        let projection = project_tool_rows(&rows, &[]);
        let ToolUpdate::AvailableCommands(update) = projection
            .updates
            .iter()
            .find(|update| update.session_update_kind() == "available_commands_update")
            .cloned()
            .expect("available_commands_update should be emitted") else {
            unreachable!("already checked the kind");
        };
        assert_eq!(update.tools, vec!["bash", "read_file"]);
        let payload = update.to_payload();
        assert_eq!(payload["sessionUpdate"], "available_commands_update");
        assert_eq!(payload["meta"]["tools"], json!(["bash", "read_file"]));
    }

    #[test]
    fn empty_rows_project_nothing() {
        let projection = project_tool_rows(&[], &[]);
        assert!(projection.updates.is_empty());
        assert!(projection.subagent_waits.is_empty());
    }

    #[test]
    fn blank_lifecycle_falls_back_to_status_vocabulary() {
        let mut row = tool_row("bash", None);
        row.status = Some("completed".to_string());
        let projection = project_tool_rows(&[row], &[]);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert_eq!(call.status, ToolCallStatus::Completed);
    }

    #[test]
    fn failure_class_is_always_failed() {
        let mut row = tool_row("bash", Some("running"));
        row.tool_failure_class = Some("transport".to_string());
        let projection = project_tool_rows(&[row], &[]);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert_eq!(call.status, ToolCallStatus::Failed);
    }

    #[test]
    fn audit_rows_never_override_the_call_row_lifecycle() {
        let mut row = tool_row("bash", Some("running"));
        row.status = Some("success".to_string());
        row.result = None;
        let results = vec![ToolResultRow {
            tool_call_doc_id: "doc-1".to_string(),
            tool_name: Some("bash".to_string()),
            output_text: Some("spilled oversized output".to_string()),
        }];
        let projection = project_tool_rows(&[row], &results);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        // The durable call row's lifecycle_state is authoritative; the
        // conversation-scoped audit row cannot downgrade it. The spilled
        // output still surfaces as the call's rawOutput/content.
        assert_eq!(call.status, ToolCallStatus::InProgress);
        assert_eq!(
            call.raw_output
                .as_ref()
                .and_then(|output| output.get("output"))
                .and_then(Value::as_str),
            Some("spilled oversized output")
        );
    }

    #[test]
    fn absent_result_omits_content_and_raw_output() {
        let mut row = tool_row("bash", Some("completed"));
        row.result = None;
        let projection = project_tool_rows(&[row], &[]);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        let payload = call.to_payload();
        assert!(payload.get("content").is_none());
        assert!(payload.get("rawOutput").is_none());
    }

    #[test]
    fn tool_call_id_falls_back_to_key_without_session_prefix() {
        let mut row = tool_row("bash", Some("completed"));
        row.tool_call_id = None;
        let projection = project_tool_rows(&[row], &[]);
        let ToolUpdate::ToolCall(call) = &projection.updates[0] else {
            panic!("first update should be a tool_call");
        };
        assert_eq!(call.tool_call_id, "call-bash");
    }

    #[test]
    fn queries_escape_interpolated_values() {
        let query = tool_calls_query(r#"request-"quoted\"-id"#);
        assert!(
            !query.contains(r#""request-"quoted\"-id""#),
            "raw value must not appear unescaped: {query}"
        );
        assert!(query.contains("AgentToolCall"));

        let results = tool_results_query(r#"request-"quoted\"-id"#);
        assert!(
            !results.contains(r#""request-"quoted\"-id""#),
            "raw value must not appear unescaped: {results}"
        );
        assert!(results.contains("AgentToolResult"));
    }

    #[test]
    fn terminal_methods_answer_pager_style_not_supported() {
        for method in [
            "terminal/create",
            "terminal/output",
            "terminal/wait_for_exit",
            "terminal/kill",
            "terminal/release",
        ] {
            let error = handle_terminal_client_method(method)
                .expect_err("terminal methods must be unsupported");
            assert_eq!(error["code"], JSONRPC_METHOD_NOT_SUPPORTED);
            assert_eq!(
                error["message"],
                format!("{method}: {TERMINAL_NOT_SUPPORTED_MESSAGE}")
            );
        }
        let unknown = handle_terminal_client_method("terminal/invent")
            .expect_err("unknown terminal methods are also rejected");
        assert_eq!(unknown["code"], JSONRPC_METHOD_NOT_SUPPORTED);
    }
}

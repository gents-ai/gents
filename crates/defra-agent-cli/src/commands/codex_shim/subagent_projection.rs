use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use codex_app_server_protocol as codex;
use defra_agent::tool_call_lifecycle::MAX_SUBAGENT_DEPTH;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::progress::{defra_tool_call_status, DefraToolCallProgress};
use super::store::query_node_json;
use super::ShimState;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LinkedSubagentThread {
    pub(super) request_id: String,
    pub(super) session_id: String,
    pub(super) parent_request_id: String,
    pub(super) parent_tool_call_id: String,
    pub(super) parent_session_id: String,
    pub(super) root_session_id: String,
    pub(super) depth: u32,
    pub(super) agent_did: String,
    pub(super) behavior_id: String,
    pub(super) nickname: String,
    pub(super) lifecycle_state: String,
    pub(super) failure_reason: Option<String>,
    pub(super) created_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct CollabProjection {
    pub(super) status: codex::CollabAgentToolCallStatus,
    pub(super) tool: codex::CollabAgentTool,
}

#[derive(Clone, Debug, Deserialize)]
struct RequestRow {
    request_id: String,
    session_id: String,
    agent_did: String,
    #[serde(default)]
    behavior_id: Option<String>,
    #[serde(default)]
    metadata: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    failure_reason: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    subagent_depth: Option<u32>,
    #[serde(default)]
    caused_by_parent_request_id: Option<String>,
    #[serde(default)]
    caused_by_parent_tool_call_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct ToolLinkRow {
    request_id: String,
    session_id: String,
    agent_did: String,
    tool_call_id: String,
    tool_name: String,
    #[serde(default)]
    child_request_id: Option<String>,
    #[serde(default)]
    spawn_target_did: Option<String>,
    #[serde(default)]
    args: String,
}

#[derive(Clone, Debug)]
struct AuthorizedRequest {
    row_index: usize,
    root_session_id: String,
}

/// Load the subagent request graph reachable from a Codex-shim-owned root.
///
/// ACP still decides which rows are visible.  On top of that boundary, this
/// verifies both halves of every bridge edge before exposing a child session:
/// the child must point at the parent request/tool call, and that tool call must
/// point back at the child request.  This prevents `thread/read` from becoming
/// an unscoped foreign-session lookup.
pub(super) async fn load_authorized_subagent_threads(
    state: &ShimState,
) -> Result<Vec<LinkedSubagentThread>> {
    let query = r#"{
        AgentRequest(order: { created_at: ASC }) {
            request_id
            session_id
            agent_did
            behavior_id
            metadata
            lifecycle_state
            failure_reason
            created_at
            subagent_depth
            caused_by_parent_request_id
            caused_by_parent_tool_call_id
        }
        AgentToolCall(filter: { child_request_id: { _ne: "" } }) {
            request_id
            session_id
            agent_did
            tool_call_id
            tool_name
            child_request_id
            spawn_target_did
            args
        }
    }"#;
    let response = query_node_json(state.node.as_ref(), query).await?;
    let requests = decode_rows::<RequestRow>(&response, "AgentRequest")
        .context("decoding subagent AgentRequest graph")?;
    let tools = decode_rows::<ToolLinkRow>(&response, "AgentToolCall")
        .context("decoding subagent AgentToolCall graph")?;
    Ok(resolve_authorized_subagent_threads(
        &requests,
        &tools,
        state.agent_did.as_ref(),
        state.behavior_id.as_ref(),
    ))
}

fn resolve_authorized_subagent_threads(
    requests: &[RequestRow],
    tools: &[ToolLinkRow],
    shim_agent_did: &str,
    shim_behavior_id: &str,
) -> Vec<LinkedSubagentThread> {
    let mut authorized = requests
        .iter()
        .enumerate()
        .filter(|(_, row)| is_codex_root(row, shim_agent_did, shim_behavior_id))
        .map(|(row_index, row)| AuthorizedRequest {
            row_index,
            root_session_id: row.session_id.clone(),
        })
        .collect::<Vec<_>>();
    let mut links = Vec::new();

    for _ in 0..MAX_SUBAGENT_DEPTH {
        let mut added = false;

        // A background child may receive steering or automated wake-up
        // requests in the same session. Once the spawn edge authorizes that
        // session, those same-principal requests are valid parent contexts for
        // nested spawns too.
        let session_requests = requests
            .iter()
            .enumerate()
            .filter(|(row_index, row)| {
                !authorized.iter().any(|entry| entry.row_index == *row_index)
                    && authorized
                        .iter()
                        .any(|entry| same_session_context(&requests[entry.row_index], row))
            })
            .map(|(row_index, row)| {
                let root_session_id = authorized
                    .iter()
                    .find(|entry| same_session_context(&requests[entry.row_index], row))
                    .expect("authorized session context")
                    .root_session_id
                    .clone();
                AuthorizedRequest {
                    row_index,
                    root_session_id,
                }
            })
            .collect::<Vec<_>>();
        added |= !session_requests.is_empty();
        authorized.extend(session_requests);

        for (row_index, child) in requests.iter().enumerate() {
            if authorized.iter().any(|entry| entry.row_index == row_index) {
                continue;
            }
            let Some(parent_request_id) = nonempty(child.caused_by_parent_request_id.as_deref())
            else {
                continue;
            };
            let Some(parent_tool_call_id) =
                nonempty(child.caused_by_parent_tool_call_id.as_deref())
            else {
                continue;
            };
            let child_depth = child.subagent_depth.unwrap_or_default();
            if child_depth == 0 || child_depth > MAX_SUBAGENT_DEPTH {
                continue;
            }
            if Uuid::parse_str(&child.session_id).is_err() {
                continue;
            }

            let Some(parent_auth) = authorized.iter().find(|entry| {
                let parent = &requests[entry.row_index];
                parent.request_id == parent_request_id
                    && parent.subagent_depth.unwrap_or_default() + 1 == child_depth
                    && tools.iter().any(|tool| {
                        tool.request_id == parent.request_id
                            && tool.session_id == parent.session_id
                            && tool.agent_did == parent.agent_did
                            && tool.tool_call_id == parent_tool_call_id
                            && tool.tool_name == "spawn_subagent"
                            && nonempty(tool.child_request_id.as_deref())
                                == Some(child.request_id.as_str())
                            && nonempty(tool.spawn_target_did.as_deref())
                                .is_none_or(|target| target == child.agent_did)
                    })
            }) else {
                continue;
            };
            let parent = &requests[parent_auth.row_index];
            let Some(tool) = tools.iter().find(|tool| {
                tool.request_id == parent.request_id
                    && tool.session_id == parent.session_id
                    && tool.agent_did == parent.agent_did
                    && tool.tool_call_id == parent_tool_call_id
                    && nonempty(tool.child_request_id.as_deref()) == Some(child.request_id.as_str())
            }) else {
                continue;
            };
            let behavior_id = nonempty(child.behavior_id.as_deref())
                .unwrap_or("subagent")
                .to_string();
            let nickname = spawn_nickname(&tool.args).unwrap_or_else(|| behavior_id.clone());
            links.push(LinkedSubagentThread {
                request_id: child.request_id.clone(),
                session_id: child.session_id.clone(),
                parent_request_id: parent.request_id.clone(),
                parent_tool_call_id: parent_tool_call_id.to_string(),
                parent_session_id: parent.session_id.clone(),
                root_session_id: parent_auth.root_session_id.clone(),
                depth: child_depth,
                agent_did: child.agent_did.clone(),
                behavior_id,
                nickname,
                lifecycle_state: nonempty(child.lifecycle_state.as_deref())
                    .unwrap_or("pending")
                    .to_string(),
                failure_reason: child
                    .failure_reason
                    .as_deref()
                    .and_then(|value| nonempty(Some(value)))
                    .map(ToOwned::to_owned),
                created_at: child.created_at.clone(),
            });
            authorized.push(AuthorizedRequest {
                row_index,
                root_session_id: parent_auth.root_session_id.clone(),
            });
            added = true;
        }
        if !added {
            break;
        }
    }

    links.sort_by(|left, right| {
        left.depth
            .cmp(&right.depth)
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    let mut seen_sessions = HashSet::new();
    links.retain(|link| seen_sessions.insert(link.session_id.clone()));
    links
}

fn same_session_context(left: &RequestRow, right: &RequestRow) -> bool {
    left.session_id == right.session_id
        && left.agent_did == right.agent_did
        && nonempty(left.behavior_id.as_deref()) == nonempty(right.behavior_id.as_deref())
        && left.subagent_depth.unwrap_or_default() == right.subagent_depth.unwrap_or_default()
}

fn is_codex_root(row: &RequestRow, agent_did: &str, behavior_id: &str) -> bool {
    row.agent_did == agent_did
        && nonempty(row.behavior_id.as_deref()) == Some(behavior_id)
        && row.subagent_depth.unwrap_or_default() == 0
        && nonempty(row.caused_by_parent_request_id.as_deref()).is_none()
        && nonempty(row.caused_by_parent_tool_call_id.as_deref()).is_none()
        && row
            .metadata
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .is_some_and(|metadata| metadata.get("codex_shim").is_some())
}

fn spawn_nickname(args: &str) -> Option<String> {
    serde_json::from_str::<Value>(args)
        .ok()?
        .get("name")?
        .as_str()
        .and_then(|value| nonempty(Some(value)))
        .map(ToOwned::to_owned)
}

fn decode_rows<T>(response: &Value, collection: &str) -> serde_json::Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    response
        .pointer(&format!("/data/{collection}"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(serde_json::from_value)
        .collect()
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

pub(super) fn is_subagent_control_tool(tool_name: &str) -> bool {
    collab_tool(tool_name).is_some()
}

fn collab_tool(tool_name: &str) -> Option<codex::CollabAgentTool> {
    match tool_name {
        "spawn_subagent" => Some(codex::CollabAgentTool::SpawnAgent),
        "wait_subagent" => Some(codex::CollabAgentTool::Wait),
        "steer_subagent" => Some(codex::CollabAgentTool::SendInput),
        "cancel_subagent" => Some(codex::CollabAgentTool::CloseAgent),
        _ => None,
    }
}

pub(super) fn attach_subagent_link(
    tool: &mut DefraToolCallProgress,
    links: &[LinkedSubagentThread],
) {
    let Some(child_request_id) = tool_child_request_id(tool) else {
        return;
    };
    tool.subagent_link = links
        .iter()
        .find(|link| link.request_id == child_request_id)
        .cloned();
}

fn tool_child_request_id(tool: &DefraToolCallProgress) -> Option<String> {
    nonempty(tool.child_request_id.as_deref())
        .map(ToOwned::to_owned)
        .or_else(|| {
            serde_json::from_str::<Value>(&tool.args)
                .ok()?
                .get("child_request_id")?
                .as_str()
                .and_then(|value| nonempty(Some(value)))
                .map(ToOwned::to_owned)
        })
}

pub(super) fn collab_projection(tool: &DefraToolCallProgress) -> Option<CollabProjection> {
    let collab_tool = collab_tool(&tool.tool_name)?;
    tool.subagent_link.as_ref()?;
    let status = match defra_tool_call_status(tool) {
        codex::McpToolCallStatus::InProgress => codex::CollabAgentToolCallStatus::InProgress,
        codex::McpToolCallStatus::Completed => codex::CollabAgentToolCallStatus::Completed,
        codex::McpToolCallStatus::Failed => codex::CollabAgentToolCallStatus::Failed,
    };
    Some(CollabProjection {
        status,
        tool: collab_tool,
    })
}

pub(super) fn collab_agent_status(lifecycle_state: &str) -> codex::CollabAgentStatus {
    match lifecycle_state.trim() {
        "pending" => codex::CollabAgentStatus::PendingInit,
        "claimed" | "processing" | "inputRequired" => codex::CollabAgentStatus::Running,
        "completed" => codex::CollabAgentStatus::Completed,
        "failed" | "dead" => codex::CollabAgentStatus::Errored,
        "superseded" | "interrupted" => codex::CollabAgentStatus::Interrupted,
        _ => codex::CollabAgentStatus::NotFound,
    }
}

pub(super) fn collab_tool_item(
    sender_thread_id: &str,
    tool: &DefraToolCallProgress,
    projection: &CollabProjection,
) -> codex::ThreadItem {
    let link = tool
        .subagent_link
        .as_ref()
        .expect("collab projection requires an authorized subagent link");
    let prompt = serde_json::from_str::<Value>(&tool.args)
        .ok()
        .and_then(|args| match projection.tool {
            codex::CollabAgentTool::SpawnAgent => args
                .get("prompt")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            codex::CollabAgentTool::SendInput => args
                .get("message")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            _ => None,
        });
    let mut agents_states = HashMap::new();
    agents_states.insert(
        link.session_id.clone(),
        codex::CollabAgentState {
            status: collab_agent_status(&link.lifecycle_state),
            message: link.failure_reason.clone(),
        },
    );
    codex::ThreadItem::CollabAgentToolCall {
        id: tool.tool_call_key.clone(),
        tool: projection.tool.clone(),
        status: projection.status.clone(),
        sender_thread_id: sender_thread_id.to_string(),
        receiver_thread_ids: vec![link.session_id.clone()],
        prompt,
        model: None,
        reasoning_effort: None,
        agents_states,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(
        request_id: &str,
        session_id: &str,
        depth: u32,
        parent_request_id: Option<&str>,
        parent_tool_call_id: Option<&str>,
    ) -> RequestRow {
        RequestRow {
            request_id: request_id.to_string(),
            session_id: session_id.to_string(),
            agent_did: if depth == 0 { "did:root" } else { "did:child" }.to_string(),
            behavior_id: Some(if depth == 0 { "root" } else { "reviewer" }.to_string()),
            metadata: (depth == 0).then(|| r#"{"codex_shim":{"cwd":"/tmp"}}"#.to_string()),
            lifecycle_state: Some(
                if depth == 0 {
                    "processing"
                } else {
                    "completed"
                }
                .to_string(),
            ),
            failure_reason: None,
            created_at: None,
            subagent_depth: Some(depth),
            caused_by_parent_request_id: parent_request_id.map(ToOwned::to_owned),
            caused_by_parent_tool_call_id: parent_tool_call_id.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn only_symmetric_edges_reachable_from_codex_roots_are_exposed() {
        let root_session = Uuid::new_v4().to_string();
        let child_session = Uuid::new_v4().to_string();
        let orphan_session = Uuid::new_v4().to_string();
        let requests = vec![
            request("root-request", &root_session, 0, None, None),
            request(
                "child-request",
                &child_session,
                1,
                Some("root-request"),
                Some("spawn-call"),
            ),
            request(
                "orphan-request",
                &orphan_session,
                1,
                Some("root-request"),
                Some("missing-call"),
            ),
        ];
        let tools = vec![ToolLinkRow {
            request_id: "root-request".to_string(),
            session_id: root_session.clone(),
            agent_did: "did:root".to_string(),
            tool_call_id: "spawn-call".to_string(),
            tool_name: "spawn_subagent".to_string(),
            child_request_id: Some("child-request".to_string()),
            spawn_target_did: Some("did:child".to_string()),
            args: r#"{"name":"reviewer"}"#.to_string(),
        }];

        let links = resolve_authorized_subagent_threads(&requests, &tools, "did:root", "root");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].session_id, child_session);
        assert_eq!(links[0].root_session_id, root_session);
        assert_eq!(links[0].nickname, "reviewer");
    }

    #[test]
    fn nested_spawn_from_later_request_in_linked_session_is_exposed() {
        let root_session = Uuid::new_v4().to_string();
        let child_session = Uuid::new_v4().to_string();
        let grandchild_session = Uuid::new_v4().to_string();
        let requests = vec![
            request("root-request", &root_session, 0, None, None),
            // Reverse the child order so the fixed-point walk, rather than
            // source ordering, is what makes the nested edge reachable.
            request(
                "grandchild-request",
                &grandchild_session,
                2,
                Some("child-followup"),
                Some("nested-spawn-call"),
            ),
            request(
                "child-request",
                &child_session,
                1,
                Some("root-request"),
                Some("spawn-call"),
            ),
            request("child-followup", &child_session, 1, None, None),
        ];
        let tools = vec![
            ToolLinkRow {
                request_id: "root-request".to_string(),
                session_id: root_session.clone(),
                agent_did: "did:root".to_string(),
                tool_call_id: "spawn-call".to_string(),
                tool_name: "spawn_subagent".to_string(),
                child_request_id: Some("child-request".to_string()),
                spawn_target_did: Some("did:child".to_string()),
                args: r#"{"name":"reviewer"}"#.to_string(),
            },
            ToolLinkRow {
                request_id: "child-followup".to_string(),
                session_id: child_session.clone(),
                agent_did: "did:child".to_string(),
                tool_call_id: "nested-spawn-call".to_string(),
                tool_name: "spawn_subagent".to_string(),
                child_request_id: Some("grandchild-request".to_string()),
                spawn_target_did: Some("did:child".to_string()),
                args: r#"{"name":"nested-reviewer"}"#.to_string(),
            },
        ];

        let links = resolve_authorized_subagent_threads(&requests, &tools, "did:root", "root");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].session_id, child_session);
        assert_eq!(links[1].session_id, grandchild_session);
        assert_eq!(links[1].parent_session_id, child_session);
        assert_eq!(links[1].root_session_id, root_session);
    }

    #[test]
    fn lean_fenced_tool_and_status_mappings_match_codex_protocol() {
        assert_eq!(
            collab_tool("spawn_subagent"),
            Some(codex::CollabAgentTool::SpawnAgent)
        );
        assert_eq!(
            collab_tool("wait_subagent"),
            Some(codex::CollabAgentTool::Wait)
        );
        assert_eq!(
            collab_tool("steer_subagent"),
            Some(codex::CollabAgentTool::SendInput)
        );
        assert_eq!(
            collab_tool("cancel_subagent"),
            Some(codex::CollabAgentTool::CloseAgent)
        );
        assert_eq!(collab_tool("list_subagents"), None);
        assert_eq!(collab_tool("read_subagent"), None);

        assert_eq!(
            collab_agent_status("pending"),
            codex::CollabAgentStatus::PendingInit
        );
        assert_eq!(
            collab_agent_status("processing"),
            codex::CollabAgentStatus::Running
        );
        assert_eq!(
            collab_agent_status("completed"),
            codex::CollabAgentStatus::Completed
        );
        assert_eq!(
            collab_agent_status("failed"),
            codex::CollabAgentStatus::Errored
        );
        assert_eq!(
            collab_agent_status("interrupted"),
            codex::CollabAgentStatus::Interrupted
        );
    }

    #[test]
    fn spawn_item_uses_child_session_as_receiver_thread() {
        let child_session_id = Uuid::new_v4().to_string();
        let mut tool = DefraToolCallProgress {
            tool_call_key: "parent:spawn-call".to_string(),
            tool_name: "spawn_subagent".to_string(),
            status: "running".to_string(),
            lifecycle_state: Some("running".to_string()),
            await_mode: Some("background".to_string()),
            child_request_id: Some("child-request".to_string()),
            args: r#"{"name":"reviewer","prompt":"Inspect the patch"}"#.to_string(),
            result: String::new(),
            subagent_link: None,
        };
        tool.subagent_link = Some(LinkedSubagentThread {
            request_id: "child-request".to_string(),
            session_id: child_session_id.clone(),
            parent_request_id: "parent-request".to_string(),
            parent_tool_call_id: "spawn-call".to_string(),
            parent_session_id: Uuid::new_v4().to_string(),
            root_session_id: Uuid::new_v4().to_string(),
            depth: 1,
            agent_did: "did:child".to_string(),
            behavior_id: "code-review".to_string(),
            nickname: "reviewer".to_string(),
            lifecycle_state: "processing".to_string(),
            failure_reason: None,
            created_at: None,
        });
        let projection = collab_projection(&tool).expect("authorized spawn projection");
        let item = collab_tool_item("parent-thread", &tool, &projection);
        let value = serde_json::to_value(&item).expect("serialize collab item");
        serde_json::from_value::<codex::ThreadItem>(value.clone())
            .expect("collab projection must be a valid pinned Codex ThreadItem");

        assert_eq!(value["type"], "collabAgentToolCall");
        assert_eq!(value["tool"], "spawnAgent");
        assert_eq!(value["senderThreadId"], "parent-thread");
        assert_eq!(value["receiverThreadIds"], json!([child_session_id]));
        assert_eq!(value["prompt"], "Inspect the patch");
        assert_eq!(
            value.pointer(&format!(
                "/agentsStates/{}/status",
                tool.subagent_link
                    .as_ref()
                    .expect("link")
                    .session_id
                    .replace('~', "~0")
                    .replace('/', "~1")
            )),
            Some(&Value::String("running".to_string()))
        );
    }
}

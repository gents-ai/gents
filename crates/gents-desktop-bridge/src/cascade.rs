use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use gents::graphql::escape_graphql_string;
use gents_desktop_core::client::ClientCore;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::snapshot::{compute_preview_signature, PreviewSignatureInput, PreviewSignatureRow};
use crate::types::{
    CascadeAffectedRequest, CascadeCancelPreview, DesktopInterruptRequest,
    DesktopPreviewInterruptCascadeRequest, InterruptRequestResult,
};

const MAX_CASCADE_DEPTH: usize = 8;
const REMOTE_GRAPHQL_TIMEOUT: Duration = Duration::from_secs(15);

enum GraphqlAccess {
    Local,
    Remote {
        graphql: String,
        client: reqwest::Client,
    },
}

impl GraphqlAccess {
    async fn for_agent(core: &Arc<ClientCore>, agent_did: Option<&str>) -> Result<Self, String> {
        let Some(agent_did) = agent_did
            .map(str::trim)
            .filter(|agent_did| !agent_did.is_empty())
        else {
            return Ok(Self::Local);
        };
        let Some(graphql) = core.graphql_for_agent(agent_did).await else {
            return Ok(Self::Local);
        };
        let client = reqwest::Client::builder()
            .timeout(REMOTE_GRAPHQL_TIMEOUT)
            .build()
            .map_err(|error| format!("building remote GraphQL client: {error}"))?;
        Ok(Self::Remote { graphql, client })
    }

    async fn execute(
        &self,
        core: &Arc<ClientCore>,
        document: &str,
        operation: &str,
    ) -> Result<Value, String> {
        match self {
            Self::Local => {
                let response = core.node().execute(document).await;
                if response.has_errors() {
                    return Err(format!(
                        "{operation} failed: {}",
                        response
                            .errors
                            .iter()
                            .map(|error| error.message.as_str())
                            .collect::<Vec<_>>()
                            .join("; ")
                    ));
                }
                Ok(response.data.unwrap_or(Value::Null))
            }
            Self::Remote { graphql, client } => {
                let response = client
                    .post(graphql)
                    .json(&json!({ "query": document }))
                    .send()
                    .await
                    .map_err(|error| {
                        format!("sending {operation} to remote GraphQL {graphql}: {error}")
                    })?;
                let status = response.status();
                let body = response.bytes().await.map_err(|error| {
                    format!("reading {operation} from remote GraphQL {graphql}: {error}")
                })?;
                if !status.is_success() {
                    return Err(format!(
                        "{operation} at remote GraphQL {graphql} failed with {status}: {}",
                        String::from_utf8_lossy(&body)
                    ));
                }
                let response: RemoteGraphqlResponse =
                    serde_json::from_slice(&body).map_err(|error| {
                        format!(
                            "decoding {operation} response from remote GraphQL {graphql}: {error}"
                        )
                    })?;
                if response
                    .errors
                    .as_ref()
                    .is_some_and(|errors| !graphql_errors_are_empty(errors))
                {
                    return Err(format!(
                        "{operation} at remote GraphQL {graphql} returned errors: {}",
                        response.errors.unwrap_or(Value::Null)
                    ));
                }
                Ok(response.data.unwrap_or(Value::Null))
            }
        }
    }
}

#[derive(Deserialize)]
struct RemoteGraphqlResponse {
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    errors: Option<Value>,
}

fn graphql_errors_are_empty(errors: &Value) -> bool {
    errors.is_null() || errors.as_array().is_some_and(Vec::is_empty)
}

const TERMINAL_STATES: &[&str] = &[
    "completed",
    "failed",
    "cancelled",
    "superseded",
    "dead",
    "interrupted",
];

#[derive(Debug, Clone)]
pub struct CascadeWalkRequest {
    pub root_request_id: String,
    pub agent_did: Option<String>,
    pub include_terminal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CascadeClassification {
    WillInterrupt,
    WillDetach,
    AlreadyTerminal,
    UnknownPolicy,
}

#[derive(Debug, Clone)]
pub struct CascadeWalkRow {
    pub request_id: String,
    pub session_id: Option<String>,
    pub behavior_id: Option<String>,
    pub lifecycle_state: Option<String>,
    pub parent_request_id: Option<String>,
    pub parent_tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
    pub classification: CascadeClassification,
}

#[derive(Debug, Clone, Default)]
pub struct CascadeWalkResult {
    pub root_state: Option<String>,
    pub root_interrupt_requested_at: Option<String>,
    pub rows: Vec<CascadeWalkRow>,
}

pub async fn walk(
    core: &Arc<ClientCore>,
    req: &CascadeWalkRequest,
) -> Result<CascadeWalkResult, String> {
    let agent_did = req.agent_did.as_deref();
    let access = GraphqlAccess::for_agent(core, agent_did).await?;

    let root = fetch_request(core, &access, &req.root_request_id, agent_did)
        .await
        .map_err(|e| format!("cascade::walk: root request not found: {e}"))?;

    let root_lifecycle_state = string_field(&root, "lifecycle_state");
    let root_interrupt_requested_at = string_field(&root, "interrupt_requested_at");

    let mut result = CascadeWalkResult {
        root_state: root_lifecycle_state,
        root_interrupt_requested_at,
        rows: Vec::new(),
    };

    let mut seen_requests: BTreeSet<String> = BTreeSet::new();
    seen_requests.insert(req.root_request_id.clone());

    bfs(
        core,
        &access,
        &req.root_request_id,
        req.include_terminal,
        0,
        &mut seen_requests,
        &mut result.rows,
    )
    .await?;

    Ok(result)
}

async fn bfs(
    core: &Arc<ClientCore>,
    access: &GraphqlAccess,
    parent_request_id: &str,
    include_terminal: bool,
    depth: usize,
    seen_requests: &mut BTreeSet<String>,
    rows: &mut Vec<CascadeWalkRow>,
) -> Result<(), String> {
    if depth >= MAX_CASCADE_DEPTH {
        return Err(format!("cascade depth exceeded at {parent_request_id}"));
    }

    let escaped_parent = escape_graphql_string(parent_request_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    request_id: {{ _eq: "{escaped_parent}" }},
                    child_request_id: {{ _ne: "" }}
                }},
                order: [{{ message_sequence: ASC }}, {{ tool_call_id: ASC }}]
            ) {{
                tool_call_id
                tool_name
                await_mode
                cancel_policy
                child_request_id
            }}
        }}"#
    );

    let data = access
        .execute(
            core,
            &query,
            &format!("cascade AgentToolCall query for {parent_request_id}"),
        )
        .await?;
    let tool_calls = data
        .get("AgentToolCall")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for tc in &tool_calls {
        let child_id = match string_field(tc, "child_request_id") {
            Some(id) if !id.is_empty() => id,
            _ => continue,
        };

        if !seen_requests.insert(child_id.clone()) {
            continue;
        }

        let tool_call_id = string_field(tc, "tool_call_id");
        let tool_name = string_field(tc, "tool_name");
        let await_mode = string_field(tc, "await_mode");
        let cancel_policy = string_field(tc, "cancel_policy");

        let child_row = match fetch_request(core, access, &child_id, None).await {
            Ok(row) => row,
            Err(e) => {
                return Err(format!(
                    "cascade::walk: child request {child_id} not found: {e}"
                ));
            }
        };

        let child_lifecycle_state = string_field(&child_row, "lifecycle_state");
        let child_session_id = string_field(&child_row, "session_id");
        let child_behavior_id = string_field(&child_row, "behavior_id");

        let is_terminal = child_lifecycle_state
            .as_deref()
            .map(|s| TERMINAL_STATES.contains(&s))
            .unwrap_or(false);

        let classification = if is_terminal {
            CascadeClassification::AlreadyTerminal
        } else {
            match cancel_policy.as_deref() {
                Some("cascade") => CascadeClassification::WillInterrupt,
                Some("detach") => CascadeClassification::WillDetach,
                _ => CascadeClassification::UnknownPolicy,
            }
        };

        rows.push(CascadeWalkRow {
            request_id: child_id.clone(),
            session_id: child_session_id,
            behavior_id: child_behavior_id,
            lifecycle_state: child_lifecycle_state,
            parent_request_id: Some(parent_request_id.to_string()),
            parent_tool_call_id: tool_call_id,
            tool_name,
            await_mode,
            cancel_policy,
            classification,
        });

        let should_recurse = match classification {
            CascadeClassification::WillInterrupt => true,
            CascadeClassification::AlreadyTerminal => include_terminal,
            CascadeClassification::WillDetach | CascadeClassification::UnknownPolicy => false,
        };

        if should_recurse {
            Box::pin(bfs(
                core,
                access,
                &child_id,
                include_terminal,
                depth + 1,
                seen_requests,
                rows,
            ))
            .await?;
        }
    }

    Ok(())
}

async fn fetch_request(
    core: &Arc<ClientCore>,
    access: &GraphqlAccess,
    request_id: &str,
    agent_did: Option<&str>,
) -> Result<Value, String> {
    let escaped = escape_graphql_string(request_id);
    let agent_did_clause = agent_did
        .map(|did| {
            let escaped_did = escape_graphql_string(did);
            format!(r#", agent_did: {{ _eq: "{escaped_did}" }}"#)
        })
        .unwrap_or_default();
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }}{agent_did_clause} }},
                limit: 1
            ) {{
                request_id
                agent_did
                behavior_id
                session_id
                lifecycle_state
                interrupt_requested_at
            }}
        }}"#
    );

    let data = access
        .execute(
            core,
            &query,
            &format!("AgentRequest query for {request_id}"),
        )
        .await?;
    data.get("AgentRequest")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .ok_or_else(|| format!("request {request_id} not found in AgentRequest collection"))
}

pub async fn build_cascade_preview(
    core: &Arc<ClientCore>,
    req: &DesktopPreviewInterruptCascadeRequest,
) -> Result<CascadeCancelPreview, String> {
    let walk_req = CascadeWalkRequest {
        root_request_id: req.request_id.clone(),
        agent_did: req.agent_did.clone(),
        include_terminal: req.include_terminal.unwrap_or(true),
    };
    let result = walk(core, &walk_req).await?;

    let mut will_interrupt = Vec::new();
    let mut will_detach = Vec::new();
    let mut already_terminal = Vec::new();
    let mut unknown_policy = Vec::new();
    let mut sig_rows = Vec::new();

    for row in &result.rows {
        let view = CascadeAffectedRequest {
            request_id: row.request_id.clone(),
            session_id: row.session_id.clone(),
            behavior_id: row.behavior_id.clone(),
            lifecycle_state: row.lifecycle_state.clone(),
            parent_request_id: row.parent_request_id.clone(),
            parent_tool_call_id: row.parent_tool_call_id.clone(),
            tool_name: row.tool_name.clone(),
            await_mode: row.await_mode.clone(),
            cancel_policy: row.cancel_policy.clone(),
        };
        sig_rows.push(PreviewSignatureRow {
            request_id: row.request_id.clone(),
            lifecycle_state: row.lifecycle_state.clone(),
            await_mode: row.await_mode.clone(),
            cancel_policy: row.cancel_policy.clone(),
            parent_tool_call_id: row.parent_tool_call_id.clone(),
        });
        match row.classification {
            CascadeClassification::WillInterrupt => will_interrupt.push(view),
            CascadeClassification::WillDetach => will_detach.push(view),
            CascadeClassification::AlreadyTerminal => already_terminal.push(view),
            CascadeClassification::UnknownPolicy => unknown_policy.push(view),
        }
    }

    let preview_signature = compute_preview_signature(&PreviewSignatureInput {
        root_request_id: req.request_id.clone(),
        root_state: result.root_state.clone(),
        root_interrupt_requested_at: result.root_interrupt_requested_at.clone(),
        affected: sig_rows,
    });

    Ok(CascadeCancelPreview {
        root_request_id: req.request_id.clone(),
        preview_signature,
        root_state: result.root_state,
        will_interrupt,
        will_detach,
        already_terminal,
        unknown_policy,
    })
}

#[derive(Debug, Clone)]
pub struct LatchResult {
    pub interrupt_requested_at: String,
    pub was_first: bool,
}

/// Latches `interrupt_requested_at` on the root `AgentRequest` identified by
/// `request_id`.
///
/// - If the field is already present, returns `LatchResult { was_first: false,
///   interrupt_requested_at: <existing> }` without issuing a mutation.
/// - Otherwise writes `chrono::Utc::now().to_rfc3339()` and returns
///   `LatchResult { was_first: true, interrupt_requested_at: <now> }`.
///
/// Mirrors `interrupt_request_graphql` in
/// `crates/gents-cli/src/commands/subagent.rs:167-200`.
pub async fn latch_root_interrupt(
    core: &Arc<ClientCore>,
    request_id: &str,
    agent_did: Option<&str>,
) -> Result<LatchResult, String> {
    let access = GraphqlAccess::for_agent(core, agent_did).await?;

    let row = fetch_request(core, &access, request_id, agent_did)
        .await
        .map_err(|e| format!("latch_root_interrupt: {e}"))?;

    if let Some(existing) = string_field(&row, "interrupt_requested_at") {
        return Ok(LatchResult {
            interrupt_requested_at: existing,
            was_first: false,
        });
    }

    let now = chrono::Utc::now().to_rfc3339();
    let escaped_id = escape_graphql_string(request_id);
    let escaped_now = escape_graphql_string(&now);
    let agent_did_clause = agent_did
        .map(str::trim)
        .filter(|agent_did| !agent_did.is_empty())
        .map(|agent_did| {
            let escaped_agent_did = escape_graphql_string(agent_did);
            format!(r#", agent_did: {{ _eq: "{escaped_agent_did}" }}"#)
        })
        .unwrap_or_default();
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_id}" }}{agent_did_clause} }},
                input: {{ interrupt_requested_at: "{escaped_now}" }}
            ) {{ _docID }}
        }}"#
    );

    access
        .execute(core, &mutation, "latch_root_interrupt update_AgentRequest")
        .await?;

    Ok(LatchResult {
        interrupt_requested_at: now,
        was_first: true,
    })
}

pub async fn interrupt_request(
    core: &Arc<ClientCore>,
    req: &DesktopInterruptRequest,
) -> Result<InterruptRequestResult, String> {
    if req.cause != "userCancelled" {
        return Err(format!(
            "operator may only authentically produce cause=\"userCancelled\", got {:?}",
            req.cause
        ));
    }

    if !req.cascade {
        let latched = latch_root_interrupt(core, &req.request_id, req.agent_did.as_deref()).await?;
        return Ok(InterruptRequestResult {
            request_id: req.request_id.clone(),
            accepted: true,
            interrupt_requested_at: Some(latched.interrupt_requested_at),
            already_interrupted: !latched.was_first,
            stale_preview: false,
            preview: None,
        });
    }

    let expected_sig = req
        .expected_preview_signature
        .clone()
        .ok_or_else(|| "cascade=true requires expectedPreviewSignature".to_string())?;
    let preview = build_cascade_preview(
        core,
        &DesktopPreviewInterruptCascadeRequest {
            request_id: req.request_id.clone(),
            agent_did: req.agent_did.clone(),
            include_terminal: Some(true),
        },
    )
    .await?;
    if preview.preview_signature != expected_sig {
        return Ok(InterruptRequestResult {
            request_id: req.request_id.clone(),
            accepted: false,
            interrupt_requested_at: None,
            already_interrupted: false,
            stale_preview: true,
            preview: Some(preview),
        });
    }

    let access = GraphqlAccess::for_agent(core, req.agent_did.as_deref()).await?;
    let latched = latch_root_interrupt(core, &req.request_id, req.agent_did.as_deref()).await?;
    latch_cascade_descendants(core, &access, &preview).await?;
    Ok(InterruptRequestResult {
        request_id: req.request_id.clone(),
        accepted: true,
        interrupt_requested_at: Some(latched.interrupt_requested_at),
        already_interrupted: !latched.was_first,
        stale_preview: false,
        preview: None,
    })
}

async fn latch_cascade_descendants(
    core: &Arc<ClientCore>,
    access: &GraphqlAccess,
    preview: &CascadeCancelPreview,
) -> Result<(), String> {
    for child in &preview.will_interrupt {
        let escaped_request_id = escape_graphql_string(&child.request_id);
        let interrupted_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                    input: {{ interrupt_requested_at: "{interrupted_at}" }}
                ) {{ _docID }}
            }}"#
        );
        access
            .execute(
                core,
                &mutation,
                &format!(
                    "cascade interrupt update_AgentRequest for {}",
                    child.request_id
                ),
            )
            .await?;
        tracing::info!(
            root_request_id = %preview.root_request_id,
            child_request_id = %child.request_id,
            "cascade interrupt latched descendant request"
        );
    }
    Ok(())
}

fn string_field(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

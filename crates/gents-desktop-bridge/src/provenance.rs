
use std::collections::BTreeMap;
use std::sync::Arc;

use gents::config_client::ConfigAccess;
use gents::graphql::graphql_string_list_literal;
use gents::session::{public_request_filter, session_scope_filter};
use gents::session_origin::{caused_requests, lineage, SessionLink, SessionScope};
use gents::toolset::{AGENT_MESSAGE_TOOL_NAME, AGENT_NEW_TOOL_NAME};
use gents_desktop_core::client::ClientCore;
use serde_json::Value;

use crate::types::{
    CausedCallView, CausedRequestView, DesktopSessionProvenanceRequest, LinkedSessionView,
    SessionProvenanceView, TurnSenderView,
};

pub async fn session_provenance_request(
    core: &Arc<ClientCore>,
    request: DesktopSessionProvenanceRequest,
) -> Result<SessionProvenanceView, String> {
    let node_did = request
        .node_did
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| core.selected_node_did())
        .ok_or("no node selected; pass nodeDid explicitly")?;
    session_provenance(
        core,
        SessionScope {
            node_did,
            session_id: request.session_id,
            requester_did: request.requester_did,
        },
    )
    .await
}

pub async fn session_provenance(
    core: &Arc<ClientCore>,
    scope: SessionScope,
) -> Result<SessionProvenanceView, String> {
    let access = ConfigAccess::local(core.node_arc());
    let fail = |error: anyhow::Error| format!("session provenance: {error:#}");
    let lineage = lineage(&access, &scope).await.map_err(fail)?;
    let calls = caused_calls(&access, &scope).await.map_err(fail)?;
    let senders = turn_senders(&lineage);
    let link = |link: &SessionLink| LinkedSessionView {
        node_did: link.scope.node_did.clone(),
        session_id: link.scope.session_id.clone(),
        requester_did: link.scope.requester_did.clone(),
        cause_request_doc_id: link.cause_request_doc_id.clone(),
    };
    Ok(SessionProvenanceView {
        session_id: scope.session_id.clone(),
        started_by: lineage.started_by.as_ref().map(link),
        started: lineage.started.iter().map(link).collect(),
        sent: lineage.sent.iter().map(link).collect(),
        received: lineage.received.iter().map(link).collect(),
        senders: senders
            .into_iter()
            .map(|(request_id, sender)| TurnSenderView {
                request_id,
                sender: link(sender),
            })
            .collect(),
        calls,
    })
}

async fn rows(access: &ConfigAccess, query: &str, collection: &str) -> anyhow::Result<Vec<Value>> {
    Ok(access
        .execute(query)
        .await?
        .pointer(&format!("/data/{collection}"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

fn text(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

async fn caused_calls(
    access: &ConfigAccess,
    scope: &SessionScope,
) -> anyhow::Result<Vec<CausedCallView>> {
    let own = session_scope_filter(
        &scope.node_did,
        &scope.session_id,
        scope.requester_did.as_deref(),
    );
    let tools = graphql_string_list_literal([AGENT_NEW_TOOL_NAME, AGENT_MESSAGE_TOOL_NAME]);
    let calls = rows(
        access,
        &format!(
            "{{AgentToolCall(filter: {{{own}, tool_name: {{_in: {tools}}}}}) {{_docID request_id tool_call_id}}}}"
        ),
        "AgentToolCall",
    )
    .await?;
    let doc_ids = calls
        .iter()
        .filter_map(|row| row.get("_docID").and_then(Value::as_str))
        .collect::<Vec<_>>();
    let caused = caused_requests(access, doc_ids.iter().copied()).await?;
    if caused.is_empty() {
        return Ok(Vec::new());
    }
    let states = rows(
        access,
        &format!(
            "{{AgentRequest(filter: {{{}}}) {{request_id node_did session_id requester_did lifecycle_state created_at caused_by_parent_tool_call_doc_id}}}}",
            public_request_filter(&format!(
                "request_id: {{_in: {}}}, caused_by_parent_tool_call_doc_id: {{_in: {}}}",
                graphql_string_list_literal(caused.values().map(String::as_str)),
                graphql_string_list_literal(caused.keys().map(String::as_str)),
            ))
        ),
        "AgentRequest",
    )
    .await?;
    let by_call = states
        .iter()
        .filter_map(|row| Some((text(row, "caused_by_parent_tool_call_doc_id")?, row)))
        .filter(|(call, row)| caused.get(call) == text(row, "request_id").as_ref())
        .collect::<BTreeMap<_, _>>();
    Ok(calls
        .iter()
        .filter_map(|call| {
            let request = by_call.get(&text(call, "_docID")?)?;
            Some(CausedCallView {
                request_id: text(call, "request_id")?,
                tool_call_id: text(call, "tool_call_id")?,
                caused: CausedRequestView {
                    request_id: text(request, "request_id")?,
                    node_did: text(request, "node_did")?,
                    session_id: text(request, "session_id")?,
                    requester_did: text(request, "requester_did"),
                    lifecycle_state: text(request, "lifecycle_state"),
                    created_at: text(request, "created_at"),
                },
            })
        })
        .collect())
}

fn turn_senders<'a>(
    lineage: &'a gents::session_origin::SessionLineage,
) -> Vec<(String, &'a SessionLink)> {
    let links = lineage
        .started_by
        .iter()
        .chain(lineage.received.iter())
        .collect::<Vec<_>>();
    if links.is_empty() {
        return Vec::new();
    }
    lineage
        .request_causes
        .iter()
        .filter_map(|request| {
            let sender = links
                .iter()
                .find(|link| link.cause_request_doc_id == request.cause_request_doc_id)
                .or_else(|| {
                    let cause_scope = request.cause_scope.as_ref()?;
                    links.iter().find(|link| link.scope == *cause_scope)
                })?;
            Some((request.request_id.clone(), *sender))
        })
        .collect()
}

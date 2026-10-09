// The desktop's view of one session's lineage. `gents::session_origin` owns
// the rule (`lineage` for the session links, `caused_requests` for the
// request each call caused); this module maps its answers onto the view and
// reads only the rows those answers name. It confers no hierarchy, cascade
// or authority.

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

/// `desktop_session_provenance`: the requested session scope, under the
/// selected node when the request names none.
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
    let access = ConfigAccess::Local(core.node_arc());
    let fail = |error: anyhow::Error| format!("session provenance: {error:#}");
    let lineage = lineage(&access, &scope).await.map_err(fail)?;
    let calls = caused_calls(&access, &scope).await.map_err(fail)?;
    let senders = turn_senders(core, &scope, &lineage);
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

/// This session's `agent_new`/`agent_message` calls, each with the request
/// the lineage owner says it caused and that request's state.
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
    // The state of exactly the requests the owner named: each is the one
    // public request carrying its call's physical edge.
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

/// Which lineage session sent each of this session's requests. A request
/// names its causing request document; that document is either the link's
/// own cause or a request the desktop store holds, whose session must be one
/// the lineage owner linked. A cause neither names stays unattributed.
fn turn_senders<'a>(
    core: &ClientCore,
    scope: &SessionScope,
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
    let store = core.store().snapshot();
    let scope_of = |doc_id: &str| {
        store
            .requests
            .iter()
            .find(|row| row.doc_id.as_deref() == Some(doc_id))
            .and_then(|row| {
                Some(SessionScope {
                    node_did: row.node_did.clone()?,
                    session_id: row.session_id.clone()?,
                    requester_did: row.requester_did.clone(),
                })
            })
    };
    store
        .requests
        .iter()
        .filter(|row| {
            row.node_did.as_deref() == Some(scope.node_did.as_str())
                && row.session_id.as_deref() == Some(scope.session_id.as_str())
                && row.requester_did == scope.requester_did
        })
        .filter_map(|row| {
            let cause = row.caused_by_parent_request_doc_id.as_deref()?;
            let sender = links
                .iter()
                .find(|link| link.cause_request_doc_id == cause)
                .or_else(|| {
                    let cause_scope = scope_of(cause)?;
                    links.iter().find(|link| link.scope == cause_scope)
                })?;
            Some((row.request_id.clone(), *sender))
        })
        .collect()
}

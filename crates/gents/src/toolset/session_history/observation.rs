use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;
use serde_json::Value;

use super::inference::{
    aggregate_inference_usage, inference_turns, latest_context_from_detail_rows,
    INFERENCE_DETAIL_FIELDS,
};
use super::investigation::session_investigation_calls_query;
use super::{
    clean, context_calls, decode, InvestigationCallsEnvelope, RequestContextObservation,
    SessionInferenceObservation,
};
use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};

pub async fn load_request_context_observation(
    node: &EmbeddedNode,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    request_id: &str,
) -> Result<Option<RequestContextObservation>> {
    anyhow::ensure!(
        !node_did.trim().is_empty()
            && !session_id.trim().is_empty()
            && !request_id.trim().is_empty(),
        "request accounting requires a node, session, and request"
    );
    let requester = requester_did
        .map(|did| format!(r#""{}""#, escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    let response = graphql_with_transaction_retry(node, &format!(r#"{{ AgentRequest(filter: {{
        purpose: {{_eq: "normal"}}, request_id: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, node_did: {{_eq: "{}"}}, requester_did: {{_eq: {requester}}}
    }}, limit: 2) {{_docID request_id node_did requester_did session_id}} }}"#, escape_graphql_string(request_id), escape_graphql_string(session_id), escape_graphql_string(node_did)), "context request ownership").await?;
    let requests: Vec<AgentRequestRow> = serde_json::from_value(
        response
            .data
            .as_ref()
            .and_then(|v| v.get("AgentRequest"))
            .cloned()
            .context("missing context owner rows")?,
    )?;
    let owner = match requests.as_slice() {
        [] => return Ok(None),
        [owner] => owner,
        _ => bail!("ambiguous context request identity"),
    };
    load_pinned_request_context_observation(node, owner).await
}

pub async fn load_pinned_request_context_observation(
    node: &EmbeddedNode,
    owner: &AgentRequestRow,
) -> Result<Option<RequestContextObservation>> {
    let node_did = owner
        .node_did
        .as_deref()
        .filter(|id| !id.is_empty())
        .context("context request lacks node")?;
    anyhow::ensure!(
        owner.session_id.as_deref().is_some_and(|id| !id.is_empty())
            && !owner.request_id.is_empty(),
        "context request lacks session or request identity"
    );
    let doc = owner
        .doc_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .context("context request lacks physical identity")?;
    let scope = crate::session::session_scope_filter(
        node_did,
        owner.session_id.as_deref().unwrap(),
        owner.requester_did.as_deref(),
    );
    let selected = graphql_with_transaction_retry(node, &format!(r#"{{ AgentRequest(filter: {{ {scope}, purpose: {{_eq: "normal"}}, _docID: {{_eq: "{}"}}, request_id: {{_eq: "{}"}} }}, limit: 2) {{_docID}} }}"#, escape_graphql_string(doc), escape_graphql_string(&owner.request_id)), "physical context request ownership").await?;
    let rows = selected
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .context("missing physical context owner rows")?;
    match rows.as_slice() {
        [] => return Ok(None),
        [row] if row["_docID"].as_str() == Some(doc) => {}
        _ => bail!("invalid physical context request identity"),
    }
    let response = graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ InferenceCall(filter: {{
        node_did: {{_eq: "{}"}}, request_doc_id: {{_eq: "{}"}}, call_kind: {{_eq: "inference"}}
    }}, order: [{{queued_at: DESC}}, {{call_seq: DESC}}, {{call_id: DESC}}], limit: 1) {{
        {INFERENCE_DETAIL_FIELDS}
    }} }}"#,
            escape_graphql_string(node_did),
            escape_graphql_string(doc)
        ),
        "live context observation",
    )
    .await?;
    let calls: InvestigationCallsEnvelope =
        decode(response.data.as_ref(), "live context observation")?;
    Ok(
        latest_context_from_detail_rows(&calls.inference_calls)?.map(|context| {
            RequestContextObservation {
                context,
                completion_tokens: calls
                    .inference_calls
                    .first()
                    .and_then(|row| row.usage.completion_tokens)
                    .and_then(|tokens| u64::try_from(tokens).ok()),
            }
        }),
    )
}

/// The caller supplies an already-authorized session scope. Physical request
/// document IDs, not logical request aliases, fence every inference read.
/// Missing requester is an exact null identity, never a wildcard.
pub async fn load_session_inference_observation(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
) -> Result<SessionInferenceObservation> {
    let ids = load_session_request_doc_ids(
        &crate::config_client::ConfigAccess::local(Arc::clone(node)),
        node_did,
        requester_did,
        session_id,
    )
    .await?
    .into_keys()
    .collect::<Vec<_>>();
    let mut rows = Vec::new();
    for batch in ids.chunks(128) {
        let response = graphql_with_transaction_retry(
            node,
            &session_investigation_calls_query(node_did, batch),
            "session inference observations",
        )
        .await?;
        let calls: InvestigationCallsEnvelope =
            decode(response.data.as_ref(), "session inference observations")?;
        rows.extend(calls.inference_calls);
    }
    let latest_context = latest_context_from_detail_rows(&rows)?;
    let latest_completion_tokens = latest_context.as_ref().and_then(|context| {
        rows.iter()
            .find(|row| row.call_id == context.call_id)
            .and_then(|row| row.usage.completion_tokens)
            .and_then(|tokens| u64::try_from(tokens).ok())
    });
    Ok(SessionInferenceObservation {
        token_usage: aggregate_inference_usage(&rows)?,
        inference_turns: inference_turns(&rows)?,
        latest_context,
        latest_completion_tokens,
    })
}

pub async fn load_session_request_doc_ids(
    access: &crate::config_client::ConfigAccess,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
) -> Result<BTreeMap<String, String>> {
    load_session_request_bindings(
        access,
        node_did,
        requester_did,
        session_id,
        RequestBindings::All,
    )
    .await
}

pub(super) enum RequestBindings<'a> {
    All,
    Physical(&'a [String]),
    Page(Option<&'a str>),
}

pub(super) async fn load_session_request_bindings(
    access: &dyn crate::config_client::ConfigRead,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    selection: RequestBindings<'_>,
) -> Result<BTreeMap<String, String>> {
    anyhow::ensure!(
        !node_did.trim().is_empty() && !session_id.trim().is_empty(),
        "session request selection requires a node and session"
    );
    let requester = requester_did
        .map(|did| format!(r#""{}""#, escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    let (physical_filter, limit) = match selection {
        RequestBindings::All => (String::new(), None),
        RequestBindings::Physical(ids) => {
            if ids.is_empty() {
                return Ok(BTreeMap::new());
            }
            let ids = ids
                .iter()
                .map(|id| format!(r#""{}""#, escape_graphql_string(id)))
                .collect::<Vec<_>>()
                .join(", ");
            (format!("_docID: {{_in: [{ids}]}},"), None)
        }
        RequestBindings::Page(after) => (
            after
                .map(|id| format!(r#"_docID: {{_gt: "{}"}},"#, escape_graphql_string(id)))
                .unwrap_or_default(),
            Some(context_calls::PAGE_SIZE),
        ),
    };
    let filter = format!(
        r#"
                {physical_filter}
                purpose: {{_eq: "normal"}},
                node_did: {{_eq: "{}"}},
                session_id: {{_eq: "{}"}},
                requester_did: {{_eq: {requester}}}
            "#,
        escape_graphql_string(node_did.trim()),
        escape_graphql_string(session_id.trim())
    );
    let query = match limit {
        Some(limit) => format!("{{ AgentRequest(filter: {{{filter}}}, order: {{_docID: ASC}}, limit: {limit}) {{ _docID request_id }} }}"),
        None => format!("{{ AgentRequest(filter: {{{filter}}}) {{ _docID request_id }} }}"),
    };
    let data = access
        .execute_read(&query)
        .await
        .context("session accounting ownership")?;
    let requests: Vec<PhysicalRequestDocIdRow> = serde_json::from_value(
        data.pointer("/data/AgentRequest")
            .cloned()
            .context("missing accounting request rows")?,
    )?;
    requests
        .into_iter()
        .map(|request| {
            Ok((
                clean(request.doc_id.as_ref())
                    .context("accounting request lacks physical identity")?,
                clean(request.request_id.as_ref())
                    .context("accounting request lacks logical identity")?,
            ))
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct PhysicalRequestDocIdRow {
    #[serde(rename = "_docID", default)]
    doc_id: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
}

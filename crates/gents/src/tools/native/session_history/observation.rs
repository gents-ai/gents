use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::{clean, context_calls};
use crate::store::graphql::escape_graphql_string;

pub(super) enum RequestBindings<'a> {
    Physical(&'a [String]),
    Page(Option<&'a str>),
}

pub(super) async fn load_session_request_bindings(
    access: &dyn crate::store::config_client::ConfigRead,
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

use rapidhash::RapidHashSet;

use crate::session::load_latest_request_in_txn;
use crate::session::origin::{
    lineage, load_session, starting_request, SessionLink, REQUEST_FIELDS,
};
use anyhow::{Context, Result};
use gents_protocol::row::AgentRequestRow;
use gents_protocol::session::SessionScope;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct CausedSession {
    pub scope: SessionScope,
    pub agent_did: String,
    pub caused_by_scope: SessionScope,
    pub depth: u32,
    pub first: AgentRequestRow,
    pub latest: AgentRequestRow,
}

pub async fn load_caused_sessions_as(
    access: &ConfigAccess,
    roots: &[SessionScope],
) -> Result<Vec<CausedSession>> {
    let mut visited = roots.iter().cloned().collect::<RapidHashSet<_>>();
    let mut frontier = roots
        .iter()
        .map(|scope| (scope.clone(), 0_u32))
        .collect::<Vec<_>>();
    let mut caused = Vec::new();
    while let Some((scope, depth)) = frontier.pop() {
        for link in lineage(access, &scope).await?.started {
            if !visited.insert(link.scope.clone()) {
                continue;
            }
            let child = link.scope.clone();
            if let Some(session) = view(access, link, scope.clone(), depth + 1).await? {
                caused.push(session);
                frontier.push((child, depth + 1));
            }
        }
    }
    caused.sort_by(|left, right| {
        left.depth
            .cmp(&right.depth)
            .then_with(|| left.first.created_at.cmp(&right.first.created_at))
            .then_with(|| left.scope.cmp(&right.scope))
    });
    Ok(caused)
}

pub async fn started_by_request(
    access: &ConfigAccess,
    cause: &AgentRequestRow,
) -> Result<Vec<(SessionLink, AgentRequestRow)>> {
    let cause_doc_id = cause
        .doc_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .context("started sessions require a physical request")?;
    let mut started = Vec::new();
    for link in lineage(access, &row_scope(cause)?).await?.started {
        if link.cause_request_doc_id != cause_doc_id {
            continue;
        }
        if let Some(first) = starting_request(access, &link).await? {
            started.push((link, first));
        }
    }
    Ok(started)
}

async fn view(
    access: &ConfigAccess,
    link: SessionLink,
    caused_by_scope: SessionScope,
    depth: u32,
) -> Result<Option<CausedSession>> {
    let Some(session) = load_session(
        access,
        &link.scope.session_id,
        link.scope.requester_did.as_deref(),
    )
    .await?
    .filter(|session| SessionScope::from(session) == link.scope) else {
        return Ok(None);
    };
    let Some(first) = starting_request(access, &link).await? else {
        return Ok(None);
    };
    let latest = load_session_head_as(access, &link.scope)
        .await?
        .unwrap_or_else(|| first.clone());
    Ok(Some(CausedSession {
        scope: link.scope,
        agent_did: session.agent_did,
        caused_by_scope,
        depth,
        first,
        latest,
    }))
}

async fn load_session_head_as(
    access: &ConfigAccess,
    scope: &SessionScope,
) -> Result<Option<AgentRequestRow>> {
    let head = access
        .transact_readonly("caused_sessions.head", |txn| {
            Box::pin(async move {
                load_latest_request_in_txn(
                    txn,
                    &scope.node_did,
                    &scope.session_id,
                    Some(scope.requester_did.as_deref()),
                )
                .await
            })
        })
        .await?;
    let Some(head) = head else {
        return Ok(None);
    };
    let query = format!(
        r#"{{AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ {REQUEST_FIELDS} }}}}"#,
        escape_graphql_string(&head.observed.request_doc_id)
    );
    let mut heads = request_rows(access, &query).await?;
    anyhow::ensure!(heads.len() <= 1, "ambiguous caused session head");
    Ok(heads.pop())
}

async fn request_rows(access: &ConfigAccess, query: &str) -> Result<Vec<AgentRequestRow>> {
    access
        .execute(query)
        .await?
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|row| serde_json::from_value(row).context("decoding AgentRequest row"))
        .collect()
}

fn row_scope(row: &AgentRequestRow) -> Result<SessionScope> {
    Ok(SessionScope {
        node_did: row
            .node_did
            .clone()
            .context("AgentRequest row omitted node_did")?,
        session_id: row
            .session_id
            .clone()
            .context("AgentRequest row omitted session_id")?,
        requester_did: row.requester_did.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::lineage_fixtures::{node_builder, seed_lineage_case, FIXTURE_DID};
    use std::sync::Arc;

    #[tokio::test]
    async fn generated_plugin_job_lineage_cases_drive_caused_session_readers() {
        let snapshot: Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let did = FIXTURE_DID;
        for case in snapshot["plugin_job_lineage_cases"].as_array().unwrap() {
            let dir = tempfile::tempdir().unwrap();
            let node = Arc::new(
                node_builder()
                    .data_path(dir.path().join("node"))
                    .build()
                    .await
                    .unwrap(),
            );
            gents_store::schema::ensure_runtime_schemas(node.as_ref())
                .await
                .unwrap();
            let ids = seed_lineage_case(&node, case, did).await;
            let access = ConfigAccess::local(node.clone());
            for query in case["queries"].as_array().unwrap() {
                let cause = query["cause_request_doc_id"].as_str().unwrap();
                let link = SessionLink {
                    scope: SessionScope {
                        node_did: did.into(),
                        session_id: query["session_id"].as_str().unwrap().into(),
                        requester_did: Some(did.into()),
                    },
                    cause_request_doc_id: ids.get(cause).cloned().unwrap_or(cause.into()),
                };
                let expected = query["expected_opening_doc_id"]
                    .as_str()
                    .map(|id| ids[id].clone());
                let found = starting_request(&access, &link)
                    .await
                    .unwrap()
                    .and_then(|row| row.doc_id);
                assert_eq!(found, expected, "{}: {query}", case["name"]);
            }
            node.shutdown().await.expect("node shutdown");
        }
    }
}

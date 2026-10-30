use rapidhash::RapidHashSet;
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use super::{
    clamp_limit, clean, decode, quoted_graphql_list, CompactionRow, SessionHistoryRow,
    SessionHistorySnapshot, SessionRow,
};
use crate::graphql::escape_graphql_string;

pub async fn load_session_history_snapshot(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    limit: Option<usize>,
) -> Result<SessionHistorySnapshot> {
    let agent = node_did.trim().to_owned();
    anyhow::ensure!(!agent.is_empty(), "session history requires a node DID");
    crate::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "session.history",
        move |txn| {
            let agent = agent.clone();
            Box::pin(async move { load_session_history_in_txn(txn, &agent, limit).await })
        },
    )
    .await
}

pub async fn load_session_history_snapshot_with_access(
    access: &crate::config_client::ConfigAccess,
    node_did: &str,
    limit: Option<usize>,
) -> Result<SessionHistorySnapshot> {
    let agent = node_did.trim().to_owned();
    anyhow::ensure!(!agent.is_empty(), "session history requires a node DID");
    access
        .transact_readonly("session.history", move |txn| {
            let agent = agent.clone();
            Box::pin(async move { load_session_history_in_txn(txn, &agent, limit).await })
        })
        .await
}

async fn load_session_history_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    limit: Option<usize>,
) -> Result<SessionHistorySnapshot> {
    let limit = clamp_limit(limit);
    let response = txn.execute(&session_index_query(node_did, limit)).await?;
    let envelope: SessionIndexEnvelope = decode(response.get("data"), "session index")?;
    let ids = envelope
        .sessions
        .into_iter()
        .map(|row| row.session_id)
        .collect::<Vec<_>>();
    let sessions = if ids.is_empty() {
        Vec::new()
    } else {
        let response = txn.execute(&session_detail_query(node_did, &ids)).await?;
        let envelope: SessionDetailEnvelope =
            decode(response.get("data"), "session history details")?;
        let request_doc_ids = envelope
            .sessions
            .iter()
            .filter_map(|session| session.observation.as_ref())
            .filter_map(|observation| observation.latest_request.as_ref())
            .filter_map(|request| clean(Some(&request.request_doc_id)))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let request_timestamps = if request_doc_ids.is_empty() {
            BTreeMap::new()
        } else {
            let response = txn
                .execute(&session_request_timestamps_query(
                    node_did,
                    &ids,
                    &request_doc_ids,
                ))
                .await?;
            let envelope: SessionRequestTimestampsEnvelope =
                decode(response.get("data"), "session request timestamps")?;
            envelope
                .requests
                .into_iter()
                .filter_map(|row| clean(row.doc_id.as_ref()).map(|doc_id| (doc_id, row.created_at)))
                .collect()
        };
        build_session_rows(&ids, envelope, &request_timestamps)?
    };
    Ok(SessionHistorySnapshot {
        node_did: node_did.to_owned(),
        limit,
        sessions,
    })
}

fn session_index_query(node_did: &str, limit: usize) -> String {
    format!(
        r#"{{AgentSession(filter: {{node_did: {{_eq: "{}"}}}}, order: [{{created_at: DESC}},{{session_id: DESC}}], limit: {limit}) {{session_id}}}}"#,
        escape_graphql_string(node_did)
    )
}

fn session_detail_query(node_did: &str, session_ids: &[String]) -> String {
    let node_did = escape_graphql_string(node_did);
    let list = quoted_graphql_list(session_ids);
    format!(
        r#"{{
            AgentSession(filter: {{ _and: [
                {{ node_did: {{ _eq: "{node_did}" }} }},
                {{ session_id: {{ _in: [{list}] }} }}
            ] }}) {{
                session_id
                agent_id
                title
                tags
                created_at
                closed_at
                observation
            }}
            AgentRequest(
                filter: {{ _and: [
                    {{ purpose: {{ _eq: "normal" }} }},
                    {{ node_did: {{ _eq: "{node_did}" }} }},
                    {{ session_id: {{ _in: [{list}] }} }}
                ] }}
                groupBy: [session_id]
            ) {{
                session_id
                request_count: COUNT(GROUP: {{}})
            }}
            AgentMessage(
                filter: {{ _and: [
                {{ node_did: {{ _eq: "{node_did}" }} }},
                {{ session_id: {{ _in: [{list}] }} }}
                ] }}
                groupBy: [session_id]
            ) {{
                session_id
                message_count: COUNT(GROUP: {{}})
                latest: GROUP(
                    filter: {{ _and: [
                        {{ created_at: {{ _neq: null }} }},
                        {{ created_at: {{ _neq: "" }} }}
                    ] }},
                    order: [{{created_at: DESC}}],
                    limit: 1
                ) {{
                    created_at
                }}
            }}
            CompactionEntry(
                filter: {{ _and: [
                    {{ node_did: {{ _eq: "{node_did}" }} }},
                    {{ session_id: {{ _in: [{list}] }} }}
                ] }}
                groupBy: [session_id]
            ) {{
                session_id
                compaction_count: COUNT(GROUP: {{}})
                latest: GROUP(
                    filter: {{ _and: [
                        {{ created_at: {{ _neq: null }} }},
                        {{ created_at: {{ _neq: "" }} }}
                    ] }},
                    order: [{{created_at: DESC}}],
                    limit: 1
                ) {{
                    created_at
                }}
            }}
        }}"#
    )
}

fn session_request_timestamps_query(
    node_did: &str,
    session_ids: &[String],
    request_doc_ids: &[String],
) -> String {
    let node_did = escape_graphql_string(node_did);
    let sessions = quoted_graphql_list(session_ids);
    let request_doc_ids = quoted_graphql_list(request_doc_ids);
    format!(
        r#"{{ AgentRequest(filter: {{ _and: [
            {{ purpose: {{ _eq: "normal" }} }},
            {{ node_did: {{ _eq: "{node_did}" }} }},
            {{ session_id: {{ _in: [{sessions}] }} }},
            {{ _docID: {{ _in: [{request_doc_ids}] }} }}
        ] }}) {{ _docID created_at }} }}"#
    )
}

fn summary_timestamp(value: Option<&String>, label: &str) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(value) = clean(Some(value)) else {
        anyhow::ensure!(
            value.is_empty(),
            "{label} timestamp contains only whitespace"
        );
        return Ok(None);
    };
    chrono::DateTime::parse_from_rfc3339(&value)
        .with_context(|| format!("{label} timestamp is not RFC3339"))?;
    Ok(Some(value))
}

fn build_session_rows(
    session_ids: &[String],
    envelope: SessionDetailEnvelope,
    request_timestamps: &BTreeMap<String, Option<String>>,
) -> Result<Vec<SessionHistoryRow>> {
    let SessionDetailEnvelope {
        sessions,
        requests,
        messages,
        compactions,
    } = envelope;
    let sessions_by_id = sessions
        .into_iter()
        .filter_map(|session| clean(Some(&session.session_id)).map(|id| (id, session)))
        .collect::<BTreeMap<_, _>>();
    let session_id_set = session_ids.iter().cloned().collect::<RapidHashSet<_>>();
    let aggregates = aggregate_by_session(requests, messages, compactions, &session_id_set)?;

    session_ids
        .iter()
        .filter_map(|session_id| {
            sessions_by_id
                .get(session_id)
                .map(|session| (session_id, session))
        })
        .map(|(session_id, session)| {
            let aggregate = aggregates.get(session_id);
            let latest_request = session
                .observation
                .as_ref()
                .and_then(|observation| observation.latest_request.as_ref());
            let latest_request_created_at = latest_request
                .and_then(|request| request_timestamps.get(&request.request_doc_id))
                .and_then(Option::as_ref);

            Ok(SessionHistoryRow {
                is_current: false,
                session_id: session_id.clone(),
                agent_id: clean(session.agent_id.as_ref()),
                title: session.title.clone(),
                tags: session.tags.clone(),
                created_at: clean(session.created_at.as_ref()),
                closed_at: clean(session.closed_at.as_ref()),
                latest_request_id: latest_request
                    .map(|request| request.request_id.trim().to_string())
                    .filter(|request_id| !request_id.is_empty()),
                latest_request_lifecycle_state: latest_request
                    .map(|request| request.lifecycle_state),
                latest_request_created_at: summary_timestamp(
                    latest_request_created_at,
                    "latest session request",
                )?,
                request_count: aggregate
                    .map(|aggregate| aggregate.request_count)
                    .unwrap_or(0),
                message_count: aggregate
                    .map(|aggregate| aggregate.message_count)
                    .unwrap_or(0),
                latest_message_at: aggregate
                    .and_then(|aggregate| aggregate.latest_message_at.clone()),
                compaction_count: aggregate
                    .map(|aggregate| aggregate.compaction_count)
                    .unwrap_or(0),
                last_compacted_at: aggregate
                    .and_then(|aggregate| aggregate.last_compacted_at.clone()),
            })
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
struct SessionAggregate {
    request_count: i64,
    message_count: i64,
    latest_message_at: Option<String>,
    compaction_count: i64,
    last_compacted_at: Option<String>,
}

fn aggregate_by_session(
    requests: Vec<SessionRequestAggregateRow>,
    messages: Vec<SessionMessageAggregateRow>,
    compactions: Vec<SessionCompactionAggregateRow>,
    session_id_set: &RapidHashSet<String>,
) -> Result<BTreeMap<String, SessionAggregate>> {
    let mut aggregates = BTreeMap::<String, SessionAggregate>::new();

    for request in requests {
        let session_id = request.session_id.trim().to_string();
        if !session_id_set.contains(&session_id) {
            continue;
        }
        let aggregate = aggregates.entry(session_id).or_default();
        aggregate.request_count = request.request_count;
    }

    for message in messages {
        let session_id = message.session_id.trim().to_string();
        if !session_id_set.contains(&session_id) {
            continue;
        }
        let aggregate = aggregates.entry(session_id).or_default();
        aggregate.message_count = message.message_count;
        aggregate.latest_message_at = message
            .latest
            .into_iter()
            .next()
            .map(|row| summary_timestamp(row.created_at.as_ref(), "latest session message"))
            .transpose()?
            .flatten();
    }

    for compaction in compactions {
        let session_id = compaction.session_id.trim().to_string();
        if !session_id_set.contains(&session_id) {
            continue;
        }
        let aggregate = aggregates.entry(session_id).or_default();
        aggregate.compaction_count = compaction.compaction_count;
        aggregate.last_compacted_at = compaction
            .latest
            .into_iter()
            .next()
            .map(|row| summary_timestamp(row.created_at.as_ref(), "latest session compaction"))
            .transpose()?
            .flatten();
    }

    Ok(aggregates)
}

#[derive(Debug, Deserialize)]
struct SessionIndexEnvelope {
    #[serde(rename = "AgentSession", default)]
    sessions: Vec<SessionRow>,
}

#[derive(Debug, Deserialize)]
struct SessionDetailEnvelope {
    #[serde(rename = "AgentSession", default)]
    sessions: Vec<SessionRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<SessionRequestAggregateRow>,
    #[serde(rename = "AgentMessage", default)]
    messages: Vec<SessionMessageAggregateRow>,
    #[serde(rename = "CompactionEntry", default)]
    compactions: Vec<SessionCompactionAggregateRow>,
}

#[derive(Debug, Deserialize)]
struct SessionRequestAggregateRow {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    request_count: i64,
}

#[derive(Debug, Deserialize)]
struct SessionRequestTimestampsEnvelope {
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<SessionRequestTimestampRow>,
}

#[derive(Debug, Deserialize)]
struct SessionRequestTimestampRow {
    #[serde(default, rename = "_docID")]
    doc_id: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionMessageAggregateRow {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    message_count: i64,
    #[serde(default)]
    latest: Vec<MessageRow>,
}

#[derive(Debug, Deserialize)]
struct SessionCompactionAggregateRow {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    compaction_count: i64,
    #[serde(default)]
    latest: Vec<CompactionRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct MessageRow {
    #[serde(default)]
    created_at: Option<String>,
}

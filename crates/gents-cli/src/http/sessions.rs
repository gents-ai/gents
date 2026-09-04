use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents::graphql::escape_graphql_string;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::post_graphql;

const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 50;
const REQUEST_SCAN_LIMIT: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SessionHistorySnapshot {
    pub(crate) generated_at: String,
    pub(crate) agent_did: String,
    pub(crate) limit: usize,
    pub(crate) request_scan_limit: usize,
    pub(crate) sessions: Vec<SessionHistoryRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SessionHistoryRow {
    pub(crate) session_id: String,
    pub(crate) agent_name: Option<String>,
    pub(crate) behavior_id: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) started_at: Option<String>,
    pub(crate) ended_at: Option<String>,
    pub(crate) latest_request_id: Option<String>,
    pub(crate) latest_request_lifecycle_state: Option<String>,
    pub(crate) latest_request_created_at: Option<String>,
    pub(crate) request_count: i64,
    pub(crate) message_count: i64,
    pub(crate) latest_message_at: Option<String>,
    pub(crate) compaction_count: i64,
    pub(crate) last_compacted_at: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct SessionHistoryParams {
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RecentEnvelope {
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<RequestRow>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DetailsEnvelope {
    #[serde(rename = "AgentSession", default)]
    sessions: Vec<SessionRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<RequestRow>,
    #[serde(rename = "AgentMessage", default)]
    messages: Vec<MessageRow>,
    #[serde(rename = "CompactionEntry", default)]
    compactions: Vec<CompactionRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionRow {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    agent_name: Option<String>,
    #[serde(default)]
    behavior_id: Option<String>,
    #[serde(default)]
    started: Option<String>,
    #[serde(default)]
    ended: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RequestRow {
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    behavior_id: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<gents_protocol::request_lifecycle::RequestLifecycleState>,
    #[serde(default)]
    created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MessageRow {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompactionRow {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
}

pub(crate) async fn load_session_history_snapshot(
    graphql: &str,
    agent_did: &str,
    limit: Option<usize>,
) -> Result<SessionHistorySnapshot> {
    let generated_at = Utc::now();
    let limit = normalize_limit(limit);
    let response = post_graphql(graphql, &recent_requests_query(agent_did)).await?;
    let recent = decode::<RecentEnvelope>(response, "recent sessions")?;
    let session_ids = recent_session_ids(&recent.requests, limit);

    let details = if session_ids.is_empty() {
        DetailsEnvelope {
            sessions: Vec::new(),
            requests: Vec::new(),
            messages: Vec::new(),
            compactions: Vec::new(),
        }
    } else {
        let response =
            post_graphql(graphql, &session_details_query(agent_did, &session_ids)).await?;
        decode::<DetailsEnvelope>(response, "session details")?
    };

    Ok(build_session_history_snapshot(
        generated_at,
        agent_did.to_string(),
        limit,
        session_ids,
        details,
    ))
}

fn normalize_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

fn recent_requests_query(agent_did: &str) -> String {
    let agent_did = escape_graphql_string(agent_did);
    format!(
        r#"{{
            AgentRequest(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                order: {{ created_at: DESC }},
                limit: {REQUEST_SCAN_LIMIT}
            ) {{
                request_id
                session_id
                behavior_id
                lifecycle_state
                created_at
            }}
        }}"#
    )
}

fn session_details_query(agent_did: &str, session_ids: &[String]) -> String {
    let agent_did = escape_graphql_string(agent_did);
    let sessions = session_ids
        .iter()
        .map(|id| format!(r#""{}""#, escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"{{
            AgentSession(filter: {{ session_id: {{ _in: [{sessions}] }} }}) {{
                session_id
                agent_name
                behavior_id
                started
                ended
                status
            }}
            AgentRequest(
                filter: {{ _and: [
                    {{ agent_did: {{ _eq: "{agent_did}" }} }},
                    {{ session_id: {{ _in: [{sessions}] }} }}
                ] }},
                order: {{ created_at: DESC }}
            ) {{
                request_id
                session_id
                behavior_id
                lifecycle_state
                created_at
            }}
            AgentMessage(filter: {{ session_id: {{ _in: [{sessions}] }} }}) {{
                session_id
                timestamp
            }}
            CompactionEntry(filter: {{ session_id: {{ _in: [{sessions}] }} }}) {{
                session_id
                created_at
            }}
        }}"#
    )
}

fn decode<T: serde::de::DeserializeOwned>(response: Value, label: &str) -> Result<T> {
    let data = response
        .get("data")
        .filter(|data| data.is_object())
        .cloned()
        .with_context(|| format!("{label} query response missing object data: {response}"))?;
    serde_json::from_value(data).with_context(|| format!("decoding {label} query response"))
}

fn recent_session_ids(requests: &[RequestRow], limit: usize) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut session_ids = Vec::new();
    for request in requests {
        let Some(session_id) = clean(request.session_id.as_deref()) else {
            continue;
        };
        if seen.insert(session_id.clone()) {
            session_ids.push(session_id);
            if session_ids.len() >= limit {
                break;
            }
        }
    }
    session_ids
}

fn build_session_history_snapshot(
    generated_at: DateTime<Utc>,
    agent_did: String,
    limit: usize,
    session_ids: Vec<String>,
    details: DetailsEnvelope,
) -> SessionHistorySnapshot {
    let sessions = details
        .sessions
        .into_iter()
        .filter_map(|session| {
            let session_id = clean(Some(&session.session_id))?;
            Some((session_id, session))
        })
        .collect::<BTreeMap<_, _>>();
    let requests = group_requests(details.requests);
    let messages = count_latest_by_session(details.messages.into_iter().filter_map(|row| {
        clean(row.session_id.as_deref()).map(|session_id| (session_id, row.timestamp))
    }));
    let compactions = count_latest_by_session(details.compactions.into_iter().filter_map(|row| {
        clean(row.session_id.as_deref()).map(|session_id| (session_id, row.created_at))
    }));

    let sessions = session_ids
        .into_iter()
        .map(|session_id| {
            let session = sessions.get(&session_id);
            let requests = requests.get(&session_id).cloned().unwrap_or_default();
            let latest_request = requests.first();
            let (message_count, latest_message_at) =
                messages.get(&session_id).cloned().unwrap_or_default();
            let (compaction_count, last_compacted_at) =
                compactions.get(&session_id).cloned().unwrap_or_default();

            SessionHistoryRow {
                session_id: session_id.clone(),
                agent_name: session.and_then(|row| clean(row.agent_name.as_deref())),
                behavior_id: session
                    .and_then(|row| clean(row.behavior_id.as_deref()))
                    .or_else(|| latest_request.and_then(|row| clean(row.behavior_id.as_deref()))),
                status: session.and_then(|row| clean(row.status.as_deref())),
                started_at: session.and_then(|row| clean(row.started.as_deref())),
                ended_at: session.and_then(|row| clean(row.ended.as_deref())),
                latest_request_id: latest_request.and_then(|row| clean(row.request_id.as_deref())),
                latest_request_lifecycle_state: latest_request
                    .and_then(|row| row.lifecycle_state)
                    .map(|state| state.as_str().to_string()),
                latest_request_created_at: latest_request
                    .and_then(|row| clean(row.created_at.as_deref())),
                request_count: requests.len() as i64,
                message_count,
                latest_message_at,
                compaction_count,
                last_compacted_at,
            }
        })
        .collect();

    SessionHistorySnapshot {
        generated_at: generated_at.to_rfc3339(),
        agent_did,
        limit,
        request_scan_limit: REQUEST_SCAN_LIMIT,
        sessions,
    }
}

fn group_requests(requests: Vec<RequestRow>) -> BTreeMap<String, Vec<RequestRow>> {
    let mut by_session = BTreeMap::<String, Vec<RequestRow>>::new();
    for request in requests {
        let Some(session_id) = clean(request.session_id.as_deref()) else {
            continue;
        };
        by_session.entry(session_id).or_default().push(request);
    }
    for rows in by_session.values_mut() {
        rows.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    }
    by_session
}

fn count_latest_by_session(
    rows: impl Iterator<Item = (String, Option<String>)>,
) -> BTreeMap<String, (i64, Option<String>)> {
    let mut by_session = BTreeMap::<String, (i64, Option<String>)>::new();
    for (session_id, timestamp) in rows {
        let entry = by_session.entry(session_id).or_default();
        entry.0 += 1;
        if timestamp > entry.1 {
            entry.1 = timestamp;
        }
    }
    by_session
}

fn clean(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope(value: Value) -> DetailsEnvelope {
        serde_json::from_value(value).unwrap()
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn builds_agent_scoped_recent_session_history() {
        let recent = vec![
            RequestRow {
                request_id: Some("req-newer".to_string()),
                session_id: Some("session-a".to_string()),
                behavior_id: Some("behavior-a".to_string()),
                lifecycle_state: Some(
                    gents_protocol::request_lifecycle::RequestLifecycleState::Completed,
                ),
                created_at: Some("2026-06-05T10:00:00Z".to_string()),
            },
            RequestRow {
                request_id: Some("req-other".to_string()),
                session_id: Some("session-b".to_string()),
                behavior_id: Some("behavior-b".to_string()),
                lifecycle_state: Some(
                    gents_protocol::request_lifecycle::RequestLifecycleState::Processing,
                ),
                created_at: Some("2026-06-05T09:00:00Z".to_string()),
            },
            RequestRow {
                request_id: Some("req-older".to_string()),
                session_id: Some("session-a".to_string()),
                behavior_id: Some("behavior-a".to_string()),
                lifecycle_state: Some(
                    gents_protocol::request_lifecycle::RequestLifecycleState::Completed,
                ),
                created_at: Some("2026-06-05T08:00:00Z".to_string()),
            },
        ];
        let session_ids = recent_session_ids(&recent, 2);
        let snapshot = build_session_history_snapshot(
            at("2026-06-05T12:00:00Z"),
            "did:key:zAgent".to_string(),
            2,
            session_ids,
            envelope(json!({
                "AgentSession": [
                    {
                        "session_id": "session-a",
                        "agent_name": "amy",
                        "behavior_id": "behavior-a",
                        "started": "2026-06-05T07:59:00Z",
                        "status": "active"
                    },
                    {
                        "session_id": "session-b",
                        "agent_name": "amy",
                        "behavior_id": "behavior-b",
                        "started": "2026-06-05T08:59:00Z",
                        "status": "active"
                    }
                ],
                "AgentRequest": recent,
                "AgentMessage": [
                    { "session_id": "session-a", "timestamp": "2026-06-05T10:01:00Z" },
                    { "session_id": "session-a", "timestamp": "2026-06-05T10:02:00Z" },
                    { "session_id": "session-b", "timestamp": "2026-06-05T09:01:00Z" }
                ],
                "CompactionEntry": [
                    { "session_id": "session-a", "created_at": "2026-06-05T10:03:00Z" }
                ]
            })),
        );

        assert_eq!(snapshot.agent_did, "did:key:zAgent");
        assert_eq!(snapshot.sessions.len(), 2);
        assert_eq!(snapshot.sessions[0].session_id, "session-a");
        assert_eq!(
            snapshot.sessions[0].latest_request_id.as_deref(),
            Some("req-newer")
        );
        assert_eq!(snapshot.sessions[0].request_count, 2);
        assert_eq!(snapshot.sessions[0].message_count, 2);
        assert_eq!(snapshot.sessions[0].compaction_count, 1);
        assert_eq!(
            snapshot.sessions[0].last_compacted_at.as_deref(),
            Some("2026-06-05T10:03:00Z")
        );
        assert_eq!(snapshot.sessions[1].session_id, "session-b");
    }

    #[test]
    fn clamps_session_history_limit() {
        assert_eq!(normalize_limit(None), DEFAULT_LIMIT);
        assert_eq!(normalize_limit(Some(0)), 1);
        assert_eq!(normalize_limit(Some(usize::MAX)), MAX_LIMIT);
    }
}

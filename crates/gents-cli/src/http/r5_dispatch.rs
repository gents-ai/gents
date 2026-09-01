use std::collections::BTreeMap;

use anyhow::{Context, Result};
use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use gents::graphql::escape_graphql_string;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{http::router::RuntimeHttpState, post_graphql};

const SNAPSHOT_SOURCE: &str = "graphql.r5_cross_deployment_dispatch_state";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SubagentDispatchSnapshot {
    pub(crate) generated_at: String,
    pub(crate) source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parent_request_id: Option<String>,
    pub(crate) dispatches: Vec<SubagentDispatchRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SubagentDispatchRow {
    pub(crate) parent_request_id: String,
    pub(crate) child_request_id: String,
    pub(crate) deployment: String,
    pub(crate) behavior_id: String,
    pub(crate) dispatch_state: String,
    pub(crate) started_at: String,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct SubagentDispatchQuery {
    parent_request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SubagentDispatchQueryEnvelope {
    #[serde(rename = "AgentToolCall", default)]
    bridges: Vec<BridgeRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<RequestRow>,
    #[serde(rename = "AgentBehavior", default)]
    behaviors: Vec<BehaviorRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct BridgeRow {
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    args: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    child_request_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RequestRow {
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    agent_did: Option<String>,
    #[serde(default)]
    behavior_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    claimed_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct BehaviorRow {
    #[serde(default)]
    behavior_id: String,
    #[serde(default)]
    agent_did: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SpawnSubagentArgs {
    #[serde(default)]
    behavior_id: Option<String>,
}

pub(crate) async fn subagent_dispatches_handler(
    State(state): State<RuntimeHttpState>,
    Query(query): Query<SubagentDispatchQuery>,
) -> Response {
    let parent_request_id = clean_optional_string(query.parent_request_id.as_deref());
    match load_subagent_dispatch_snapshot(&state.graphql, parent_request_id.as_deref()).await {
        Ok(snapshot) => (StatusCode::OK, Json(snapshot)).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            format!("subagent dispatch snapshot failed: {error:#}"),
        )
            .into_response(),
    }
}

pub(crate) async fn load_subagent_dispatch_snapshot(
    graphql: &str,
    parent_request_id: Option<&str>,
) -> Result<SubagentDispatchSnapshot> {
    let generated_at = Utc::now();
    let parent_request_id = clean_optional_string(parent_request_id);
    let query = subagent_dispatch_query(parent_request_id.as_deref());
    let response = post_graphql(graphql, &query).await?;
    let envelope = decode_subagent_dispatch_query_response(response)?;
    Ok(build_subagent_dispatch_snapshot(
        generated_at,
        parent_request_id,
        envelope,
    ))
}

fn decode_subagent_dispatch_query_response(
    response: Value,
) -> Result<SubagentDispatchQueryEnvelope> {
    let data = response
        .get("data")
        .filter(|data| data.is_object())
        .cloned()
        .with_context(|| {
            format!("subagent dispatch query response missing object data: {response}")
        })?;
    serde_json::from_value(data).context("decoding subagent dispatch query response")
}

fn subagent_dispatch_query(parent_request_id: Option<&str>) -> String {
    let tool_filter = match parent_request_id {
        Some(parent_request_id) => {
            let parent_request_id = escape_graphql_string(parent_request_id);
            format!(
                r#"{{
                    _and: [
                        {{ request_id: {{ _eq: "{parent_request_id}" }} }},
                        {{ tool_name: {{ _eq: "spawn_subagent" }} }},
                        {{ child_request_id: {{ _ne: "" }} }}
                    ]
                }}"#
            )
        }
        None => r#"{
            _and: [
                { tool_name: { _eq: "spawn_subagent" } },
                { child_request_id: { _ne: "" } },
                { lifecycle_state: { _eq: "running" } }
            ]
        }"#
        .to_string(),
    };

    let request_filter = match parent_request_id {
        Some(parent_request_id) => {
            let parent_request_id = escape_graphql_string(parent_request_id);
            format!(r#"{{ caused_by_parent_request_id: {{ _eq: "{parent_request_id}" }} }}"#)
        }
        None => r#"{
            caused_by_parent_request_id: { _ne: "" }
        }"#
        .to_string(),
    };

    format!(
        r#"{{
            AgentToolCall(
                filter: {tool_filter},
                order: [{{ started_at: ASC }}, {{ child_request_id: ASC }}]
            ) {{
                request_id
                tool_call_id
                args
                status
                lifecycle_state
                started_at
                child_request_id
            }}
            AgentRequest(
                filter: {request_filter},
                order: [{{ created_at: ASC }}, {{ request_id: ASC }}]
            ) {{
                request_id
                agent_did
                behavior_id
                status
                lifecycle_state
                created_at
                claimed_at
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
            }}
            AgentBehavior(order: {{ behavior_id: ASC }}) {{
                behavior_id
                agent_did
            }}
        }}"#
    )
}

fn build_subagent_dispatch_snapshot(
    generated_at: DateTime<Utc>,
    parent_request_id: Option<String>,
    envelope: SubagentDispatchQueryEnvelope,
) -> SubagentDispatchSnapshot {
    let requests_by_id = envelope
        .requests
        .into_iter()
        .filter_map(|request| {
            let request_id = clean_string(&request.request_id);
            (!request_id.is_empty()).then_some((request_id, request))
        })
        .collect::<BTreeMap<_, _>>();
    let behavior_deployments = envelope
        .behaviors
        .into_iter()
        .filter_map(|behavior| {
            let behavior_id = clean_string(&behavior.behavior_id);
            let deployment = clean_optional_string(behavior.agent_did.as_deref())?;
            (!behavior_id.is_empty()).then_some((behavior_id, deployment))
        })
        .collect::<BTreeMap<_, _>>();

    let mut dispatches = envelope
        .bridges
        .into_iter()
        .filter_map(|bridge| {
            let parent_request_id = clean_string(&bridge.request_id);
            let child_request_id = clean_optional_string(bridge.child_request_id.as_deref())?;
            if parent_request_id.is_empty() {
                return None;
            }

            let child = requests_by_id.get(&child_request_id);
            let behavior_id = child
                .and_then(|child| clean_optional_string(child.behavior_id.as_deref()))
                .or_else(|| target_behavior_id_from_args(bridge.args.as_deref()))
                .unwrap_or_default();
            let deployment = child
                .and_then(|child| clean_optional_string(child.agent_did.as_deref()))
                .or_else(|| behavior_deployments.get(&behavior_id).cloned())
                .unwrap_or_default();
            let dispatch_state = clean_optional_string(bridge.lifecycle_state.as_deref())
                .or_else(|| clean_optional_string(bridge.status.as_deref()))
                .or_else(|| {
                    child.and_then(|child| clean_optional_string(child.lifecycle_state.as_deref()))
                })
                .or_else(|| child.and_then(|child| clean_optional_string(child.status.as_deref())))
                .unwrap_or_else(|| "unknown".to_string());
            let started_at = clean_optional_string(bridge.started_at.as_deref())
                .or_else(|| {
                    child.and_then(|child| clean_optional_string(child.claimed_at.as_deref()))
                })
                .or_else(|| {
                    child.and_then(|child| clean_optional_string(child.created_at.as_deref()))
                })
                .unwrap_or_default();

            Some(SubagentDispatchRow {
                parent_request_id,
                child_request_id,
                deployment,
                behavior_id,
                dispatch_state,
                started_at,
            })
        })
        .collect::<Vec<_>>();

    dispatches.sort_by(|left, right| {
        (
            left.started_at.as_str(),
            left.parent_request_id.as_str(),
            left.child_request_id.as_str(),
        )
            .cmp(&(
                right.started_at.as_str(),
                right.parent_request_id.as_str(),
                right.child_request_id.as_str(),
            ))
    });

    SubagentDispatchSnapshot {
        generated_at: generated_at.to_rfc3339(),
        source: SNAPSHOT_SOURCE.to_string(),
        parent_request_id,
        dispatches,
    }
}

fn target_behavior_id_from_args(args: Option<&str>) -> Option<String> {
    let args = args?.trim();
    if args.is_empty() {
        return None;
    }
    let parsed = serde_json::from_str::<SpawnSubagentArgs>(args).ok()?;
    clean_optional_string(parsed.behavior_id.as_deref())
}

fn clean_optional_string(value: Option<&str>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn clean_string(value: &str) -> String {
    value.trim().to_string()
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        net::SocketAddr,
        sync::{Arc, Mutex},
    };

    use axum::{extract::State, routing::post, Router};
    use serde_json::json;
    use tokio::net::TcpListener;

    use super::*;

    #[derive(Clone)]
    struct MockGraphqlState {
        response: Value,
        queries: Arc<Mutex<Vec<String>>>,
    }

    async fn mock_graphql(
        State(state): State<MockGraphqlState>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let query = body
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        state.queries.lock().unwrap().push(query);
        Json(state.response.clone())
    }

    async fn spawn_mock_graphql(
        response: Value,
    ) -> anyhow::Result<(String, Arc<Mutex<Vec<String>>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let queries = Arc::new(Mutex::new(Vec::new()));
        let router = Router::new()
            .route("/api/v0/graphql", post(mock_graphql))
            .with_state(MockGraphqlState {
                response,
                queries: queries.clone(),
            });
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok((format!("http://{addr}/api/v0/graphql"), queries))
    }

    async fn spawn_runtime_router(graphql: String) -> anyhow::Result<SocketAddr> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let router = crate::http::runtime_contract_router(
            graphql,
            "r5-dispatch-test-agent".to_string(),
            "did:key:z6Mkr5dispatchtest".to_string(),
            None,
            None,
            None,
            None,
            crate::http::enrollment::empty_issuer_handle(),
            crate::http::enrollment::empty_decision_service_handle(),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(addr)
    }

    async fn get_subagent_dispatches(
        runtime_addr: SocketAddr,
        parent_request_id: &str,
    ) -> anyhow::Result<SubagentDispatchSnapshot> {
        let response = reqwest::Client::new()
            .get(format!(
                "http://{runtime_addr}/subagents/dispatches?parent_request_id={parent_request_id}"
            ))
            .send()
            .await?;
        let status = response.status();
        let body = response.json::<SubagentDispatchSnapshot>().await?;
        assert!(status.is_success(), "unexpected status {status}: {body:?}");
        Ok(body)
    }

    fn r5_dispatch_graphql_response() -> Value {
        json!({
            "data": {
                "AgentToolCall": [
                    {
                        "request_id": "parent-r5-api",
                        "tool_call_id": "tool-r5-api",
                        "args": "{\"behavior_id\":\"child-behavior-r5\"}",
                        "status": "running",
                        "lifecycle_state": "running",
                        "started_at": "2026-05-20T12:00:00Z",
                        "child_request_id": "child-r5-api"
                    }
                ],
                "AgentRequest": [
                    {
                        "request_id": "child-r5-api",
                        "agent_did": "deployment-b",
                        "behavior_id": "child-behavior-r5",
                        "status": "processing",
                        "lifecycle_state": "processing",
                        "created_at": "2026-05-20T12:00:01Z",
                        "claimed_at": "2026-05-20T12:00:02Z",
                        "caused_by_parent_request_id": "parent-r5-api",
                        "caused_by_parent_tool_call_id": "tool-r5-api"
                    }
                ],
                "AgentBehavior": [
                    {
                        "behavior_id": "child-behavior-r5",
                        "agent_did": "deployment-b"
                    }
                ]
            }
        })
    }

    fn request_parent_walk(
        response: &Value,
        parent_request_id: &str,
    ) -> BTreeSet<(String, String)> {
        response
            .get("data")
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|row| {
                row.get("caused_by_parent_request_id")
                    .and_then(Value::as_str)
                    == Some(parent_request_id)
            })
            .filter_map(|row| {
                let child_request_id = row.get("request_id")?.as_str()?;
                Some((parent_request_id.to_string(), child_request_id.to_string()))
            })
            .collect()
    }

    #[test]
    fn snapshot_preserves_unclaimed_bridge_with_target_behavior_fallback() {
        let generated_at = DateTime::parse_from_rfc3339("2026-05-20T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let snapshot = build_subagent_dispatch_snapshot(
            generated_at,
            None,
            SubagentDispatchQueryEnvelope {
                bridges: vec![BridgeRow {
                    request_id: "parent-unclaimed".to_string(),
                    args: Some(r#"{"behavior_id":"remote-worker"}"#.to_string()),
                    status: Some("running".to_string()),
                    lifecycle_state: Some("running".to_string()),
                    started_at: Some("2026-05-20T12:00:01Z".to_string()),
                    child_request_id: Some("child-unclaimed".to_string()),
                }],
                requests: vec![],
                behaviors: vec![BehaviorRow {
                    behavior_id: "remote-worker".to_string(),
                    agent_did: Some("deployment-remote".to_string()),
                }],
            },
        );

        assert_eq!(snapshot.source, SNAPSHOT_SOURCE);
        assert_eq!(
            snapshot.dispatches,
            vec![SubagentDispatchRow {
                parent_request_id: "parent-unclaimed".to_string(),
                child_request_id: "child-unclaimed".to_string(),
                deployment: "deployment-remote".to_string(),
                behavior_id: "remote-worker".to_string(),
                dispatch_state: "running".to_string(),
                started_at: "2026-05-20T12:00:01Z".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn subagent_dispatch_endpoint_matches_agent_request_parent_walk() -> anyhow::Result<()> {
        let graphql_response = r5_dispatch_graphql_response();
        let expected_walk = request_parent_walk(&graphql_response, "parent-r5-api");
        let (graphql, queries) = spawn_mock_graphql(graphql_response).await?;
        let runtime_addr = spawn_runtime_router(graphql).await?;

        let snapshot = get_subagent_dispatches(runtime_addr, "parent-r5-api").await?;
        let actual_walk = snapshot
            .dispatches
            .iter()
            .map(|row| (row.parent_request_id.clone(), row.child_request_id.clone()))
            .collect::<BTreeSet<_>>();

        assert_eq!(snapshot.parent_request_id.as_deref(), Some("parent-r5-api"));
        assert_eq!(
            actual_walk, expected_walk,
            "/subagents/dispatches drifted from AgentRequest.caused_by_parent_request_id walk"
        );
        assert_eq!(snapshot.dispatches.len(), 1);
        assert_eq!(snapshot.dispatches[0].deployment, "deployment-b");
        assert_eq!(snapshot.dispatches[0].behavior_id, "child-behavior-r5");
        assert_eq!(snapshot.dispatches[0].dispatch_state, "running");
        assert_eq!(snapshot.dispatches[0].started_at, "2026-05-20T12:00:00Z");

        let queries = queries.lock().unwrap();
        let query = queries
            .first()
            .expect("runtime should issue one GraphQL query");
        assert!(query.contains(r#"request_id: { _eq: "parent-r5-api" }"#));
        assert!(query.contains(r#"caused_by_parent_request_id: { _eq: "parent-r5-api" }"#));

        Ok(())
    }
}

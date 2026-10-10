use gents::config_client::GraphqlEndpoint;
use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use gents_protocol::row::{
    decode_node_readiness_snapshot, project_node_readiness_summary, NodeReadinessRow,
    ProjectedNodeReadinessSummary,
};
use serde_json::{json, Value};

use crate::cli::args::StatusArgs;
use crate::config_writes::ConfigAccess;
use crate::{
    post_graphql, print_json, read_runtime_state, resolve_graphql_endpoint, resolve_home_dir,
    resolve_node_did,
};

pub(crate) async fn status(args: StatusArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let node_did = resolve_node_did(args.home.as_deref(), args.node_did.as_deref())?;
    let output = load_runtime_status_output(args.home.as_deref(), &graphql, &node_did).await?;
    print_json(&output)?;
    Ok(())
}

pub(crate) async fn load_runtime_status_output(
    home: Option<&Path>,
    graphql: &GraphqlEndpoint,
    node_did: &str,
) -> Result<Value> {
    let node_readiness_row = load_live_node_readiness(graphql, node_did).await?;
    let lifecycle = node_readiness_row
        .as_ref()
        .and_then(|row| decode_node_readiness_snapshot(row, node_did).ok());
    let (node_readiness, readiness_status, runnable_agent_count, unavailable_agents) =
        match project_node_readiness_summary(node_readiness_row.as_ref(), node_did) {
            ProjectedNodeReadinessSummary::Observed(summary) => {
                let unavailable = summary
                    .unavailable_agents
                    .iter()
                    .map(|(agent_id, reason)| {
                        (agent_id.clone(), reason.public_message().to_string())
                    })
                    .collect::<BTreeMap<_, _>>();
                let status = if unavailable.is_empty() {
                    "ready"
                } else {
                    "degraded"
                };
                (
                    serde_json::to_value(&summary.snapshot).unwrap_or(Value::Null),
                    status,
                    summary.ready_count,
                    unavailable,
                )
            }
            ProjectedNodeReadinessSummary::Unknown(reason) => (
                json!({ "state": "unknown", "reason": reason }),
                "unknown",
                0,
                BTreeMap::new(),
            ),
        };
    let query = format!(
        r#"{{
            NodeRuntime(
                filter: {{ node_did: {{ _eq: "{node_did}" }} }},
                limit: 1
            ) {{
                node_did
                reconcile_phase
                agent_executor_capacity
                agent_executor_queue_depth
                agent_executor_status_json
                last_reconcile_result
                last_reconcile_error
                last_reconcile_completed_at
                updated_at
            }}
        }}"#,
        node_did = escape_graphql_string(node_did),
    );
    let response = post_graphql(graphql, &query).await?;
    let runtime_row = response
        .pointer("/data/NodeRuntime")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .unwrap_or(Value::Null);
    let liveness_value = crate::commands::status::load_liveness_value(graphql, node_did).await;
    let home_dir = resolve_home_dir(home);
    let runtime_state = read_runtime_state(&home_dir)?;
    let p2p_status = crate::commands::p2p::load_live_http_p2p_status(home, graphql).await;
    let background_completion = match gents::load_background_completion_diagnostics(
        &ConfigAccess::Graphql(graphql.clone()),
        node_did,
    )
    .await
    {
        Ok(diagnostics) => serde_json::to_value(diagnostics).unwrap_or(Value::Null),
        Err(error) => json!({
            "state": "unavailable",
            "error": error.to_string(),
        }),
    };
    let mut output = json!({
        "home": home_dir,
        "graphql": graphql,
        "node_did": node_did,
        "runtime_state": runtime_state,
        "runtime": runtime_row,
        "liveness": liveness_value,
        "p2p": p2p_status,
        "background_completion": background_completion,
        "node_readiness": node_readiness,
        "readiness_status": readiness_status,
        "runnable_agent_count": runnable_agent_count,
        "unavailable_agent_count": unavailable_agents.len(),
        "unavailable_agents": unavailable_agents,
    });
    if let Some(map) = output.as_object_mut() {
        for field in [
            "reconcile_phase",
            "agent_executor_capacity",
            "agent_executor_queue_depth",
            "last_reconcile_result",
            "last_reconcile_error",
            "last_reconcile_completed_at",
        ] {
            map.insert(
                field.to_string(),
                runtime_row.get(field).cloned().unwrap_or(Value::Null),
            );
        }
        for (field, value) in [
            (
                "process_state",
                lifecycle
                    .as_ref()
                    .map(|snapshot| json!(snapshot.process_state))
                    .unwrap_or(Value::Null),
            ),
            (
                "active_generation",
                lifecycle
                    .as_ref()
                    .map(|snapshot| json!(snapshot.active_generation))
                    .unwrap_or(Value::Null),
            ),
            (
                "router_generation",
                lifecycle
                    .as_ref()
                    .map(|snapshot| json!(snapshot.router_generation))
                    .unwrap_or(Value::Null),
            ),
            (
                "default_agent_id",
                lifecycle
                    .as_ref()
                    .map(|snapshot| json!(snapshot.default_agent_id))
                    .unwrap_or(Value::Null),
            ),
        ] {
            map.insert(field.to_string(), value);
        }
        let agent_executors = runtime_row
            .get("agent_executor_status_json")
            .and_then(Value::as_str)
            .and_then(|json| serde_json::from_str::<Value>(json).ok())
            .unwrap_or(Value::Null);
        map.insert("agent_executors".to_string(), agent_executors);
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        crate::commands::p2p::flatten_p2p_fields(map, &p2p_value);
    }
    Ok(output)
}

pub(crate) async fn load_liveness_value(graphql: &GraphqlEndpoint, node_did: &str) -> Value {
    if let Some(liveness) = load_live_http_liveness_value(graphql).await {
        return liveness;
    }
    match crate::http::prometheus::load_metrics_query_data(graphql, node_did).await {
        Ok(data) => serde_json::to_value(&data.liveness).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    }
}

async fn load_live_http_liveness_value(graphql: &GraphqlEndpoint) -> Option<Value> {
    let status_url = runtime_status_url(graphql).ok()?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?;
    let response = client.get(status_url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: Value = response.json().await.ok()?;
    body.get("liveness").cloned()
}

fn runtime_status_url(graphql: &GraphqlEndpoint) -> Result<String> {
    let mut url = reqwest::Url::parse(graphql.url()).context("parsing GraphQL endpoint URL")?;
    url.set_path("/status");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

pub(crate) async fn load_live_node_readiness(
    graphql: &GraphqlEndpoint,
    node_did: &str,
) -> Result<Option<NodeReadinessRow>> {
    load_node_readiness(&ConfigAccess::Graphql(graphql.clone()), node_did).await
}

pub(crate) async fn load_node_readiness(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<Option<NodeReadinessRow>> {
    let query = format!(
        r#"{{
            NodeReadiness(
                filter: {{ node_did: {{ _eq: "{node_did}" }} }},
                limit: 1
            ) {{
                node_did
                snapshot_json
                updated_at
            }}
        }}"#,
        node_did = escape_graphql_string(node_did),
    );
    crate::graphql_rows(access, "NodeReadiness", &query)
        .await?
        .into_iter()
        .next()
        .map(|row| serde_json::from_value(row).context("decoding node readiness row"))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_status_url_points_at_server_status_root() {
        assert_eq!(
            runtime_status_url(&gents::config_client::GraphqlEndpoint::anonymous(
                "http://127.0.0.1:9191/api/v0/graphql?ignored=true"
            ))
            .unwrap(),
            "http://127.0.0.1:9191/status"
        );
    }
}

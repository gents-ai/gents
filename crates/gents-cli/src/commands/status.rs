use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use gents_protocol::row::{
    project_behavior_readiness_summary, AgentBehaviorReadinessRow,
    ProjectedBehaviorReadinessSummary,
};
use serde_json::{json, Value};

use crate::cli::args::StatusArgs;
use crate::config_writes::ConfigAccess;
use crate::{
    post_graphql, print_json, read_runtime_state, resolve_agent_did, resolve_graphql_endpoint,
    resolve_home_dir,
};

pub(crate) async fn status(args: StatusArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let output = load_runtime_status_output(args.home.as_deref(), &graphql, &agent_did).await?;
    print_json(&output)?;
    Ok(())
}

pub(crate) async fn load_runtime_status_output(
    home: Option<&Path>,
    graphql: &str,
    agent_did: &str,
) -> Result<Value> {
    let behavior_readiness_row = load_live_behavior_readiness(graphql, agent_did).await?;
    let (behavior_readiness, readiness_status, runnable_behavior_count, unavailable_behaviors) =
        match project_behavior_readiness_summary(behavior_readiness_row.as_ref(), agent_did) {
            ProjectedBehaviorReadinessSummary::Observed(summary) => {
                let unavailable = summary
                    .unavailable_behaviors
                    .iter()
                    .map(|(behavior_id, reason)| {
                        (behavior_id.clone(), reason.public_message().to_string())
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
            ProjectedBehaviorReadinessSummary::Unknown(reason) => (
                json!({ "state": "unknown", "reason": reason }),
                "unknown",
                0,
                BTreeMap::new(),
            ),
        };
    let query = format!(
        r#"{{
            AgentRuntime(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                limit: 1
            ) {{
                agent_did
                process_state
                reconcile_phase
                active_generation
                router_generation
                default_behavior_id
                behavior_executor_capacity
                behavior_executor_queue_depth
                behavior_executor_status_json
                last_reconcile_result
                last_reconcile_error
                last_reconcile_completed_at
                updated_at
            }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
    );
    let response = post_graphql(graphql, &query).await?;
    let runtime_row = response
        .pointer("/data/AgentRuntime")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .unwrap_or(Value::Null);
    let liveness_value = crate::commands::status::load_liveness_value(graphql, agent_did).await;
    let home_dir = resolve_home_dir(home);
    let runtime_state = read_runtime_state(&home_dir)?;
    let p2p_status = crate::commands::p2p::load_live_http_p2p_status(home, graphql).await;
    let background_completion = match gents::load_background_completion_diagnostics(
        &ConfigAccess::Graphql(graphql.to_string()),
        agent_did,
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
        "agent_did": agent_did,
        "runtime_state": runtime_state,
        "runtime": runtime_row,
        "liveness": liveness_value,
        "p2p": p2p_status,
        "background_completion": background_completion,
        "behavior_readiness": behavior_readiness,
        "readiness_status": readiness_status,
        "runnable_behavior_count": runnable_behavior_count,
        "unavailable_behavior_count": unavailable_behaviors.len(),
        "unavailable_behaviors": unavailable_behaviors,
    });
    if let Some(map) = output.as_object_mut() {
        for field in [
            "process_state",
            "reconcile_phase",
            "active_generation",
            "router_generation",
            "default_behavior_id",
            "behavior_executor_capacity",
            "behavior_executor_queue_depth",
            "last_reconcile_result",
            "last_reconcile_error",
            "last_reconcile_completed_at",
        ] {
            map.insert(
                field.to_string(),
                runtime_row.get(field).cloned().unwrap_or(Value::Null),
            );
        }
        let behavior_executors = runtime_row
            .get("behavior_executor_status_json")
            .and_then(Value::as_str)
            .and_then(|json| serde_json::from_str::<Value>(json).ok())
            .unwrap_or(Value::Null);
        map.insert("behavior_executors".to_string(), behavior_executors);
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        crate::commands::p2p::flatten_p2p_fields(map, &p2p_value);
    }
    Ok(output)
}

pub(crate) async fn load_liveness_value(graphql: &str, agent_did: &str) -> Value {
    if let Some(liveness) = load_live_http_liveness_value(graphql).await {
        return liveness;
    }
    match crate::http::prometheus::load_metrics_query_data(graphql, agent_did).await {
        Ok(data) => serde_json::to_value(&data.liveness).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    }
}

async fn load_live_http_liveness_value(graphql: &str) -> Option<Value> {
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

fn runtime_status_url(graphql: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(graphql).context("parsing GraphQL endpoint URL")?;
    url.set_path("/status");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

pub(crate) async fn load_live_behavior_readiness(
    graphql: &str,
    agent_did: &str,
) -> Result<Option<AgentBehaviorReadinessRow>> {
    load_behavior_readiness(&ConfigAccess::Graphql(graphql.to_string()), agent_did).await
}

pub(crate) async fn load_behavior_readiness(
    access: &ConfigAccess,
    agent_did: &str,
) -> Result<Option<AgentBehaviorReadinessRow>> {
    let query = format!(
        r#"{{
            AgentBehaviorReadiness(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                limit: 1
            ) {{
                agent_did
                snapshot_json
                updated_at
            }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
    );
    crate::graphql_rows(access, "AgentBehaviorReadiness", &query)
        .await?
        .into_iter()
        .next()
        .map(|row| serde_json::from_value(row).context("decoding behavior readiness row"))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_status_url_points_at_server_status_root() {
        assert_eq!(
            runtime_status_url("http://127.0.0.1:9191/api/v0/graphql?ignored=true").unwrap(),
            "http://127.0.0.1:9191/status"
        );
    }
}

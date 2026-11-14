use gents_host::readiness::load_live_node_readiness;
use gents_store::config_client::GraphqlEndpoint;
use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use gents_protocol::row::{
    decode_node_readiness_snapshot, project_node_readiness_summary, ProjectedNodeReadinessSummary,
};
use serde_json::{json, Value};

use std::io::IsTerminal as _;

use crate::cli::args::StatusArgs;
use crate::cli::output_format::OutputFormat;
use crate::config_writes::ConfigAccess;
use crate::print_json;
use gents_service::home_state::read_runtime_state;
use gents_service::home_state::resolve_graphql_endpoint;
use gents_service::home_state::resolve_home_dir;
use gents_service::home_state::resolve_node_did;

pub(crate) async fn status(args: StatusArgs) -> Result<()> {
    let format = match args.output {
        Some(format) => {
            format.ensure_supported("status", &[OutputFormat::Text, OutputFormat::Json])?
        }
        None if std::io::stdout().is_terminal() => OutputFormat::Text,
        None => OutputFormat::Json,
    };
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref()).await?;
    let node_did = resolve_node_did(args.home.as_deref(), args.node_did.as_deref())?;
    let output = load_runtime_status_output(args.home.as_deref(), &graphql, &node_did).await?;
    if format == OutputFormat::Json {
        return print_json(&output);
    }
    let home_flag = args
        .home
        .as_deref()
        .map(|home| match home.display().to_string() {
            home if home.contains(char::is_whitespace) => format!(" --home '{home}'"),
            home => format!(" --home {home}"),
        })
        .unwrap_or_default();
    for line in status_summary(&output, &home_flag) {
        println!("{line}");
    }
    Ok(())
}

/// The terminal summary of `load_runtime_status_output`; `home_flag` is the
/// ` --home H` suffix the next calls repeat, empty for the default home.
fn status_summary(output: &Value, home_flag: &str) -> Vec<String> {
    let text = |field: &str| output[field].as_str().unwrap_or("unknown").to_owned();
    let readiness = text("readiness_status");
    let node_name = output["runtime_state"]["node_name"]
        .as_str()
        .unwrap_or("unknown");
    let ready = output["runnable_agent_count"].as_u64().unwrap_or(0);
    let off: Vec<&str> = output["off_agents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let unavailable = output["unavailable_agent_count"]
        .as_u64()
        .unwrap_or(0)
        .saturating_sub(off.len() as u64);
    let backends = output["backend_probes"]
        .as_object()
        .map(|probes| {
            probes
                .iter()
                .map(|(backend, probe)| {
                    format!("{backend} {}", probe["state"].as_str().unwrap_or("unknown"))
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| "not reported".to_owned());
    let mut lines = vec![
        format!("readiness  {readiness}"),
        format!("node       {node_name} ({})", text("node_did")),
        format!("home       {}", text("home")),
        format!("endpoint   {}", text("graphql")),
        match off.len() {
            0 => format!("agents     {ready} of {} ready", ready + unavailable),
            off => format!(
                "agents     {ready} of {} ready, {off} off",
                ready + unavailable
            ),
        },
        format!("backends   {backends}"),
    ];
    if readiness == "unknown" {
        let reason = output["node_readiness"]["reason"]
            .as_str()
            .unwrap_or("not reported")
            .replace('_', " ");
        lines.push(format!("why        {reason}"));
    }
    if let Some(agents) = output["unavailable_agents"].as_object() {
        lines.extend(
            agents
                .iter()
                .filter(|(agent, _)| !off.contains(&agent.as_str()))
                .map(|(agent, reason)| {
                    format!("  {agent}: {}", reason.as_str().unwrap_or_default())
                }),
        );
    }
    if readiness != "ready" {
        lines.push(format!("Next: gents runtime diagnose{home_flag}"));
    }
    lines.push(format!(
        "Every field: gents status{home_flag} --output json"
    ));
    lines
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
    let (node_readiness, readiness_status, runnable_agent_count, unavailable_agents, off_agents) =
        match project_node_readiness_summary(node_readiness_row.as_ref(), node_did) {
            ProjectedNodeReadinessSummary::Observed(summary) => {
                let unavailable = summary
                    .unavailable_agents
                    .iter()
                    .map(|(agent_did, reason)| {
                        (agent_did.clone(), reason.public_message().to_string())
                    })
                    .collect::<BTreeMap<_, _>>();
                let status = if summary.is_ready() {
                    "ready"
                } else {
                    "degraded"
                };
                let off = summary
                    .unavailable_agents
                    .iter()
                    .filter(|(_, reason)| !reason.degrades_node())
                    .map(|(agent_did, _)| agent_did.clone())
                    .collect::<Vec<_>>();
                (
                    serde_json::to_value(&summary.snapshot).unwrap_or(Value::Null),
                    status,
                    summary.ready_count,
                    unavailable,
                    off,
                )
            }
            ProjectedNodeReadinessSummary::Unknown(reason) => (
                json!({ "state": "unknown", "reason": reason }),
                "unknown",
                0,
                BTreeMap::new(),
                Vec::new(),
            ),
        };
    let runtime_row = ConfigAccess::Graphql(graphql.clone())
        .read_view(&gents_store::views::runtime::NodeRuntimeList::new(node_did).limit(1))
        .await?
        .into_iter()
        .next()
        .map(serde_json::to_value)
        .transpose()?
        .unwrap_or(Value::Null);
    let runtime_status = fetch_runtime_status(graphql, RUNTIME_STATUS_TIMEOUT)
        .await
        .ok();
    let liveness_value = liveness_value(runtime_status.as_ref(), graphql, node_did).await;
    let backend_probes = runtime_status
        .as_ref()
        .map(|status| json!(runtime_backend_probes(status, node_did)))
        .unwrap_or(Value::Null);
    let home_dir = resolve_home_dir(home);
    let runtime_state = read_runtime_state(&home_dir)?;
    let p2p_status =
        gents_host::p2p_status::load_live_http_p2p_status(Some(&home_dir), graphql).await;
    let background_completion =
        match gents_toolcalls::background::diagnostics::load_background_completion_diagnostics(
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
        "backend_probes": backend_probes,
        "p2p": p2p_status,
        "background_completion": background_completion,
        "node_readiness": node_readiness,
        "readiness_status": readiness_status,
        "runnable_agent_count": runnable_agent_count,
        "unavailable_agent_count": unavailable_agents.len(),
        "unavailable_agents": unavailable_agents,
        "off_agents": off_agents,
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
                "default_agent_did",
                lifecycle
                    .as_ref()
                    .map(|snapshot| json!(snapshot.default_agent_did))
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
        gents_host::p2p_status::flatten_p2p_fields(map, &p2p_value);
    }
    Ok(output)
}

const RUNTIME_STATUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

pub(crate) async fn load_liveness_value(graphql: &GraphqlEndpoint, node_did: &str) -> Value {
    let runtime_status = fetch_runtime_status(graphql, RUNTIME_STATUS_TIMEOUT)
        .await
        .ok();
    liveness_value(runtime_status.as_ref(), graphql, node_did).await
}

async fn liveness_value(
    runtime_status: Option<&Value>,
    graphql: &GraphqlEndpoint,
    node_did: &str,
) -> Value {
    if let Some(liveness) = runtime_status.and_then(|status| status.get("liveness")) {
        return liveness.clone();
    }
    match gents_host::http::prometheus::load_metrics_query_data(graphql, node_did).await {
        Ok(data) => serde_json::to_value(&data.liveness).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    }
}

/// The runtime's measured backend probes keyed by backend ID, only when the
/// runtime serving `graphql` is `node_did`'s: probes carry no owner scope.
pub(crate) async fn load_runtime_backend_probes(
    graphql: &GraphqlEndpoint,
    node_did: &str,
) -> BTreeMap<String, Value> {
    match fetch_runtime_status(graphql, RUNTIME_STATUS_TIMEOUT).await {
        Ok(status) => runtime_backend_probes(&status, node_did),
        Err(_) => BTreeMap::new(),
    }
}

fn runtime_backend_probes(status: &Value, node_did: &str) -> BTreeMap<String, Value> {
    if status.get("node_did").and_then(Value::as_str) != Some(node_did) {
        return BTreeMap::new();
    }
    status
        .get(gents_host::http::router::BACKEND_PROBES_STATUS_FIELD)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|probe| {
            let backend_id = probe.get("backend_id")?.as_str()?;
            Some((backend_id.to_string(), probe.clone()))
        })
        .collect()
}

pub(crate) async fn fetch_runtime_status(
    graphql: &GraphqlEndpoint,
    timeout: std::time::Duration,
) -> Result<Value> {
    let status_url = gents_host::readiness::runtime_status_url(graphql)?;
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .context("building runtime status client")?;
    graphql
        .authorize(client.get(&status_url))?
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .with_context(|| format!("reading runtime status from {status_url}"))?
        .json()
        .await
        .with_context(|| format!("decoding runtime status from {status_url}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_backend_probes_belong_to_the_status_node() {
        let status = json!({
            "node_did": "did:key:owner",
            "backend_probes": [
                {"backend_id": "claude", "state": "degraded", "last_error": "404"},
                {"state": "healthy"},
            ],
        });
        let probes = runtime_backend_probes(&status, "did:key:owner");
        assert_eq!(probes.len(), 1);
        assert_eq!(probes["claude"]["last_error"], "404");
        assert!(runtime_backend_probes(&status, "did:key:other").is_empty());
    }

    #[test]
    fn a_degraded_summary_names_the_unavailable_agent_and_the_next_call() {
        let output = json!({
            "home": "/h",
            "graphql": "http://127.0.0.1:9191/api/v0/graphql",
            "node_did": "did:key:node",
            "runtime_state": {"node_name": "default"},
            "readiness_status": "degraded",
            "runnable_agent_count": 1,
            "unavailable_agent_count": 1,
            "unavailable_agents": {"did:key:agent": "behavior is disabled"},
            "backend_probes": {"claude": {"state": "healthy"}},
        });
        let lines = status_summary(&output, " --home /h");
        assert_eq!(
            lines,
            [
                "readiness  degraded",
                "node       default (did:key:node)",
                "home       /h",
                "endpoint   http://127.0.0.1:9191/api/v0/graphql",
                "agents     1 of 2 ready",
                "backends   claude healthy",
                "  did:key:agent: behavior is disabled",
                "Next: gents runtime diagnose --home /h",
                "Every field: gents status --home /h --output json",
            ]
        );
    }
}

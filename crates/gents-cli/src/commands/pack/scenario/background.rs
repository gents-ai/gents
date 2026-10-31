use gents::store::config_client::GraphqlEndpoint;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use super::super::cli_process::{path_arg, run_cli_json};
use super::manifest::BackgroundCompletionExpectation;
use super::results::graphql_rows;
use gents::store::graphql::escape_graphql_string;
use gents_protocol::client_protocol::RequestLifecycleState;

#[derive(Debug, Clone)]
pub(super) struct BackgroundCompletionEvidence {
    pub(super) completed_caused_request_ids: Vec<String>,
    pub(super) failed_caused_request_ids: Vec<String>,
    pub(super) completed_wake_request_ids: Vec<String>,
    pub(super) pending_notifications: usize,
    pub(super) acknowledged_notifications: usize,
    pub(super) stranded_notifications: usize,
    pub(super) diagnostics: Value,
}

impl BackgroundCompletionEvidence {
    pub(super) fn satisfies(&self, expected: &BackgroundCompletionExpectation) -> bool {
        self.completed_caused_request_ids.len() >= expected.min_completed_caused_requests
            && self.completed_wake_request_ids.len() >= expected.min_completed_wakes
            && self.acknowledged_notifications >= expected.min_acknowledged_notifications
            && self.pending_notifications <= expected.max_pending_notifications
            && self.stranded_notifications <= expected.max_stranded_notifications
    }
}

fn usize_field(value: &Value, pointer: &str) -> Result<usize> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .with_context(|| format!("status output has no integer {pointer}"))
}

async fn load_background_completion_evidence(
    bin: &Path,
    home: &Path,
    graphql: &GraphqlEndpoint,
    node_did: &str,
) -> Result<BackgroundCompletionEvidence> {
    let status = run_cli_json(
        bin,
        &[
            "status".to_string(),
            "--home".to_string(),
            path_arg(home),
            "--graphql".to_string(),
            graphql.to_string(),
            "--node-did".to_string(),
            node_did.to_string(),
        ],
    )
    .await
    .context("loading background-completion status")?;
    let diagnostics = status
        .get("background_completion")
        .cloned()
        .context("status output has no background_completion diagnostics")?;
    if diagnostics.get("state").and_then(Value::as_str) == Some("unavailable") {
        bail!(
            "background-completion diagnostics unavailable: {}",
            diagnostics
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
        );
    }

    let query = format!(
        r#"{{
            AgentRequest(filter: {{ node_did: {{ _eq: "{}" }} }}) {{
                request_id lifecycle_state execution_origin request_hop input
            }}
        }}"#,
        escape_graphql_string(node_did),
    );
    let rows = graphql_rows(graphql, "AgentRequest", &query).await?;
    let mut completed_caused_request_ids = Vec::new();
    let mut failed_caused_request_ids = Vec::new();
    let mut completed_wake_request_ids = Vec::new();
    for row in rows {
        let request_id = row
            .get("request_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let lifecycle_state = row
            .get("lifecycle_state")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let request_hop = row
            .get("request_hop")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        if request_hop > 0 {
            match RequestLifecycleState::parse_opt(Some(lifecycle_state)) {
                Some(RequestLifecycleState::Completed) => {
                    completed_caused_request_ids.push(request_id.to_string())
                }
                Some(
                    RequestLifecycleState::Failed
                    | RequestLifecycleState::Dead
                    | RequestLifecycleState::Interrupted,
                ) => failed_caused_request_ids.push(request_id.to_string()),
                _ => {}
            }
        }
        let input = row
            .get("input")
            .filter(|value| !value.is_null())
            .cloned()
            .map(serde_json::from_value::<gents_protocol::request_input::RequestInput>)
            .transpose()
            .context("decoding background-completion request input")?
            .unwrap_or_default();
        if row.get("execution_origin").and_then(Value::as_str) == Some("scheduled")
            && gents::request::lifecycle::is_background_completion_request(&input)
            && RequestLifecycleState::parse_opt(Some(lifecycle_state))
                == Some(RequestLifecycleState::Completed)
        {
            completed_wake_request_ids.push(request_id.to_string());
        }
    }
    completed_caused_request_ids.sort();
    failed_caused_request_ids.sort();
    completed_wake_request_ids.sort();

    Ok(BackgroundCompletionEvidence {
        completed_caused_request_ids,
        failed_caused_request_ids,
        completed_wake_request_ids,
        pending_notifications: usize_field(&diagnostics, "/pending_notifications")?,
        acknowledged_notifications: usize_field(&diagnostics, "/acknowledged_notifications")?,
        stranded_notifications: usize_field(&diagnostics, "/stranded_notifications")?,
        diagnostics,
    })
}

pub(super) async fn await_background_completion(
    bin: &Path,
    home: &Path,
    graphql: &GraphqlEndpoint,
    node_did: &str,
    expected: &BackgroundCompletionExpectation,
    deadline: Duration,
) -> Result<BackgroundCompletionEvidence> {
    let started = Instant::now();
    let mut last = None;
    loop {
        match load_background_completion_evidence(bin, home, graphql, node_did).await {
            Ok(evidence) => {
                if evidence.stranded_notifications > expected.max_stranded_notifications {
                    bail!(
                        "background completion stranded {} notification(s), expected at most {}: {:?}",
                        evidence.stranded_notifications,
                        expected.max_stranded_notifications,
                        evidence.diagnostics
                    );
                }
                if evidence.failed_caused_request_ids.len()
                    + evidence.completed_caused_request_ids.len()
                    >= expected.min_completed_caused_requests
                    && evidence.completed_caused_request_ids.len()
                        < expected.min_completed_caused_requests
                {
                    bail!(
                        "background requests terminalized unsuccessfully: {:?}",
                        evidence.failed_caused_request_ids
                    );
                }
                if evidence.satisfies(expected) {
                    return Ok(evidence);
                }
                last = Some(evidence);
            }
            Err(error) => {
                tracing::debug!(%error, "background-completion scenario evidence not ready")
            }
        }
        if started.elapsed() >= deadline {
            bail!(
                "timed out after {}s waiting for background completion; last evidence: {last:?}",
                deadline.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

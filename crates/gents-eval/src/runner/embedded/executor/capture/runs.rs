use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::CaptureBindings;
use crate::runner::embedded::observe::{
    collect_request_evidence_with_access, final_assistant_message,
};
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;

/// Lean `Eval.RunSelector`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunSelector {
    pub(crate) trigger: Option<String>,
    pub(crate) collection: Option<String>,
    pub(crate) of_seed: bool,
}

/// Lean `Eval.FiredRun`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FiredRun {
    pub(crate) trigger: Option<String>,
    pub(crate) watched: Option<String>,
    pub(crate) source: Option<String>,
    pub(crate) prior_to_stage: bool,
}

/// Lean `Eval.runAddressed`.
pub(crate) fn run_addressed(selector: &RunSelector, seed: Option<&str>, run: &FiredRun) -> bool {
    run.trigger.is_some()
        && !run.prior_to_stage
        && selector
            .trigger
            .as_ref()
            .is_none_or(|trigger| run.trigger.as_ref() == Some(trigger))
        && selector
            .collection
            .as_ref()
            .is_none_or(|collection| run.watched.as_ref() == Some(collection))
        && (!selector.of_seed || seed.is_some_and(|seed| run.source.as_deref() == Some(seed)))
}

/// The trigger-fired requests the evaluator reads before a stage begins, so
/// its `runs` captures address only what came into being while it ran.
pub(in crate::runner::embedded::executor) async fn prior_runs(
    access: &ConfigAccess,
) -> Result<BTreeSet<String>> {
    Ok(fired_requests(access)
        .await?
        .into_iter()
        .filter_map(|row| string(&row, "request_id"))
        .collect())
}

pub(super) async fn capture_runs(
    access: &ConfigAccess,
    trial_did: &str,
    selector: &RunSelector,
    bindings: CaptureBindings<'_>,
) -> Result<Vec<Value>> {
    let watched = watched_collections(access, trial_did).await?;
    let mut rows = Vec::new();
    for request in fired_requests(access).await? {
        let request_id = string(&request, "request_id").context("a fired request has no id")?;
        let run = FiredRun {
            trigger: string(&request, "caused_by_trigger_id"),
            watched: watched.get(&request_id).cloned(),
            source: string(&request, "caused_by_source_doc_id"),
            prior_to_stage: bindings
                .prior_runs
                .is_some_and(|prior| prior.contains(&request_id)),
        };
        if !run_addressed(selector, bindings.seed_doc_id, &run) {
            continue;
        }
        let evidence = collect_request_evidence_with_access(access, &request_id).await?;
        rows.push(json!({
            "request_id": request_id,
            "trigger_id": run.trigger,
            "source_collection": run.watched,
            "source_doc_id": run.source,
            "session_id": request.get("session_id"),
            "lifecycle_state": request.get("lifecycle_state"),
            "final_message": final_assistant_message(&evidence.messages)
                .map(|message| message.content.as_str()),
            "tool_results": evidence
                .tool_calls
                .iter()
                .map(|call| json!({
                    "tool_name": call.tool_name,
                    "status": call.status.as_deref().or(call.lifecycle_state.as_deref()),
                    "result": call.result,
                }))
                .collect::<Vec<_>>(),
        }));
    }
    Ok(rows)
}

async fn fired_requests(access: &ConfigAccess) -> Result<Vec<Value>> {
    let response = access
        .execute(
            "{ AgentRequest(order: { created_at: ASC }) { request_id session_id lifecycle_state caused_by_trigger_id caused_by_source_doc_id } }",
        )
        .await?;
    Ok(response
        .get("data")
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .context("observing the evaluator-visible requests")?
        .iter()
        .filter(|row| string(row, "caused_by_trigger_id").is_some())
        .cloned()
        .collect())
}

/// Each fired request's `TriggerFire.source_collection`: the collection the
/// trigger that wrote it watches.
async fn watched_collections(
    access: &ConfigAccess,
    trial_did: &str,
) -> Result<BTreeMap<String, String>> {
    let response = access
        .execute(&format!(
            r#"{{ TriggerFire(filter: {{ owner_did: {{ _eq: "{}" }} }}) {{ request_id source_collection }} }}"#,
            escape_graphql_string(trial_did)
        ))
        .await?;
    Ok(response
        .get("data")
        .and_then(|data| data.get("TriggerFire"))
        .and_then(Value::as_array)
        .context("observing the evaluator-visible trigger fires")?
        .iter()
        .filter_map(|row| {
            Some((
                string(row, "request_id")?,
                string(row, "source_collection")?,
            ))
        })
        .collect())
}

fn string(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests;

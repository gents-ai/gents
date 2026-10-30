use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use super::super::admit::{root_call_of, StepCall, StepRow};
use super::super::input::{chain_of, derive_children_in_txn, CallArgs};
use super::super::records::{read_pin, read_progress, read_step_state};
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;
use crate::plugin::job_calls::job_state;
use crate::tools::call::lifecycle::query::load_tool_call_arguments_in_txn;
use crate::tools::call::lifecycle::{ToolCallLifecycle, ToolCallState};

pub(super) async fn read_lifecycle_state(
    txn: &ConfigApplyTxn<'_>,
    row_doc_id: &str,
) -> Result<String> {
    let response = txn
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
            escape_graphql_string(row_doc_id)
        ))
        .await?;
    response
        .pointer("/data/AgentToolCall/0/lifecycle_state")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("job row {row_doc_id} does not exist"))
}

/// The job row's lifecycle, loaded by its physical identity.
pub(in crate::tools::background::job) async fn load_lifecycle(
    access: &ConfigAccess,
    row_doc_id: &str,
) -> Result<ToolCallLifecycle> {
    let response = access
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ session_id node_did requester_did request_doc_id }} }}"#,
            escape_graphql_string(row_doc_id)
        ))
        .await?;
    let stored: StoredRow = serde_json::from_value(
        response
            .pointer("/data/AgentToolCall/0")
            .cloned()
            .with_context(|| format!("job row {row_doc_id} does not exist"))?,
    )
    .with_context(|| format!("decoding job row {row_doc_id}"))?;
    let ConfigAccess::Local(node) = access else {
        anyhow::bail!("a job step needs a local node");
    };
    ToolCallLifecycle::load_by_doc_id(
        node.node().clone(),
        row_doc_id,
        &stored.node_did,
        &stored.session_id,
        stored.requester_did.as_deref(),
    )
    .await?
    .with_context(|| format!("job row {row_doc_id} does not exist"))
}

#[derive(Deserialize)]
struct StoredRow {
    session_id: String,
    node_did: String,
    #[serde(default)]
    requester_did: Option<String>,
    request_doc_id: String,
    #[serde(default)]
    lifecycle_state: Option<String>,
}

/// The admission row (`admit::StepRow`) of a stored job row, read through
/// `txn`. The one place a job row becomes a `StepRow`.
pub(crate) async fn read_step_row_in_txn(
    txn: &ConfigApplyTxn<'_>,
    row_doc_id: &str,
) -> Result<StepRow> {
    let mut row = read_row(txn, row_doc_id).await?;
    if row.root != row_doc_id {
        let root = read_row(txn, &row.root).await?;
        row.root_call = Some(root_call_of(&root));
    }
    Ok(row)
}

/// [`read_step_row_in_txn`] in a read-only transaction of `access`.
pub(crate) async fn read_step_row(access: &ConfigAccess, row_doc_id: &str) -> Result<StepRow> {
    access
        .transact_readonly("plugin_job.read_step_row", |txn| {
            Box::pin(read_step_row_in_txn(txn, row_doc_id))
        })
        .await
}

/// One row's own facts; `root_call` is the caller's to fill from the chain root.
async fn read_row(txn: &ConfigApplyTxn<'_>, row_doc_id: &str) -> Result<StepRow> {
    let stored = txn
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ session_id node_did requester_did request_doc_id lifecycle_state }} }}"#,
            escape_graphql_string(row_doc_id)
        ))
        .await?;
    let stored: StoredRow = serde_json::from_value(
        stored
            .pointer("/data/AgentToolCall/0")
            .cloned()
            .with_context(|| format!("job row {row_doc_id} does not exist"))?,
    )
    .with_context(|| format!("decoding job row {row_doc_id}"))?;
    let hop = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ request_hop }} }}"#,
            escape_graphql_string(&stored.request_doc_id)
        ))
        .await?;
    let caller_hop = hop
        .pointer("/data/AgentRequest/0")
        .with_context(|| format!("the caller request of job row {row_doc_id} does not exist"))?
        .get("request_hop")
        .and_then(Value::as_u64)
        .map_or(0, |hop| u32::try_from(hop).unwrap_or(u32::MAX));
    let text = load_tool_call_arguments_in_txn(
        txn,
        row_doc_id,
        &stored.node_did,
        &stored.session_id,
        stored.requester_did.as_deref(),
    )
    .await?;
    let call: CallArgs = serde_json::from_str(&text)
        .with_context(|| format!("decoding the accepted arguments of job row {row_doc_id}"))?;
    let pin = read_pin(txn, row_doc_id).await?;
    let progress = read_progress(txn, row_doc_id).await?;
    let lifecycle = stored
        .lifecycle_state
        .as_deref()
        .and_then(ToolCallState::from_persisted)
        .unwrap_or(ToolCallState::Running);
    let state = job_state(lifecycle, progress.as_ref());
    let chain = chain_of(txn, row_doc_id, pin.as_ref()).await?;
    let result_bytes = pin
        .as_ref()
        .map_or(0, |pin| pin.ceiling_at_pin.result_bytes);
    let children = derive_children_in_txn(txn, &chain, result_bytes)
        .await?
        .into_iter()
        .map(|(child, _)| child)
        .collect();
    Ok(StepRow {
        root: pin.as_ref().map_or_else(
            || row_doc_id.to_owned(),
            |pin| pin.chain_root_doc_id.clone(),
        ),
        row_doc_id: row_doc_id.to_owned(),
        caller_session: stored.session_id,
        caller_cwd: pin.as_ref().map(|pin| pin.cwd.clone()).unwrap_or_default(),
        handle: pin
            .as_ref()
            .map(|pin| pin.handle.clone())
            .unwrap_or_default(),
        caller_hop,
        call: StepCall {
            argv: call.argv,
            args: call
                .args
                .map(|args| serde_json::to_string(&args))
                .transpose()?,
            script: call.script,
            agent_budget: call.agent_budget,
        },
        root_call: None,
        resume: read_step_state(txn, row_doc_id)
            .await?
            .and_then(|state| state.resume),
        pin,
        children,
        spent: progress.as_ref().map_or(0, |progress| progress.spawned),
        observed_terminal: progress
            .as_ref()
            .map_or(0, |progress| progress.observed_terminal_children),
        consecutive_failures: 0,
        state,
    })
}

/// Stores a running job row's progress at revision 1 (spawned 2, one child
/// observed terminal), as adoption and the first step leave it.
#[cfg(test)]
pub(in crate::tools::background::job) async fn store_running_progress(
    admission: &crate::tools::call::lifecycle::admission_fixture::PublishedAdmission,
) {
    use super::super::records::{write_progress_in_txn, HostProgressFacts};
    use crate::config_client::ConfigAccess;
    use gents_protocol::job::{JobBudget, JobProgress, JobState};
    let doc_id = admission.tool.doc_id().unwrap().to_owned();
    ConfigAccess::transact_local_write(&admission.node, None, "test.driver.progress", |txn| {
        let doc_id = doc_id.clone();
        Box::pin(async move {
            let guest = JobProgress {
                title: "audit".to_owned(),
                state: JobState::Running,
                pause_reason: None,
                phases: Vec::new(),
                active: Vec::new(),
                spawned: 0,
                budget: JobBudget {
                    total: 0,
                    spent: 0,
                    remaining: 0,
                },
                last_event: "scanning".to_owned(),
                last_event_at: None,
                question: None,
                observed_terminal_children: 0,
                revision: 0,
            };
            let host = HostProgressFacts {
                spawned: 2,
                budget: JobBudget {
                    total: 4,
                    spent: 2,
                    remaining: 2,
                },
                observed_terminal_children: 1,
                state: JobState::Running,
                pause_reason: None,
            };
            write_progress_in_txn(txn, &doc_id, 0, guest, host).await
        })
    })
    .await
    .unwrap();
}

//! A client's read of one plugin job row, through any [`ConfigAccess`].

use gents_protocol::error_chain::ErrorChain;
use std::collections::{BTreeMap, BTreeSet};

use gents_protocol::job::{
    JobChildRow, JobChildView, JobOutcome, JobPin, JobPinSummary, JobProgress, JobReadError,
    JobScope, JobState, JobView,
};
use serde::Deserialize;
use serde_json::Value;

use crate::background_tools::{
    observe_canonical_tool_output_with_access, CanonicalToolOutputObservation, ProcessControlScope,
};
use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::plugin::job_calls::job_state;
use crate::plugin_job::admit::StepChild;
use crate::plugin_job::input::{chain_of, derive_children};
use crate::plugin_job::keys::job_spawn_key;
use crate::tool_call_lifecycle::ToolCallState;

/// Which job row a view reads: by physical doc id, or by its address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobKey {
    RowDocId(String),
    Address {
        session_id: String,
        tool_call_id: String,
    },
}

const FIELDS: &str = "_docID session_id tool_call_id request_id request_doc_id node_did requester_did lifecycle_state started_at completed_at job_pin job_progress";

#[derive(Deserialize)]
struct Owner {
    session_id: String,
    node_did: String,
    #[serde(default)]
    requester_did: Option<String>,
}

#[derive(Deserialize)]
struct Stored {
    #[serde(rename = "_docID")]
    doc_id: String,
    tool_call_id: String,
    request_id: String,
    request_doc_id: String,
    lifecycle_state: String,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
    #[serde(default)]
    job_progress: Option<Value>,
}

fn malformed(message: impl std::fmt::Display) -> JobReadError {
    JobReadError::Malformed {
        message: message.to_string(),
    }
}

fn unavailable(error: anyhow::Error) -> JobReadError {
    JobReadError::Unavailable {
        message: format!(
            "{}; retry when the node answers",
            ErrorChain(error.as_ref())
        ),
    }
}

fn decode<T>(value: &Value, what: &str) -> Result<T, JobReadError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(value.clone()).map_err(|error| malformed(format!("{what}: {error}")))
}

fn epoch_ms(field: &str, stamp: &str) -> Result<u64, JobReadError> {
    let parsed = chrono::DateTime::parse_from_rfc3339(stamp)
        .map_err(|error| malformed(format!("{field} `{stamp}`: {error}")))?;
    u64::try_from(parsed.timestamp_millis())
        .map_err(|_| malformed(format!("{field} `{stamp}` is before the epoch")))
}

/// A job row's decoded fields, before children and outcome.
struct Decoded {
    owner: Owner,
    stored: Stored,
    pin: JobPin,
    progress: Option<JobProgress>,
    state: JobState,
    created_at: u64,
    completed_at: Option<u64>,
}

fn decode_job(row: &Value, owner: Owner) -> Result<Decoded, JobReadError> {
    let Some(pin) = row.get("job_pin").filter(|pin| !pin.is_null()) else {
        return Err(JobReadError::NotAJob);
    };
    let pin: JobPin = decode(pin, "job_pin")?;
    let stored: Stored = decode(row, "job row")?;
    let progress = stored
        .job_progress
        .as_ref()
        .map(|progress| decode::<JobProgress>(progress, "job_progress"))
        .transpose()?;
    let lifecycle = ToolCallState::from_persisted(&stored.lifecycle_state).ok_or_else(|| {
        malformed(format!(
            "unknown lifecycle_state `{}`",
            stored.lifecycle_state
        ))
    })?;
    let created_at = match stored.started_at.as_deref() {
        Some(stamp) => epoch_ms("started_at", stamp)?,
        None => return Err(malformed("the job row has no started_at")),
    };
    let completed_at = stored
        .completed_at
        .as_deref()
        .map(|stamp| epoch_ms("completed_at", stamp))
        .transpose()?;
    let state = job_state(lifecycle, progress.as_ref());
    Ok(Decoded {
        owner,
        stored,
        pin,
        progress,
        state,
        created_at,
        completed_at,
    })
}

fn assemble(decoded: Decoded, outcome: Option<JobOutcome>, children: Vec<JobChildRow>) -> JobView {
    let Decoded {
        owner,
        stored,
        pin,
        progress,
        state,
        created_at,
        completed_at,
    } = decoded;
    JobView {
        address: format!("{}:{}", owner.session_id, stored.tool_call_id),
        row_doc_id: stored.doc_id,
        session_id: owner.session_id.clone(),
        tool_call_id: stored.tool_call_id,
        scope: JobScope {
            session_id: owner.session_id,
            request_id: stored.request_id,
            node_did: owner.node_did,
            requester_did: owner.requester_did,
        },
        handle: pin.handle.clone(),
        state,
        outcome,
        progress,
        children,
        created_at,
        adopted_at: pin.adopted_at,
        completed_at,
        pin: JobPinSummary {
            script_id: pin.script_id,
            origin: pin.origin,
            engine_digest: pin.engine_digest,
            plugin_ref: pin.plugin_ref,
            agent_budget: pin.agent_budget,
            cwd: pin.cwd,
            chain_root_doc_id: pin.chain_root_doc_id,
            resume_of: pin.resume_of,
        },
    }
}

/// `scope` is the reader's: `Some` refuses a row it does not authorize with
/// `NotAuthorized`; `None` reads as the node's operator, who holds `access`.
pub async fn load_job_view(
    access: &ConfigAccess,
    scope: Option<&ProcessControlScope>,
    key: JobKey,
) -> Result<JobView, JobReadError> {
    let filter = match &key {
        JobKey::RowDocId(doc_id) => {
            format!(r#"_docID: {{ _eq: "{}" }}"#, escape_graphql_string(doc_id))
        }
        JobKey::Address {
            session_id,
            tool_call_id,
        } => format!(
            r#"session_id: {{ _eq: "{}" }}, tool_call_id: {{ _eq: "{}" }}"#,
            escape_graphql_string(session_id),
            escape_graphql_string(tool_call_id)
        ),
    };
    let response = access
        .execute(&format!(
            "{{ AgentToolCall(filter: {{ {filter} }}) {{ {FIELDS} }} }}"
        ))
        .await
        .map_err(|error| unavailable(error.into()))?;
    let Some(row) = response
        .pointer("/data/AgentToolCall/0")
        .filter(|row| !row.is_null())
    else {
        return Err(JobReadError::NotYetObservable);
    };
    let owner: Owner = decode(row, "job row")?;
    if scope.is_some_and(|scope| {
        !scope.authorizes(
            &owner.session_id,
            &owner.node_did,
            owner.requester_did.as_deref(),
        )
    }) {
        return Err(JobReadError::NotAuthorized);
    }
    let decoded = decode_job(row, owner)?;
    let outcome = outcome_of(access, &decoded).await?;
    let chain = chain_of(access, &decoded.stored.doc_id, Some(&decoded.pin))
        .await
        .map_err(unavailable)?;
    let pairs = derive_children(access, &chain, decoded.pin.ceiling_at_pin.result_bytes)
        .await
        .map_err(unavailable)?;
    let children = children_of(&decoded.pin, pairs);
    Ok(assemble(decoded, outcome, children))
}

/// Which job rows [`list_jobs`] returns. Serves L3's Grok discovery
/// (session, requester, `NotTerminalOrUndelivered`) and `gents job list`
/// (optional session, a `JobState` set, optional plugin). `None` does not filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobFilter {
    pub session_id: Option<String>,
    /// The requester principal's DID, matched against the row's `requester_did`.
    pub requester: Option<String>,
    pub state: StateFilter,
    /// `namespace/name`, matched against `JobView.pin.plugin_ref`.
    pub plugin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateFilter {
    /// Rows whose `JobView.state` is one of these.
    States(Vec<JobState>),
    /// Rows whose state is `JobState::Running`, and rows in any other state
    /// whose `row_doc_id` is not in `delivered`. The host records no client
    /// delivery, so the caller passes the rows it has already delivered.
    NotTerminalOrUndelivered { delivered: BTreeSet<String> },
}

impl StateFilter {
    fn keeps(&self, view: &Decoded) -> bool {
        match self {
            StateFilter::States(states) => states.contains(&view.state),
            StateFilter::NotTerminalOrUndelivered { delivered } => {
                view.state == JobState::Running || !delivered.contains(&view.stored.doc_id)
            }
        }
    }
}

/// The job rows matching `filter`, newest first by `adopted_at`. A row that
/// does not decode is skipped with a warning; children are derived in one batch.
pub async fn list_jobs(
    access: &ConfigAccess,
    filter: &JobFilter,
) -> Result<Vec<JobView>, JobReadError> {
    let mut clauses = vec!["job_pin: { _ne: null }".to_owned()];
    if let Some(session_id) = &filter.session_id {
        clauses.push(format!(
            r#"session_id: {{ _eq: "{}" }}"#,
            escape_graphql_string(session_id)
        ));
    }
    if let Some(requester) = &filter.requester {
        clauses.push(format!(
            r#"requester_did: {{ _eq: "{}" }}"#,
            escape_graphql_string(requester)
        ));
    }
    let response = access
        .execute(&format!(
            "{{ AgentToolCall(filter: {{ {} }}) {{ {FIELDS} }} }}",
            clauses.join(", ")
        ))
        .await
        .map_err(|error| unavailable(error.into()))?;
    let rows = response
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed("the AgentToolCall query omitted rows"))?;
    let mut jobs = Vec::new();
    for row in rows {
        match decode(row, "job row").and_then(|owner| decode_job(row, owner)) {
            Ok(decoded)
                if filter
                    .plugin
                    .as_ref()
                    .is_none_or(|plugin| *plugin == decoded.pin.plugin_ref)
                    && filter.state.keeps(&decoded) =>
            {
                jobs.push(decoded);
            }
            Ok(_) => {}
            Err(error) => {
                let row_doc_id = row
                    .get("_docID")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                tracing::warn!(row_doc_id, %error, "skipping an undecodable plugin job row");
            }
        }
    }
    let mut chains = Vec::new();
    for job in &jobs {
        chains.push(
            chain_of(access, &job.stored.doc_id, Some(&job.pin))
                .await
                .map_err(unavailable)?,
        );
    }
    let rows: BTreeSet<String> = chains.into_iter().flatten().collect();
    let result_bytes = jobs
        .iter()
        .map(|job| job.pin.ceiling_at_pin.result_bytes)
        .max();
    let pairs = match result_bytes {
        Some(result_bytes) => {
            let rows: Vec<String> = rows.into_iter().collect();
            derive_children(access, &rows, result_bytes)
                .await
                .map_err(unavailable)?
        }
        None => Vec::new(),
    };
    let mut views = Vec::new();
    for job in jobs {
        let outcome = outcome_of(access, &job).await?;
        let children = children_of(&job.pin, pairs.clone());
        views.push(assemble(job, outcome, children));
    }
    views.sort_by(|left, right| {
        right
            .adopted_at
            .cmp(&left.adopted_at)
            .then_with(|| left.row_doc_id.cmp(&right.row_doc_id))
    });
    Ok(views)
}

/// The code and message of a failed or timed-out row's closed terminal
/// output; a guest `Result` value is never read as an outcome.
async fn outcome_of(
    access: &ConfigAccess,
    decoded: &Decoded,
) -> Result<Option<JobOutcome>, JobReadError> {
    if !matches!(decoded.state, JobState::Failed | JobState::TimedOut) {
        return Ok(None);
    }
    let (stored, owner) = (&decoded.stored, &decoded.owner);
    let observed = observe_canonical_tool_output_with_access(
        access,
        &stored.doc_id,
        &stored.request_doc_id,
        &owner.session_id,
        &owner.node_did,
        owner.requester_did.as_deref(),
    )
    .await
    .map_err(unavailable)?;
    let CanonicalToolOutputObservation::Closed(text) = observed else {
        return Ok(None);
    };
    let output: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let field = |name| output.get(name).and_then(Value::as_str).map(str::to_owned);
    Ok(field("code")
        .zip(field("message"))
        .map(|(code, message)| JobOutcome { code, message }))
}

/// One row per slot from its highest attempt, among the children spawned by
/// this job's own retry keys.
fn children_of(pin: &JobPin, pairs: Vec<(StepChild, JobChildView)>) -> Vec<JobChildRow> {
    let mut latest: BTreeMap<String, (StepChild, JobChildView)> = BTreeMap::new();
    for (child, view) in pairs {
        if child.retry_key != job_spawn_key(&pin.chain_root_doc_id, &child.slot, child.attempt) {
            continue;
        }
        if latest
            .get(&child.slot)
            .is_none_or(|(held, _)| held.attempt < child.attempt)
        {
            latest.insert(child.slot.clone(), (child, view));
        }
    }
    latest
        .into_values()
        .map(|(child, view)| JobChildRow {
            slot: child.slot,
            attempt: child.attempt,
            session_id: view.session_id,
            request_id: view.request_id,
            state: view.state,
            title: view.title,
            usage: view.usage,
            duration_ms: view.duration_ms,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use defra_node::EmbeddedNode;
    use gents_protocol::job::{
        JobChildRow, JobChildState, JobControlKind, JobFailure, JobOutcome, JobPin, JobPinSummary,
        JobPolicy, JobReadError, JobScope, JobState, JobSuspend, JobSuspendKind, JobUsage, JobView,
    };
    use serde_json::{json, Value};

    use super::{list_jobs, load_job_view, JobFilter, JobKey, StateFilter};
    use crate::background_tools::ProcessControlScope;
    use crate::config_client::ConfigAccess;
    use crate::graphql::escape_graphql_string;
    use crate::plugin_job::admit::Refusal;
    use crate::plugin_job::driver::tests::{pinned_job_row, settle_row, start_running};
    use crate::plugin_job::driver::{load_lifecycle, settle_plugin_job, Settlement};
    use crate::plugin_job::input::tests::{insert, live_settings, new_job, node_did, request, Job};
    use crate::plugin_job::keys::{job_session_id, job_spawn_key};
    use crate::plugin_job::records::{read_pin, read_progress, write_pin_in_txn};
    use crate::tool_call_lifecycle::admission_fixture::{
        claimed_request, fixture_job_pin, publish_accepted_on_claimed_request,
    };
    use crate::tool_call_lifecycle::AwaitMode;

    async fn new() -> Job {
        new_job(Some(&live_settings(JobPolicy::default())), None).await
    }

    async fn view(job: &Job, row: &str) -> Result<JobView, JobReadError> {
        load_job_view(&job.access, None, JobKey::RowDocId(row.to_owned())).await
    }

    fn scope(session_id: &str, requester_did: Option<&str>) -> ProcessControlScope {
        ProcessControlScope {
            request_id: String::new(),
            session_id: session_id.to_owned(),
            node_did: node_did().to_owned(),
            requester_did: requester_did.map(str::to_owned),
        }
    }

    fn fail(code: &str) -> Settlement {
        Settlement::Fail(JobFailure {
            code: code.to_owned(),
            message: "boom".to_owned(),
        })
    }

    fn await_user() -> Settlement {
        Settlement::Suspend(JobSuspend {
            kind: JobSuspendKind::AwaitUser,
            reason: None,
            message: "needs a decision".into(),
            question: Some("ship it?".into()),
        })
    }

    async fn child(job: &Job, row: &str, request_id: &str, slot: &str, attempt: u32, title: &str) {
        request(
            &job.node,
            row,
            request_id,
            &job_session_id(row, slot, attempt),
            &job_spawn_key(row, slot, attempt),
            "agent-a",
            "processing",
            Some(title),
            "2026-01-01T00:00:00Z",
            None,
        )
        .await;
    }

    async fn write_job_pin_json(job: &Job, row: &str, value: Value) {
        let mutation = format!(
            r#"mutation($job_pin: JSON) {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ job_pin: $job_pin }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(row),
        );
        let variables = json!({ "job_pin": value });
        ConfigAccess::transact_local_write(&job.node, None, "test.view.job_pin", |txn| {
            let (mutation, variables) = (&mutation, &variables);
            Box::pin(async move { txn.execute_with_variables(mutation, variables).await })
        })
        .await
        .unwrap();
    }

    async fn started_at_ms(job: &Job, row: &str) -> u64 {
        let response = job
            .access
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ started_at }} }}"#,
                escape_graphql_string(row)
            ))
            .await
            .unwrap();
        let stamp = response["data"]["AgentToolCall"][0]["started_at"]
            .as_str()
            .unwrap();
        u64::try_from(
            chrono::DateTime::parse_from_rfc3339(stamp)
                .unwrap()
                .timestamp_millis(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn running_job_view_has_address_state_pin_summary_progress_and_children() {
        let mut job = new().await;
        let row = pinned_job_row(&mut job, "call-1").await;
        child(&job, &row, "child-a0", "slot-a", 0, "Alpha").await;
        child(&job, &row, "child-a1", "slot-a", 1, "Alpha again").await;
        child(&job, &row, "child-b0", "slot-b", 0, "Beta").await;
        let pin = job.pin(&row, None);
        let stored = read_progress(&job.access, &row).await.unwrap();
        let live = |slot: &str, attempt, request_id: &str, title: &str| JobChildRow {
            slot: slot.to_owned(),
            attempt,
            session_id: job_session_id(&row, slot, attempt),
            request_id: request_id.to_owned(),
            state: JobChildState::Live,
            title: title.to_owned(),
            usage: JobUsage {
                input: 0,
                output: 0,
                total: 0,
            },
            duration_ms: 0,
        };

        let loaded = view(&job, &row).await.unwrap();

        assert_eq!(loaded.address, "session-1:call-1");
        assert_eq!(loaded.row_doc_id, row);
        assert_eq!(loaded.session_id, "session-1");
        assert_eq!(loaded.tool_call_id, "call-1");
        assert_eq!(
            loaded.scope,
            JobScope {
                session_id: "session-1".into(),
                request_id: "request-1".into(),
                node_did: node_did().into(),
                requester_did: None,
            }
        );
        assert_eq!(loaded.handle, pin.handle);
        assert_eq!(loaded.state, JobState::Running);
        assert_eq!(loaded.outcome, None);
        assert_eq!(
            loaded.pin,
            JobPinSummary {
                script_id: pin.script_id.clone(),
                origin: pin.origin,
                engine_digest: pin.engine_digest.clone(),
                plugin_ref: pin.plugin_ref.clone(),
                agent_budget: pin.agent_budget,
                cwd: pin.cwd.clone(),
                chain_root_doc_id: row.clone(),
                resume_of: None,
            }
        );
        assert!(stored.is_some());
        assert_eq!(loaded.progress, stored);
        assert_eq!(
            loaded.children,
            vec![
                live("slot-a", 1, "child-a1", "Alpha again"),
                live("slot-b", 0, "child-b0", "Beta"),
            ]
        );
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn settled_job_view_carries_outcome_for_a_host_code() {
        let mut job = new().await;
        let row = pinned_job_row(&mut job, "call-1").await;
        settle_row(&job, &row, Settlement::Refused(Refusal::TimedOut)).await;

        let loaded = view(&job, &row).await.unwrap();

        assert_eq!(loaded.state, JobState::TimedOut);
        let outcome = loaded.outcome.unwrap();
        assert_eq!(outcome.code, "plugin_job_timed_out");
        assert!(!outcome.message.is_empty());
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn settled_job_view_carries_outcome_for_a_guest_fail_code() {
        let mut job = new().await;
        let row = pinned_job_row(&mut job, "call-1").await;
        settle_row(&job, &row, fail("engine_boom")).await;

        let loaded = view(&job, &row).await.unwrap();

        assert_eq!(loaded.state, JobState::Failed);
        assert_eq!(
            loaded.outcome,
            Some(JobOutcome {
                code: "engine_boom".into(),
                message: "boom".into(),
            })
        );
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn job_view_outcome_is_none_without_a_code() {
        let mut job = new().await;
        let settlements = [
            Settlement::Result(json!({"code": "looks_like_one", "message": "a result"})),
            Settlement::Control(JobControlKind::Stop),
            Settlement::Control(JobControlKind::Pause),
            await_user(),
        ];
        for (index, settlement) in settlements.into_iter().enumerate() {
            let row = pinned_job_row(&mut job, &format!("call-{}", index + 1)).await;
            settle_row(&job, &row, settlement).await;
            let loaded = view(&job, &row).await.unwrap();
            assert_eq!(loaded.outcome, None, "case {}", index + 1);
        }
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn job_view_state_follows_the_row_lifecycle_first() {
        let mut job = new().await;
        let cases = [
            (Some(fail("engine_boom")), JobState::Failed),
            (
                Some(Settlement::Control(JobControlKind::Stop)),
                JobState::Cancelled,
            ),
            (
                Some(Settlement::Refused(Refusal::TimedOut)),
                JobState::TimedOut,
            ),
            (Some(Settlement::Result(json!("done"))), JobState::Completed),
            (
                Some(Settlement::Control(JobControlKind::Pause)),
                JobState::Paused,
            ),
            (Some(await_user()), JobState::AwaitingUser),
            (None, JobState::Running),
        ];
        for (index, (settlement, expected)) in cases.into_iter().enumerate() {
            let row = pinned_job_row(&mut job, &format!("call-{}", index + 1)).await;
            if let Some(settlement) = settlement {
                settle_row(&job, &row, settlement).await;
            }
            let loaded = view(&job, &row).await.unwrap();
            assert_eq!(loaded.state, expected, "case {}", index + 1);
        }
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn job_view_times_are_epoch_ms_from_the_row_and_pin() {
        let mut job = new().await;
        let row = pinned_job_row(&mut job, "call-1").await;
        let started = started_at_ms(&job, &row).await;

        let running = view(&job, &row).await.unwrap();
        assert_eq!(running.created_at, started);
        assert_eq!(running.adopted_at, job.pin(&row, None).adopted_at);
        assert_eq!(running.completed_at, None);

        settle_row(&job, &row, Settlement::Result(json!("done"))).await;
        let settled = view(&job, &row).await.unwrap();
        assert_eq!(settled.created_at, started);
        assert!(settled.completed_at.unwrap() >= started);

        let unstarted = job
            .row("workflow", "call-2", json!({"argv": ["run"]}))
            .await;
        job.write_pin(&unstarted, &job.pin(&unstarted, None)).await;
        assert!(matches!(
            view(&job, &unstarted).await,
            Err(JobReadError::Malformed { .. })
        ));
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn job_read_errors_are_returned_per_class_and_classified_by_is_transient() {
        let mut job = new().await;
        let missing = "bae-00000000-0000-0000-0000-000000000000".to_owned();
        let unpinned = job
            .row("workflow", "call-1", json!({"argv": ["run"]}))
            .await;
        let pinned = pinned_job_row(&mut job, "call-2").await;
        let bogus = job
            .row("workflow", "call-3", json!({"argv": ["run"]}))
            .await;
        write_job_pin_json(&job, &bogus, json!({"bogus": 1})).await;
        let read = |scope: Option<ProcessControlScope>, key: JobKey| {
            let access = job.access.clone();
            async move { load_job_view(&access, scope.as_ref(), key).await }
        };

        assert_eq!(
            read(None, JobKey::RowDocId(missing)).await,
            Err(JobReadError::NotYetObservable)
        );
        assert_eq!(
            read(
                None,
                JobKey::Address {
                    session_id: "session-1".into(),
                    tool_call_id: "no-such-call".into(),
                }
            )
            .await,
            Err(JobReadError::NotYetObservable)
        );
        assert_eq!(
            read(None, JobKey::RowDocId(unpinned.clone())).await,
            Err(JobReadError::NotAJob)
        );
        for outsider in [
            scope("other-session", None),
            scope("session-1", Some("did:key:other")),
        ] {
            assert_eq!(
                read(Some(outsider.clone()), JobKey::RowDocId(pinned.clone())).await,
                Err(JobReadError::NotAuthorized)
            );
            assert_eq!(
                read(Some(outsider), JobKey::RowDocId(unpinned.clone())).await,
                Err(JobReadError::NotAuthorized),
                "the scope check runs before the pin check"
            );
        }
        let by_address = JobKey::Address {
            session_id: "session-1".into(),
            tool_call_id: "call-2".into(),
        };
        for reader in [None, Some(scope("session-1", None))] {
            let loaded = read(reader.clone(), JobKey::RowDocId(pinned.clone()))
                .await
                .unwrap();
            assert_eq!(loaded.row_doc_id, pinned);
            assert_eq!(
                read(reader, by_address.clone()).await.unwrap().row_doc_id,
                pinned
            );
        }
        assert!(matches!(
            read(None, JobKey::RowDocId(bogus)).await,
            Err(JobReadError::Malformed { .. })
        ));
        let unreachable = load_job_view(
            &ConfigAccess::graphql("http://127.0.0.1:1"),
            None,
            JobKey::RowDocId(pinned),
        )
        .await;
        assert!(matches!(unreachable, Err(JobReadError::Unavailable { .. })));

        let classes = [
            (JobReadError::NotAJob, false),
            (JobReadError::NotAuthorized, false),
            (JobReadError::NotYetObservable, true),
            (
                JobReadError::Unavailable {
                    message: "m".into(),
                },
                true,
            ),
            (
                JobReadError::Malformed {
                    message: "m".into(),
                },
                false,
            ),
        ];
        for (error, transient) in classes {
            assert_eq!(error.is_transient(), transient, "{error}");
        }
        job.node.shutdown().await;
    }

    const EVERY_STATE: [JobState; 7] = [
        JobState::Running,
        JobState::Paused,
        JobState::AwaitingUser,
        JobState::Completed,
        JobState::Failed,
        JobState::Cancelled,
        JobState::TimedOut,
    ];

    fn every_job() -> JobFilter {
        JobFilter {
            session_id: None,
            requester: None,
            state: StateFilter::States(EVERY_STATE.to_vec()),
            plugin: None,
        }
    }

    async fn listed(job: &Job, filter: &JobFilter) -> Vec<String> {
        list_jobs(&job.access, filter)
            .await
            .unwrap()
            .into_iter()
            .map(|view| view.row_doc_id)
            .collect()
    }

    fn sorted(mut rows: Vec<String>) -> Vec<String> {
        rows.sort();
        rows
    }

    async fn update_row(job: &Job, row: &str, field: &str, value: &str) {
        let mutation = format!(
            r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ {field}: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(row),
            escape_graphql_string(value),
        );
        ConfigAccess::transact_local_write(&job.node, None, "test.view.update_row", |txn| {
            let mutation = &mutation;
            Box::pin(async move { txn.execute(mutation).await })
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn list_jobs_returns_two_session_jobs_newest_first() {
        let mut job = new().await;
        let older = job
            .row("workflow", "call-1", json!({"argv": ["run"]}))
            .await;
        let newer = job
            .row("workflow", "call-2", json!({"argv": ["run"]}))
            .await;
        for (row, adopted_at) in [(&older, 1_700_000_000_000), (&newer, 1_700_000_000_001)] {
            job.write_pin(
                row,
                &JobPin {
                    adopted_at,
                    ..job.pin(row, None)
                },
            )
            .await;
            start_running(&job, row).await;
        }
        let filter = JobFilter {
            session_id: Some("session-1".into()),
            ..every_job()
        };

        assert_eq!(listed(&job, &filter).await, vec![newer, older]);
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn list_jobs_excludes_non_job_rows() {
        let mut job = new().await;
        job.row("workflow", "call-1", json!({"argv": ["run"]}))
            .await;
        let pinned = pinned_job_row(&mut job, "call-2").await;

        assert_eq!(listed(&job, &every_job()).await, vec![pinned]);
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn list_jobs_query_count_is_independent_of_job_count() {
        let mut job = new().await;
        pinned_job_row(&mut job, "call-1").await;
        let count = |job: &Job| {
            let queries = Arc::new(AtomicUsize::new(0));
            let access = job.access.clone();
            async move {
                crate::plugin_job::input::CHILD_QUERIES
                    .scope(queries.clone(), async {
                        list_jobs(&access, &every_job()).await.unwrap()
                    })
                    .await;
                queries.load(Ordering::Relaxed)
            }
        };

        let one = count(&job).await;
        pinned_job_row(&mut job, "call-2").await;
        pinned_job_row(&mut job, "call-3").await;
        let three = count(&job).await;

        assert!(one > 0);
        assert_eq!(three, one);
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn list_jobs_skips_an_undecodable_row_with_a_warning() {
        let mut job = new().await;
        let bogus = job
            .row("workflow", "call-1", json!({"argv": ["run"]}))
            .await;
        write_job_pin_json(&job, &bogus, json!({"bogus": 1})).await;
        let good = pinned_job_row(&mut job, "call-2").await;

        assert_eq!(listed(&job, &every_job()).await, vec![good]);
        job.node.shutdown().await;
    }

    /// A running job row of `requester`, inserted directly because
    /// `requester_did` is immutable.
    async fn requester_job_row(job: &Job, requester: &str, call_id: &str) -> String {
        insert(
            &job.node,
            "AgentToolCall",
            json!({
                "request_id": "request-1",
                "request_doc_id": "request-doc",
                "node_did": node_did(),
                "requester_did": requester,
                "session_id": "session-1",
                "tool_call_id": call_id,
                "tool_name": "workflow",
                "lifecycle_state": "running",
                "started_at": "2026-01-01T00:00:00Z",
                "job_pin": job.pin(call_id, None),
            }),
        )
        .await
    }

    #[tokio::test]
    async fn list_jobs_keeps_only_the_requesters_rows() {
        let job = new().await;
        let a = requester_job_row(&job, "a", "call-1").await;
        let b = requester_job_row(&job, "b", "call-2").await;
        let only = |requester| JobFilter {
            requester,
            ..every_job()
        };

        assert_eq!(listed(&job, &only(Some("a".into()))).await, vec![a.clone()]);
        assert_eq!(sorted(listed(&job, &only(None)).await), sorted(vec![a, b]));
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn list_jobs_without_a_session_lists_job_rows_of_every_session() {
        let mut job = new().await;
        let s1 = pinned_job_row(&mut job, "call-1").await;
        let s2 = pinned_job_row(&mut job, "call-2").await;
        update_row(&job, &s2, "session_id", "s2").await;
        let only = |session_id| JobFilter {
            session_id,
            ..every_job()
        };

        assert_eq!(
            listed(&job, &only(Some("session-1".into()))).await,
            vec![s1.clone()]
        );
        assert_eq!(
            listed(&job, &only(Some("s2".into()))).await,
            vec![s2.clone()]
        );
        assert_eq!(
            sorted(listed(&job, &only(None)).await),
            sorted(vec![s1, s2])
        );
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn list_jobs_states_keeps_only_the_listed_states() {
        let mut job = new().await;
        let running = pinned_job_row(&mut job, "call-1").await;
        let paused = pinned_job_row(&mut job, "call-2").await;
        settle_row(&job, &paused, Settlement::Control(JobControlKind::Pause)).await;
        let awaiting = pinned_job_row(&mut job, "call-3").await;
        settle_row(&job, &awaiting, await_user()).await;
        let completed = pinned_job_row(&mut job, "call-4").await;
        settle_row(&job, &completed, Settlement::Result(json!("done"))).await;
        let only = |states: Vec<JobState>| JobFilter {
            state: StateFilter::States(states),
            ..every_job()
        };

        assert_eq!(
            sorted(
                listed(
                    &job,
                    &only(vec![
                        JobState::Running,
                        JobState::Paused,
                        JobState::AwaitingUser
                    ])
                )
                .await
            ),
            sorted(vec![running.clone(), paused.clone(), awaiting.clone()])
        );
        assert_eq!(
            listed(&job, &only(vec![JobState::Completed])).await,
            vec![completed.clone()]
        );
        assert_eq!(
            sorted(listed(&job, &only(EVERY_STATE.to_vec())).await),
            sorted(vec![running, paused, awaiting, completed])
        );
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn list_jobs_not_terminal_or_undelivered_keeps_running_and_undelivered_rows() {
        let mut job = new().await;
        let running = pinned_job_row(&mut job, "call-1").await;
        let delivered = pinned_job_row(&mut job, "call-2").await;
        settle_row(&job, &delivered, Settlement::Result(json!("done"))).await;
        let undelivered = pinned_job_row(&mut job, "call-3").await;
        settle_row(
            &job,
            &undelivered,
            Settlement::Control(JobControlKind::Pause),
        )
        .await;
        let filter = JobFilter {
            state: StateFilter::NotTerminalOrUndelivered {
                delivered: BTreeSet::from([delivered]),
            },
            ..every_job()
        };

        assert_eq!(
            sorted(listed(&job, &filter).await),
            sorted(vec![running, undelivered])
        );
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn list_jobs_keeps_only_the_named_plugins_rows() {
        let mut job = new().await;
        let engine = pinned_job_row(&mut job, "call-1").await;
        let other = job
            .row("workflow", "call-2", json!({"argv": ["run"]}))
            .await;
        job.write_pin(
            &other,
            &JobPin {
                plugin_ref: "acme/other".into(),
                ..job.pin(&other, None)
            },
        )
        .await;
        start_running(&job, &other).await;
        let only = |plugin: &str| JobFilter {
            plugin: Some(plugin.into()),
            ..every_job()
        };

        assert_eq!(
            listed(&job, &only("acme/engine")).await,
            vec![engine.clone()]
        );
        assert_eq!(listed(&job, &only("acme/other")).await, vec![other.clone()]);
        assert_eq!(
            sorted(listed(&job, &every_job()).await),
            sorted(vec![engine, other])
        );
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn job_view_reads_over_a_served_graphql_endpoint() {
        let address = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(node_did())
                .data_path(dir.path().join("db"))
                .with_http(defra_node::HttpConfig::with_addr(address))
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let served = ConfigAccess::graphql(format!("http://{address}/api/v0/graphql"));
        tokio::time::timeout(Duration::from_secs(10), async {
            while served
                .execute("query { AgentRequest { _docID } }")
                .await
                .is_err()
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the embedded HTTP GraphQL endpoint must become ready");

        let mut request = claimed_request(&node, "request-1", "session-1", node_did()).await;
        let call = publish_accepted_on_claimed_request(
            node.clone(),
            &mut request,
            node_did(),
            0,
            "workflow",
            "call-1",
            json!({"argv": ["run"]}),
            AwaitMode::Foreground,
            true,
        )
        .await
        .unwrap();
        let row = call.doc_id().unwrap().to_owned();
        let pin = JobPin {
            chain_root_doc_id: row.clone(),
            ..fixture_job_pin()
        };
        ConfigAccess::transact_local_write(&node, None, "test.view.served_pin", |txn| {
            let (row, pin) = (&row, &pin);
            Box::pin(async move { write_pin_in_txn(txn, row, pin).await })
        })
        .await
        .unwrap();
        let local = ConfigAccess::local(node.clone());
        assert!(read_pin(&local, &row).await.unwrap().is_some());
        let mut lifecycle = load_lifecycle(&local, &row).await.unwrap();
        settle_plugin_job(&mut lifecycle, fail("engine_boom"))
            .await
            .unwrap();

        let over_local = load_job_view(&local, None, JobKey::RowDocId(row.clone()))
            .await
            .unwrap();
        let over_http = load_job_view(&served, None, JobKey::RowDocId(row.clone()))
            .await
            .unwrap();

        assert_eq!(over_local.state, JobState::Failed);
        assert_eq!(
            over_local
                .outcome
                .as_ref()
                .map(|outcome| outcome.code.as_str()),
            Some("engine_boom")
        );
        assert_eq!(over_http, over_local);
        node.shutdown().await;
    }
}

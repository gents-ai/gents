//! A client's read of one plugin job row, through any [`ConfigAccess`].

use std::collections::BTreeMap;

use gents_protocol::job::{
    JobChildRow, JobChildView, JobOutcome, JobPin, JobPinSummary, JobProgress, JobReadError,
    JobScope, JobState, JobView,
};
use serde::Deserialize;
use serde_json::Value;

use crate::plugin::job_calls::job_state;
use crate::store::config_client::ConfigAccess;
use crate::tools::background::job::admit::StepChild;
use crate::tools::background::job::input::{chain_of, derive_children, derive_children_for_jobs};
use crate::tools::background::job::keys::job_spawn_key;
use crate::tools::background::status::{
    observe_canonical_tool_output_with_access, CanonicalToolOutputObservation, ProcessControlScope,
};
use crate::tools::call::lifecycle::ToolCallState;

/// Which job row a view reads: by physical doc id, or by its address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobKey {
    RowDocId(String),
    Address {
        session_id: String,
        tool_call_id: String,
    },
}

fn job_row_selection() -> gents_protocol::document_query::Selection<
    gents_protocol::document_query::fields::agent_tool_call::Doc,
> {
    use gents_protocol::document_query::{fields::agent_tool_call as fields, Selection};
    Selection::doc_id()
        .field(fields::SESSION_ID)
        .field(fields::TOOL_CALL_ID)
        .field(fields::REQUEST_ID)
        .field(fields::REQUEST_DOC_ID)
        .field(fields::NODE_DID)
        .field(fields::REQUESTER_DID)
        .field(fields::LIFECYCLE_STATE)
        .field(fields::STARTED_AT)
        .field(fields::COMPLETED_AT)
        .field(fields::JOB_PIN)
        .field(fields::JOB_PROGRESS)
}

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

fn unavailable(error: impl Into<anyhow::Error>) -> JobReadError {
    let error = error.into();
    if crate::store::config_client::TransactionFailure::config_client_error(&error)
        .is_some_and(crate::store::config_client::ConfigClientError::is_permission_denied)
    {
        return JobReadError::PermissionDenied;
    }
    JobReadError::Unavailable {
        message: format!("{error:#}; retry when the node answers"),
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
    let query = {
        use gents_protocol::document_query::{fields::agent_tool_call as fields, Filter, Query};
        let query = Query::select(job_row_selection());
        match &key {
            JobKey::RowDocId(doc_id) => query.filter(Filter::eq(fields::DOC_ID, doc_id)),
            JobKey::Address {
                session_id,
                tool_call_id,
            } => query
                .filter(Filter::eq(fields::SESSION_ID, session_id))
                .filter(Filter::eq(fields::TOOL_CALL_ID, tool_call_id)),
        }
        .render()
    };
    let response = access.read(&query).await.map_err(unavailable)?;
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

/// Which job rows [`list_jobs`] returns: an optional session, a `JobState` set and an
/// optional plugin. `None` does not filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobFilter {
    pub session_id: Option<String>,
    pub state: StateFilter,
    /// `namespace/name`, matched against `JobView.pin.plugin_ref`.
    pub plugin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateFilter {
    /// Rows whose `JobView.state` is one of these.
    States(Vec<JobState>),
}

impl StateFilter {
    fn keeps(&self, view: &Decoded) -> bool {
        match self {
            StateFilter::States(states) => states.contains(&view.state),
        }
    }
}

/// The job rows matching `filter`, newest first by `adopted_at`. A row that
/// does not decode is skipped with a warning; children are derived in one batch.
pub async fn list_jobs(
    access: &ConfigAccess,
    filter: &JobFilter,
) -> Result<Vec<JobView>, JobReadError> {
    let rendered = {
        use gents_protocol::document_query::{fields::agent_tool_call as fields, Filter, Query};
        let mut query =
            Query::select(job_row_selection()).filter(Filter::not_null(fields::JOB_PIN));
        if let Some(session_id) = &filter.session_id {
            query = query.filter(Filter::eq(fields::SESSION_ID, session_id));
        }
        query.render()
    };
    let response = access.read(&rendered).await.map_err(unavailable)?;
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
    let scopes: Vec<_> = chains
        .iter()
        .zip(&jobs)
        .map(|(chain, job)| (chain.as_slice(), job.pin.ceiling_at_pin.result_bytes))
        .collect();
    let children = derive_children_for_jobs(access, &scopes)
        .await
        .map_err(unavailable)?;
    let mut views = Vec::new();
    for (job, pairs) in jobs.into_iter().zip(children) {
        let outcome = outcome_of(access, &job).await?;
        let children = children_of(&job.pin, pairs);
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
            target: child.target.1,
        })
        .collect()
}

#[cfg(test)]
mod tests;

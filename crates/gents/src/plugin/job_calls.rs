//! `job_calls`: the round service that serves a job step's children and a
//! job row's stored result. Requests and answers are `gents_protocol::job`'s
//! [`JobCall`] and [`RoundResult`], keyed by request id under `job_results`.

use rapidhash::RapidHashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use gents_protocol::job::{
    ChildOutputRequest, ChildOutputResponse, ChildOutputSlice, JobCall, JobChildView, JobPin,
    JobProgress, JobResultRequest, JobResultResponse, JobState, RoundResult,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::data::{Invocation, StampContext};
use crate::background_tools::{
    load_caused_request_terminals, read_tool_output_slice, LiveToolOutputRegistry,
    ProcessControlScope, ReadToolOutputOutcome,
};
use crate::config_client::ConfigAccess;
use crate::defra_node::EmbeddedNode;
use crate::graphql::{escape_graphql_string, graphql_string_list_literal};
use crate::plugin_job::input::derive_children;
use crate::tool_call_lifecycle::{CausedRequestTerminal, ToolCallState};

const MAX_REQUESTS_PER_ROUND: usize = 16;

/// The invoking tool call a plugin's `job_calls` run for.
#[derive(Clone)]
pub struct JobCaller {
    node: Arc<EmbeddedNode>,
    scope: ProcessControlScope,
    stamp: StampContext,
    chain: Vec<String>,
    result_bytes: u64,
}

impl JobCaller {
    /// A caller with no children: `child_output` finds no slot.
    pub fn new(node: Arc<EmbeddedNode>, scope: ProcessControlScope, stamp: StampContext) -> Self {
        Self {
            node,
            scope,
            stamp,
            chain: Vec::new(),
            result_bytes: 0,
        }
    }

    /// A job step's chain rows, whose caused requests are its children, and
    /// the `result_bytes` that caps each slot's page.
    pub fn with_children(mut self, chain: Vec<String>, result_bytes: u64) -> Self {
        self.chain = chain;
        self.result_bytes = result_bytes;
        self
    }
}

/// Lean `resultReadAuthorized`: the caller may manage the owner's background
/// calls, the row belongs to the same plugin, and it is terminal.
pub fn result_read(
    caller: &ProcessControlScope,
    owner: &ProcessControlScope,
    same_plugin: bool,
    terminal: bool,
) -> bool {
    caller.authorizes(
        &owner.session_id,
        &owner.node_did,
        owner.requester_did.as_deref(),
    ) && same_plugin
        && terminal
}

/// Lean `resultOffered`: tool-call invocations, job steps and non-job calls
/// alike; a callback has no caller session.
pub fn result_offer(invocation: &StampContext) -> bool {
    matches!(
        invocation.invocation(),
        Invocation::ToolCall | Invocation::JobStep
    )
}

/// The requests of one `job_calls` round.
pub(super) struct Batch {
    pub(super) requests: Vec<JobCall>,
    pub(super) state: Option<Value>,
}

fn id_of(call: &JobCall) -> &str {
    match call {
        JobCall::ChildOutput { id, .. } | JobCall::Result { id, .. } => id,
    }
}

/// The output's `job_calls.requests` array of [`JobCall`] and its optional
/// sibling `job_calls.state`; a `job_calls` that is not an object (an echoed
/// `true`) is no request.
pub(super) fn parse_batch(output: &Value) -> Result<Option<Batch>, String> {
    let Some(calls) = output.get("job_calls").and_then(Value::as_object) else {
        return Ok(None);
    };
    let requests = calls
        .get("requests")
        .filter(|requests| requests.is_array())
        .ok_or("job_calls.requests must be an array")?;
    let requests: Vec<JobCall> = serde_json::from_value(requests.clone()).map_err(|error| {
        format!(
            "job_calls.requests needs JobCall requests {{id, child_output: {{slots, offset, limit}}}} or {{id, result: {{row_doc_id, offset, limit}}}}: {error}"
        )
    })?;
    let mut ids = BTreeSet::new();
    if requests.is_empty()
        || requests.len() > MAX_REQUESTS_PER_ROUND
        || requests
            .iter()
            .any(|request| id_of(request).is_empty() || !ids.insert(id_of(request)))
    {
        return Err(format!(
            "job_calls.requests needs 1 to {MAX_REQUESTS_PER_ROUND} requests with distinct nonempty ids"
        ));
    }
    Ok(Some(Batch {
        requests,
        state: calls.get("state").filter(|state| !state.is_null()).cloned(),
    }))
}

/// `job_calls` for one call of a plugin that declares `job`.
pub(super) struct Session {
    caller: JobCaller,
    coordinate: String,
}

impl Session {
    /// Offered when the plugin declares `job`, the caller supplied its context
    /// and the invocation is offered the service.
    pub(super) fn for_call(
        declared: bool,
        coordinate: &str,
        caller: Option<JobCaller>,
    ) -> Option<Self> {
        let caller = caller.filter(|caller| declared && result_offer(&caller.stamp))?;
        Some(Self {
            caller,
            coordinate: coordinate.to_owned(),
        })
    }

    /// Answers by request id; a refused request is its own error result,
    /// never the call's.
    pub(super) async fn serve(
        &self,
        requests: Vec<JobCall>,
        max_output_bytes: usize,
    ) -> Map<String, Value> {
        let mut results = Map::new();
        for request in requests {
            let (id, answer) = match request {
                JobCall::ChildOutput { id, child_output } => (
                    id,
                    answer(child_output_slots(&self.caller, &child_output).await),
                ),
                JobCall::Result {
                    id,
                    result: request,
                } => (
                    id,
                    answer(
                        result(&self.caller, &self.coordinate, &request, max_output_bytes).await,
                    ),
                ),
            };
            results.insert(id, answer);
        }
        results
    }
}

fn answer<T>(outcome: Result<T>) -> Value
where
    T: Serialize,
{
    let round = match outcome {
        Ok(result) => RoundResult::Result { result },
        Err(error) => RoundResult::Error {
            error: format!("{error:#}"),
        },
    };
    serde_json::to_value(round).expect("a round result has a JSON encoding")
}

/// `text` from byte `offset` for at most `max` bytes on character boundaries,
/// and whether the text continues past the page.
fn page(text: &str, offset: u64, max: usize) -> (&str, bool) {
    let mut start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(text.len());
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let mut end = start.saturating_add(max).min(text.len());
    while end > start && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[start..end], end < text.len())
}

/// Each requested slot's latest attempt's terminal text, paged and capped at
/// the step's `result_bytes`.
async fn child_output_slots(
    caller: &JobCaller,
    request: &ChildOutputRequest,
) -> Result<ChildOutputResponse> {
    let access = ConfigAccess::local(caller.node.clone());
    let mut latest: BTreeMap<String, JobChildView> = BTreeMap::new();
    for (child, view) in derive_children(&access, &caller.chain, caller.result_bytes).await? {
        if latest
            .get(&child.slot)
            .is_none_or(|kept| kept.attempt <= view.attempt)
        {
            latest.insert(child.slot, view);
        }
    }
    let wanted = request
        .slots
        .iter()
        .map(|slot| {
            latest.get(slot).map(|view| (slot, view)).with_context(|| {
                format!("slot {slot:?} has no child; the step input's children names the slots")
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let list = graphql_string_list_literal(wanted.iter().map(|(_, view)| view.request_id.as_str()));
    let requests = access
        .execute(&format!(
            "{{ AgentRequest(filter: {{ request_id: {{ _in: {list} }} }}) {{ _docID request_id }} }}"
        ))
        .await?;
    let doc_ids: RapidHashMap<&str, &str> = requests
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|row| Some((row["request_id"].as_str()?, row["_docID"].as_str()?)))
        .collect();
    let terminals = load_caused_request_terminals(
        &access,
        &doc_ids
            .values()
            .map(|doc_id| (*doc_id).to_owned())
            .collect::<Vec<_>>(),
    )
    .await?;
    let cap = usize::try_from(request.limit.min(caller.result_bytes)).unwrap_or(usize::MAX);
    let mut slots = BTreeMap::new();
    for (slot, view) in wanted {
        let terminal = doc_ids
            .get(view.request_id.as_str())
            .and_then(|doc_id| terminals.get(*doc_id));
        let Some(CausedRequestTerminal::Completed { output }) = terminal else {
            bail!(
                "slot {slot:?} has no output yet (it is {:?}); read it once its children state is completed",
                view.state
            );
        };
        let (text, truncated) = page(output, request.offset, cap);
        slots.insert(
            slot.clone(),
            ChildOutputSlice {
                text: text.to_owned(),
                truncated,
            },
        );
    }
    Ok(ChildOutputResponse { slots })
}

#[derive(Deserialize)]
struct Row {
    tool_call_id: String,
    session_id: String,
    node_did: String,
    #[serde(default)]
    requester_did: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    job_pin: Option<JobPin>,
    #[serde(default)]
    job_progress: Option<JobProgress>,
}

/// The job state of a plugin job row: the native mirror of Lean
/// `JobState.of` (`Proofs/PluginJob/Types.lean`). `job_progress.state` alone
/// is not the state of an ended row, because a settle rewrites it only for a
/// pause or an await.
pub fn job_state(lifecycle: ToolCallState, progress: Option<&JobProgress>) -> JobState {
    match lifecycle {
        ToolCallState::Pending | ToolCallState::Running => JobState::Running,
        ToolCallState::Completed => progress
            .map(|progress| progress.state)
            .filter(|state| matches!(state, JobState::Paused | JobState::AwaitingUser))
            .unwrap_or(JobState::Completed),
        ToolCallState::Failed => JobState::Failed,
        ToolCallState::TimedOut => JobState::TimedOut,
        ToolCallState::Cancelled => JobState::Cancelled,
    }
}

/// `job_calls.result`: a terminal job row's stored canonical output from
/// `offset`, at most `limit` and `max_bytes` bytes, never re-rendered. The row
/// must lie in the caller's scope and be pinned to `coordinate`; both
/// decisions and the terminal check are [`result_read`].
pub async fn result(
    caller: &JobCaller,
    coordinate: &str,
    request: &JobResultRequest,
    max_bytes: usize,
) -> Result<JobResultResponse> {
    let row_doc_id = request.row_doc_id.as_str();
    let outside = || {
        anyhow::anyhow!(
            "job row {row_doc_id} is not in this call's scope; read only the results of this session's jobs"
        )
    };
    let response = ConfigAccess::local(caller.node.clone())
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ tool_call_id session_id node_did requester_did lifecycle_state job_pin job_progress }} }}"#,
            escape_graphql_string(row_doc_id)
        ))
        .await?;
    let row = response
        .pointer("/data/AgentToolCall/0")
        .ok_or_else(outside)?
        .clone();
    let row: Row = serde_json::from_value(row).context("decoding the job row")?;
    let owner = ProcessControlScope {
        request_id: String::new(),
        session_id: row.session_id,
        node_did: row.node_did,
        requester_did: row.requester_did,
    };
    let lifecycle = row
        .lifecycle_state
        .as_deref()
        .and_then(ToolCallState::from_persisted)
        .unwrap_or(ToolCallState::Running);
    let terminal = !matches!(lifecycle, ToolCallState::Pending | ToolCallState::Running);
    let same_plugin = row
        .job_pin
        .as_ref()
        .is_some_and(|pin| pin.plugin_ref == coordinate);
    if !result_read(&caller.scope, &owner, same_plugin, terminal) {
        if !result_read(&caller.scope, &owner, true, true) {
            return Err(outside());
        }
        if !same_plugin {
            bail!("job row {row_doc_id} belongs to another plugin; read it through that plugin");
        }
        bail!(
            "not_terminal: job row {row_doc_id} is still running; call `status` on it and read its result once it is terminal"
        );
    }
    let limit = usize::try_from(request.limit)
        .unwrap_or(usize::MAX)
        .min(max_bytes);
    let ReadToolOutputOutcome::Found(page) = read_tool_output_slice(
        &caller.node,
        &caller.scope,
        &LiveToolOutputRegistry::default(),
        &row.tool_call_id,
        request.offset,
        limit,
    )
    .await?
    else {
        return Err(outside());
    };
    Ok(JobResultResponse {
        row_doc_id: row_doc_id.to_owned(),
        state: job_state(lifecycle, row.job_progress.as_ref()),
        offset: page.next_offset.saturating_sub(page.output.len() as u64),
        result: page.output,
        total_bytes: page.total_bytes,
        truncated: page.has_more,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(session: &str, node: &str, requester: Option<&str>) -> ProcessControlScope {
        ProcessControlScope {
            request_id: "request".to_owned(),
            session_id: session.to_owned(),
            node_did: node.to_owned(),
            requester_did: requester.map(str::to_owned),
        }
    }

    #[test]
    fn result_read_is_scope_same_plugin_and_terminal() {
        let owner = scope("session", "node", Some("user"));
        assert!(result_read(&owner.clone(), &owner, true, true));
        for caller in [
            scope("other", "node", Some("user")),
            scope("session", "other", Some("user")),
            scope("session", "node", Some("other")),
            scope("session", "node", None),
        ] {
            assert!(!result_read(&caller, &owner, true, true), "{caller:?}");
        }
        assert!(!result_read(&owner.clone(), &owner, false, true));
        assert!(!result_read(&owner.clone(), &owner, true, false));
    }

    #[test]
    fn result_offer_is_false_only_for_callbacks() {
        let node_did = "node".to_owned();
        assert!(result_offer(&StampContext::ToolOutput {
            node_did: node_did.clone(),
            session_id: "s".to_owned(),
            request_doc_id: "r".to_owned(),
            tool_call_doc_id: "t".to_owned(),
        }));
        assert!(result_offer(&StampContext::JobStep {
            node_did: node_did.clone(),
            session_id: "s".to_owned(),
            request_doc_id: "r".to_owned(),
            tool_call_doc_id: "t".to_owned(),
            job_row_doc_id: "j".to_owned(),
            chain_root_doc_id: "c".to_owned(),
        }));
        assert!(!result_offer(&StampContext::Callback { node_did }));
    }
}

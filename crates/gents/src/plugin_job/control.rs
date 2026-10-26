//! The single kill owner for stop, pause, deadline and fail-closed: it
//! interrupts a job's live children and ends the row now.

use crate::plugin_job::origin::PluginJobError;
use gents_protocol::error_chain::ErrorChain;
use rapidhash::{HashMapExt, RapidHashMap};

use gents_protocol::job::{
    JobChildState, JobControlEcho, JobControlEchoKind, JobControlKind, JobControlOutcome,
    JobGuestPin, JobPin, JobReadError, JobResume, JobResumeEngine, JobState,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::{Deserialize, Serialize};

use super::admit::{
    resume_pin, resume_valid, root_call_of, HostFacts, Refusal, RootCall, StepChild, StepRow,
};
use super::driver::{read_step_row_in_txn, settle_plugin_job_in_txn, Settlement};
use super::input::chain_of;
use crate::background_tools::ProcessControlScope;
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::{escape_graphql_string, graphql_string_list_literal};
use crate::plugin_job::keys::job_spawn_key;
use crate::tool_call_lifecycle::{PluginJobTerminal, ToolCallLifecycle, ToolRowKind};

type Result<T, E = PluginJobError> = std::result::Result<T, E>;

/// Why a job row is killed: a control, the deadline, or a fail-closed refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillCause {
    Stop,
    Pause,
    Deadline,
    FailClosed(Refusal),
}

impl KillCause {
    /// Lean `Control.ends`; Lean has no fail-closed control, and `settleStep`
    /// ends such a row `failed`.
    fn ends(self) -> JobState {
        match self {
            Self::Stop => JobState::Cancelled,
            Self::Pause => JobState::Completed,
            Self::Deadline => JobState::TimedOut,
            Self::FailClosed(_) => JobState::Failed,
        }
    }

    fn settlement(self) -> Settlement {
        match self {
            Self::Stop => Settlement::Control(JobControlKind::Stop),
            Self::Pause => Settlement::Control(JobControlKind::Pause),
            Self::Deadline => Settlement::Refused(Refusal::TimedOut),
            Self::FailClosed(refusal) => Settlement::Refused(refusal),
        }
    }
}

/// Lean `Recovery.KillObservation`: what a kill reads about a child's caused request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KillObservation {
    CausedTerminal,
    CausedLiveLocal,
    CausedLiveRemote,
    Unresolved,
}

impl KillObservation {
    /// Lean `Recovery.killAction`.
    pub fn action(self) -> KillAction {
        match self {
            Self::CausedTerminal => KillAction::Settle,
            Self::CausedLiveLocal => KillAction::InterruptCaused,
            Self::CausedLiveRemote => KillAction::InterruptAndCancelRow,
            Self::Unresolved => KillAction::CancelRow,
        }
    }
}

/// Lean `Recovery.KillAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KillAction {
    Settle,
    InterruptCaused,
    InterruptAndCancelRow,
    CancelRow,
}

/// Lean `Control.kill`'s result: the row after the kill and each killed child's retry key and action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillOutcome {
    pub row: StepRow,
    pub won: bool,
    pub killed: Vec<(String, KillAction)>,
}

/// Lean `liveOwnChildren`: `ch.key.root == job.root`, read by re-encoding.
fn live_own_children(job: &StepRow) -> impl Iterator<Item = &StepChild> {
    job.children.iter().filter(|child| {
        child.state == JobChildState::Live
            && child.retry_key == job_spawn_key(&job.root, &child.slot, child.attempt)
    })
}

/// Lean `Control.kill`: only a running row is terminalized, and only that
/// terminalization kills the row's live own children.
pub fn kill_decision(
    job: &StepRow,
    cause: KillCause,
    observe: &dyn Fn(&str) -> KillObservation,
) -> KillOutcome {
    if job.state != JobState::Running {
        return KillOutcome {
            row: job.clone(),
            won: false,
            killed: Vec::new(),
        };
    }
    KillOutcome {
        won: true,
        row: StepRow {
            state: cause.ends(),
            ..job.clone()
        },
        killed: live_own_children(job)
            .map(|child| (child.retry_key.clone(), observe(&child.retry_key).action()))
            .collect(),
    }
}

/// The answer to a stop or pause control: a paused row reads `paused`
/// (Lean `JobState.of` with the pause note).
pub fn control_outcome(
    job: &StepRow,
    kind: JobControlKind,
    observe: &dyn Fn(&str) -> KillObservation,
) -> JobControlOutcome {
    if job.state != JobState::Running {
        return JobControlOutcome::AlreadyTerminal { state: job.state };
    }
    let (cause, state) = match kind {
        JobControlKind::Stop => (KillCause::Stop, JobState::Cancelled),
        JobControlKind::Pause => (KillCause::Pause, JobState::Paused),
    };
    JobControlOutcome::Applied {
        state,
        killed_children: kill_decision(job, cause, observe).killed.len() as u32,
    }
}

#[derive(Debug, Deserialize)]
struct CausedRequest {
    #[serde(rename = "_docID")]
    doc_id: String,
    retry_key: String,
    node_did: String,
    requester_did: Option<String>,
    lifecycle_state: Option<String>,
}

/// The requests the row's live own children caused, by retry key.
async fn caused_requests(
    txn: &ConfigApplyTxn<'_>,
    job: &StepRow,
) -> Result<RapidHashMap<String, CausedRequest>> {
    let keys: Vec<&str> = live_own_children(job)
        .map(|child| child.retry_key.as_str())
        .collect();
    if keys.is_empty() {
        return Ok(RapidHashMap::new());
    }
    let response = txn
        .execute(&format!(
            "{{ AgentRequest(filter: {{ retry_key: {{ _in: {} }} }}) {{ _docID retry_key node_did requester_did lifecycle_state }} }}",
            graphql_string_list_literal(keys)
        ))
        .await?;
    let rows: Vec<CausedRequest> = serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .cloned()
            .ok_or(PluginJobError::CausedRequestQueryOmittedRows)?,
    )
    .map_err(PluginJobError::DecodingCausedRequests)?;
    Ok(rows
        .into_iter()
        .map(|row| (row.retry_key.clone(), row))
        .collect())
}

/// The requests the row's live own children caused and what each child's
/// kill observes, by retry key: one observation per live own child.
async fn observe_children_in_txn(
    txn: &ConfigApplyTxn<'_>,
    job: &StepRow,
    node_did: &str,
) -> Result<(
    RapidHashMap<String, CausedRequest>,
    RapidHashMap<String, KillObservation>,
)> {
    let caused = caused_requests(txn, job).await?;
    let observed = live_own_children(job)
        .map(|child| {
            let observation = match caused.get(&child.retry_key) {
                None => KillObservation::Unresolved,
                Some(request)
                    if RequestLifecycleState::is_terminal_str(
                        request.lifecycle_state.as_deref(),
                    ) =>
                {
                    KillObservation::CausedTerminal
                }
                Some(request) if request.node_did == node_did => KillObservation::CausedLiveLocal,
                Some(_) => KillObservation::CausedLiveRemote,
            };
            (child.retry_key.clone(), observation)
        })
        .collect();
    Ok((caused, observed))
}

async fn observe_children(
    access: &ConfigAccess,
    job: &StepRow,
    node_did: &str,
) -> Result<(
    RapidHashMap<String, CausedRequest>,
    RapidHashMap<String, KillObservation>,
)> {
    access
        .transact_readonly("plugin_job.observe_children", |txn| {
            Box::pin(observe_children_in_txn(txn, job, node_did))
        })
        .await
}

/// What a kill of `job` observes about each live own child, by retry key.
pub(crate) async fn observe_live_children(
    access: &ConfigAccess,
    job: &StepRow,
    node_did: &str,
) -> Result<RapidHashMap<String, KillObservation>> {
    Ok(observe_children(access, job, node_did).await?.1)
}

/// Kills the job row `lifecycle` for `cause`: interrupts each live child its
/// chain caused and ends the row now with the cause's state, even while local
/// children are live. Authority is the job's own spawn edge; an interrupted
/// caller keeps its job. The row's execution receipt is the evidence
/// `settle_plugin_job` publishes: the caller's staged receipt, else the step
/// evidence persisted on the row.
pub async fn kill(
    access: &ConfigAccess,
    lifecycle: &mut ToolCallLifecycle,
    job: &StepRow,
    cause: KillCause,
) -> Result<KillOutcome> {
    let lifecycle_ref: &ToolCallLifecycle = lifecycle;
    let (outcome, terminal) = access
        .transact_write("plugin_job.kill", |txn| {
            Box::pin(kill_in_txn(txn, lifecycle_ref, job, cause))
        })
        .await?;
    if let Some(terminal) = terminal {
        crate::plugin_job::driver::finish_plugin_job_row(lifecycle, terminal).await?;
    }
    Ok(outcome)
}

async fn kill_in_txn(
    txn: &ConfigApplyTxn<'_>,
    lifecycle: &ToolCallLifecycle,
    job: &StepRow,
    cause: KillCause,
) -> Result<(KillOutcome, Option<PluginJobTerminal>)> {
    if job.state != JobState::Running {
        return Ok((
            KillOutcome {
                row: job.clone(),
                won: false,
                killed: Vec::new(),
            },
            None,
        ));
    }
    let (caused, observed) = observe_children_in_txn(txn, job, lifecycle.node_did()).await?;
    let observe = |key: &str| {
        observed
            .get(key)
            .copied()
            .unwrap_or(KillObservation::Unresolved)
    };
    let outcome = kill_decision(job, cause, &observe);
    let Some(terminal) =
        settle_plugin_job_in_txn(txn, lifecycle, cause.settlement(), false).await?
    else {
        return Ok((
            KillOutcome {
                row: job.clone(),
                won: false,
                killed: Vec::new(),
            },
            None,
        ));
    };
    if !terminal.won() {
        let row = read_step_row_in_txn(
            txn,
            lifecycle
                .doc_id()
                .ok_or(PluginJobError::JobRowLacksPhysicalIdentity)?,
        )
        .await?;
        return Ok((
            KillOutcome {
                row,
                won: false,
                killed: Vec::new(),
            },
            Some(terminal),
        ));
    }
    for (key, action) in &outcome.killed {
        let local = match action {
            KillAction::InterruptCaused => true,
            KillAction::InterruptAndCancelRow => false,
            KillAction::Settle | KillAction::CancelRow => continue,
        };
        let Some(request) = caused.get(key) else {
            continue;
        };
        let interrupted = crate::interrupt::interrupt_request_by_doc_id_in_txn(
            txn,
            &request.doc_id,
            &request.node_did,
            request.requester_did.as_deref(),
        )
        .await;
        match interrupted {
            Ok(()) => {}
            Err(error) if local => return Err(error.into()),
            Err(error) => tracing::warn!(
                caused_request_doc_id = request.doc_id,
                error = %ErrorChain(&error),
                "peer interrupt of a killed job child was not written"
            ),
        }
    }
    Ok((outcome, Some(terminal)))
}

/// Stops or pauses the job `scope`'s session started at `tool_call_id`.
/// Local only: a graphql access cannot reach the node's request interrupts.
/// The row ends through [`kill`], whose settlement publishes the receipt.
pub async fn control(
    access: &ConfigAccess,
    scope: &ProcessControlScope,
    tool_call_id: &str,
    action: JobControlKind,
) -> Result<JobControlOutcome, JobReadError> {
    let unavailable = |error: &(dyn std::error::Error + 'static)| JobReadError::Unavailable {
        message: format!("{}; retry when the node answers", ErrorChain(error)),
    };
    let ConfigAccess::Local(node) = access else {
        return Err(JobReadError::Unavailable {
            message: "job control is local-only in v1; stop or pause the job from its session (`workflow stop|pause <handle>`), the desktop, Grok or Codex".into(),
        });
    };
    let mut lifecycle =
        ToolCallLifecycle::load(node.node().clone(), &scope.session_id, tool_call_id)
            .await
            .map_err(|error| unavailable(&error))?
            .ok_or(JobReadError::NotYetObservable)?;
    if !scope.authorizes(
        lifecycle.session_id(),
        lifecycle.node_did(),
        lifecycle.requester_did(),
    ) {
        return Err(JobReadError::NotAuthorized);
    }
    if lifecycle.kind() != ToolRowKind::PluginJob {
        return Err(JobReadError::NotAJob);
    }
    let doc_id = lifecycle
        .doc_id()
        .ok_or_else(|| unavailable(&PluginJobError::JobRowLacksPhysicalIdentity))?
        .to_owned();
    let cause = match action {
        JobControlKind::Stop => KillCause::Stop,
        JobControlKind::Pause => KillCause::Pause,
    };
    let (killed, terminal) = access
        .transact_write("plugin_job.control", |txn| {
            let lifecycle = &lifecycle;
            let doc_id = &doc_id;
            Box::pin(async move {
                let row = read_step_row_in_txn(txn, doc_id).await?;
                kill_in_txn(txn, lifecycle, &row, cause).await
            })
        })
        .await
        .map_err(|error| unavailable(&error))?;
    if let Some(terminal) = terminal {
        crate::plugin_job::driver::finish_plugin_job_row(&mut lifecycle, terminal)
            .await
            .map_err(|error| unavailable(&error))?;
    }
    let outcome = if killed.won {
        JobControlOutcome::Applied {
            state: match action {
                JobControlKind::Stop => JobState::Cancelled,
                JobControlKind::Pause => JobState::Paused,
            },
            killed_children: killed.killed.len() as u32,
        }
    } else {
        JobControlOutcome::AlreadyTerminal {
            state: killed.row.state,
        }
    };
    Ok(outcome)
}

/// Lean `echo`: what every step of a resumed row sees of its resume block.
pub fn resume_echo(resume: &JobResume) -> JobControlEcho {
    JobControlEcho {
        kind: if resume.answer.is_some() {
            JobControlEchoKind::Answer
        } else if resume.engine == Some(JobResumeEngine::Current) {
            JobControlEchoKind::EngineCurrent
        } else {
            JobControlEchoKind::Resume
        },
        payload: Some(resume.clone()),
    }
}

/// A resume `validate_resume` admitted: the pin, root call and echo the new
/// row adopts with, and the chain it continues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedResume {
    pub pin: JobPin,
    pub root_call: RootCall,
    pub echo: JobControlEcho,
    /// The prior row's chain, then the prior row.
    pub chain: Vec<String>,
    pub resume_of: String,
    pub replayed_from: Option<String>,
}

#[derive(Deserialize)]
struct OwnerRow {
    session_id: String,
    node_did: String,
    #[serde(default)]
    requester_did: Option<String>,
}

#[derive(Deserialize)]
struct SessionPin {
    #[serde(default)]
    job_pin: Option<serde_json::Value>,
}

/// Lean `resumeTarget` and `resumeIsHead` for `row_doc_id`, read through
/// `txn`: `(row_doc_id, head, Some((owner, same_plugin, prior, root)))`, or
/// `(row_doc_id, false, None)` when no such row exists.
pub(crate) async fn resume_target_in_txn(
    txn: &ConfigApplyTxn<'_>,
    plugin: &str,
    row_doc_id: &str,
) -> Result<super::admit::ResumeTarget> {
    let response = txn
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ session_id node_did requester_did }} }}"#,
            escape_graphql_string(row_doc_id)
        ))
        .await?;
    let Some(owner) = response.pointer("/data/AgentToolCall/0") else {
        return Ok((row_doc_id.to_owned(), false, None));
    };
    let owner: OwnerRow =
        serde_json::from_value(owner.clone()).map_err(PluginJobError::DecodingResumeTargetRow)?;
    let prior = read_step_row_in_txn(txn, row_doc_id).await?;
    let root = if prior.root == row_doc_id {
        prior.clone()
    } else {
        read_step_row_in_txn(txn, &prior.root).await?
    };
    let same_plugin = prior
        .pin
        .as_ref()
        .is_some_and(|pin| pin.plugin_ref == plugin);
    let session = txn
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ job_pin }} }}"#,
            escape_graphql_string(&owner.session_id)
        ))
        .await?;
    let rows: Vec<SessionPin> = serde_json::from_value(
        session
            .pointer("/data/AgentToolCall")
            .cloned()
            .ok_or(PluginJobError::SessionJobQueryOmittedRows)?,
    )
    .map_err(PluginJobError::DecodingSessionJobRows)?;
    let head = !rows.into_iter().any(|row| {
        row.job_pin
            .and_then(|pin| serde_json::from_value::<JobPin>(pin).ok())
            .is_some_and(|pin| pin.resume_of.as_deref() == Some(row_doc_id))
    });
    let owner = ProcessControlScope {
        request_id: String::new(),
        session_id: owner.session_id,
        node_did: owner.node_did,
        requester_did: owner.requester_did,
    };
    Ok((
        row_doc_id.to_owned(),
        head,
        Some((owner, same_plugin, prior, root)),
    ))
}

/// Validates a step-0 `effects.resume` inside the adoption transaction: Lean
/// `resumeValid` over the prior row, the chain root and the owner scope read
/// through `txn`, with no second rule. The chain-head flag is admission's.
/// `guest` and `facts` are the step output's pin and `StepContext.resume_facts`.
/// A refusal is `Ok(Err(Refusal::PolicyDenied))`; `Err` is a storage failure.
pub async fn validate_resume(
    txn: &ConfigApplyTxn<'_>,
    caller: &ProcessControlScope,
    plugin: &str,
    resume: &JobResume,
    guest: &JobGuestPin,
    facts: &HostFacts,
) -> Result<std::result::Result<ValidatedResume, Refusal>> {
    let (_, _, target) = resume_target_in_txn(txn, plugin, &resume.row_doc_id).await?;
    let Some((owner, same_plugin, prior, root)) = target else {
        return Ok(Err(Refusal::PolicyDenied));
    };
    if root.pin.is_none()
        || !resume_valid(caller, &owner, same_plugin, &prior, guest, facts, resume)
    {
        return Ok(Err(Refusal::PolicyDenied));
    }
    let pin = resume_pin(&root, &prior, guest, facts);
    let chain = chain_of(txn, &prior.row_doc_id, prior.pin.as_ref()).await?;
    Ok(Ok(ValidatedResume {
        replayed_from: pin.replayed_from.clone(),
        pin,
        root_call: root_call_of(&root),
        echo: resume_echo(resume),
        chain,
        resume_of: prior.row_doc_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::background_tools::{
        observe_canonical_tool_output_with_access, CanonicalToolOutputObservation,
    };
    use crate::graphql::escape_graphql_string;
    use crate::plugin_job::admit::StepCall;
    use crate::tool_call_lifecycle::admission_fixture::{
        published_admission, PublishedAdmission, PublishedAdmissionOptions,
    };
    use crate::tool_call_lifecycle::AwaitMode;
    use serde_json::{json, Value};

    const ROOT: &str = "root";

    fn child(slot: &str, state: JobChildState) -> StepChild {
        StepChild {
            retry_key: job_spawn_key(ROOT, slot, 1),
            slot: slot.to_owned(),
            attempt: 1,
            target: ("node".to_owned(), "general".to_owned()),
            state,
            messages: 0,
        }
    }

    fn row(state: JobState, children: Vec<StepChild>) -> StepRow {
        StepRow {
            root: ROOT.to_owned(),
            row_doc_id: ROOT.to_owned(),
            caller_session: "session".to_owned(),
            caller_cwd: "/work".to_owned(),
            handle: "handle".to_owned(),
            caller_hop: 0,
            call: StepCall {
                argv: vec!["run".to_owned()],
                args: None,
                path: None,
                script: None,
                agent_budget: None,
            },
            root_call: None,
            resume: None,
            pin: None,
            spent: children.len() as u32,
            children,
            observed_terminal: 0,
            consecutive_failures: 0,
            state,
        }
    }

    /// A running background job row with its invocation receipt published and
    /// its progress stored.
    async fn job_row(name: &str) -> PublishedAdmission {
        let mut admission = published_admission(PublishedAdmissionOptions {
            name: name.to_owned(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            job_pin: true,
            ..Default::default()
        })
        .await
        .unwrap();
        assert!(admission
            .tool
            .publish_background_receipt("started")
            .await
            .unwrap());
        super::super::driver::store_running_progress(&admission).await;
        admission
    }

    async fn finish(admission: PublishedAdmission) {
        admission.node.shutdown().await;
        let _ = std::fs::remove_dir_all(admission.path);
    }

    fn access(admission: &PublishedAdmission) -> ConfigAccess {
        ConfigAccess::local(admission.node.clone())
    }

    /// The request a job child `slot` caused, owned by `node_did`.
    async fn caused(
        admission: &PublishedAdmission,
        slot: &str,
        node_did: &str,
        state: &str,
    ) -> String {
        let value = json!({
            "request_id": format!("child-{slot}"),
            "purpose": "normal",
            "node_did": node_did,
            "requester_did": null,
            "session_id": format!("child-session-{slot}"),
            "agent_id": "general",
            "lifecycle_state": state,
            "retry_key": job_spawn_key(ROOT, slot, 1),
            "created_at": "2026-01-01T00:00:00Z",
            "caused_by_parent_tool_call_doc_id": admission.tool.doc_id().unwrap(),
        });
        ConfigAccess::transact_local_write(&admission.node, None, "test.control.caused", |txn| {
            let value = &value;
            Box::pin(async move {
                let response = txn
                    .execute_with_variables(
                        "mutation($input: AgentRequestMutationInputArg!) {create_AgentRequest(input:$input){_docID}}",
                        &json!({ "input": value }),
                    )
                    .await?;
                Ok::<_, PluginJobError>(gents_protocol::graphql::extract_mutation_doc_id(&response, "AgentRequest")?)
            })
        })
        .await
        .unwrap()
    }

    async fn interrupted(admission: &PublishedAdmission, doc_id: &str) -> bool {
        crate::interrupt::fetch_interrupt_requested_at_by_doc_id(&admission.node, doc_id)
            .await
            .unwrap()
            .is_some()
    }

    async fn row_state(admission: &PublishedAdmission) -> String {
        let response = access(admission)
            .execute(&format!(
                r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state }} }}"#,
                escape_graphql_string(admission.tool.doc_id().unwrap()),
            ))
            .await
            .unwrap();
        response["data"]["AgentToolCall"][0]["lifecycle_state"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn output(admission: &PublishedAdmission) -> Value {
        let tool = &admission.tool;
        match observe_canonical_tool_output_with_access(
            &access(admission),
            tool.doc_id().unwrap(),
            tool.request_doc_id().unwrap(),
            tool.session_id(),
            tool.node_did(),
            tool.requester_did(),
        )
        .await
        .unwrap()
        {
            CanonicalToolOutputObservation::Closed(output) => {
                serde_json::from_str(&output).unwrap()
            }
            CanonicalToolOutputObservation::Open(output) => {
                panic!("job row output is still open: {output}")
            }
        }
    }

    #[tokio::test]
    async fn rolling_back_a_kill_preserves_the_running_row_and_child_interrupts() {
        let admission = job_row("control-kill-rollback").await;
        let node_did = admission.tool.node_did().to_owned();
        let live = caused(&admission, "a", &node_did, "processing").await;
        let job = row(JobState::Running, vec![child("a", JobChildState::Live)]);
        let access = access(&admission);
        let result: Result<()> = access
            .transact_write("test.control.kill_rollback", |txn| {
                let (lifecycle, job) = (&admission.tool, &job);
                Box::pin(async move {
                    let (killed, terminal) =
                        kill_in_txn(txn, lifecycle, job, KillCause::Stop).await?;
                    assert!(killed.won);
                    assert!(terminal.is_some_and(|terminal| terminal.won()));
                    return Err(PluginJobError::AbortStagedKill);
                })
            })
            .await;
        assert!(format!("{}", ErrorChain(&result.unwrap_err()))
            .contains("abort the staged kill"));
        assert_eq!(row_state(&admission).await, "running");
        assert!(!interrupted(&admission, &live).await);
        finish(admission).await;
    }

    #[tokio::test]
    async fn kill_for_each_cause_interrupts_live_local_children_and_ends_the_row_now() {
        let cases = [
            (KillCause::Stop, "cancelled", "status", "cancelled"),
            (KillCause::Pause, "completed", "status", "paused"),
            (
                KillCause::Deadline,
                "timedOut",
                "code",
                "plugin_job_timed_out",
            ),
            (
                KillCause::FailClosed(Refusal::Stalled),
                "failed",
                "code",
                "plugin_job_stalled",
            ),
        ];
        for (index, (cause, state, field, value)) in cases.into_iter().enumerate() {
            let mut admission = job_row(&format!("control-kill-{index}")).await;
            let node_did = admission.tool.node_did().to_owned();
            let live = caused(&admission, "a", &node_did, "processing").await;
            let job = row(JobState::Running, vec![child("a", JobChildState::Live)]);
            let outcome = kill(&access(&admission), &mut admission.tool, &job, cause)
                .await
                .unwrap();
            assert_eq!(
                outcome.killed,
                vec![(job_spawn_key(ROOT, "a", 1), KillAction::InterruptCaused)]
            );
            assert!(interrupted(&admission, &live).await, "{cause:?}");
            assert_eq!(row_state(&admission).await, state, "{cause:?}");
            assert_eq!(output(&admission).await[field], value, "{cause:?}");
            finish(admission).await;
        }
    }

    #[tokio::test]
    async fn kill_leaves_terminal_children_untouched() {
        let mut admission = job_row("control-kill-terminal-children").await;
        let node_did = admission.tool.node_did().to_owned();
        let ended = caused(&admission, "a", &node_did, "completed").await;
        let settling = caused(&admission, "b", &node_did, "completed").await;
        let job = row(
            JobState::Running,
            vec![
                child("a", JobChildState::Completed),
                child("b", JobChildState::Live),
            ],
        );
        let outcome = kill(
            &access(&admission),
            &mut admission.tool,
            &job,
            KillCause::Stop,
        )
        .await
        .unwrap();
        assert_eq!(
            outcome.killed,
            vec![(job_spawn_key(ROOT, "b", 1), KillAction::Settle)]
        );
        assert!(!interrupted(&admission, &ended).await);
        assert!(!interrupted(&admission, &settling).await);
        assert_eq!(row_state(&admission).await, "cancelled");
        finish(admission).await;
    }

    #[tokio::test]
    async fn kill_on_a_terminal_row_is_a_no_op() {
        let mut admission = job_row("control-kill-terminal-row").await;
        let node_did = admission.tool.node_did().to_owned();
        let live = caused(&admission, "a", &node_did, "processing").await;
        crate::plugin_job::driver::settle_plugin_job(
            &mut admission.tool,
            Settlement::Result(json!("done")),
        )
        .await
        .unwrap();
        let job = row(JobState::Completed, vec![child("a", JobChildState::Live)]);
        let outcome = kill(
            &access(&admission),
            &mut admission.tool,
            &job,
            KillCause::Stop,
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            KillOutcome {
                row: job,
                won: false,
                killed: Vec::new()
            }
        );
        assert!(!interrupted(&admission, &live).await);
        assert_eq!(row_state(&admission).await, "completed");
        finish(admission).await;
    }

    #[tokio::test]
    async fn interrupted_caller_keeps_its_job() {
        let admission = job_row("control-caller-interrupted").await;
        let node_did = admission.tool.node_did().to_owned();
        let live = caused(&admission, "a", &node_did, "processing").await;
        crate::interrupt::interrupt_request_by_doc_id_with_access(
            &access(&admission),
            admission.tool.request_doc_id().unwrap(),
            &node_did,
            admission.tool.requester_did(),
        )
        .await
        .unwrap();
        assert!(interrupted(&admission, admission.tool.request_doc_id().unwrap()).await);
        // The owner that reacts to a parent request for background rows must
        // leave the job running and its child alone.
        let report = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &admission.node,
            &node_did,
            &crate::BackgroundExecutionRegistry::default(),
        )
        .await
        .unwrap();
        assert_eq!(report.tool_calls_terminalized, 0);
        assert_eq!(row_state(&admission).await, "running");
        assert!(!interrupted(&admission, &live).await);
        finish(admission).await;
    }

    #[test]
    fn kill_decision_ends_a_running_row_and_maps_each_live_own_child() {
        let foreign = StepChild {
            retry_key: job_spawn_key("other", "d", 1),
            ..child("d", JobChildState::Live)
        };
        let job = row(
            JobState::Running,
            vec![
                child("a", JobChildState::Live),
                child("b", JobChildState::Live),
                child("c", JobChildState::Completed),
                foreign,
            ],
        );
        let (k1, k2) = (job_spawn_key(ROOT, "a", 1), job_spawn_key(ROOT, "b", 1));
        let observe = |key: &str| {
            if key == k1 {
                KillObservation::CausedLiveLocal
            } else if key == k2 {
                KillObservation::CausedLiveRemote
            } else {
                KillObservation::CausedLiveLocal
            }
        };
        for (cause, state) in [
            (KillCause::Stop, JobState::Cancelled),
            (KillCause::Pause, JobState::Completed),
            (KillCause::Deadline, JobState::TimedOut),
            (KillCause::FailClosed(Refusal::Stalled), JobState::Failed),
        ] {
            let outcome = kill_decision(&job, cause, &observe);
            assert_eq!(outcome.row.state, state, "{cause:?}");
            assert_eq!(
                outcome.killed,
                vec![
                    (k1.clone(), KillAction::InterruptCaused),
                    (k2.clone(), KillAction::InterruptAndCancelRow),
                ],
                "{cause:?}"
            );
        }
    }

    #[test]
    fn kill_decision_on_a_terminal_row_kills_nothing() {
        let job = row(JobState::Completed, vec![child("a", JobChildState::Live)]);
        for cause in [
            KillCause::Stop,
            KillCause::Pause,
            KillCause::Deadline,
            KillCause::FailClosed(Refusal::Stalled),
        ] {
            assert_eq!(
                kill_decision(&job, cause, &|_| KillObservation::CausedLiveLocal),
                KillOutcome {
                    row: job.clone(),
                    won: false,
                    killed: Vec::new()
                }
            );
        }
    }

    #[test]
    fn kill_action_and_spellings_are_lean_kill_action() {
        for (observation, action, observation_spelling, action_spelling) in [
            (
                KillObservation::CausedTerminal,
                KillAction::Settle,
                "causedTerminal",
                "settle",
            ),
            (
                KillObservation::CausedLiveLocal,
                KillAction::InterruptCaused,
                "causedLiveLocal",
                "interruptCaused",
            ),
            (
                KillObservation::CausedLiveRemote,
                KillAction::InterruptAndCancelRow,
                "causedLiveRemote",
                "interruptAndCancelRow",
            ),
            (
                KillObservation::Unresolved,
                KillAction::CancelRow,
                "unresolved",
                "cancelRow",
            ),
        ] {
            assert_eq!(observation.action(), action);
            assert_eq!(
                serde_json::to_value(observation).unwrap(),
                observation_spelling
            );
            assert_eq!(serde_json::to_value(action).unwrap(), action_spelling);
        }
    }

    #[test]
    fn control_outcome_is_applied_with_the_killed_count_or_already_terminal() {
        let children = vec![
            child("a", JobChildState::Live),
            child("b", JobChildState::Live),
        ];
        let running = row(JobState::Running, children.clone());
        let observe = |_: &str| KillObservation::CausedLiveLocal;
        assert_eq!(
            control_outcome(&running, JobControlKind::Stop, &observe),
            JobControlOutcome::Applied {
                state: JobState::Cancelled,
                killed_children: 2
            }
        );
        assert!(matches!(
            control_outcome(&running, JobControlKind::Pause, &observe),
            JobControlOutcome::Applied {
                state: JobState::Paused,
                ..
            }
        ));
        let completed = row(JobState::Completed, children);
        for kind in [JobControlKind::Stop, JobControlKind::Pause] {
            assert_eq!(
                control_outcome(&completed, kind, &observe),
                JobControlOutcome::AlreadyTerminal {
                    state: JobState::Completed
                }
            );
        }
    }

    #[tokio::test]
    async fn kill_after_a_restart_publishes_the_persisted_step_evidence() {
        use gents_protocol::plugin::{PluginExecutionReceipt, PluginExecutionVerdict};
        let admission = job_row("control-kill-restart").await;
        let evidence = PluginExecutionReceipt {
            coordinate: "fixture/engine".into(),
            artifact_digest: "sha256:engine".into(),
            input_digest: "persisted".into(),
            output_digest: Some("sha256:output".into()),
            authority: None,
            limits: None,
            verdict: PluginExecutionVerdict::Success,
        };
        let doc_id = admission.tool.doc_id().unwrap().to_owned();
        ConfigAccess::transact_local_write(&admission.node, None, "test.control.step", |txn| {
            let (doc_id, evidence) = (doc_id.clone(), evidence.clone());
            Box::pin(async move {
                crate::plugin_job::records::write_step_state_in_txn(txn, &doc_id, &evidence, None)
                    .await
            })
        })
        .await
        .unwrap();
        let load = || async {
            ToolCallLifecycle::load_by_doc_id(
                admission.node.clone(),
                &doc_id,
                admission.tool.node_did(),
                admission.tool.session_id(),
                admission.tool.requester_did(),
            )
            .await
            .unwrap()
            .unwrap()
        };
        let mut loaded = load().await;
        let job = row(JobState::Running, Vec::new());
        kill(&access(&admission), &mut loaded, &job, KillCause::Deadline)
            .await
            .unwrap();
        let stored_receipt = || async {
            access(&admission)
                .execute(&format!(
                    r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state plugin_execution_receipt }} }}"#,
                    escape_graphql_string(&doc_id),
                ))
                .await
                .unwrap()["data"]["AgentToolCall"][0]
                .clone()
        };
        let ended = stored_receipt().await;
        assert_eq!(ended["lifecycle_state"], "timedOut");
        assert_eq!(
            serde_json::from_value::<PluginExecutionReceipt>(
                ended["plugin_execution_receipt"].clone()
            )
            .unwrap(),
            PluginExecutionReceipt {
                output_digest: None,
                verdict: PluginExecutionVerdict::Timeout,
                ..evidence
            }
        );
        let ended_job = row(JobState::TimedOut, Vec::new());
        let mut again = load().await;
        kill(&access(&admission), &mut again, &ended_job, KillCause::Stop)
            .await
            .unwrap();
        assert_eq!(stored_receipt().await, ended);
        finish(admission).await;
    }

    use crate::plugin_job::admit::RootCall;
    use crate::plugin_job::driver::read_step_row;
    use crate::plugin_job::input::tests::{
        live_settings, new_job, node_did, resume_block, set_lifecycle, Job,
    };
    use crate::tool_call_lifecycle::admission_fixture::fixture_job_pin;
    use gents_protocol::job::{JobOrigin, JobPolicy};

    const PLUGIN: &str = "acme/engine";

    /// A pinned job row of `PLUGIN` in the caller's session, in `state`; its
    /// own call arguments and `args_digest` are named by `call`.
    async fn finished(
        job: &mut Job,
        call: &str,
        root: Option<&str>,
        resume_of: Option<&str>,
        state: JobState,
    ) -> String {
        let row = job
            .row("workflow", call, json!({"argv": ["run", call]}))
            .await;
        let pin = JobPin {
            args_digest: format!("sha256:args-{call}"),
            ..job.pin(root.unwrap_or(&row), resume_of)
        };
        job.write_pin(&row, &pin).await;
        job.write_progress(&row, state, 0).await;
        if state == JobState::Completed {
            set_lifecycle(&job.node, &row, "completed").await;
        }
        row
    }

    async fn new_chain() -> Job {
        new_job(Some(&live_settings(JobPolicy::default())), None).await
    }

    fn caller() -> ProcessControlScope {
        ProcessControlScope {
            request_id: "request-1".to_owned(),
            session_id: "session-1".to_owned(),
            node_did: node_did().to_owned(),
            requester_did: None,
        }
    }

    fn guest(script_id: &str, agent_budget: u32) -> JobGuestPin {
        JobGuestPin {
            script_id: script_id.to_owned(),
            source_digest: "sha256:fixture-source".to_owned(),
            agent_budget,
            handle: "resume-handle".to_owned(),
        }
    }

    fn facts(prior: &str, engine_digest: &str, replayed_from: Option<&str>) -> HostFacts {
        HostFacts {
            engine_digest: engine_digest.to_owned(),
            plugin_ref: PLUGIN.to_owned(),
            cwd: "/work".to_owned(),
            args_digest: "sha256:resume-args".to_owned(),
            chain_root: prior.to_owned(),
            origin: JobOrigin::Installed,
            adopted_at: 1_700_000_001_000,
            resume_of: Some(prior.to_owned()),
            replayed_from: replayed_from.map(str::to_owned),
            ceiling_at_pin: fixture_job_pin().ceiling_at_pin,
        }
    }

    const ENGINE: &str = "sha256:fixture-engine";

    async fn validate(
        job: &Job,
        caller: &ProcessControlScope,
        plugin: &str,
        resume: &JobResume,
        guest: &JobGuestPin,
        facts: &HostFacts,
    ) -> std::result::Result<ValidatedResume, Refusal> {
        job.access
            .transact_readonly("test.control.validate_resume", |txn| {
                Box::pin(validate_resume(txn, caller, plugin, resume, guest, facts))
            })
            .await
            .unwrap()
    }

    /// The caller resumes `prior` with an unchanged script, engine and budget.
    async fn plain_resume(job: &Job, prior: &str) -> std::result::Result<ValidatedResume, Refusal> {
        validate(
            job,
            &caller(),
            PLUGIN,
            &resume_block(prior),
            &guest("fixture-script", 4),
            &facts(prior, ENGINE, None),
        )
        .await
    }

    #[tokio::test]
    async fn resume_outside_scope_is_refused() {
        let mut job = new_chain().await;
        let prior = finished(&mut job, "call-1", None, None, JobState::Completed).await;
        let stranger = ProcessControlScope {
            session_id: "session-2".to_owned(),
            ..caller()
        };
        let refused = validate(
            &job,
            &stranger,
            PLUGIN,
            &resume_block(&prior),
            &guest("fixture-script", 4),
            &facts(&prior, ENGINE, None),
        )
        .await;
        assert_eq!(refused, Err(Refusal::PolicyDenied));
        assert!(plain_resume(&job, &prior).await.is_ok());
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn resume_of_another_plugin_is_refused() {
        let mut job = new_chain().await;
        let prior = finished(&mut job, "call-1", None, None, JobState::Completed).await;
        let refused = validate(
            &job,
            &caller(),
            "acme/other",
            &resume_block(&prior),
            &guest("fixture-script", 4),
            &facts(&prior, ENGINE, None),
        )
        .await;
        assert_eq!(refused, Err(Refusal::PolicyDenied));
        assert!(plain_resume(&job, &prior).await.is_ok());
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn resume_of_a_running_or_pinless_row_is_refused() {
        let mut job = new_chain().await;
        let running = finished(&mut job, "call-1", None, None, JobState::Running).await;
        assert_eq!(
            plain_resume(&job, &running).await,
            Err(Refusal::PolicyDenied)
        );
        let pinless = job
            .row("workflow", "call-2", json!({"argv": ["run"]}))
            .await;
        job.write_progress(&pinless, JobState::Completed, 0).await;
        assert_eq!(
            plain_resume(&job, &pinless).await,
            Err(Refusal::PolicyDenied)
        );
        assert_eq!(
            plain_resume(&job, "no-such-row").await,
            Err(Refusal::PolicyDenied)
        );
        let done = finished(&mut job, "call-3", None, None, JobState::Completed).await;
        assert!(plain_resume(&job, &done).await.is_ok());
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn resume_with_a_different_script_id_is_refused() {
        let mut job = new_chain().await;
        let prior = finished(&mut job, "call-1", None, None, JobState::Completed).await;
        let refused = validate(
            &job,
            &caller(),
            PLUGIN,
            &resume_block(&prior),
            &guest("other-script", 4),
            &facts(&prior, ENGINE, None),
        )
        .await;
        assert_eq!(refused, Err(Refusal::PolicyDenied));
        assert!(plain_resume(&job, &prior).await.is_ok());
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn resume_with_a_different_engine_digest_is_refused_unless_engine_current() {
        let mut job = new_chain().await;
        let prior = finished(&mut job, "call-1", None, None, JobState::Completed).await;
        let moved = facts(&prior, "sha256:new-engine", None);
        let refused = validate(
            &job,
            &caller(),
            PLUGIN,
            &resume_block(&prior),
            &guest("fixture-script", 4),
            &moved,
        )
        .await;
        assert_eq!(refused, Err(Refusal::PolicyDenied));
        let current = JobResume {
            engine: Some(JobResumeEngine::Current),
            ..resume_block(&prior)
        };
        let admitted = validate(
            &job,
            &caller(),
            PLUGIN,
            &current,
            &guest("fixture-script", 4),
            &moved,
        )
        .await;
        assert!(admitted.is_ok());
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn engine_current_repins_and_records_replayed_from() {
        let mut job = new_chain().await;
        let prior = finished(&mut job, "call-1", None, None, JobState::Completed).await;
        let current = JobResume {
            engine: Some(JobResumeEngine::Current),
            ..resume_block(&prior)
        };
        let validated = validate(
            &job,
            &caller(),
            PLUGIN,
            &current,
            &guest("fixture-script", 4),
            &facts(&prior, "sha256:new-engine", Some(&prior)),
        )
        .await
        .unwrap();
        assert_eq!(validated.pin.engine_digest, "sha256:new-engine");
        assert_eq!(validated.pin.replayed_from.as_deref(), Some(prior.as_str()));
        assert_eq!(validated.replayed_from.as_deref(), Some(prior.as_str()));
        assert_eq!(validated.echo.kind, JobControlEchoKind::EngineCurrent);
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn resume_with_a_lower_budget_is_refused() {
        let mut job = new_chain().await;
        let prior = finished(&mut job, "call-1", None, None, JobState::Completed).await;
        assert_eq!(
            validate_budget(&job, &prior, 3).await,
            Err(Refusal::PolicyDenied)
        );
        assert!(validate_budget(&job, &prior, 4).await.is_ok());
        assert!(validate_budget(&job, &prior, 5).await.is_ok());
        job.node.shutdown().await;
    }

    async fn validate_budget(
        job: &Job,
        prior: &str,
        budget: u32,
    ) -> std::result::Result<ValidatedResume, Refusal> {
        validate(
            job,
            &caller(),
            PLUGIN,
            &resume_block(prior),
            &guest("fixture-script", budget),
            &facts(prior, ENGINE, None),
        )
        .await
    }

    #[tokio::test]
    async fn resume_keeps_the_root_args_digest_handle_and_chain_root() {
        let mut job = new_chain().await;
        let root = finished(&mut job, "call-1", None, None, JobState::Completed).await;
        let prior = finished(
            &mut job,
            "call-2",
            Some(&root),
            Some(&root),
            JobState::Completed,
        )
        .await;
        let validated = plain_resume(&job, &prior).await.unwrap();
        assert_eq!(validated.pin.args_digest, "sha256:args-call-1");
        assert_eq!(validated.pin.handle, format!("handle-{root}"));
        assert_eq!(validated.pin.chain_root_doc_id, root);
        assert_eq!(validated.chain, [root.clone(), prior.clone()]);
        assert_eq!(validated.resume_of, prior);
        assert_eq!(
            validated.root_call.argv,
            ["run".to_owned(), "call-1".to_owned()]
        );
        assert_eq!(validated.echo, resume_echo(&resume_block(&prior)));
        job.node.shutdown().await;
    }

    fn pinned(row_pin: JobPin) -> StepRow {
        StepRow {
            pin: Some(row_pin),
            ..row(JobState::Completed, Vec::new())
        }
    }

    #[test]
    fn resume_pin_and_root_call_of_are_lean_resume_pin_and_root_call_of() {
        let root = StepRow {
            call: StepCall {
                argv: vec!["root-argv".to_owned()],
                args: Some(r#"{"a":1}"#.to_owned()),
                path: Some("root/path".to_owned()),
                script: Some("root script".to_owned()),
                agent_budget: Some(9),
            },
            ..pinned(JobPin {
                handle: "root-handle".to_owned(),
                args_digest: "sha256:root-args".to_owned(),
                chain_root_doc_id: "chain-root".to_owned(),
                script_id: "root-script".to_owned(),
                ..fixture_job_pin()
            })
        };
        let prior = pinned(JobPin {
            handle: "prior-handle".to_owned(),
            args_digest: "sha256:prior-args".to_owned(),
            chain_root_doc_id: "prior-root".to_owned(),
            ..fixture_job_pin()
        });
        let guest = JobGuestPin {
            script_id: "guest-script".to_owned(),
            source_digest: "sha256:guest-source".to_owned(),
            agent_budget: 6,
            handle: "guest-handle".to_owned(),
        };
        let mut facts = facts("prior", "sha256:facts-engine", Some("replayed"));
        facts.cwd = "/facts/cwd".to_owned();
        facts.plugin_ref = "facts/plugin".to_owned();
        let pin = resume_pin(&root, &prior, &guest, &facts);
        assert_eq!(
            pin,
            JobPin {
                script_id: "guest-script".to_owned(),
                source_digest: "sha256:guest-source".to_owned(),
                origin: facts.origin,
                engine_digest: "sha256:facts-engine".to_owned(),
                plugin_ref: "facts/plugin".to_owned(),
                agent_budget: 6,
                handle: "root-handle".to_owned(),
                cwd: "/facts/cwd".to_owned(),
                adopted_at: facts.adopted_at,
                args_digest: "sha256:root-args".to_owned(),
                chain_root_doc_id: "chain-root".to_owned(),
                resume_of: Some("prior".to_owned()),
                replayed_from: Some("replayed".to_owned()),
                ceiling_at_pin: facts.ceiling_at_pin.clone(),
            }
        );
        assert_eq!(
            root_call_of(&root),
            RootCall {
                argv: vec!["root-argv".to_owned()],
                args: Some(r#"{"a":1}"#.to_owned()),
                path: Some("root/path".to_owned()),
                agent_budget: Some(9),
            }
        );
    }

    #[test]
    fn resume_echo_kind_is_answer_then_engine_current_then_resume() {
        let block = resume_block("prior");
        let cases = [
            (
                JobResume {
                    answer: Some("yes".to_owned()),
                    engine: Some(JobResumeEngine::Current),
                    ..block.clone()
                },
                JobControlEchoKind::Answer,
            ),
            (
                JobResume {
                    engine: Some(JobResumeEngine::Current),
                    ..block.clone()
                },
                JobControlEchoKind::EngineCurrent,
            ),
            (block, JobControlEchoKind::Resume),
        ];
        for (resume, kind) in cases {
            assert_eq!(
                resume_echo(&resume),
                JobControlEcho {
                    kind,
                    payload: Some(resume.clone()),
                }
            );
        }
    }

    type Target = crate::plugin_job::admit::ResumeTarget;

    async fn target(job: &Job, plugin: &str, row: &str) -> Target {
        job.access
            .transact_readonly("test.control.resume_target", |txn| {
                Box::pin(resume_target_in_txn(txn, plugin, row))
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_row_already_resumed_is_not_a_chain_head() {
        let mut job = new_chain().await;
        let p = finished(&mut job, "call-1", None, None, JobState::Completed).await;
        let (id, head, found) = target(&job, PLUGIN, &p).await;
        let (owner, same_plugin, prior, root) = found.unwrap();
        assert_eq!((id.as_str(), head, same_plugin), (p.as_str(), true, true));
        assert_eq!(
            owner,
            ProcessControlScope {
                request_id: String::new(),
                ..caller()
            }
        );
        assert_eq!(prior, read_step_row(&job.access, &p).await.unwrap());
        assert_eq!(root, prior);

        let (_, _, found) = target(&job, "acme/other", &p).await;
        assert!(!found.unwrap().1);

        assert_eq!(
            target(&job, PLUGIN, "no-such-row").await,
            ("no-such-row".to_owned(), false, None)
        );

        let resumed = finished(&mut job, "call-2", Some(&p), Some(&p), JobState::Completed).await;
        assert!(!target(&job, PLUGIN, &p).await.1);
        assert!(target(&job, PLUGIN, &resumed).await.1);
        job.node.shutdown().await;
    }
}

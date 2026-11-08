use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::Value;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::capture::{capture_documents_with_access, run_captures_with_access, CaptureBindings};
use super::evidence::usage_of;
use super::outcome::{provider_reason, stage_failure_kind, trigger_failure};
use super::settle::{extend_with_later_requests, remaining_of, settle_home};
use super::submit::{seed_and_await_fire, submit_stage, StageSubmission};
use super::{Reconciled, POLL};
use crate::defra_node::Node as EmbeddedNode;
use crate::eval::runner::embedded::home::{EmbeddedHome, RunningRuntime};
#[cfg(test)]
use crate::eval::runner::embedded::observe::collect_request_evidence;
use crate::eval::runner::embedded::observe::{
    await_submitted_terminal_with_access, await_terminal_with_access,
    collect_request_evidence_with_access, poll_request_with_access, NoHook, RequestEvidence,
    TerminalObservation,
};
use crate::eval::runner::executor::{Capture, StageEvidence, StageSpec, TrialLocator, TrialSpec};
use crate::eval::OutcomeKind;

const GRACE: Duration = Duration::from_secs(30);

pub(super) async fn run_stages(spec: &TrialSpec, env: &StageEnv<'_>) -> Vec<StageEvidence> {
    let mut stages = Vec::new();
    let mut root_grants = std::collections::BTreeSet::new();
    for authored in &spec.stages {
        if let Err(error) = super::root_grants::grant_pending_root_readers(
            env.home,
            &spec.evaluator_did,
            env.workspace,
            &mut root_grants,
        )
        .await
        {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(error.as_ref()), "the trial User could not grant pending root readers");
        }
        let mut reviewed = authored.clone();
        if authored.review_previous {
            let feedback = stages
                .last()
                .ok_or_else(|| anyhow::anyhow!("no previous stage"))
                .and_then(|previous| spec.review.feedback(previous));
            match feedback {
                Ok(feedback) => reviewed.prompt.push_str(&format!("\n\n{feedback}")),
                Err(error) => {
                    tracing::warn!(%error, "eval review could not be prepared");
                    stages.push(StageEvidence {
                        subject_agent: None,
                        stage_id: authored.stage_id.clone(),
                        request_id: None,
                        terminal_state: None,
                        failure_kind: Some(OutcomeKind::Grader),
                        provider_reason: None,
                        messages: Vec::new(),
                        tool_calls: Vec::new(),
                        inference_calls: Vec::new(),
                        captures: Default::default(),
                        prods: 0,
                    });
                    break;
                }
            }
        }
        let stage = &reviewed;
        spec.progress.stage_started(&stage.stage_id);
        let evidence = run_stage(spec, env, stage).await;
        let mut summary = spec.review.summarize(&evidence);
        summary["usage"] =
            serde_json::to_value(usage_of(&evidence.inference_calls)).unwrap_or(Value::Null);
        spec.progress.stage_result(&stage.stage_id, summary);
        spec.progress.stage_ended(&stage.stage_id);
        let failed = evidence.failure_kind.is_some();
        stages.push(evidence);
        if failed {
            break;
        }
    }
    stages
}

#[derive(Clone, Copy)]
pub(super) struct StageEnv<'a> {
    pub(super) agent_did: &'a str,
    pub(super) cancel: &'a CancellationToken,
    pub(super) home: &'a EmbeddedHome,
    pub(super) runtime: &'a RunningRuntime,
    pub(super) locator: &'a TrialLocator,
    pub(super) workspace: &'a Path,
    pub(super) ready: &'a watch::Receiver<Reconciled>,
}

async fn run_stage(spec: &TrialSpec, env: &StageEnv<'_>, stage: &StageSpec) -> StageEvidence {
    let StageEnv {
        home,
        locator,
        workspace,
        ..
    } = *env;
    let observed = submit_and_observe(spec, env, stage).await;
    StageEvidence {
        subject_agent: Some(super::super::super::subject_binding::SubjectAgent {
            slot: spec.agent_slot.clone(),
            agent_did: env.agent_did.to_owned(),
        }),
        stage_id: stage.stage_id.clone(),
        request_id: observed.request_id.clone(),
        terminal_state: observed.terminal_state,
        provider_reason: provider_reason(observed.failure_kind, &observed.evidence),
        failure_kind: observed.failure_kind,
        messages: observed.evidence.messages,
        tool_calls: observed.evidence.tool_calls,
        inference_calls: observed.evidence.inference_calls,
        captures: match home.client_access(&spec.evaluator_did) {
            Ok(access) => {
                run_captures_with_access(
                    &access,
                    &locator.trial_node_did,
                    &locator.session_id,
                    workspace,
                    &stage.captures,
                    CaptureBindings {
                        subject: Some(&super::super::super::subject_binding::SubjectAgent {
                            slot: spec.agent_slot.clone(),
                            agent_did: env.agent_did.to_owned(),
                        }),
                        evaluator_did: Some(&spec.evaluator_did),
                        request_id: observed.request_id.as_deref(),
                    },
                )
                .await
            }
            Err(error) => {
                tracing::warn!(%error, "eval captures have an invalid evaluator identity");
                Default::default()
            }
        },
        prods: observed.prods,
    }
}

struct ObservedStage {
    request_id: Option<String>,
    prods: u32,
    terminal_state: Option<RequestLifecycleState>,
    failure_kind: Option<OutcomeKind>,
    evidence: RequestEvidence,
}

impl ObservedStage {
    fn unsubmitted() -> Self {
        Self {
            request_id: None,
            prods: 0,
            terminal_state: None,
            failure_kind: Some(OutcomeKind::Infrastructure),
            evidence: RequestEvidence::default(),
        }
    }

    fn cancelled() -> Self {
        Self {
            failure_kind: Some(OutcomeKind::Runtime),
            ..Self::unsubmitted()
        }
    }
}

async fn observe_stage_terminal(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    cancel: &CancellationToken,
    runtime: &RunningRuntime,
    request_id: &str,
    remaining: Option<Duration>,
) -> (Result<TerminalObservation>, bool) {
    let watch_started = std::time::Instant::now();
    let stopped;
    let observed = tokio::select! {
        observed = await_submitted_terminal_with_access(node, access, request_id, remaining, GRACE, &NoHook) => {
            stopped = false;
            observed
        }
        () = cancel.cancelled() => {
            stopped = true;
            interrupt_and_settle(node, access, request_id).await
        }
        // Nothing is left to settle the request; the request may have settled
        // within the same poll, so one last unbounded read first.
        () = runtime_exited(runtime) => {
            let row = poll_request_with_access(access, request_id).await.ok().flatten();
            let settled = row.and_then(|row| {
                let terminal_state = RequestLifecycleState::parse(row.lifecycle_state.as_deref()?)
                    .ok()
                    .filter(|state| state.is_terminal())?;
                Some(TerminalObservation {
                    terminal_state: Some(terminal_state),
                    session_id: row.session_id,
                    interrupted_on_deadline: false,
                    elapsed: watch_started.elapsed(),
                })
            });
            stopped = settled.is_none();
            settled.ok_or_else(|| anyhow!("the trial runtime exited before the request settled"))
        }
    };
    (observed, stopped)
}

async fn submit_and_observe(
    spec: &TrialSpec,
    env: &StageEnv<'_>,
    stage: &StageSpec,
) -> ObservedStage {
    let StageEnv {
        agent_did,
        cancel,
        home,
        runtime,
        locator,
        ready,
        ..
    } = *env;
    let access = match home.client_access(&spec.evaluator_did) {
        Ok(access) => access,
        Err(error) => {
            tracing::warn!(%error, "eval stage has an invalid evaluator identity");
            return ObservedStage::unsubmitted();
        }
    };
    let stage_started = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let deadline = stage.deadline_secs.map(Duration::from_secs);
    let started = Instant::now();
    let submit = async {
        match &stage.seed {
            Some(seed) => {
                let schema = super::install::trusted_seed_schema(spec, &seed.collection)?;
                seed_and_await_fire(
                    home,
                    &spec.evaluator_did,
                    ready,
                    stage,
                    Some(&schema),
                    &spec.trial_id,
                )
                .await
            }
            None => {
                let request_id = uuid::Uuid::new_v4().to_string();
                submit_stage(
                    home,
                    &spec.evaluator_did,
                    locator,
                    agent_did,
                    stage,
                    &request_id,
                )
                .await
                .map(|()| StageSubmission::Submitted {
                    request_id,
                    remaining: deadline,
                })
            }
        }
    };
    // A cancel that lands between the request write and its return ends here
    // without interrupt_and_settle; the trial home is torn down with it.
    let submitted = tokio::select! {
        biased;
        () = cancel.cancelled() => return ObservedStage::cancelled(),
        submitted = submit => submitted,
    };
    let (request_id, remaining) = match submitted {
        Ok(StageSubmission::Submitted {
            request_id,
            remaining,
        }) => (request_id, remaining),
        Ok(StageSubmission::Failed(kind)) => {
            return ObservedStage {
                failure_kind: Some(kind),
                ..ObservedStage::unsubmitted()
            };
        }
        Err(error) => {
            tracing::warn!(
                error = %format!("{error:#}"),
                trial_id = %spec.trial_id,
                stage_id = %stage.stage_id,
                "eval stage could not be submitted"
            );
            return ObservedStage::unsubmitted();
        }
    };

    let (observed, stopped) =
        observe_stage_terminal(&home.node, &access, cancel, runtime, &request_id, remaining).await;
    let observed = match observed {
        Ok(observed) => Some(observed),
        Err(error) => {
            tracing::warn!(
                error = %format!("{error:#}"),
                trial_id = %spec.trial_id,
                stage_id = %stage.stage_id,
                "eval stage could not be observed to a terminal state"
            );
            None
        }
    };

    let settling = stage.settle && !stopped && !runtime.handle.is_finished() && observed.is_some();
    if settling {
        let budget = remaining_of(deadline, started);
        tokio::select! {
            biased;
            () = cancel.cancelled() => {}
            quiet = settle_home(&access, budget) => match quiet {
                Ok(true) => {}
                Ok(false) => tracing::warn!(
                    trial_id = %spec.trial_id,
                    stage_id = %stage.stage_id,
                    "eval stage deadline ended before the trial home was quiet; capturing it with requests still running"
                ),
                Err(error) => tracing::warn!(
                    error = %format!("{error:#}"),
                    trial_id = %spec.trial_id,
                    stage_id = %stage.stage_id,
                    "eval stage could not observe the trial home settling"
                ),
            },
        }
    }

    let (prods, prod_failed) = if settling {
        continue_until_done(spec, env, stage, deadline, started).await
    } else {
        (0, false)
    };

    let mut collected = true;
    let mut evidence = collect_request_evidence_with_access(&access, &request_id)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(
                error = %format!("{error:#}"),
                trial_id = %spec.trial_id,
                stage_id = %stage.stage_id,
                "eval stage evidence could not be collected"
            );
            collected = false;
            RequestEvidence::default()
        });
    // The stage's own request decides how the stage ended; later requests in
    // the session are what the subject did next, not a second verdict on it.
    let failure_kind = if prod_failed {
        Some(OutcomeKind::Infrastructure)
    } else {
        stage_failure_kind(observed.as_ref(), collected, stopped, &evidence)
    };
    if settling && collected {
        if let Err(error) =
            extend_with_later_requests(&access, &locator.session_id, &request_id, &mut evidence)
                .await
        {
            tracing::warn!(
                error = %format!("{error:#}"),
                trial_id = %spec.trial_id,
                stage_id = %stage.stage_id,
                "eval stage evidence of later session requests could not be collected"
            );
            return ObservedStage {
                request_id: Some(request_id),
                prods,
                terminal_state: observed
                    .as_ref()
                    .and_then(|observed| observed.terminal_state),
                failure_kind: Some(OutcomeKind::Infrastructure),
                evidence,
            };
        }
    }

    ObservedStage {
        request_id: Some(request_id),
        prods,
        terminal_state: observed
            .as_ref()
            .and_then(|observed| observed.terminal_state),
        failure_kind: match failure_kind {
            None => {
                trigger_failure(
                    &home.node,
                    &locator.trial_node_did,
                    &stage_started,
                    None,
                    &spec.trial_id,
                    &stage.stage_id,
                )
                .await
            }
            failed => failed,
        },
        evidence,
    }
}

async fn continue_until_done(
    spec: &TrialSpec,
    env: &StageEnv<'_>,
    stage: &StageSpec,
    deadline: Option<Duration>,
    started: Instant,
) -> (u32, bool) {
    let StageEnv {
        agent_did,
        cancel,
        home,
        locator,
        ..
    } = *env;
    let access = match home.client_access(&spec.evaluator_did) {
        Ok(access) => access,
        Err(error) => {
            tracing::warn!(%error, "eval continuation has an invalid evaluator identity");
            return (0, true);
        }
    };
    let Some(continuation) = &stage.continuation else {
        return (0, false);
    };
    let until = stage.captures.iter().find_map(|capture| match capture {
        Capture::Documents {
            name,
            collection,
            filter,
            fields,
        } if name == &continuation.until => Some((collection, filter, fields)),
        _ => None,
    });
    let Some((collection, filter, fields)) = until else {
        return (0, false);
    };
    let prod = StageSpec {
        prompt: continuation.prompt.clone(),
        ..stage.clone()
    };
    let mut prods = 0;
    while prods < continuation.max && !cancel.is_cancelled() {
        match capture_documents_with_access(
            &access,
            collection,
            filter,
            fields,
            &locator.trial_node_did,
        )
        .await
        {
            Ok(rows) if !rows.is_empty() => break,
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    error = %format!("{error:#}"),
                    trial_id = %spec.trial_id,
                    "eval continuation could not read its until capture"
                );
                return (prods, true);
            }
        }
        let remaining = remaining_of(deadline, started);
        if remaining.is_some_and(|remaining| remaining.is_zero()) {
            break;
        }
        let request_id = uuid::Uuid::new_v4().to_string();
        if let Err(error) = submit_stage(
            home,
            &spec.evaluator_did,
            locator,
            agent_did,
            &prod,
            &request_id,
        )
        .await
        {
            tracing::warn!(
                error = %format!("{error:#}"),
                trial_id = %spec.trial_id,
                "eval continuation prod could not be submitted"
            );
            return (prods, true);
        }
        prods += 1;
        tracing::info!(target: "gents::eval", trial_id = %spec.trial_id, prods, "continuation prod sent");
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            _ = await_submitted_terminal_with_access(&home.node, &access, &request_id, remaining, GRACE, &NoHook) => {}
        }
        let remaining = remaining_of(deadline, started);
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            _ = settle_home(&access, remaining) => {}
        }
    }
    (prods, false)
}

async fn runtime_exited(runtime: &RunningRuntime) {
    while !runtime.handle.is_finished() {
        tokio::time::sleep(POLL).await;
    }
}

async fn interrupt_and_settle(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    request_id: &str,
) -> Result<TerminalObservation> {
    let row = poll_request_with_access(access, request_id)
        .await?
        .context("cancelled stage request is unavailable")?;
    crate::request::interrupt::interrupt_request_by_doc_id_with_access(
        access,
        row.doc_id
            .as_deref()
            .context("request has no physical identity")?,
        row.node_did
            .as_deref()
            .context("request has no node identity")?,
        row.requester_did.as_deref(),
    )
    .await
    .context("interrupting a cancelled stage")?;
    await_terminal_with_access(
        node,
        access,
        request_id,
        Some(Duration::ZERO),
        GRACE,
        &NoHook,
    )
    .await
}

#[cfg(test)]
mod tests;

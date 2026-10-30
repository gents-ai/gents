use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::Value;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::capture::{capture_documents, run_captures};
use super::evidence::usage_of;
use super::outcome::{provider_reason, stage_failure_kind, trigger_failure};
use super::settle::{extend_with_later_requests, remaining_of, settle_home};
use super::submit::{seed_and_await_fire, submit_stage, StageSubmission};
use super::{Reconciled, POLL};
use crate::defra_node::EmbeddedNode;
use crate::eval::runner::embedded::home::{EmbeddedHome, RunningRuntime};
use crate::eval::runner::embedded::observe::{
    await_terminal, collect_request_evidence, poll_request, RequestEvidence, TerminalObservation,
};
use crate::eval::runner::executor::{Capture, StageEvidence, StageSpec, TrialLocator, TrialSpec};
use crate::eval::OutcomeKind;

const GRACE: Duration = Duration::from_secs(30);

pub(super) async fn run_stages(
    spec: &TrialSpec,
    cancel: &CancellationToken,
    home: &EmbeddedHome,
    runtime: &RunningRuntime,
    locator: &TrialLocator,
    workspace: &Path,
    ready: &watch::Receiver<Reconciled>,
) -> Vec<StageEvidence> {
    let mut stages = Vec::new();
    for authored in &spec.stages {
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
        let env = StageEnv {
            cancel,
            home,
            runtime,
            locator,
            workspace,
            ready,
        };
        let evidence = run_stage(spec, &env, stage).await;
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
struct StageEnv<'a> {
    cancel: &'a CancellationToken,
    home: &'a EmbeddedHome,
    runtime: &'a RunningRuntime,
    locator: &'a TrialLocator,
    workspace: &'a Path,
    ready: &'a watch::Receiver<Reconciled>,
}

async fn run_stage(spec: &TrialSpec, env: &StageEnv<'_>, stage: &StageSpec) -> StageEvidence {
    let StageEnv {
        cancel,
        home,
        runtime,
        locator,
        workspace,
        ready,
    } = *env;
    let observed = submit_and_observe(spec, cancel, home, runtime, locator, stage, ready).await;
    StageEvidence {
        stage_id: stage.stage_id.clone(),
        request_id: observed.request_id,
        terminal_state: observed.terminal_state,
        provider_reason: provider_reason(observed.failure_kind, &observed.evidence),
        failure_kind: observed.failure_kind,
        messages: observed.evidence.messages,
        tool_calls: observed.evidence.tool_calls,
        inference_calls: observed.evidence.inference_calls,
        captures: run_captures(
            &home.node,
            &locator.trial_node_did,
            &locator.session_id,
            workspace,
            &stage.captures,
        )
        .await,
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
    cancel: &CancellationToken,
    runtime: &RunningRuntime,
    request_id: &str,
    remaining: Option<Duration>,
) -> (Result<TerminalObservation>, bool) {
    let watch_started = std::time::Instant::now();
    let stopped;
    let observed = tokio::select! {
        observed = await_terminal(node, request_id, remaining, GRACE) => {
            stopped = false;
            observed
        }
        () = cancel.cancelled() => {
            stopped = true;
            interrupt_and_settle(node, request_id).await
        }
        // Nothing is left to settle the request; the request may have settled
        // within the same poll, so one last unbounded read first.
        () = runtime_exited(runtime) => {
            let row = poll_request(node, request_id).await.ok().flatten();
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
    cancel: &CancellationToken,
    home: &EmbeddedHome,
    runtime: &RunningRuntime,
    locator: &TrialLocator,
    stage: &StageSpec,
    ready: &watch::Receiver<Reconciled>,
) -> ObservedStage {
    let stage_started = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let deadline = stage.deadline_secs.map(Duration::from_secs);
    let started = Instant::now();
    let submit = async {
        match &stage.seed {
            Some(seed) => {
                seed_and_await_fire(home, ready, seed, deadline, &spec.trial_id, &stage.stage_id)
                    .await
            }
            None => {
                let request_id = uuid::Uuid::new_v4().to_string();
                submit_stage(&home.node, locator, &spec.agent_id, stage, &request_id)
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
        observe_stage_terminal(&home.node, cancel, runtime, &request_id, remaining).await;
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
            quiet = settle_home(&home.node, budget) => match quiet {
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
        continue_until_done(spec, cancel, home, locator, stage, deadline, started).await
    } else {
        (0, false)
    };

    let mut collected = true;
    let mut evidence = collect_request_evidence(&home.node, &request_id)
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
            extend_with_later_requests(&home.node, &locator.session_id, &request_id, &mut evidence)
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
    cancel: &CancellationToken,
    home: &EmbeddedHome,
    locator: &TrialLocator,
    stage: &StageSpec,
    deadline: Option<Duration>,
    started: Instant,
) -> (u32, bool) {
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
        match capture_documents(
            &home.node,
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
        if let Err(error) =
            submit_stage(&home.node, locator, &spec.agent_id, &prod, &request_id).await
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
            _ = await_terminal(&home.node, &request_id, remaining, GRACE) => {}
        }
        let remaining = remaining_of(deadline, started);
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            _ = settle_home(&home.node, remaining) => {}
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
    request_id: &str,
) -> Result<TerminalObservation> {
    crate::interrupt::interrupt_request(node, request_id)
        .await
        .context("interrupting a cancelled stage")?;
    await_terminal(node, request_id, Some(Duration::ZERO), GRACE).await
}

#[cfg(test)]
mod tests;

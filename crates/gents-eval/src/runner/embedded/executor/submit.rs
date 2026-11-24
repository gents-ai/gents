use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::watch;

use super::install::{bind_fixture_actors, create_document};
use super::outcome::{failed_fires, fire_failure_kind, FailedFires, FireFailure};
use super::settle::{remaining_of, request_counts, Quiet};
use super::{Reconciled, POLL};
use crate::runner::embedded::home::{EmbeddedHome, RunningRuntime, RUNTIME_READY_TIMEOUT};
use crate::runner::executor::{StageSpec, TrialLocator};
use crate::OutcomeKind;
use gents_session::request::lifecycle::{
    build_signed_client_request, ExecutionOrigin, RequestIdentity, RequestSigner, RequestSpec,
};
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;

/// How long a seed stage without its own deadline waits for a trigger to fire
/// a request, or a seed that must be skipped waits for the home to be quiet.
/// No request exists yet, so no turn is bounded by it; a subject that never
/// configured the trigger would otherwise hold the trial forever.
const SEED_FIRE_WINDOW: Duration = Duration::from_secs(600);

#[derive(Debug)]
pub(super) enum StageSubmission {
    Submitted {
        request_id: String,
        /// Absent, the stage waits for the request's own terminal state.
        remaining: Option<Duration>,
    },
    /// A seed the automation had to skip, skipped.
    Skipped,
    Failed {
        kind: OutcomeKind,
        detail: Option<String>,
    },
}

/// A seed stage's document and how its wait ended.
#[derive(Debug)]
pub(super) struct Seeded {
    pub(super) doc_id: String,
    pub(super) submission: StageSubmission,
}

/// Lean `Eval.SeedObservation`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SeedObservation {
    pub(crate) expect_fire: bool,
    pub(crate) observable: bool,
    pub(crate) fired: bool,
    pub(crate) failures: Vec<FireFailure>,
    pub(crate) quiet: bool,
    pub(crate) window_ended: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SeedStep {
    Wait,
    Observe,
    Pass,
    Fail(OutcomeKind),
}

/// Lean `Eval.seedStep`.
pub(crate) fn seed_step(observation: &SeedObservation) -> SeedStep {
    if !observation.observable {
        return SeedStep::Fail(OutcomeKind::Infrastructure);
    }
    if observation.fired {
        return if observation.expect_fire {
            SeedStep::Observe
        } else {
            SeedStep::Fail(OutcomeKind::ModelAcceptance)
        };
    }
    if let Some(kind) = fire_failure_kind(&observation.failures) {
        return SeedStep::Fail(kind);
    }
    match (
        observation.expect_fire,
        observation.quiet,
        observation.window_ended,
    ) {
        (true, _, true) => SeedStep::Fail(OutcomeKind::ModelAcceptance),
        (true, _, false) => SeedStep::Wait,
        (false, true, _) => SeedStep::Pass,
        (false, false, true) => SeedStep::Fail(OutcomeKind::Inconclusive),
        (false, false, false) => SeedStep::Wait,
    }
}

pub(super) async fn submit_stage(
    home: &EmbeddedHome,
    evaluator_did: &str,
    locator: &TrialLocator,
    agent_did: &str,
    stage: &StageSpec,
    request_id: &str,
) -> Result<()> {
    let identity = home.evaluator(evaluator_did)?;
    let access = home.client_access(evaluator_did)?;
    let create = build_signed_client_request(
        &access,
        RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            RequestIdentity {
                request_id: request_id.to_string(),
                node_did: locator.trial_node_did.clone(),
                requester_did: Some(evaluator_did.to_owned()),
                agent_did: agent_did.to_string(),
                session_id: locator.session_id.clone(),
                content: stage.prompt.clone(),
                execution_origin: ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(evaluator_did),
        ),
        RequestSigner::Identity(identity),
    )
    .await?;
    gents_session::request::lifecycle::submit_client_request(&access, &create)
        .await
        .context("submitting the evaluator's stage request")?;
    Ok(())
}

pub(super) async fn seed_and_await_fire(
    home: &EmbeddedHome,
    evaluator_did: &str,
    ready: &watch::Receiver<Reconciled>,
    runtime: &RunningRuntime,
    stage: &StageSpec,
    trusted_schema: Option<&str>,
) -> Result<Seeded> {
    let seed = stage.seed.as_ref().context("stage has no seed document")?;
    let deadline = stage.deadline_secs.map(Duration::from_secs);
    anyhow::ensure!(
        !seed.expired_client_request,
        "expired client requests are trial fixtures, not event seeds"
    );
    let reconciled = tokio::time::timeout(
        RUNTIME_READY_TIMEOUT,
        ready.clone().wait_for(Option::is_some),
    )
    .await
    .context("waiting for the trial runtime's event sources")?
    .context("the trial runtime stopped before its event sources were reconciled")?
    .clone();
    if let Some(Err(error)) = reconciled {
        anyhow::bail!("the trial runtime's event sources did not reconcile: {error}");
    }
    let access = home.client_access(evaluator_did)?;
    let since = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let doc_id = create_document(
        &access,
        "eval.trial.seed_document",
        &seed.collection,
        &bind_fixture_actors(&seed.document, home.did(), evaluator_did),
        trusted_schema,
    )
    .await?;
    tracing::debug!(collection = %seed.collection, doc_id = %doc_id, "eval seed document written");
    let started = Instant::now();
    let window = deadline.unwrap_or(SEED_FIRE_WINDOW);
    let mut quiet = Quiet::default();
    loop {
        let window_ended = started.elapsed() >= window;
        let read = observe_seed(
            &access,
            home,
            &since,
            &doc_id,
            stage.expect_fire,
            &mut quiet,
        )
        .await;
        let observable = read.is_ok() && !runtime.handle.is_finished();
        let seen = read.as_ref().ok();
        let observation = SeedObservation {
            expect_fire: stage.expect_fire,
            observable,
            fired: seen.is_some_and(|seen| seen.fired.is_some()),
            failures: seen
                .map(|seen| seen.fires.failures.clone())
                .unwrap_or_default(),
            quiet: seen.is_some_and(|seen| seen.quiet),
            window_ended,
        };
        let submission = match seed_step(&observation) {
            SeedStep::Wait => {
                tokio::time::sleep(POLL).await;
                continue;
            }
            SeedStep::Observe => StageSubmission::Submitted {
                request_id: seen
                    .and_then(|seen| seen.fired.as_ref())
                    .map(|(request_id, _)| request_id.clone())
                    .context("an observed seed fired no request")?,
                remaining: remaining_of(deadline, started),
            },
            SeedStep::Pass => StageSubmission::Skipped,
            SeedStep::Fail(kind) => StageSubmission::Failed {
                kind,
                detail: Some(seed_failure_detail(
                    &read,
                    observable,
                    stage.expect_fire,
                    &seed.collection,
                    window,
                )),
            },
        };
        return Ok(Seeded { doc_id, submission });
    }
}

/// What ended a seed stage that failed, for its verdicts to carry.
fn seed_failure_detail(
    read: &Result<SeedRead>,
    observable: bool,
    expect_fire: bool,
    collection: &str,
    window: Duration,
) -> String {
    match read {
        Err(error) => format!("the seed could not be observed: {error:#}"),
        Ok(_) if !observable => "the trial runtime exited while the seed was observed".into(),
        Ok(SeedRead {
            fired: Some((request_id, trigger)),
            ..
        }) => format!(
            "trigger {} fired request {request_id} for the {collection} seed",
            trigger.as_deref().unwrap_or("unknown")
        ),
        Ok(SeedRead {
            fires:
                FailedFires {
                    detail: Some(detail),
                    ..
                },
            ..
        }) => detail.clone(),
        Ok(_) if expect_fire => {
            format!("no trigger fired a request within {window:?} of seeding {collection}")
        }
        Ok(_) => format!("the trial home was not quiet within {window:?} of seeding {collection}"),
    }
}

struct SeedRead {
    fires: FailedFires,
    quiet: bool,
    fired: Option<(String, Option<String>)>,
}

/// Fires are read after failures and quiet, so a request written meanwhile
/// is seen and takes precedence.
async fn observe_seed(
    access: &ConfigAccess,
    home: &EmbeddedHome,
    since: &str,
    doc_id: &str,
    expect_fire: bool,
    quiet: &mut Quiet,
) -> Result<SeedRead> {
    let fires = failed_fires(&home.node, home.did(), since, Some(doc_id)).await?;
    let quiet = !expect_fire && quiet.observe(request_counts(access).await?, Instant::now());
    let fired = fired_request(access, home.did(), doc_id).await?;
    Ok(SeedRead {
        fires,
        quiet,
        fired,
    })
}

/// The first request the seed caused, and the trigger that fired it.
pub(super) async fn fired_request(
    access: &ConfigAccess,
    node_did: &str,
    doc_id: &str,
) -> Result<Option<(String, Option<String>)>> {
    let query = format!(
        r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }}, caused_by_source_doc_id: {{ _eq: "{}" }} }}, order: {{ created_at: ASC }}) {{ request_id caused_by_trigger_id }} }}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(doc_id)
    );
    let response = access.execute(&query).await?;
    Ok(response
        .get("data")
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .context("observing the evaluator-visible triggered requests")?
        .iter()
        .find_map(|row| {
            Some((
                row.get("request_id")?.as_str()?.to_owned(),
                row.get("caused_by_trigger_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            ))
        }))
}

#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::watch;

use super::install::{bind_fixture_actors, create_document};
use super::outcome::trigger_failure;
use super::settle::remaining_of;
use super::{Reconciled, POLL};
use crate::defra_node::Node as EmbeddedNode;
use crate::eval::runner::embedded::home::{EmbeddedHome, RUNTIME_READY_TIMEOUT};
use crate::eval::runner::executor::{FixtureDocument, StageSpec, TrialLocator};
use crate::eval::OutcomeKind;
use crate::request::lifecycle::{
    build_signed_client_request, ExecutionOrigin, RequestIdentity, RequestSigner, RequestSpec,
};
use crate::store::graphql::escape_graphql_string;
use crate::ConfigAccess;

/// How long a seed stage without its own deadline waits for a trigger to fire
/// a request. No request exists yet, so no turn is bounded by it; a subject
/// that never configured the trigger would otherwise hold the trial forever.
const SEED_FIRE_WINDOW: Duration = Duration::from_secs(600);

#[derive(Debug)]
pub(super) enum StageSubmission {
    Submitted {
        request_id: String,
        /// Absent, the stage waits for the request's own terminal state.
        remaining: Option<Duration>,
    },
    Failed(OutcomeKind),
}

pub(super) async fn submit_stage(
    node: &std::sync::Arc<EmbeddedNode>,
    evaluator_did: &str,
    locator: &TrialLocator,
    agent_did: &str,
    stage: &StageSpec,
    request_id: &str,
) -> Result<()> {
    let identity =
        crate::store::identity::RegisteredIdentity::from_registered_did(evaluator_did, None)?;
    let access = ConfigAccess::local_as(
        node.clone(),
        ::identity::Did::new(evaluator_did.to_owned())?,
    );
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
        RequestSigner::Identity(&identity),
    )
    .await?;
    crate::request::lifecycle::submit_client_request(&access, &create)
        .await
        .context("submitting the evaluator's stage request")?;
    Ok(())
}

pub(super) async fn seed_and_await_fire(
    home: &EmbeddedHome,
    evaluator_did: &str,
    agent_did: &str,
    ready: &watch::Receiver<Reconciled>,
    seed: &FixtureDocument,
    deadline: Option<Duration>,
    trial_id: &str,
    stage_id: &str,
) -> Result<StageSubmission> {
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
    let access = ConfigAccess::local_as(
        home.node.clone(),
        ::identity::Did::new(evaluator_did.to_owned())?,
    );
    let since = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let doc_id = create_document(
        &access,
        "eval.trial.seed_document",
        &seed.collection,
        &bind_fixture_actors(&seed.document, home.did(), evaluator_did),
    )
    .await?;
    tracing::debug!(collection = %seed.collection, doc_id = %doc_id, "eval seed document written");
    let started = Instant::now();
    loop {
        if let Some(request_id) = fired_request(&home.node, &doc_id, agent_did).await? {
            return Ok(StageSubmission::Submitted {
                request_id,
                remaining: remaining_of(deadline, started),
            });
        }
        if let Some(kind) = trigger_failure(
            &home.node,
            home.did(),
            &since,
            Some(&doc_id),
            trial_id,
            stage_id,
        )
        .await
        {
            return resolve_seed_failure(&home.node, &doc_id, agent_did, kind, deadline, started)
                .await;
        }
        let fire_window = deadline.unwrap_or(SEED_FIRE_WINDOW);
        anyhow::ensure!(
            started.elapsed() < fire_window,
            "no trigger fired a request within {fire_window:?} of seeding {}",
            seed.collection
        );
        tokio::time::sleep(POLL).await;
    }
}

async fn resolve_seed_failure(
    node: &std::sync::Arc<EmbeddedNode>,
    doc_id: &str,
    agent_did: &str,
    kind: OutcomeKind,
    deadline: Option<Duration>,
    started: Instant,
) -> Result<StageSubmission> {
    Ok(match fired_request(node, doc_id, agent_did).await? {
        Some(request_id) => StageSubmission::Submitted {
            request_id,
            remaining: remaining_of(deadline, started),
        },
        None => StageSubmission::Failed(kind),
    })
}

async fn fired_request(
    node: &std::sync::Arc<EmbeddedNode>,
    doc_id: &str,
    agent_did: &str,
) -> Result<Option<String>> {
    let access = ConfigAccess::local_as(node.clone(), identity::Did::new(agent_did)?);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ agent_did: {{ _eq: "{}" }}, caused_by_source_doc_id: {{ _eq: "{}" }} }}, order: {{ created_at: ASC }}) {{ request_id }} }}"#,
        escape_graphql_string(agent_did),
        escape_graphql_string(doc_id)
    );
    let response = access.execute(&query).await?;
    Ok(response
        .get("data")
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .find_map(|row| row.get("request_id")?.as_str())
        .map(ToOwned::to_owned))
}

#[cfg(test)]
mod tests;

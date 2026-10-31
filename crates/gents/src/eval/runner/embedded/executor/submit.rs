use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use tokio::sync::watch;

use super::install::{bind_fixture_owner, create_document};
use super::outcome::trigger_failure;
use super::settle::remaining_of;
use super::{Reconciled, POLL};
use crate::defra_node::EmbeddedNode;
use crate::eval::runner::embedded::home::{EmbeddedHome, RUNTIME_READY_TIMEOUT};
use crate::eval::runner::executor::{FixtureDocument, StageSpec, TrialLocator};
use crate::eval::OutcomeKind;
use crate::request::lifecycle::{
    build_signed_request, ExecutionOrigin, RequestIdentity, RequestSigner, RequestSpec,
};
use crate::store::graphql::graphql_with_transaction_retry;
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
    locator: &TrialLocator,
    agent_id: &str,
    stage: &StageSpec,
    request_id: &str,
) -> Result<()> {
    let create = build_signed_request(
        RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            RequestIdentity {
                request_id: request_id.to_string(),
                node_did: locator.trial_node_did.clone(),
                requester_did: None,
                agent_id: agent_id.to_string(),
                session_id: locator.session_id.clone(),
                content: stage.prompt.clone(),
                execution_origin: ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                &locator.trial_node_did,
            ),
        ),
        RequestSigner::RegisteredTarget,
    )
    .await?;
    let mutation = create
        .graphql_mutation()
        .map_err(|error| anyhow!("building the stage request mutation: {error}"))?;
    ConfigAccess::write_local(node, "eval.trial.submit_stage", &mutation).await?;
    Ok(())
}

pub(super) async fn seed_and_await_fire(
    home: &EmbeddedHome,
    ready: &watch::Receiver<Reconciled>,
    seed: &FixtureDocument,
    deadline: Option<Duration>,
    trial_id: &str,
    stage_id: &str,
) -> Result<StageSubmission> {
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
    let access = ConfigAccess::local(home.node.clone());
    let since = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let doc_id = create_document(
        &access,
        "eval.trial.seed_document",
        &seed.collection,
        &bind_fixture_owner(&seed.document, home.did()),
    )
    .await?;
    tracing::debug!(collection = %seed.collection, doc_id = %doc_id, "eval seed document written");
    let started = Instant::now();
    loop {
        if let Some(request_id) = fired_request(&home.node, &doc_id).await? {
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
            return resolve_seed_failure(&home.node, &doc_id, kind, deadline, started).await;
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
    node: &EmbeddedNode,
    doc_id: &str,
    kind: OutcomeKind,
    deadline: Option<Duration>,
    started: Instant,
) -> Result<StageSubmission> {
    Ok(match fired_request(node, doc_id).await? {
        Some(request_id) => StageSubmission::Submitted {
            request_id,
            remaining: remaining_of(deadline, started),
        },
        None => StageSubmission::Failed(kind),
    })
}

async fn fired_request(node: &EmbeddedNode, doc_id: &str) -> Result<Option<String>> {
    let query =
        r#"{ AgentRequest(order: { created_at: ASC }) { request_id caused_by_source_doc_id } }"#;
    let response = graphql_with_transaction_retry(node, query, "eval trial fired request").await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|row| row.get("caused_by_source_doc_id").and_then(Value::as_str) == Some(doc_id))
        .find_map(|row| row.get("request_id")?.as_str())
        .map(ToOwned::to_owned))
}

#[cfg(test)]
mod tests;

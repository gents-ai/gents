use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::Node as EmbeddedNode;
use gents_protocol::output::extent::ObservedShape;
use gents_protocol::output::{OutputSegment, OutputSource, OutputWriter};

use crate::request::lifecycle::execution_policy::{authorize_output_append, LeaseObservation};
use gents_protocol::output::rows::{OutputSegmentRow, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION};
use gents_store::config_client::session_children::output_segment_bound_variables;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use gents_store::graphql::created_doc_id;

use super::{
    source::{load_request_in_txn, load_segment_document_in_txn, load_source_shapes_in_txn},
    ProviderAppendRejection,
};

pub async fn append_provider_segment_with_signer(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: identity::Did,
    signer: Option<gents_store::identity::ManagedAgentSigner>,
    generation: &str,
    prepared: &OutputSegment,
) -> Result<String> {
    let access =
        ConfigAccess::local_with_optional_agent_signer(node.clone(), actor.clone(), signer)?;
    let result = access
        .transact_idempotent(
            IdempotentTransactionRetry::Standard,
            "streaming.append_provider_segment",
            |txn| Box::pin(async move { append_in_txn(txn, generation, prepared).await }),
        )
        .await?;
    gents_store::config_client::session_children::publish_agent_session_child_relationships_with_access(
        &access,
        &prepared.node_did,
        &actor,
        &prepared.session_id,
    )
    .await?;
    Ok(result)
}

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn append_provider_segment_at_with_access(
    access: &ConfigAccess,
    generation: &str,
    prepared: &OutputSegment,
    now: DateTime<Utc>,
) -> Result<String> {
    let actor = identity::Did::new(access.actor_did().context("missing fixture actor")?)?;
    let result = access
        .transact_idempotent(
            IdempotentTransactionRetry::Standard,
            "streaming.append_provider_segment",
            |txn| Box::pin(async move { append_in_txn_at(txn, generation, prepared, now).await }),
        )
        .await?;
    gents_store::config_client::session_children::publish_agent_session_child_relationships_with_access(
        access,
        &prepared.node_did,
        &actor,
        &prepared.session_id,
    )
    .await?;
    Ok(result)
}

pub(crate) async fn append_in_txn(
    txn: &ConfigApplyTxn<'_>,
    generation: &str,
    prepared: &OutputSegment,
) -> Result<String> {
    append_in_txn_with_time(txn, generation, prepared, None).await
}

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn append_in_txn_at(
    txn: &ConfigApplyTxn<'_>,
    generation: &str,
    prepared: &OutputSegment,
    now: DateTime<Utc>,
) -> Result<String> {
    append_in_txn_with_time(txn, generation, prepared, Some(now)).await
}

async fn append_in_txn_with_time(
    txn: &ConfigApplyTxn<'_>,
    generation: &str,
    prepared: &OutputSegment,
    fixture_now: Option<DateTime<Utc>>,
) -> Result<String> {
    anyhow::ensure!(
        matches!(&prepared.source, OutputSource::ProviderTurn { .. })
            && matches!(&prepared.writer, OutputWriter::RequestExecution { execution_generation }
                if execution_generation == generation)
            && prepared.ordinal.is_some()
            && prepared.close.is_none(),
        "raw append requires an open provider flush and its exact request writer"
    );
    let request = load_request_in_txn(txn, prepared).await?;
    let records = load_source_shapes_in_txn(txn, prepared).await?;
    let shape = prepared.shape();
    let mut replays = Vec::new();
    for candidate in records.iter().filter(|row| row.shape == shape) {
        let recorded = load_segment_document_in_txn(txn, prepared, &candidate.doc_id).await?;
        if recorded.segment == *prepared {
            replays.push(candidate.doc_id.clone());
        }
    }
    if !replays.is_empty() {
        anyhow::ensure!(replays.len() == 1, "ambiguous immutable segment replay");
        return Ok(replays.remove(0));
    }
    if records.iter().any(|row| row.shape.close.is_some()) {
        return Err(ProviderAppendRejection::ClosedSource.into());
    }
    let owner = request
        .execution_generation
        .as_deref()
        .context("request generation missing")?;
    let deadline = DateTime::parse_from_rfc3339(
        request
            .execution_lease_expires_at
            .as_deref()
            .context("request lease deadline missing")?,
    )?
    .timestamp_millis();
    if !authorize_output_append(
        LeaseObservation {
            request: request
                .lifecycle_state
                .context("request lifecycle missing")?,
            generation: owner,
            deadline_ms: deadline,
        },
        generation,
        fixture_now.unwrap_or_else(Utc::now).timestamp_millis(),
    ) {
        return Err(ProviderAppendRejection::LostLease.into());
    }

    let result = txn
        .execute_with_variables(
            CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
            &output_segment_bound_variables(txn, prepared).await?,
        )
        .await?;
    let doc_id = created_doc_id(&result, "AgentOutputSegment")?;
    let observations = records
        .iter()
        .map(|row| ObservedShape::from_shape(&row.doc_id, &row.shape))
        .chain(std::iter::once(ObservedShape::from_segment(
            &doc_id, prepared,
        )))
        .collect::<Vec<_>>();
    gents_protocol::output::extent::inspect_open_source_shape(
        &observations,
        &prepared.request_doc_id,
        &prepared.source,
        &prepared.writer,
    )?;
    Ok(doc_id)
}

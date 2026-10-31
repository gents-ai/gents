use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::{OutputSegment, OutputSource, OutputWriter};

use crate::request::lifecycle::execution_policy::{authorize_output_append, LeaseObservation};
use crate::session::canonical_rows::{
    output_segment_create_variables, OutputSegmentRow, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use crate::store::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use crate::store::graphql::created_doc_id;

use super::{
    source::{load_request_in_txn, load_source_in_txn},
    ProviderAppendRejection,
};

pub(crate) async fn append_provider_segment(
    node: &std::sync::Arc<EmbeddedNode>,
    generation: &str,
    prepared: &OutputSegment,
) -> Result<String> {
    ConfigAccess::transact_local_idempotent(
        node,
        None,
        IdempotentTransactionRetry::Standard,
        "streaming.append_provider_segment",
        |txn| Box::pin(async move { append_in_txn(txn, generation, prepared).await }),
    )
    .await
}

#[cfg(test)]
pub(crate) async fn append_provider_segment_at(
    node: &std::sync::Arc<EmbeddedNode>,
    generation: &str,
    prepared: &OutputSegment,
    now: DateTime<Utc>,
) -> Result<String> {
    ConfigAccess::transact_local_idempotent(
        node,
        None,
        IdempotentTransactionRetry::Standard,
        "streaming.append_provider_segment",
        |txn| Box::pin(async move { append_in_txn_at(txn, generation, prepared, now).await }),
    )
    .await
}

pub(crate) async fn append_in_txn(
    txn: &ConfigApplyTxn<'_>,
    generation: &str,
    prepared: &OutputSegment,
) -> Result<String> {
    append_in_txn_with_time(txn, generation, prepared, None).await
}

#[cfg(test)]
pub(crate) async fn append_in_txn_at(
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
    let mut records = load_source_in_txn(txn, prepared).await?;
    let replays = records
        .iter()
        .filter(|row| row.segment == *prepared)
        .collect::<Vec<_>>();
    if !replays.is_empty() {
        anyhow::ensure!(replays.len() == 1, "ambiguous immutable segment replay");
        return Ok(replays[0].doc_id.clone());
    }
    if records.iter().any(|row| row.segment.close.is_some()) {
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
            &output_segment_create_variables(prepared)?,
        )
        .await?;
    let doc_id = created_doc_id(&result, "AgentOutputSegment")?;
    records.push(OutputSegmentRow {
        doc_id: doc_id.clone(),
        segment: prepared.clone(),
    });
    let observations = records
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    gents_protocol::output::extent::inspect_open_source(
        &observations,
        &prepared.request_doc_id,
        &prepared.source,
        &prepared.writer,
    )?;
    Ok(doc_id)
}

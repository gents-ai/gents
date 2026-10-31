use anyhow::{Context, Result};
use chrono::DateTime;
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::{
    MessageBlock, OutputOutcome, OutputSegment, OutputSource, OutputWriter, PayloadPresentation,
    PayloadRef, PresentedPayload, ReconstructionError, SourceClose, StreamPayload,
};
use gents_protocol::row::AgentRequestRow;

use crate::session::canonical_rows::OutputSegmentRow;
use crate::store::config_client::ConfigApplyTxn;
use crate::store::graphql::escape_graphql_string;

pub(crate) fn retained_partial_blocks(
    records: &[ObservedSegment<'_>],
    request_doc_id: &str,
    source: &OutputSource,
    writer: &OutputWriter,
    closing: &OutputSegmentRow,
) -> Result<Vec<MessageBlock>> {
    let Some(SourceClose::Closed {
        outcome: OutputOutcome::Partial,
        segments,
        stream_bytes,
    }) = closing.segment.close.as_ref()
    else {
        anyhow::bail!("reusable recovery closure is not Partial")
    };
    anyhow::ensure!(
        &closing.segment.writer == writer,
        "reusable recovery closure has wrong writer"
    );
    let extent = gents_protocol::output::extent::inspect_open_source(
        records,
        request_doc_id,
        source,
        writer,
    )?;
    anyhow::ensure!(
        &extent.segments == segments && &extent.stream_bytes == stream_bytes,
        "reusable recovery closure truncates or disagrees with committed extent"
    );
    if source.is_auxiliary_audit() {
        return Ok(Vec::new());
    }
    let mut retained = Vec::new();
    for (stream, reconstructed) in extent.streams.iter().enumerate() {
        if matches!(reconstructed.declaration.payload, StreamPayload::Text)
            && reconstructed.declaration.part_index == 0
        {
            retained.push((
                reconstructed.declaration.block_index,
                MessageBlock::Text {
                    text: PresentedPayload {
                        output: PayloadRef {
                            close_doc_id: closing.doc_id.clone(),
                            stream: u32::try_from(stream)?,
                        },
                        presentation: PayloadPresentation::Full,
                    },
                },
            ));
        }
    }
    retained.sort_by_key(|(block, _)| *block);
    Ok(retained.into_iter().map(|(_, block)| block).collect())
}

fn invalid_source(detail: impl Into<String>) -> ReconstructionError {
    ReconstructionError::InvalidStructure {
        detail: detail.into(),
    }
}

pub(crate) fn validate_source_physical_identities(
    records: &[OutputSegmentRow],
    request_doc_id: &str,
    source: &OutputSource,
) -> Result<(), ReconstructionError> {
    let mut physical = std::collections::BTreeMap::new();
    for record in records.iter().filter(|record| {
        record.segment.request_doc_id == request_doc_id && record.segment.source == *source
    }) {
        if let Some(previous) = physical.insert(&record.doc_id, &record.segment) {
            if previous != &record.segment {
                return Err(invalid_source(
                    "one source physical identity has conflicting facts",
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_closed_source_extent(
    records: &[OutputSegmentRow],
    request_doc_id: &str,
    source: &OutputSource,
    closing: &OutputSegmentRow,
) -> Result<(), ReconstructionError> {
    let Some(SourceClose::Closed {
        segments,
        stream_bytes,
        ..
    }) = closing.segment.close.as_ref()
    else {
        return Err(invalid_source("source closure is not Closed"));
    };
    if closing.segment.request_doc_id != request_doc_id || closing.segment.source != *source {
        return Err(invalid_source("Closed record has invalid source identity"));
    }
    let scoped = records
        .iter()
        .filter(|record| {
            record.segment.request_doc_id == request_doc_id && record.segment.source == *source
        })
        .collect::<Vec<_>>();
    let closures = scoped
        .iter()
        .filter(|record| record.segment.close.is_some())
        .collect::<Vec<_>>();
    if closures.len() != 1
        || closures[0].doc_id != closing.doc_id
        || closures[0].segment != closing.segment
    {
        return Err(invalid_source(
            "Closed source has conflicting closure facts",
        ));
    }
    let prefix = scoped
        .into_iter()
        .filter(|record| {
            record.doc_id == closing.doc_id
                || record
                    .segment
                    .ordinal
                    .is_some_and(|ordinal| ordinal < *segments)
        })
        .map(|record| ObservedSegment {
            doc_id: &record.doc_id,
            segment: &record.segment,
        })
        .collect::<Vec<_>>();
    let extent = gents_protocol::output::extent::inspect_open_source(
        &prefix,
        request_doc_id,
        source,
        &closing.segment.writer,
    )?;
    if extent.segments != *segments || extent.stream_bytes != *stream_bytes {
        return Err(invalid_source(
            "Closed source disagrees with its committed extent",
        ));
    }
    DateTime::parse_from_rfc3339(&closing.segment.created_at)
        .map_err(|_| invalid_source("Closed source has malformed closure timestamp"))?;
    Ok(())
}

/// Publication's fence against recovery/cancellation: native OCC validates
/// point-read keys at commit (RequestExecutionLease/ObservedTransaction.lean).
/// Keep this an in-transaction `_docID` point read; do not add a same-value
/// request write.
pub(super) async fn load_request_in_txn(
    txn: &ConfigApplyTxn<'_>,
    prepared: &OutputSegment,
) -> Result<AgentRequestRow> {
    let id = escape_graphql_string(&prepared.request_doc_id);
    let response = txn
        .execute_local_response(&format!(
            r#"{{ AgentRequest(
        filter: {{ _docID: {{ _eq: "{id}" }} }}) {{
        _docID request_id purpose node_did requester_did session_id lifecycle_state
        execution_generation execution_lease_expires_at request_hop
        workspace_id workspace_owner_node_did workspace_authority workspace_seal_hash
    }} }}"#
        ))
        .await?;
    let mut rows: Vec<AgentRequestRow> = crate::store::graphql::rows(&response, "AgentRequest")?;
    anyhow::ensure!(
        rows.len() == 1,
        "provider output request is missing or ambiguous"
    );
    let row = rows.pop().expect("one request checked");
    anyhow::ensure!(
        row.doc_id.as_deref() == Some(prepared.request_doc_id.as_str())
            && row.node_did.as_deref() == Some(prepared.node_did.as_str())
            && row.requester_did == prepared.requester_did
            && row.session_id.as_deref() == Some(prepared.session_id.as_str()),
        "provider segment crossed its request owner or session"
    );
    validate_source_purpose(
        row.purpose.context("output request purpose missing")?,
        &prepared.source,
    )?;
    Ok(row)
}

pub(crate) fn validate_source_purpose(
    purpose: gents_protocol::request_admission::RequestPurpose,
    source: &OutputSource,
) -> Result<()> {
    let expected = match source {
        OutputSource::ProviderTurn { scope, .. } => scope.kind.request_purpose(),
        _ => gents_protocol::request_admission::RequestPurpose::Normal,
    };
    anyhow::ensure!(
        purpose == expected,
        "output source does not match its request purpose"
    );
    Ok(())
}

pub(super) async fn load_source_in_txn(
    txn: &ConfigApplyTxn<'_>,
    prepared: &OutputSegment,
) -> Result<Vec<OutputSegmentRow>> {
    let response = txn
        .execute_local_response(
            &crate::session::canonical_rows::source_output_segments_query(
                &prepared.request_doc_id,
                &prepared.source,
            )?,
        )
        .await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentOutputSegment"))
        .and_then(serde_json::Value::as_array)
        .context("source query omitted rows")?;
    crate::session::canonical_rows::decode_scoped_request_output_segments(
        rows,
        &prepared.node_did,
        Some(&prepared.session_id),
        prepared.requester_did.as_deref(),
    )
    .map(|rows| {
        rows.into_iter()
            .filter(|row| row.segment.source == prepared.source)
            .collect()
    })
}

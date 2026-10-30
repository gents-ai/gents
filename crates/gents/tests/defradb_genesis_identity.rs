//! Native premise experiment for the pinned DefraDB genesis-derived identity.
//! This exercises the database directly, not the canonical writer's replay
//! policy: a duplicate create is an error.

use anyhow::{ensure, Context, Result};
use gents::defra_node::{EmbeddedNode, QueryRequest, QueryResponse};
use gents::session::canonical_rows::{
    decode_output_segment_row, output_segment_create_variables, AGENT_OUTPUT_SEGMENT_FIELDS,
    CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::output::{
    OutputOutcome, OutputSegment, OutputSource, OutputWriter, PayloadRef, ReconstructionError,
    SegmentRun, SourceClose, StreamDeclaration, StreamPayload,
};
use std::sync::Arc;

fn segment(payload: &str) -> OutputSegment {
    OutputSegment {
        node_did: "did:test:genesis".into(),
        requester_did: None,
        session_id: "genesis-session".into(),
        request_doc_id: "genesis-request".into(),
        source: OutputSource::ProviderTurn {
            scope: "inference.0".parse().unwrap(),
            turn_index: 0,
            attempt: 0,
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: "genesis-generation".into(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: payload.len() as u32,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::Text,
            }),
        }],
        payload: payload.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![payload.len() as u64],
        }),
        created_at: "2026-09-22T00:00:00Z".into(),
    }
}

async fn create(node: &EmbeddedNode, value: &OutputSegment) -> QueryResponse {
    node.execute_request(
        QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
            .with_variables(output_segment_create_variables(value).unwrap()),
    )
    .await
}

fn created_id(response: &QueryResponse) -> Result<Option<String>> {
    if response.has_errors() {
        return Ok(None);
    }
    let row = gents::graphql::single_mutation_document(response, "create_AgentOutputSegment")?
        .context("successful create omitted its row")?;
    Ok(Some(
        row.get("_docID")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
            .context("successful create omitted _docID")?
            .to_owned(),
    ))
}

async fn rows(node: &EmbeddedNode) -> Result<Vec<(String, OutputSegment)>> {
    let response = node
        .execute(&format!(
            "query {{ AgentOutputSegment {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"
        ))
        .await;
    ensure!(!response.has_errors(), "row query: {:?}", response.errors);
    let data = response.data.context("row query omitted data")?;
    data["AgentOutputSegment"]
        .as_array()
        .context("row query omitted AgentOutputSegment array")?
        .iter()
        .map(|value| {
            let row = decode_output_segment_row(value)?;
            Ok((row.doc_id, row.segment))
        })
        .collect()
}

#[tokio::test]
async fn canonical_segment_genesis_is_immutable_under_concurrent_and_repeated_create() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let identity = gents::KeyIdentity::load_or_create(directory.path().join("node.key"), None)?;
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(gents::NodeIdentity::did(&identity))
            .build()
            .await?,
    );
    gents::store::schema::ensure_runtime_schemas(&node).await?;
    let original = segment("original");

    let (first, second) = tokio::join!(create(&node, &original), create(&node, &original));
    let first_id = created_id(&first)?;
    let second_id = created_id(&second)?;
    tracing::info!(?first_id, ?second_id, first_errors = ?first.errors, second_errors = ?second.errors,
        "concurrent identical canonical genesis creates");
    let persisted = rows(&node).await?;
    ensure!(
        persisted.len() == 1,
        "identical genesis produced {persisted:?}"
    );
    ensure!(
        persisted[0].1 == original,
        "original fact changed: {persisted:?}"
    );
    ensure!(
        first_id.as_ref() == Some(&persisted[0].0)
            || second_id.as_ref() == Some(&persisted[0].0),
        "neither create acknowledged the persisted ID: first={first_id:?}, second={second_id:?}, persisted={persisted:?}"
    );
    ensure!(
        first_id.is_none() || second_id.is_none() || first_id == second_id,
        "identical genesis yielded different acknowledged IDs: {first_id:?}, {second_id:?}"
    );

    let replay = create(&node, &original).await;
    ensure!(
        replay.has_errors(),
        "duplicate create unexpectedly succeeded: {replay:?}"
    );
    ensure!(
        rows(&node).await? == persisted,
        "duplicate changed the original row"
    );

    let changed = segment("changed");
    let changed_response = create(&node, &changed).await;
    ensure!(
        !changed_response.has_errors(),
        "changed genesis create failed: {:?}",
        changed_response.errors
    );
    let changed_id = created_id(&changed_response)?.context("changed create had no ID")?;
    ensure!(
        changed_id != persisted[0].0,
        "changed genesis reused the original ID"
    );
    let after_change = rows(&node).await?;
    ensure!(
        after_change.len() == 2,
        "changed genesis did not add a row: {after_change:?}"
    );
    ensure!(
        after_change.contains(&persisted[0]) && after_change.contains(&(changed_id, changed)),
        "changed genesis overwrote or altered a physical row: {after_change:?}"
    );
    let mut observed = after_change;
    for _ in 0..2 {
        let records = observed
            .iter()
            .map(
                |(doc_id, segment)| gents_protocol::output::reconstruction::ObservedSegment {
                    doc_id,
                    segment,
                },
            )
            .collect::<Vec<_>>();
        for (close_doc_id, _) in &observed {
            let result = gents_protocol::output::reconstruction::reconstruct_stream(
                &records,
                &[],
                &[],
                &PayloadRef {
                    close_doc_id: close_doc_id.clone(),
                    stream: 0,
                },
            );
            ensure!(
                matches!(result, Err(ReconstructionError::ConflictingClosures { .. })),
                "physical closure twins must not select a winner: {result:?}"
            );
        }
        observed.reverse();
    }
    Ok(())
}

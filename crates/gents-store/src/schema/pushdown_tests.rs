//! Native premise experiment for the pinned DefraDB genesis-derived identity.
//! This exercises the database directly, not the canonical writer's replay
//! policy: a duplicate create is an error.

use anyhow::{ensure, Context, Result};
use defra_node::{Node as EmbeddedNode, QueryRequest, QueryResponse};
use gents_protocol::output::rows::{
    decode_output_segment_row, output_segment_create_variables, AGENT_OUTPUT_SEGMENT_FIELDS,
    CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::output::{
    OutputOutcome, OutputSegment, OutputSource, OutputWriter, PayloadRef, ReconstructionError,
    SegmentRun, SourceClose, StreamDeclaration, StreamPayload,
};
use rapidhash::RapidHashSet;
use std::sync::Arc;

use super::ensure_runtime_schemas;

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
        stream_bytes_after: Some(vec![payload.len() as u64]),
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
    let row = crate::graphql::single_mutation_document(response, "create_AgentOutputSegment")?
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
    let identity =
        crate::identity::KeyIdentity::load_or_create(directory.path().join("node.key"), None)?;
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(crate::identity::NodeIdentity::did(&identity))
            .build()
            .await?,
    );
    ensure_runtime_schemas(&node).await?;
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

#[tokio::test]
async fn agent_tool_call_carries_only_generic_background_fields() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("embedded node"),
    );
    ensure_runtime_schemas(&node)
        .await
        .expect("runtime schemas");
    let response = node
        .execute(
            r#"{
                __type(name: "AgentToolCall") {
                    fields { name }
                }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "introspection errors: {:?}",
        response.errors
    );
    let names: RapidHashSet<String> = response
        .data
        .as_ref()
        .and_then(|d| d.get("__type"))
        .and_then(|t| t.get("fields"))
        .and_then(|fs| fs.as_array())
        .map(|fs| {
            fs.iter()
                .filter_map(|f| f.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    for field in [
        "await_mode",
        "cancel_cause",
        "stuck_since",
        "spawned_by_tool_call_doc_id",
        "completion_notification_delivered_at",
    ] {
        assert!(names.contains(field), "AgentToolCall missing field {field}");
    }
    for field in [
        "child_request_id",
        "spawn_target_did",
        "spawn_agent_did",
        "delegated_input",
        "delegated_workspace",
        "cancel_policy",
        "cancel_cascade_intent_at",
        "cancel_pending_remote_ack",
        "unclaimed_deadline_at",
    ] {
        assert!(
            !names.contains(field),
            "AgentToolCall retains field {field}"
        );
    }
}

/// Runs a builder-rendered document on `node`, failing on the store's errors.
async fn run_rendered(
    node: &EmbeddedNode,
    rendered: gents_protocol::document_query::Rendered,
) -> Result<QueryResponse> {
    let response = node
        .execute_request(QueryRequest::new(rendered.document).with_variables(rendered.variables))
        .await;
    ensure!(!response.has_errors(), "{:?}", response.errors);
    Ok(response)
}

/// The `_docID`s a builder-rendered read of `ids` returns from `node`, or the
/// store's errors. The read must reach the store as the `docID` argument.
async fn read_doc_ids(node: &EmbeddedNode, ids: &[&str]) -> Result<Vec<String>, String> {
    read_doc_ids_as(node, None, ids).await
}

/// [`read_doc_ids`] as the identity `context` names, or as no one.
async fn read_doc_ids_as(
    node: &EmbeddedNode,
    context: Option<&defra_core::RequestContext>,
    ids: &[&str],
) -> Result<Vec<String>, String> {
    use gents_protocol::document_query::{Filter, Query, Selection};
    use gents_schemas::fields::agent_request as request;
    let rendered = Query::select(Selection::doc_id())
        .filter(Filter::any_of(request::DOC_ID, ids.iter().copied()))
        .render();
    assert!(
        rendered.document.contains("(docID: $v0)"),
        "the set is read as the docID argument alone: {}",
        rendered.document
    );
    let request = QueryRequest::new(rendered.document).with_variables(rendered.variables);
    let response = match context {
        Some(context) => node
            .execute_request_as(context, request)
            .await
            .map_err(|error| error.to_string())?,
        None => node.execute_request(request).await,
    };
    rows_or_errors(response)
}

/// The `_docID`s of a read's `AgentRequest` rows, or its errors.
fn rows_or_errors(response: QueryResponse) -> Result<Vec<String>, String> {
    if response.has_errors() {
        return Err(format!("{:?}", response.errors));
    }
    let data = response.data.unwrap_or_default();
    Ok(data["AgentRequest"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row["_docID"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default())
}

/// Premise for reading a set of document ids as the `docID` list argument,
/// which `gents_protocol::document_query` renders for a top-level `_docID`
/// set: a deleted id, a well-formed id that never existed, a malformed id and
/// a document the reader may not read are absent, as they were under
/// `_docID: {_in: [...]}`, and the read does not fail.
#[tokio::test]
async fn a_doc_id_list_reads_only_the_live_documents_it_names() -> Result<()> {
    use gents_protocol::document_query::{Create, Delete, Input};
    use gents_schemas::fields::agent_request as request;
    let node = Arc::new(crate::test_support::node_builder().build().await?);
    ensure_runtime_schemas(&node).await?;
    let mut ids = Vec::new();
    for request_id in ["live", "deleted"] {
        let created = run_rendered(
            &node,
            Create::new(
                Input::new()
                    .set(request::NODE_DID, "did:test:node")
                    .set(request::REQUEST_ID, request_id),
            )
            .render(),
        )
        .await?;
        let row = crate::graphql::single_mutation_document(&created, "create_AgentRequest")?
            .context("create omitted its row")?;
        ids.push(
            row["_docID"]
                .as_str()
                .context("create omitted _docID")?
                .to_owned(),
        );
    }
    let (live, deleted) = (ids[0].as_str(), ids[1].as_str());
    run_rendered(&node, Delete::<request::Doc>::doc(deleted).render()).await?;
    let never = "bae-c94acbfa-dd53-40d0-97f3-29ce16c333fc";

    assert_eq!(
        read_doc_ids(&node, &[live, deleted, never]).await,
        Ok(vec![live.to_owned()])
    );

    /* a malformed id, which a caller could pass on, is not an error either:
    it names no document, under the argument as under the old filter */
    assert_eq!(
        read_doc_ids(&node, &[live, "not-a-doc-id"]).await,
        Ok(vec![live.to_owned()])
    );
    let old_filter = node
        .execute_request(
            QueryRequest::new(
                "query($ids: [String!]) { AgentRequest(filter: {_docID: {_in: $ids}}) { _docID } }",
            )
            .with_variables(serde_json::json!({ "ids": [live, "not-a-doc-id"] })),
        )
        .await;
    ensure!(
        !old_filter.has_errors(),
        "old filter: {:?}",
        old_filter.errors
    );
    assert_eq!(
        old_filter.data.context("old filter omitted data")?["AgentRequest"],
        serde_json::json!([{ "_docID": live }])
    );

    /* a document the reader may not read is left out, not an error, under
    the argument as under the old filter: one such id in the list never
    fails the read of the others. Each actor here reads only what it
    created. */
    use crate::identity::NodeIdentity as _;
    let as_actor = |name: &str| {
        defra_core::RequestContext::default().with_identity(Some(
            crate::test_support::test_identity(name).did().to_owned(),
        ))
    };
    let (owner, reader) = (as_actor("owner"), as_actor("reader"));
    let mut created = Vec::new();
    for (actor, request_id) in [(&owner, "owned"), (&reader, "readable")] {
        let rendered = Create::new(
            Input::new()
                .set(request::NODE_DID, "did:test:node")
                .set(request::REQUEST_ID, request_id),
        )
        .render();
        let response = node
            .execute_request_as(
                actor,
                QueryRequest::new(rendered.document).with_variables(rendered.variables),
            )
            .await?;
        ensure!(!response.has_errors(), "{:?}", response.errors);
        created.push(
            crate::graphql::single_mutation_document(&response, "create_AgentRequest")?
                .context("create omitted its row")?["_docID"]
                .as_str()
                .context("create omitted _docID")?
                .to_owned(),
        );
    }
    let (owned, readable) = (created[0].as_str(), created[1].as_str());
    assert_eq!(
        read_doc_ids_as(&node, Some(&reader), &[readable, owned, live]).await,
        Ok(vec![readable.to_owned()])
    );
    assert_eq!(
        read_doc_ids_as(&node, Some(&owner), &[readable, owned]).await,
        Ok(vec![owned.to_owned()]),
        "the owner reads its own"
    );
    assert_eq!(
        rows_or_errors(
            node.execute_request_as(
                &reader,
                QueryRequest::new(
                    "query($ids: [String!]) { AgentRequest(filter: {_docID: {_in: $ids}}) { _docID } }",
                )
                .with_variables(serde_json::json!({ "ids": [readable, owned, live] })),
            )
            .await?
        ),
        Ok(vec![readable.to_owned()]),
        "the old filter"
    );
    Ok(())
}

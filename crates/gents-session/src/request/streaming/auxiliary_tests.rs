use std::sync::Arc;
use std::time::Duration;

use defra_node::Node as EmbeddedNode;
use gents_loop::provider_audit::{AuxiliaryOutputEvent, AuxiliaryOutputObservation};
use gents_loop::provider_input::ProviderInputProfile;
use gents_protocol::output::live::reconstruct_dense_prefix;
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::{OutputSource, OutputWriter, SourceClose, StreamPayload};
use gents_protocol::rendered_request::CaptureScope;

use super::{auxiliary, DefraStreamWriter};
use crate::request::lifecycle::RequestLifecycle;
use gents_protocol::output::rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
use gents_store::config_client::ConfigAccess;

struct TitleFixture {
    _home: gents_documents::test_support::SignedHome,
    node: Arc<EmbeddedNode>,
    node_did: String,
    agent: Arc<dyn gents_store::identity::NodeIdentity>,
    agent_access: ConfigAccess,
    lifecycle: RequestLifecycle,
    writer: Arc<DefraStreamWriter>,
}

/// A claimed title request whose owned execution has begun.
async fn claimed_title_fixture() -> TitleFixture {
    use gents_protocol::request_admission::RequestPurpose;

    let home = gents_documents::test_support::SignedHome::start(
        "claimed-title-fixture-node",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let binding = gents_inference::test_support::default_agent(&home).await;
    let parent = crate::test_support::ClaimedRequest::self_signed(&home, &binding)
        .content("Name this conversation")
        .drive(crate::test_support::Stage::Submitted)
        .await;
    let title = crate::request::lifecycle::materialize::write_pending_auxiliary_request(
        &home.store.node,
        parent.lifecycle.request(),
        RequestPurpose::TitleAudit,
        parent.lifecycle.request().content.clone(),
        None,
        &parent.agent.managed_agent_signer().unwrap(),
    )
    .await
    .unwrap();
    let mut claimed = crate::test_support::ClaimedRequest::existing(&home, &binding, &title.doc_id)
        .drive(crate::test_support::Stage::Executing)
        .await;
    let node = home.store.node.clone();
    let node_did = home.node_did().to_owned();
    let writer = Arc::new(claimed.writer.take().unwrap());
    TitleFixture {
        _home: home,
        node,
        node_did,
        agent: claimed.agent,
        agent_access: claimed.agent_access,
        lifecycle: claimed.lifecycle,
        writer,
    }
}

#[tokio::test]
async fn cancelled_title_flush_replays_committed_bytes_before_shutdown_close() {
    use gents_protocol::output::OutputOutcome;
    use gents_store::config_client::ConfigApplyTxn;

    let TitleFixture {
        _home,
        node,
        node_did,
        agent,
        agent_access,
        lifecycle,
        writer,
    } = claimed_title_fixture().await;
    let request = lifecycle.request();
    let scope: CaptureScope = "title.1".parse().unwrap();
    let generation = lifecycle.execution_generation().unwrap();
    let source = OutputSource::ProviderTurn {
        scope,
        turn_index: 0,
        attempt: 0,
    };
    let output_writer = OutputWriter::RequestExecution {
        execution_generation: generation.into(),
    };
    let sink = auxiliary::sink_with_writer(
        writer,
        request.clone(),
        generation.into(),
        ProviderInputProfile::OpenAiChatCompletions,
    );
    let observation = |event| AuxiliaryOutputObservation {
        capture_scope: scope,
        turn: 0,
        attempt: 0,
        event,
    };
    (sink.observe)(observation(AuxiliaryOutputEvent::AttemptStarted))
        .await
        .unwrap();
    let committed = Arc::new(tokio::sync::Notify::new());
    let mut flush = Box::pin(ConfigApplyTxn::with_embedded_commit_ack_pause(
        committed.clone(),
        async {
            (sink.observe)(observation(AuxiliaryOutputEvent::ReasoningDelta {
                id: None,
                fragment: "received before shutdown".into(),
            }))
            .await?;
            (sink.flush_pending)(scope, 0, 0).await
        },
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            () = committed.notified() => {},
            result = &mut flush => panic!("flush skipped the commit boundary: {result:?}"),
        }
    })
    .await
    .expect("title flush reaches its real commit before acknowledgement");
    drop(flush);

    let query = format!(
        "{{AgentOutputSegment(filter: {{request_doc_id: {{_eq: \"{}\"}}}}) {{{AGENT_OUTPUT_SEGMENT_FIELDS}}} AgentMessage {{_docID}}}}",
        gents_store::graphql::escape_graphql_string(&request.doc_id),
    );
    let before = agent_access.execute(&query).await.unwrap();
    let originals = before["data"]["AgentOutputSegment"].as_array().unwrap();
    assert_eq!(originals.len(), 1);
    assert!(originals[0]["close"].is_null());
    (sink.observe)(observation(AuxiliaryOutputEvent::ClosePartial))
        .await
        .expect("shutdown must replay the committed flush before closing the source");
    let after = agent_access.execute(&query).await.unwrap();
    let raw = after["data"]["AgentOutputSegment"].as_array().unwrap();
    assert_eq!(raw.len(), 2, "one data flush and one closure");
    assert!(
        raw.contains(&originals[0]),
        "retain the exact physical flush and timestamp"
    );
    assert!(after["data"]["AgentMessage"].as_array().unwrap().is_empty());
    let rows = raw
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let expected_signer =
        gents_store::identity::commit_signer_identity_for_did(agent.did()).unwrap();
    let node_signer = gents_store::identity::commit_signer_identity_for_did(&node_did).unwrap();
    for row in &rows {
        let response = agent_access.execute(&format!(
            r#"{{ _commits(docID: "{}") {{ cid height fieldName signature {{ identity type }} }} }}"#,
            gents_store::graphql::escape_graphql_string(&row.doc_id),
        )).await.unwrap();
        let commits = gents_store::graphql::composite_commits_at(
            response["data"].get("_commits"),
            "_commits",
        )
        .unwrap();
        assert!(commits.iter().any(|commit| commit
            .signature
            .as_ref()
            .is_some_and(|signature| signature.identity == expected_signer)));
        assert!(!commits.iter().any(|commit| commit
            .signature
            .as_ref()
            .is_some_and(|signature| signature.identity == node_signer)));
    }
    let close = rows.iter().find(|row| row.segment.close.is_some()).unwrap();
    assert!(
        matches!(close.segment.close.as_ref(), Some(SourceClose::Closed {
        outcome: OutputOutcome::Partial, segments: 1, stream_bytes,
    }) if stream_bytes == &[24])
    );
    let observed = rows
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let retained =
        reconstruct_dense_prefix(&observed, &request.doc_id, &source, &output_writer, None)
            .unwrap();
    assert_eq!(retained.streams.len(), 1);
    assert_eq!(
        retained.streams[0].declaration.payload,
        StreamPayload::Reasoning
    );
    assert_eq!(retained.streams[0].text, "received before shutdown");
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn shutdown_close_replays_a_dropped_complete_title_closure() {
    use gents_loop::stream_processor::AssistantTurnAccumulator;
    use gents_protocol::output::OutputOutcome;
    use gents_store::config_client::ConfigApplyTxn;

    let TitleFixture {
        _home,
        node,
        agent_access,
        lifecycle,
        writer,
        ..
    } = claimed_title_fixture().await;
    let request = lifecycle.request();
    let scope: CaptureScope = "title.1".parse().unwrap();
    let generation = lifecycle.execution_generation().unwrap();
    let sink = auxiliary::sink_with_writer(
        writer,
        request.clone(),
        generation.into(),
        ProviderInputProfile::OpenAiChatCompletions,
    );
    let observation = |event| AuxiliaryOutputObservation {
        capture_scope: scope,
        turn: 0,
        attempt: 0,
        event,
    };
    (sink.observe)(observation(AuxiliaryOutputEvent::AttemptStarted))
        .await
        .unwrap();
    (sink.observe)(observation(AuxiliaryOutputEvent::TextDelta(
        "Sealed workspace".into(),
    )))
    .await
    .unwrap();
    (sink.flush_pending)(scope, 0, 0).await.unwrap();
    let mut turn = AssistantTurnAccumulator::default();
    turn.push_text("Sealed workspace");
    let message = turn.message_snapshot().expect("received title text");

    // The provider's completed turn closes the source, and shutdown drops that
    // observation after its closure commits but before it is acknowledged.
    let committed = Arc::new(tokio::sync::Notify::new());
    let mut close = Box::pin(ConfigApplyTxn::with_embedded_commit_ack_pause(
        committed.clone(),
        (sink.observe)(observation(AuxiliaryOutputEvent::TurnReady { message })),
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            () = committed.notified() => {},
            result = &mut close => panic!("closure skipped the commit boundary: {result:?}"),
        }
    })
    .await
    .expect("title closure reaches its real commit before acknowledgement");
    drop(close);

    (sink.observe)(observation(AuxiliaryOutputEvent::ClosePartial))
        .await
        .expect("shutdown replays the started closure instead of changing its outcome");
    (sink.observe)(observation(AuxiliaryOutputEvent::ClosePartial))
        .await
        .expect("a closed source accepts a repeated shutdown close");
    let query = format!(
        "{{AgentOutputSegment(filter: {{request_doc_id: {{_eq: \"{}\"}}}}) {{{AGENT_OUTPUT_SEGMENT_FIELDS}}}}}",
        gents_store::graphql::escape_graphql_string(&request.doc_id),
    );
    let response = agent_access.execute(&query).await.unwrap();
    let rows = response["data"]["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let closures = rows
        .iter()
        .filter_map(|row| row.segment.close.as_ref())
        .collect::<Vec<_>>();
    assert!(
        matches!(
            closures.as_slice(),
            [SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                ..
            }]
        ),
        "the source keeps its single complete closure: {closures:?}"
    );
    node.shutdown().await.expect("shut down test node");
}

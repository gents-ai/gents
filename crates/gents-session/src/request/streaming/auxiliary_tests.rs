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
use crate::request::lifecycle::{ClaimOutcome, RequestLifecycle};
use gents_protocol::output::rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
use gents_store::config_client::ConfigAccess;

struct TitleFixture {
    _db: gents_inference::test_support::TestDb,
    node: Arc<EmbeddedNode>,
    identity: gents_store::identity::KeyIdentity,
    agent: Arc<dyn gents_store::identity::NodeIdentity>,
    agent_access: ConfigAccess,
    lifecycle: RequestLifecycle,
    writer: Arc<DefraStreamWriter>,
}

/// A claimed title request whose owned execution has begun.
async fn claimed_title_fixture() -> TitleFixture {
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };
    use gents_store::identity::NodeIdentity;

    let db = gents_inference::test_support::test_db(
        "claimed-title-fixture-node",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = db.node.clone();
    let identity = gents_store::test_support::held_runtime_identity(db.node_identity.did());
    let root_did = gents_store::graphql::escape_graphql_string(identity.did());
    ConfigAccess::local(node.clone())
        .write("test.title.node", &format!(r#"mutation {{ create_Node(input: {{node_did: "{root_did}", display_name: "fixture", enabled: true}}) {{ _docID }} }}"#))
        .await.unwrap();
    let binding = gents_store::identity::provision_agent_identity(&identity).unwrap();
    gents_inference::test_support::install_test_agent_binding(
        &node,
        identity.did(),
        "title fixture",
        &binding,
    )
    .await;
    let agent = gents_store::identity::resolve_agent_identity(&identity, &binding).unwrap();
    let signer = agent.managed_agent_signer().unwrap();
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        uuid::Uuid::new_v4().to_string(),
        identity.did(),
        identity.did(),
        agent.did(),
        uuid::Uuid::new_v4().to_string(),
        "Name this conversation",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    let session_id = gents_store::graphql::escape_graphql_string(&create.session_id);
    let agent_did = gents_store::graphql::escape_graphql_string(agent.did());
    let agent_access = ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    agent_access
        .write("test.title.session", &format!(r#"mutation {{ create_AgentSession(input: {{session_id: "{session_id}", node_did: "{root_did}", requester_did: "{root_did}", agent_did: "{agent_did}", created_at: "2026-01-01T00:00:00Z"}}) {{ _docID }} }}"#))
        .await.unwrap();
    crate::request::admission::sign_agent_request_create(&identity, &mut create)
        .await
        .unwrap();
    let access = ConfigAccess::local(node.clone());
    let created = access
        .write(
            "test.title_flush_parent",
            &create.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    let parent_doc = gents_store::graphql::created_doc_id(&created, "AgentRequest").unwrap();
    ConfigAccess::local_as(node.clone(), identity.did().parse().unwrap())
        .add_actor_relationship(
            "AgentRequest",
            &parent_doc,
            "working_agent",
            &agent.did().parse().unwrap(),
        )
        .await
        .unwrap();
    let parent = crate::request::admission::load_request_for_admission_test(&node, &parent_doc)
        .await
        .unwrap();
    let title = crate::request::lifecycle::materialize::write_pending_auxiliary_request(
        &node,
        &parent,
        RequestPurpose::TitleAudit,
        parent.content.clone(),
        None,
        &signer,
    )
    .await
    .unwrap();
    let mut lifecycle = RequestLifecycle::new_with_node_did(node.clone(), title, 60);
    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let writer = Arc::new(
        DefraStreamWriter::new_managed(node.clone(), agent.as_ref(), Duration::from_millis(1))
            .unwrap(),
    );
    lifecycle
        .begin_owned_execution(writer.as_ref())
        .await
        .unwrap();
    TitleFixture {
        _db: db,
        node,
        identity,
        agent,
        agent_access,
        lifecycle,
        writer,
    }
}

#[tokio::test]
async fn cancelled_title_flush_replays_committed_bytes_before_shutdown_close() {
    use gents_protocol::output::OutputOutcome;
    use gents_store::config_client::ConfigApplyTxn;
    use gents_store::identity::NodeIdentity;

    let TitleFixture {
        _db,
        node,
        identity,
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
    let node_signer =
        gents_store::identity::commit_signer_identity_for_did(identity.did()).unwrap();
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
        _db,
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

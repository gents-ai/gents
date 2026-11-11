use super::provider_context_reduction::capture_source_boundary;
use std::sync::Arc;
use defra_node::Node as EmbeddedNode;
use gents_protocol::message::Message;
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, ReasoningPart, SegmentRun,
    SourceClose, StreamDeclaration, StreamPayload, TranscriptMessage,
};
use gents_protocol::rendered_request::RenderedRequestSource;
use gents_store::identity::NodeIdentity;
use super::output::replay_candidates::load_canonical_assistant_candidates;
use super::output::replay_request::ReplayRequestReader;
use super::output::CanonicalReplayScope;
use gents_protocol::output::rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::transcript::SourceBoundary;
use gents_loop::claude_messages_body::{
    replay_stage, select_replay, ReplayIssuer, ReplayTag, ReplayWire, TaggedAssistantRow,
};
use gents_loop::provider_input::replay_frontier::flatten;

#[tokio::test]
async fn non_required_signed_history_with_wrong_provider_scope_stays_permissive() {
    use gents_loop::loop_stream::provider_view_tagged;
    use gents_loop::provider_input::ProviderInputProfile;
    use gents_protocol::message::AssistantContent;
    use gents_protocol::rendered_request::CaptureScopeKind;

    let fixture =
        fixture_with_provider_payload_and_scope_and_tool(true, CaptureScopeKind::OneShot, false)
            .await;
    assert_eq!(fixture.candidates().await.unwrap().len(), 1);
    let inference_scope = CanonicalReplayScope {
        expected_scope_kind: CaptureScopeKind::Inference,
        ..fixture.scope()
    };
    assert!(load_canonical_assistant_candidates(
        &fixture.access,
        inference_scope,
        &fixture.boundary,
    )
    .await
    .unwrap()
    .is_empty());

    let history = gents_session::session::output::sequenced::load_sequenced_messages_using(
        gents_session::session::output::access::ReadAccess::Config(&fixture.access),
        SESSION_ID,
        &fixture.node_did,
        Some(&fixture.requester_did),
        None,
        None,
        Some(&fixture.request_doc_id),
        Some(ProviderInputProfile::ClaudeMessages),
    )
    .await
    .unwrap();
    assert_eq!(history.len(), 1);
    assert!(history[0].provider_source.is_none());
    assert!(matches!(&history[0].message,
        Message::Assistant { content, .. }
        if content.iter().filter(|block| matches!(block, AssistantContent::Reasoning(_))).count() == 2
    ));

    let tagged = crate::turn::provider_input::replay::tag_canonical_history(&history);
    let projected = provider_view_tagged(tagged).unwrap();
    let rows = projected
        .iter()
        .map(|row| {
            let Message::Assistant { id, content, .. } = &row.message else {
                panic!("fixture history is one assistant")
            };
            TaggedAssistantRow {
                source: row.source.clone(),
                physical_header: row.physical_header.clone(),
                block_indices: row.block_indices.clone(),
                id: id.clone(),
                content: content.clone(),
            }
        })
        .collect::<Vec<_>>();
    let issuer = gents_inference::llm::backend_client::claude_subscription_replay_issuer()
        .unwrap()
        .unwrap();
    let selected = select_replay(
        &rows,
        &issuer,
        ReplayWire::ClaudeMessages,
        |_| Vec::new(),
        &[],
    );
    let projected = [gents_loop::loop_stream::TaggedMessage {
        message: Message::Assistant {
            request_id: None,
            id: selected[0].id.clone(),
            content: selected[0].content.clone(),
        },
        source: selected[0].source.clone(),
        physical_header: selected[0].physical_header.clone(),
        block_indices: selected[0].block_indices.clone(),
    }];
    assert_eq!(projected.len(), 1);
    assert!(projected[0].source.is_none());
    assert!(matches!(&projected[0].message,
        Message::Assistant { content, .. }
        if matches!(content.as_slice(), [AssistantContent::Text(text)] if text.text == "answer")
    ));
}

async fn stored_agent_request(fixture: &ReplayFixture) -> gents_protocol::row::AgentRequest {
    let response = fixture_query(
        &fixture.access,
        &format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            gents_store::graphql::escape_graphql_string(&fixture.request_doc_id),
            gents_protocol::row::AGENT_REQUEST_FIELDS,
        ),
    )
    .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row = response.data.unwrap()["AgentRequest"][0].clone();
    gents_protocol::row::AgentRequest::try_from(
        serde_json::from_value::<gents_protocol::row::AgentRequestRow>(row).unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn replay_resolution_observes_capture_arriving_for_existing_header() {
    let fixture = signed_fixture().await;
    let candidate = fixture.candidates().await.unwrap().remove(0);
    let tag = ReplayTag {
        request_doc_id: candidate.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: candidate.coordinate.scope,
            turn_index: candidate.coordinate.turn_index,
            attempt: candidate.coordinate.attempt,
        },
    };
    let replay = crate::turn::provider_input::replay::owned_replay_input(
        fixture.node.clone(),
        stored_agent_request(&fixture).await,
        fixture.request_commit_cid.clone(),
        fixture.scope_kind,
        None,
        gents_loop::provider_input::ProviderInputProfile::ClaudeMessages,
    );
    let resolve = replay.resolve.expect("canonical resolver");
    assert!(resolve(vec![tag.clone()]).await.unwrap().is_empty());
    fixture
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await
        .unwrap();
    assert_eq!(
        resolve(vec![tag]).await.unwrap().len(),
        1,
        "a capture arriving for the same physical header must become replay evidence"
    );
}

#[tokio::test]
async fn replay_resolution_observes_a_header_replicated_before_dispatch() {
    let fixture = signed_fixture().await;
    fixture
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await
        .unwrap();
    let candidate = fixture.candidates().await.unwrap().remove(0);
    let tag = ReplayTag {
        request_doc_id: candidate.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: candidate.coordinate.scope,
            turn_index: candidate.coordinate.turn_index,
            attempt: candidate.coordinate.attempt,
        },
    };
    let replay = crate::turn::provider_input::replay::owned_replay_input(
        fixture.node.clone(),
        stored_agent_request(&fixture).await,
        fixture.request_commit_cid.clone(),
        fixture.scope_kind,
        None,
        gents_loop::provider_input::ProviderInputProfile::ClaudeMessages,
    );
    let resolve = replay.resolve.expect("canonical resolver");
    let estimate = resolve(vec![tag.clone()]).await.unwrap();
    assert_eq!(estimate.len(), 1);
    let (unchanged, scans) =
        gents_session::session::count_request_output_scans(resolve(vec![tag.clone()])).await;
    assert_eq!(
        unchanged.unwrap().len(),
        1,
        "an unchanged view resolves the same evidence"
    );
    assert_eq!(
        scans, 1,
        "each dispatch lookup revalidates canonical evidence"
    );
    assert_eq!(resolve(vec![tag.clone()]).await.unwrap().len(), 1);

    let mut duplicate = fixture.header.clone();
    duplicate.sequence += 1;
    duplicate.message_key.push_str("-replicated-twin");
    fixture_write(
        &fixture.access,
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_create_variables(&duplicate).unwrap(),
    )
    .await
    .unwrap();

    let dispatch = resolve(vec![tag]).await.unwrap();
    assert_eq!(
        dispatch.len(),
        2,
        "the dispatch lookup sees the replicated physical twin"
    );
    assert_ne!(
        dispatch[0].evidence.physical_header,
        dispatch[1].evidence.physical_header
    );
}

#[tokio::test]
async fn a_header_twin_arriving_between_the_admission_estimate_and_dispatch_waits_for_the_next_lookup(
) {
    let fixture = signed_fixture().await;
    fixture
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await
        .unwrap();
    let candidate = fixture.candidates().await.unwrap().remove(0);
    let tag = ReplayTag {
        request_doc_id: candidate.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: candidate.coordinate.scope,
            turn_index: candidate.coordinate.turn_index,
            attempt: candidate.coordinate.attempt,
        },
    };
    let replay = crate::turn::provider_input::replay::owned_replay_input(
        fixture.node.clone(),
        stored_agent_request(&fixture).await,
        fixture.request_commit_cid.clone(),
        fixture.scope_kind,
        None,
        gents_loop::provider_input::ProviderInputProfile::ClaudeMessages,
    );
    let resolve = replay.resolve.clone().expect("canonical resolver");
    let estimate = {
        let _estimate_seed = replay.arm_estimate_seed();
        resolve(vec![tag.clone()]).await.unwrap()
    };
    assert_eq!(estimate.len(), 1);

    let mut duplicate = fixture.header.clone();
    duplicate.sequence += 1;
    duplicate.message_key.push_str("-estimate-twin");
    fixture_write(
        &fixture.access,
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_create_variables(&duplicate).unwrap(),
    )
    .await
    .unwrap();

    let dispatch =
        gents_loop::loop_stream::resolve_replay_evidence(&replay, std::slice::from_ref(&tag))
            .await
            .unwrap();
    assert_eq!(
        dispatch, estimate,
        "the seeded dispatch answers the estimate's window, not the replicated twin"
    );
    let observed =
        gents_loop::loop_stream::resolve_replay_evidence(&replay, std::slice::from_ref(&tag))
            .await
            .unwrap();
    assert_eq!(
        observed.len(),
        2,
        "the next resolution observes the replicated physical twin"
    );
}

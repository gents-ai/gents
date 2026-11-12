use std::sync::Arc;
use std::time::Duration;

use gents_inference::llm::HookAction;
use gents_loop::provider_input::ProviderInputProfile;

use gents_loop::loop_stream::LoopStreamItem;
use gents_loop::stream_processor::*;
use gents_session::request::streaming::DefraStreamWriter;

use gents_toolcalls::test_support::stream_processor::*;

#[path = "stream_cadence_tests.rs"]
mod cadence;

#[tokio::test]
async fn first_visible_flushes_immediately_and_followup_waits_for_cadence() {
    let SignedRun {
        db: _db,
        node,
        access,
        hook,
        mut lifecycle,
        writer,
        ..
    } = signed_run("first-visible", Duration::from_secs(60)).await;
    assert!(matches!(
        hook.on_completion_call(&user_text_message("cadence"), &[])
            .await,
        HookAction::Continue
    ));
    bind_active_request(&hook, &lifecycle).await;
    let doc_id = lifecycle.request().doc_id.clone();
    let mut processor = StreamProcessor::new(
        &hook,
        &writer,
        &mut lifecycle,
        &doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );
    processor
        .process_item(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 0,
            attempt: 0,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();
    processor.process_item(text_item("first")).await.unwrap();
    assert_eq!(count_segments(&access, &doc_id).await, 1);
    processor.process_item(text_item(" second")).await.unwrap();
    assert_eq!(count_segments(&access, &doc_id).await, 1);
    assert!(processor.next_flush_deadline().await.is_some());
    processor.flush_pending().await.unwrap();
    assert_eq!(count_segments(&access, &doc_id).await, 2);
    assert!(
        processor.next_flush_deadline().await.is_none(),
        "a committed batch must disarm its timer so provider polling can resume"
    );
    processor.flush_pending().await.unwrap();
    assert_eq!(
        count_segments(&access, &doc_id).await,
        2,
        "no-op flush must not append"
    );
    assert!(processor.next_flush_deadline().await.is_none());
    processor.process_item(text_item(" third")).await.unwrap();
    assert!(processor.next_flush_deadline().await.is_some());
    processor.flush_pending().await.unwrap();
    assert_eq!(count_segments(&access, &doc_id).await, 3);
    assert!(processor.next_flush_deadline().await.is_none());
    processor
        .process_item(Ok(LoopStreamItem::ReasoningDelta {
            id: None,
            reasoning: "first reasoning".into(),
        }))
        .await
        .unwrap();
    assert_eq!(
        count_segments(&access, &doc_id).await,
        4,
        "first reasoning must be durable immediately too"
    );
    assert!(processor.next_flush_deadline().await.is_none());
    processor.process_item(text_item(" fourth")).await.unwrap();
    assert!(processor.next_flush_deadline().await.is_some());
    let message = processor.assistant_turn.clone().take_message().unwrap();
    processor
        .process_item(Ok(LoopStreamItem::ProviderTurnReady {
            turn: 0,
            attempt: 0,
            message,
        }))
        .await
        .unwrap();
    assert!(processor.assistant_turn.clone().take_message().is_none());
    assert!(
        processor.next_flush_deadline().await.is_none(),
        "publication must consume the pending batch timer before the accumulator disappears"
    );
    processor.flush_pending().await.unwrap();
    assert!(
        processor.next_flush_deadline().await.is_none(),
        "the idle processor must let dispatch and finalization run"
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn pre_stream_failures_do_not_fabricate_provider_attempt_closures() {
    let SignedRun {
        db: _db,
        access,
        hook,
        mut lifecycle,
        writer,
        ..
    } = signed_run("pre-stream-failure", Duration::from_secs(60)).await;
    assert!(matches!(
        hook.on_completion_call(&user_text_message("retry provider"), &[])
            .await,
        HookAction::Continue
    ));
    bind_active_request(&hook, &lifecycle).await;
    let doc_id = lifecycle.request().doc_id.clone();
    let mut processor = StreamProcessor::new(
        &hook,
        &writer,
        &mut lifecycle,
        &doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );

    for (attempt, will_retry, error) in [
        (
            0,
            true,
            gents_loop::error::InferenceError::RateLimited {
                retry_after: Some(Duration::from_secs(1)),
            },
        ),
        (
            1,
            false,
            gents_loop::error::InferenceError::ModelUnreachable {
                endpoint: "http://provider.invalid".into(),
            },
        ),
    ] {
        assert!(matches!(
            processor
                .process_item(Ok(LoopStreamItem::AttemptFailed {
                    turn: 0,
                    attempt,
                    error,
                    will_retry,
                    backoff: Duration::ZERO,
                }))
                .await
                .unwrap(),
            StreamAction::Continue
        ));
    }
    drop(processor);

    let request_doc_id = gents_store::graphql::escape_graphql_string(&doc_id);
    let response = exec(&access, &format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "load output segments: {:?}",
        response.errors
    );
    assert_eq!(
        response.data.as_ref().unwrap()["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "a provider failure before stream creation has no canonical output attempt to close"
    );
}

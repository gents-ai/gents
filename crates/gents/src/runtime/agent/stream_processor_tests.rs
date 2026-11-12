use std::time::Duration;

use gents_inference::llm::HookAction;
use gents_loop::provider_input::ProviderInputProfile;

use gents_loop::loop_stream::LoopStreamItem;
use gents_loop::stream_processor::*;

use gents_toolcalls::test_support::stream_processor::*;

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

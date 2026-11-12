fn reasoning_rejection() -> CompletionError {
    CompletionError::ProviderError(
        "400 invalid_request_error: messages.1.content.0: Invalid `signature` in `thinking` \
         block"
            .to_string(),
    )
}

#[tokio::test]
async fn owned_hook_observes_retry_and_clears_after_execution() {
    use gents_inference::inference_observation::{InferenceKey, InferenceObservationRegistry};
    use gents_inference::rendered_request::scope::{scope_request, test_scope};
    let (_db, hook, _writer, lifecycle) = owned_test_hook().await;
    let request = lifecycle.request();
    let registry = InferenceObservationRegistry::default();
    let key = InferenceKey {
        node: request.node_did.clone(),
        session: request.session_id.clone(),
        request: request.request_id.clone(),
        generation: lifecycle.execution_generation().unwrap().to_owned(),
    };
    let guard = Arc::new(registry.begin(key.clone()).unwrap());
    let hook = hook.with_inference_observation(Some(guard.clone()));
    let context = gents_inference::rendered_request::context_for_claimed_request(
        request,
        "",
        "test-model".into(),
        None,
    );
    let scope = test_scope(context, Arc::new(|_| Box::pin(async { Ok(()) })));
    let mut settings = owned_config(0);
    settings.retry_policy.transport_backoff = vec![Duration::from_secs(1)];
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::FailStream(transient_provider_error("reset")),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("recovered".into()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);
    let expected_retry =
        &gents_lean_vocab::lean_contract_snapshot().inference_observation_cases[0].expected[1];
    scope_request(scope, async {
        let stream = run_loop_stream(
            model,
            Some(hook),
            TaggedMessage::unassociated(Message::user("hi")),
            Vec::new(),
            Arc::new(Vec::new()),
            settings,
        );
        futures::pin_mut!(stream);
        loop {
            match stream.next().await.unwrap().unwrap() {
                LoopStreamItem::AuthoredInputReady { reply, .. }
                | LoopStreamItem::SteeringBoundary { reply } => {
                    reply.send(Ok(Vec::new())).unwrap();
                }
                LoopStreamItem::AttemptFailed { will_retry, .. } => {
                    assert!(will_retry);
                    break;
                }
                _ => {}
            }
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(10), stream.next())
                .await
                .is_err()
        );
        let retry = registry.snapshot(&key).expect("backoff is observable");
        assert_eq!(
            serde_json::to_value(retry.activity).unwrap(),
            serde_json::to_value(expected_retry).unwrap()
        );
        assert!(!serde_json::to_string(&retry).unwrap().contains("reset"));
        while let Some(item) = stream.next().await {
            match item.unwrap() {
                LoopStreamItem::AuthoredInputReady { reply, .. }
                | LoopStreamItem::SteeringBoundary { reply } => {
                    let _ = reply.send(Ok(Vec::new()));
                }
                LoopStreamItem::FinishOrIntake { reply, .. } => {
                    let _ = reply.send(Ok(gents_loop::loop_stream::FinishOrIntake::Finished));
                }
                _ => {}
            }
        }
    })
    .await;
    drop(guard);
    assert!(registry.snapshot(&key).is_none());
}

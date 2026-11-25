use std::sync::Arc;

use futures::StreamExt;
use gents_loop::provider_call::admission::AdmissionCallContext;
use gents_loop::provider_call::{ForCall, ProviderCall};
use gents_loop::provider_input::routing_affinity::RoutingAffinity;
use rig::client::CompletionClient;
use rig::completion::CompletionModel;

use crate::llm::rig_compat::http::{HeaderMap, HeaderValue};
use crate::test_support::doubles::RecordingTransport;

/// Sends one streaming request through `model` and drains its stream.
async fn stream_once<M>(model: M)
where
    M: CompletionModel,
{
    let request = model.completion_request("hi").build();
    if let Ok(mut stream) = model.stream(request).await {
        while stream.next().await.is_some() {}
    }
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

#[tokio::test]
async fn a_calls_admission_session_tags_the_chat_completions_request() {
    let recorder = RecordingTransport::default();
    let client = super::providers::build_openai_chat_completions_client(
        "key",
        "http://provider.invalid/v1",
        crate::clients::inference_http::SessionTaggingHttpClient::new(recorder.clone()),
    )
    .unwrap();
    let model = client.completion_model("model");
    let admission = AdmissionCallContext::for_request(
        &crate::test_support::agent_request("req-tagged"),
        "agent",
        "backend",
    );

    stream_once(model.for_call(&ProviderCall {
        admission: Some(admission.clone()),
        ..Default::default()
    }))
    .await;
    stream_once(model.for_call(&ProviderCall::default())).await;

    let sent = recorder.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(
        header(&sent[0], "x-session-id").as_deref(),
        Some(admission.session_id())
    );
    assert_eq!(header(&sent[1], "x-session-id"), None);
}

#[tokio::test]
async fn codex_calls_sharing_an_affinity_carry_its_session_and_retained_turn_state() {
    let mut first_reply = HeaderMap::new();
    first_reply.insert("x-codex-turn-state", HeaderValue::from_static("turn-1"));
    let recorder = RecordingTransport::replying(vec![first_reply, HeaderMap::new()]);
    let http = crate::clients::chatgpt_codex::ChatGptCodexHttpClient::with_inner(
        Arc::new(crate::clients::claude_subscription::StaticBearer::new(
            "token",
        )),
        recorder.clone(),
    )
    .with_policy(crate::clients::chatgpt_codex::ChatGptCodexPolicy::default());
    let client = super::providers::build_openai_responses_client(
        "chatgpt-oauth-managed",
        crate::clients::chatgpt_codex::default_backend_endpoint(),
        http,
        crate::clients::chatgpt_codex::build_chatgpt_codex_headers(None, false, None).unwrap(),
    )
    .unwrap();
    let model = client.completion_model("gpt-5-codex");
    let affinity = RoutingAffinity::start(true, true, Some("session-1".into()));
    assert!(affinity.is_some());
    let call = ProviderCall {
        routing_affinity: affinity,
        ..Default::default()
    };

    stream_once(model.for_call(&call)).await;
    stream_once(model.for_call(&call)).await;

    let sent = recorder.sent();
    assert_eq!(sent.len(), 2);
    for request in &sent {
        assert_eq!(header(request, "session_id").as_deref(), Some("session-1"));
        assert_eq!(
            header(request, "x-client-request-id").as_deref(),
            Some("session-1")
        );
    }
    assert_eq!(header(&sent[0], "x-codex-turn-state"), None);
    assert_eq!(
        header(&sent[1], "x-codex-turn-state").as_deref(),
        Some("turn-1")
    );
}

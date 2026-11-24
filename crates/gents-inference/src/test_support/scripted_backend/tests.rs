use futures::StreamExt as _;
use rig::client::CompletionClient as _;
use rig::completion::CompletionModel;
use rig::streaming::StreamedAssistantContent;

use super::*;
use crate::clients::provider_http::ProviderHttpClient;
use crate::llm::rig_compat::providers::{
    build_openai_chat_completions_client, build_openai_responses_client,
};

async fn complete(endpoint: String, marker: &'static str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{endpoint}/chat/completions"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(
            json!({ "stream": true, "messages": [{ "role": "user", "content": marker }] })
                .to_string(),
        )
        .send()
        .await
        .unwrap()
}

/// What a production client's stream yielded: its text and the names of the
/// tools it called, and whether it carried reasoning.
#[derive(Debug, Default, PartialEq)]
struct Streamed {
    text: String,
    calls: Vec<String>,
    reasoning: bool,
}

async fn collect<R>(
    mut stream: rig::streaming::StreamingCompletionResponse<R>,
) -> Result<Streamed, String>
where
    R: Clone + Unpin + rig::completion::GetTokenUsage,
{
    let mut streamed = Streamed::default();
    while let Some(item) = stream.next().await {
        match item.map_err(|error| error.to_string())? {
            StreamedAssistantContent::Text(text) => streamed.text.push_str(&text.text),
            StreamedAssistantContent::ToolCall { tool_call, .. } => {
                streamed.calls.push(tool_call.function.name)
            }
            StreamedAssistantContent::Reasoning(_)
            | StreamedAssistantContent::ReasoningDelta { .. } => streamed.reasoning = true,
            _ => {}
        }
    }
    Ok(streamed)
}

/// Streams one completion through `model`, offering one tool when `tool`.
async fn stream<M>(model: M, tool: bool) -> Result<Streamed, String>
where
    M: CompletionModel,
{
    let mut request = model
        .completion_request("hello")
        .preamble("system preamble".to_owned())
        .max_tokens(64);
    if tool {
        request = request.tool(rig::completion::ToolDefinition {
            name: "lookup".to_owned(),
            description: "Look up a test value".to_owned(),
            parameters: json!({ "type": "object", "properties": {} }),
        });
    }
    let stream = request.stream().await.map_err(|error| error.to_string())?;
    collect(stream).await
}

async fn chat(backend: &ScriptedBackend, tool: bool) -> Result<Streamed, String> {
    let client = build_openai_chat_completions_client(
        "key",
        backend.endpoint(),
        ProviderHttpClient::default(),
    )
    .unwrap();
    stream(client.completion_model("model"), tool).await
}

async fn responses(backend: &ScriptedBackend, tool: bool) -> Result<Streamed, String> {
    let client = build_openai_responses_client(
        "key",
        backend.endpoint(),
        ProviderHttpClient::default(),
        Default::default(),
    )
    .unwrap();
    stream(client.completion_model("model"), tool).await
}

#[tokio::test]
async fn a_held_call_ends_as_the_test_ends_it() {
    let backend = ScriptedBackend::builder("model")
        .without_usage()
        .start()
        .unwrap();

    let released = backend.hold("released-call");
    let call = tokio::spawn(complete(backend.endpoint().to_owned(), "released-call"));
    released.entered().await;
    released.release();
    let body = call.await.unwrap().text().await.unwrap();
    assert!(body.contains(DEFAULT_REPLY), "{body}");
    assert!(body.contains("[DONE]"), "{body}");
    assert!(!body.contains("prompt_tokens"), "usage is off: {body}");

    let failed = backend.hold("failed-call");
    let call = tokio::spawn(complete(backend.endpoint().to_owned(), "failed-call"));
    failed.entered().await;
    failed.fail(503, "provider unavailable");
    let response = call.await.unwrap();
    assert_eq!(response.status().as_u16(), 503);
    assert!(response
        .text()
        .await
        .unwrap()
        .contains("provider unavailable"));

    let closed = backend.hold("closed-call");
    let call = tokio::spawn(complete(backend.endpoint().to_owned(), "closed-call"));
    closed.entered().await;
    closed.close();
    let body = call.await.unwrap().text().await.unwrap();
    assert!(
        !body.contains("[DONE]"),
        "a closed stream has no terminal event: {body}"
    );
}

#[tokio::test]
async fn usage_is_reported_unless_turned_off() {
    let backend = ScriptedBackend::start("model", Vec::new()).unwrap();
    let body = complete(backend.endpoint().to_owned(), "any")
        .await
        .text()
        .await
        .unwrap();
    assert!(body.contains("prompt_tokens"), "{body}");
}

#[tokio::test]
async fn the_chat_completions_client_reads_scripted_replies_and_calls() {
    let backend = ScriptedBackend::builder("model")
        .respond(|body| {
            body["tools"]
                .is_array()
                .then(|| StreamResponse::tool_call("call-1", "lookup", r#"{"key":"a"}"#))
        })
        .start()
        .unwrap();
    assert_eq!(
        chat(&backend, false).await.unwrap(),
        Streamed {
            text: DEFAULT_REPLY.to_owned(),
            ..Streamed::default()
        }
    );
    assert_eq!(chat(&backend, true).await.unwrap().calls, ["lookup"]);
    let requests = backend.recorded_requests();
    assert!(requests
        .iter()
        .all(|request| request.path == "/v1/chat/completions"));
    assert_eq!(backend.observed_completion_requests(), 2);
}

#[tokio::test]
async fn the_responses_client_reads_a_reasoning_tool_turn() {
    let backend = ScriptedBackend::builder("model")
        .unscripted(Unscripted::ReasoningToolTurn)
        .start()
        .unwrap();
    let turn = responses(&backend, true).await.unwrap();
    assert_eq!(turn.calls, ["lookup"]);
    assert!(turn.reasoning, "{turn:?}");
    assert_eq!(
        responses(&backend, false).await.unwrap().text,
        DEFAULT_REPLY
    );
    assert!(backend
        .recorded_requests()
        .iter()
        .all(|request| request.path == "/v1/responses"));
}

#[tokio::test]
async fn a_gated_reply_streams_its_chunks_around_the_release() {
    let backend = ScriptedBackend::builder("model")
        .respond(|_| {
            Some(StreamResponse::Stream(
                StreamScript::paused("gate", ["first"]).then(vec![StreamChunk::text(" rest")]),
            ))
        })
        .start()
        .unwrap();
    let call = {
        let endpoint = backend.endpoint().to_owned();
        tokio::spawn(async move { complete(endpoint, "any").await.text().await.unwrap() })
    };
    backend.wait_for_chunks("gate", 1).await;
    assert_eq!(backend.observed_chunks("gate"), 1);
    backend.release("gate");
    let body = call.await.unwrap();
    assert!(body.contains("first") && body.contains(" rest"), "{body}");

    let held = ScriptedBackend::start_with_plans(
        "model",
        vec![StreamPlan::new(
            "gated",
            vec![StreamResponse::text("later").after("open")],
        )],
    )
    .unwrap();
    let call = tokio::spawn(complete(held.endpoint().to_owned(), "gated"));
    held.wait_for_request("gated").await;
    assert!(!call.is_finished());
    held.release("open");
    assert!(call.await.unwrap().text().await.unwrap().contains("later"));
}

#[tokio::test]
async fn the_models_endpoint_lists_every_model_and_can_be_held() {
    let backend = ScriptedBackend::builder("first")
        .models(&["second"])
        .start()
        .unwrap();
    let models = format!("{}/models", backend.endpoint());
    let listed = reqwest::get(&models).await.unwrap().text().await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&listed).unwrap(),
        json!({
            "data": [{ "id": "first" }, { "id": "second" }],
            "models": [
                { "model": "first", "name": "first" },
                { "model": "second", "name": "second" },
            ],
        })
    );

    let held = backend.hold_models();
    let probe = tokio::spawn(reqwest::get(models.clone()));
    held.entered().await;
    assert!(!probe.is_finished());
    held.release();
    assert!(probe.await.unwrap().unwrap().status().is_success());
}

#[tokio::test]
async fn a_disconnect_ends_the_request_without_a_response() {
    let backend = ScriptedBackend::builder("model")
        .respond(|_| Some(StreamResponse::Disconnect))
        .start()
        .unwrap();
    let error = reqwest::Client::new()
        .post(format!("{}/chat/completions", backend.endpoint()))
        .body(r#"{"stream":true,"messages":[]}"#)
        .send()
        .await
        .expect_err("no response");
    assert!(!error.is_timeout(), "{error}");
}

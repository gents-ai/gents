use rapidhash::{HashSetExt, RapidHashSet};

use bytes::Bytes;
use futures::{FutureExt, StreamExt};
use rig::completion::{CompletionRequest, ToolDefinition};
use rig::http_client;
use rig::streaming::RawStreamingChoice;
use serde_json::{json, Value};

use super::*;
use crate::llm::message::{
    AssistantContent, Image, Message, ReasoningContent, ToolCall, ToolFunction, ToolResultContent,
    UserContent,
};
use gents_loop::claude_messages_body::ClaudeRequestContext;

#[test]
fn cumulative_usage_keeps_start_counters_and_requires_message_stop() {
    let start = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":0,\"cache_read_input_tokens\":64,\"cache_creation_input_tokens\":36,\"output_tokens\":1}}}\n\n";
    let delta = "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":20}}\n\n";
    for (name, stop, expected) in [
        (
            "messages_cumulative",
            "data: {\"type\":\"message_stop\"}\n\n",
            gents_protocol::usage::TokenUsage {
                input_tokens: Some(100),
                output_tokens: Some(20),
                cached_input_tokens: Some(64),
                cache_creation_input_tokens: Some(36),
                reasoning_output_tokens: None,
                provider_total_tokens: None,
                final_response: true,
            },
        ),
        (
            "messages_eof",
            "",
            gents_protocol::usage::TokenUsage {
                input_tokens: Some(100),
                output_tokens: Some(20),
                cached_input_tokens: Some(64),
                cache_creation_input_tokens: Some(36),
                reasoning_output_tokens: None,
                provider_total_tokens: None,
                final_response: false,
            },
        ),
    ] {
        let events =
            parse_messages_sse(&format!("{start}{delta}{stop}"), &RapidHashSet::new()).unwrap();
        let response = events
            .iter()
            .find_map(|event| match event {
                RawStreamingChoice::FinalResponse(response) => Some(response),
                _ => None,
            })
            .unwrap();
        assert_eq!(response.token_usage, expected, "{name}");
        assert_eq!(
            response.usage.unwrap().input_tokens,
            expected.input_tokens.unwrap(),
            "{name}"
        );
    }
}

#[test]
fn body_effort_follows_the_lean_model() {
    gents_lean_vocab::families::claude_prompt_assembly_body_effort::replay(|input| {
        let mut request = echo_request();
        request.additional_params = Some(json!({
            (ADVERTISED_REASONING_EFFORTS_PARAM): input.supported_efforts,
            "output_config": {"effort": input.effort, "untrusted": true},
            "thinking": {"type": "enabled", "budget_tokens": 999},
            "temperature": 0.5,
        }));
        let body = build_messages_body(&input.model, &request).unwrap();
        assert!(body.get("temperature").is_none());
        assert!(body.get(ADVERTISED_REASONING_EFFORTS_PARAM).is_none());
        assert!(body["output_config"].get("untrusted").is_none());
        assert_eq!(
            body["thinking"],
            json!({"type": "adaptive", "display": "updates"})
        );
        gents_lean_vocab::families::claude_prompt_assembly_body_effort::BodyEffort {
            body_effort: body["output_config"]["effort"].as_str().map(str::to_owned),
        }
    });
}

fn request_from_native(
    preamble: Option<&str>,
    history: Vec<Message>,
    tools: Vec<ToolDefinition>,
) -> CompletionRequest {
    let rig_history = crate::llm::rig_compat::to_rig_messages(&history);
    CompletionRequest {
        model: None,
        preamble: preamble.map(str::to_string),
        chat_history: rig::OneOrMany::many(rig_history).expect("at least one row"),
        documents: Vec::new(),
        tools,
        temperature: None,
        max_tokens: Some(128),
        tool_choice: None,
        additional_params: None,
        output_schema: None,
    }
}

fn echo_tool() -> ToolDefinition {
    ToolDefinition {
        name: "echo".into(),
        description: "echo".into(),
        parameters: json!({"type":"object","properties":{}}),
    }
}

pub(super) fn echo_request() -> CompletionRequest {
    request_from_native(
        Some("You are helpful."),
        vec![Message::user("use echo")],
        vec![echo_tool()],
    )
}

#[test]
fn messages_body_includes_gents_tools_and_system() {
    let body = build_messages_body("claude-sonnet-5", &echo_request()).unwrap();
    assert_eq!(body["model"], "claude-sonnet-5");
    assert_eq!(body["stream"], true);
    assert_eq!(body["max_tokens"], 64_000, "Claude Code's catalog default");
    assert_eq!(body["tools"][0]["name"], "echo");
    assert_eq!(body["system"][2]["text"], "You are helpful.");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_messages_body_has_no_sampling(&body);
}

#[test]
fn messages_body_system_is_identity_only_without_preamble() {
    for preamble in [None, Some(String::new()), Some("   ".to_string())] {
        let mut request = echo_request();
        request.preamble = preamble;
        let body = build_messages_body("claude-sonnet-5", &request).unwrap();
        let system = body["system"].as_array().expect("system array");
        assert_eq!(system.len(), 2, "{body}");
        assert_eq!(system[1]["text"], CLAUDE_CODE_IDENTITY);
    }
}

#[test]
fn messages_body_omits_sampling_even_when_request_sets_it() {
    let mut request = echo_request();
    request.temperature = Some(0.7);
    request.additional_params = Some(json!({
        "temperature": 0.2,
        "top_p": 0.9,
        "top_k": 40,
        "seed": 1,
        "min_p": 0.05,
        "frequency_penalty": 0.1,
        "presence_penalty": 0.1,
    }));
    let body = build_messages_body("claude-sonnet-5", &request).unwrap();
    assert_messages_body_has_no_sampling(&body);
    assert_eq!(body["tools"][0]["name"], "echo");
}

const MESSAGES_BODY_ALLOWED_KEYS: &[&str] = &[
    "model",
    "max_tokens",
    "stream",
    "messages",
    "tools",
    "system",
    "metadata",
    "thinking",
    "context_management",
    "output_config",
    "diagnostics",
];

fn assert_messages_body_has_no_sampling(body: &Value) {
    let keys: Vec<&String> = body.as_object().expect("object body").keys().collect();
    assert!(
        keys.iter()
            .all(|key| MESSAGES_BODY_ALLOWED_KEYS.contains(&key.as_str())),
        "unexpected Messages key: {body}"
    );
    for forbidden in [
        "temperature",
        "top_p",
        "top_k",
        "seed",
        "min_p",
        "frequency_penalty",
        "presence_penalty",
    ] {
        assert!(
            body.get(forbidden).is_none(),
            "{forbidden} must not appear on Claude Messages: {body}"
        );
    }
}

#[test]
fn messages_body_threads_tool_result() {
    let request = request_from_native(
        None,
        vec![
            Message::Assistant {
                id: None,
                request_id: None,
                content: vec![AssistantContent::ToolCall(ToolCall::new(
                    "toolu_1".into(),
                    ToolFunction::new("echo".into(), json!({})),
                ))],
            },
            Message::User {
                content: vec![UserContent::tool_result(
                    "toolu_1",
                    vec![ToolResultContent::text("ECHOED")],
                )],
            },
        ],
        vec![echo_tool()],
    );
    let body = build_messages_body("claude-sonnet-5", &request).unwrap();
    assert_eq!(body["messages"][0]["content"][0]["type"], "tool_use");
    assert_eq!(body["messages"][1]["content"][0]["type"], "tool_result");
    assert_eq!(body["messages"][1]["content"][0]["content"], "ECHOED");
}

#[test]
fn tool_result_images_reach_the_wire_body() {
    let image = |data: &str, mime: &str| {
        ToolResultContent::Image(Image {
            data: crate::llm::message::DocumentSourceKind::Base64(data.into()),
            media_type: crate::llm::message::ImageMediaType::from_mime_type(mime),
            detail: None,
            additional_params: None,
        })
    };
    let request = request_from_native(
        None,
        vec![
            Message::user("draw it"),
            Message::Assistant {
                id: None,
                request_id: None,
                content: vec![AssistantContent::ToolCall(ToolCall::new(
                    "toolu_1".into(),
                    ToolFunction::new("echo".into(), json!({})),
                ))],
            },
            Message::User {
                content: vec![UserContent::tool_result(
                    "toolu_1",
                    vec![
                        ToolResultContent::text(r#"{"height":2,"width":2}"#),
                        image("iVBORw0KGgo=", "image/png"),
                        image("PHN2Zy8+", "image/svg+xml"),
                    ],
                )],
            },
        ],
        vec![echo_tool()],
    );
    let bearer = crate::clients::claude_subscription::ApiKeyBearer::new("placeholder-key");
    let wire = captured_request(
        &ClaudeAccountContext {
            subscriber: false,
            api_key: true,
            ..test_account()
        },
        &bearer,
        &request,
    );
    let (_, body) = wire.split_once("\r\n\r\n").expect("HTTP body");
    let body: Value = serde_json::from_str(body).expect("JSON body");
    assert_eq!(
        body["messages"][2]["content"][0]["content"],
        json!([
            {"type": "text", "text": r#"{"height":2,"width":2}"#},
            {"type": "image", "source": {
                "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo=",
            }},
            {"type": "text", "text": gents_loop::loop_stream::TOOL_RESULT_IMAGE_OMITTED},
        ])
    );
}

fn png_header(width: u32, height: u32) -> String {
    use base64::Engine as _;
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0]);
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn tool_result_images_on_the_wire(images: &[String]) -> Vec<Value> {
    let mut history = vec![Message::user("draw them")];
    for (index, data) in images.iter().enumerate() {
        let id = format!("toolu_{index}");
        history.push(Message::Assistant {
            id: None,
            request_id: None,
            content: vec![AssistantContent::ToolCall(ToolCall::new(
                id.clone(),
                ToolFunction::new("echo".into(), json!({})),
            ))],
        });
        history.push(Message::User {
            content: vec![UserContent::tool_result(
                id,
                vec![ToolResultContent::Image(Image {
                    data: crate::llm::message::DocumentSourceKind::Base64(data.clone()),
                    media_type: Some(crate::llm::message::ImageMediaType::PNG),
                    detail: None,
                    additional_params: None,
                })],
            )],
        });
    }
    let request = request_from_native(None, history, vec![echo_tool()]);
    let bearer = crate::clients::claude_subscription::ApiKeyBearer::new("placeholder-key");
    let wire = captured_request(
        &ClaudeAccountContext {
            subscriber: false,
            api_key: true,
            ..test_account()
        },
        &bearer,
        &request,
    );
    let (_, body) = wire.split_once("\r\n\r\n").expect("HTTP body");
    let body: Value = serde_json::from_str(body).expect("JSON body");
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().unwrap().clone())
        .filter(|block| block["type"] == "tool_result")
        .map(|block| block["content"].clone())
        .collect()
}

fn wire_image(data: &str) -> Value {
    json!([{"type": "image", "source": {
        "type": "base64", "media_type": "image/png", "data": data,
    }}])
}

fn wire_note(note: &str) -> Value {
    json!([{"type": "text", "text": note}])
}

#[test]
fn an_image_over_claudes_side_limit_becomes_a_note() {
    let images = [
        png_header(9000, 4000),
        png_header(3000, 3000),
        "iVBORw0KGgo=".into(),
    ];
    assert_eq!(
        tool_result_images_on_the_wire(&images),
        vec![
            wire_note("[image omitted: 9000×4000 px exceeds Claude's 8000 px limit; ask the tool for a smaller view]"),
            wire_image(&images[1]),
            wire_image(&images[2]),
        ]
    );
}

#[test]
fn past_twenty_images_each_is_held_to_two_thousand_px() {
    let mut images = vec![png_header(3000, 1000)];
    images.extend((0..20).map(|_| png_header(2000, 2000)));
    let wire = tool_result_images_on_the_wire(&images);
    assert_eq!(
        wire[0],
        wire_note("[image omitted: 3000×1000 px exceeds Claude's 2000 px limit for requests with more than 20 images; ask the tool for a smaller view]")
    );
    assert_eq!(
        wire[1..],
        images[1..]
            .iter()
            .map(|data| wire_image(data))
            .collect::<Vec<_>>()[..]
    );

    let twenty = &images[..20];
    assert_eq!(
        tool_result_images_on_the_wire(twenty)[0],
        wire_image(&twenty[0])
    );
}

#[test]
fn past_twenty_large_images_the_newest_twenty_are_sent() {
    let large: Vec<String> = (0..21).map(|_| png_header(3000, 3000)).collect();
    let wire = tool_result_images_on_the_wire(&large);
    assert_eq!(
        wire[0],
        wire_note("[image omitted: 3000×3000 px exceeds Claude's 2000 px limit for requests with more than 20 images; ask the tool for a smaller view]")
    );
    assert_eq!(
        wire[1..],
        large[1..]
            .iter()
            .map(|data| wire_image(data))
            .collect::<Vec<_>>()[..]
    );

    let mut mixed: Vec<String> = (0..20).map(|_| png_header(100, 100)).collect();
    mixed.push(png_header(3000, 3000));
    let wire = tool_result_images_on_the_wire(&mixed);
    assert_eq!(
        wire[0],
        wire_note("[image omitted: a Claude request with an image over 2000 px carries at most 20 images and later ones were kept; ask the tool again to see it]")
    );
    assert_eq!(
        wire[1..],
        mixed[1..]
            .iter()
            .map(|data| wire_image(data))
            .collect::<Vec<_>>()[..]
    );
}

#[test]
fn past_the_image_count_the_oldest_images_become_notes() {
    let images: Vec<String> = (0..101).map(|side| png_header(side + 1, 1)).collect();
    let wire = tool_result_images_on_the_wire(&images);
    assert_eq!(
        wire[0],
        wire_note("[image omitted: a Claude request carries at most 100 images and later ones were kept; ask the tool again to see it]")
    );
    assert_eq!(
        wire[1..],
        images[1..]
            .iter()
            .map(|data| wire_image(data))
            .collect::<Vec<_>>()[..]
    );
}

#[test]
fn messages_body_sends_an_empty_tools_list_when_surface_is_empty() {
    let mut request = echo_request();
    request.tools.clear();
    let body = build_messages_body("claude-sonnet-5", &request).unwrap();
    assert_eq!(body["tools"], json!([]), "{body}");
    let with_tools = build_messages_body("claude-sonnet-5", &echo_request()).unwrap();
    assert_eq!(with_tools["tools"][0]["name"], "echo");
    assert_eq!(with_tools["tools"].as_array().map(Vec::len), Some(1));
}

#[test]
fn sse_maps_echo_and_rejects_bash() {
    let sse = r#"
event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"echo","input":{}}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_stop
data: {"type":"message_stop"}
"#;
    let surface = RapidHashSet::from_iter(["echo".to_string()]);
    let events = parse_messages_sse(sse, &surface).expect("map");
    assert!(matches!(
        &events[0],
        RawStreamingChoice::ToolCall(call) if call.name == "echo" && call.id == "toolu_1"
    ));

    let bash = sse.replace("echo", "Bash");
    let err = parse_messages_sse(&bash, &surface).expect_err("Bash");
    assert!(err.to_string().contains("Bash"), "{err}");
}

fn sse_tool_use_block(id: &str, name: &str, start_input: &str, deltas: &[&str]) -> String {
    let mut sse = format!(
        "event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"{name}\",\"input\":{start_input}}}}}\n\n"
    );
    for partial in deltas {
        let escaped = serde_json::to_string(partial).expect("escape partial_json");
        sse.push_str(&format!(
            "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":{escaped}}}}}\n\n"
        ));
    }
    sse.push_str(
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    );
    sse
}

fn tool_call_arguments(events: &[RawStreamingChoice<ClaudeStreamResponse>]) -> Value {
    match &events[0] {
        RawStreamingChoice::ToolCall(call) => call.arguments.clone(),
        other => panic!("expected ToolCall first, got {other:?}"),
    }
}

#[test]
fn sse_tool_use_deltas_yield_exact_arguments() {
    let sse = sse_tool_use_block("toolu_1", "echo", "{}", &["{\"text\":", " \"hi\"}"]);
    let surface = RapidHashSet::from_iter(["echo".to_string()]);
    let events = parse_messages_sse(&sse, &surface).expect("parse");
    assert_eq!(tool_call_arguments(&events), json!({"text": "hi"}));

    let single = sse_fixture_tool_use("toolu_1", "echo", "{\"text\":\"hi\"}");
    let events = parse_messages_sse(&single, &surface).expect("parse fixture");
    assert_eq!(tool_call_arguments(&events), json!({"text": "hi"}));
}

#[test]
fn sse_tool_use_without_deltas_uses_start_input() {
    let sse = sse_tool_use_block("toolu_1", "echo", "{\"text\":\"hi\"}", &[]);
    let surface = RapidHashSet::from_iter(["echo".to_string()]);
    let events = parse_messages_sse(&sse, &surface).expect("parse");
    assert_eq!(tool_call_arguments(&events), json!({"text": "hi"}));
}

#[test]
fn sse_tool_use_with_no_input_at_all_is_empty_object() {
    let sse = sse_tool_use_block("toolu_1", "echo", "{}", &[]);
    let surface = RapidHashSet::from_iter(["echo".to_string()]);
    let events = parse_messages_sse(&sse, &surface).expect("parse");
    assert_eq!(tool_call_arguments(&events), json!({}));
}

#[test]
fn push_line_yields_text_before_the_body_ends() {
    let mut state = MessagesSseState::new(RapidHashSet::new());
    assert!(state
        .push_line("event: content_block_delta")
        .unwrap()
        .is_empty());
    let events = state
        .push_line(
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hel"}}"#,
        )
        .unwrap();
    assert!(
        events.is_empty(),
        "a data line is not complete until the blank line"
    );
    let events = state.push_line("").unwrap();
    assert!(matches!(&events[..], [RawStreamingChoice::Message(t)] if t == "hel"));
    let events = state.finish().unwrap();
    assert!(matches!(
        &events[..],
        [RawStreamingChoice::FinalResponse(_)]
    ));
}

#[test]
fn sse_error_event_becomes_provider_error() {
    let sse = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
    let err = parse_messages_sse(sse, &RapidHashSet::new()).expect_err("error event");
    let msg = err.to_string();
    assert!(
        msg.contains("overloaded_error") && msg.contains("Overloaded"),
        "{msg}"
    );
}

fn event_key(event: &RawStreamingChoice<ClaudeStreamResponse>) -> String {
    match event {
        RawStreamingChoice::Message(text) => format!("text:{text}"),
        RawStreamingChoice::ToolCall(call) => {
            format!("tool:{}:{}:{}", call.id, call.name, call.arguments)
        }
        RawStreamingChoice::FinalResponse(response) => format!("final:{:?}", response.usage),
        other => format!("{other:?}"),
    }
}

#[tokio::test]
async fn chunk_boundaries_do_not_change_the_event_sequence() {
    let sse = format!(
        "{}{}",
        sse_fixture_text("hello world"),
        sse_fixture_tool_use("toolu_1", "echo", "{\"text\":\"hi\"}")
    );
    let surface = RapidHashSet::from_iter(["echo".to_string()]);
    let whole: Vec<String> = parse_messages_sse(&sse, &surface)
        .unwrap()
        .iter()
        .map(event_key)
        .collect();
    for chunk_len in [1usize, 3, 7, 64, 4096] {
        let chunks: Vec<Result<Bytes, http_client::Error>> = sse
            .as_bytes()
            .chunks(chunk_len)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        let body: http_client::sse::BoxedStream = Box::pin(futures::stream::iter(chunks));
        let events: Vec<String> = stream_sse_body(body, MessagesSseState::new(surface.clone()))
            .map(|e| event_key(&e.expect("event")))
            .collect()
            .await;
        assert_eq!(events, whole, "chunk_len={chunk_len}");
    }
}

#[tokio::test]
async fn split_multibyte_sse_signature_retains_exact_utf8() {
    let sse = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"署名\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n";
    assert!(sse
        .as_bytes()
        .windows(3)
        .any(|bytes| bytes == "署".as_bytes()));
    let chunks = sse
        .as_bytes()
        .chunks(1)
        .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
        .collect::<Vec<Result<Bytes, http_client::Error>>>();
    let body: http_client::sse::BoxedStream = Box::pin(futures::stream::iter(chunks));
    let events = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        stream_sse_body(body, MessagesSseState::new(RapidHashSet::new())).collect::<Vec<_>>(),
    )
    .await
    .expect("bounded SSE decode");
    assert!(events.iter().any(|event| match event {
        Ok(RawStreamingChoice::Reasoning { content, .. }) => matches!(
            crate::llm::rig_compat::from_rig_reasoning_part(content),
            ReasoningContent::Text { signature: Some(signature), .. } if signature == "署名"
        ),
        _ => false,
    }));
    assert!(events.iter().all(Result::is_ok));
}

#[tokio::test]
async fn malformed_utf8_fails_after_prior_valid_sse_events() {
    let mut bytes = sse_fixture_text("prior").into_bytes();
    bytes.extend_from_slice(b"data: \xff\n\n");
    let body: http_client::sse::BoxedStream =
        Box::pin(futures::stream::once(async { Ok(Bytes::from(bytes)) }));
    let events = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        stream_sse_body(body, MessagesSseState::new(RapidHashSet::new())).collect::<Vec<_>>(),
    )
    .await
    .expect("bounded SSE decode");
    assert!(events
        .iter()
        .any(|event| matches!(event, Ok(RawStreamingChoice::Message(text)) if text == "prior")));
    assert!(events
        .last()
        .expect("decoder error")
        .as_ref()
        .unwrap_err()
        .to_string()
        .contains("invalid UTF-8"));
}

#[tokio::test]
async fn first_text_event_is_observable_before_the_body_is_exhausted() {
    let (tx, rx) = futures::channel::mpsc::unbounded::<Result<Bytes, http_client::Error>>();
    let body: http_client::sse::BoxedStream = Box::pin(rx);
    let mut events = Box::pin(stream_sse_body(
        body,
        MessagesSseState::new(RapidHashSet::new()),
    ));
    tx.unbounded_send(Ok(Bytes::from(sse_fixture_text("first"))))
        .unwrap();
    let first = events.next().await.expect("event").expect("ok");
    assert!(matches!(first, RawStreamingChoice::Message(ref t) if t == "first"));
    tx.unbounded_send(Ok(Bytes::from_static(
        b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    )))
    .unwrap();
    drop(tx);
    let rest: Vec<_> = events.collect().await;
    assert!(matches!(
        rest.last().unwrap().as_ref().unwrap(),
        RawStreamingChoice::FinalResponse(_)
    ));
}

#[tokio::test]
async fn cancellation_drains_received_signature_without_polling_provider_again() {
    use crate::rendered_request::scope::{
        arm, claim_pending, current_audit_sender, drain_ready_audit, scope_request, test_scope,
        CaptureScopeKind,
    };
    use crate::rendered_request::{
        AssemblyBuildPath, AssemblyTrace, RenderedRequestCaptureSink, RenderedRequestContext,
    };
    use gents_loop::provider_audit::ClaudeAuditEvent;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::Poll;

    let sink: RenderedRequestCaptureSink = Arc::new(|_| Box::pin(async { Ok(()) }));
    let scope = test_scope(
        RenderedRequestContext {
            request_doc_id: "doc-audit".into(),
            request_commit_cid: "bafy-audit".into(),
            request_id: "req-audit".into(),
            node_did: "did:key:agent".into(),
            requester_did: String::new(),
            agent_did: "behavior".into(),
            session_id: "session".into(),
            model_name: "claude".into(),
            provider_family: None,
        },
        sink,
    );
    scope_request(scope, async {
        arm(
            CaptureScopeKind::Inference,
            0,
            0,
            AssemblyTrace::from_effective_messages(
                AssemblyBuildPath::Budgeted,
                Vec::new(),
            ),
        )
        .unwrap();
        claim_pending().expect("armed provider attempt");
        let sender = current_audit_sender().expect("exact attempt sender");
        let polls = Arc::new(AtomicUsize::new(0));
        let body_polls = Arc::clone(&polls);
        let mut first = Some(Bytes::from_static(
            b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"partial-sig\"}}\n\n",
        ));
        let body: http_client::sse::BoxedStream =
            Box::pin(futures::stream::poll_fn(move |_| {
                body_polls.fetch_add(1, Ordering::SeqCst);
                match first.take() {
                    Some(chunk) => Poll::Ready(Some(Ok(chunk))),
                    None => Poll::Pending,
                }
            }));
        let mut stream = Box::pin(stream_sse_body_observed(
            body,
            MessagesSseState::new(RapidHashSet::new()),
            Some(sender),
        ));
        assert!(stream.next().now_or_never().is_none());
        let before_drain = polls.load(Ordering::SeqCst);
        let received = drain_ready_audit().await;
        assert_eq!(polls.load(Ordering::SeqCst), before_drain);
        assert_eq!(received.len(), 2);
        assert!(matches!(received[0].event, ClaudeAuditEvent::BlockStart { index: 0, .. }));
        assert!(matches!(
            &received[1].event,
            ClaudeAuditEvent::Signature { index: 0, fragment } if fragment == "partial-sig"
        ));
        drop(stream);
    })
    .await;
}

#[test]
fn claude_sse_dispatch_cases_drive_the_stream_state() {
    use gents_lean_vocab::families::claude_prompt_assembly_sse_dispatch as sse;
    use gents_loop::provider_audit::{ClaudeAuditEvent, ClaudeBlockKind};

    fn tool_number(id: &str) -> u64 {
        id.strip_prefix("toolu_")
            .and_then(|number| number.parse().ok())
            .unwrap_or_else(|| panic!("unmodeled tool id {id}"))
    }

    fn payload(p: &sse::SsePayload<String>) -> Value {
        let text = p.text.clone();
        match p.kind.as_str() {
            "tool_start" => json!({"type":"content_block_start","index":p.index,"content_block":{
                "type":"tool_use",
                "id":format!("toolu_{}", p.id.expect("modeled tool id")),
                "name":p.name,
                "input":serde_json::from_str::<Value>(text.as_deref().expect("modeled input")).expect("modeled input object"),
            }}),
            "input_delta" => json!({"type":"content_block_delta","index":p.index,
                "delta":{"type":"input_json_delta","partial_json":text}}),
            "thinking_start" => json!({"type":"content_block_start","index":p.index,
                "content_block":{"type":"thinking","thinking":text,"signature":p.signature}}),
            "thinking_delta" => json!({"type":"content_block_delta","index":p.index,
                "delta":{"type":"thinking_delta","thinking":text}}),
            "signature_delta" => json!({"type":"content_block_delta","index":p.index,
                "delta":{"type":"signature_delta","signature":text}}),
            "block_stop" => json!({"type":"content_block_stop","index":p.index}),
            "stop_reason" => json!({"type":"message_delta","delta":{"stop_reason":text},
                "usage":{"output_tokens":0}}),
            "message_stop" => json!({"type":"message_stop"}),
            other => panic!("unmodeled payload {other}"),
        }
    }

    fn emitted(event: &RawStreamingChoice<ClaudeStreamResponse>) -> sse::SseEmitted {
        let base = |kind: &str| sse::SseEmitted {
            kind: kind.into(),
            text: None,
            signature: None,
            id: None,
            name: None,
            arguments: None,
        };
        match event {
            RawStreamingChoice::Message(text) => sse::SseEmitted {
                text: Some(text.clone()),
                ..base("message")
            },
            RawStreamingChoice::ReasoningDelta { reasoning, .. } => sse::SseEmitted {
                text: Some(reasoning.clone()),
                ..base("reasoning_delta")
            },
            RawStreamingChoice::Reasoning { content, .. } => {
                match crate::llm::rig_compat::from_rig_reasoning_part(content) {
                    ReasoningContent::Text { text, signature } => sse::SseEmitted {
                        text: Some(text),
                        signature,
                        ..base("reasoning")
                    },
                    other => panic!("unmodeled reasoning {other:?}"),
                }
            }
            RawStreamingChoice::ToolCall(call) => sse::SseEmitted {
                id: Some(tool_number(&call.id)),
                name: Some(call.name.clone()),
                arguments: Some(call.arguments.to_string()),
                ..base("tool_call")
            },
            RawStreamingChoice::FinalResponse(_) => base("final"),
            other => panic!("unmodeled stream item {other:?}"),
        }
    }

    fn audit(event: &ClaudeAuditEvent) -> sse::SseAudit {
        let (kind, index, fragment, block) = match event {
            ClaudeAuditEvent::BlockStart { index, kind } => (
                "block_start",
                index,
                None,
                Some(match kind {
                    ClaudeBlockKind::Thinking => "thinking",
                    ClaudeBlockKind::ToolUse { .. } => "tool_use",
                    other => panic!("unmodeled block {other:?}"),
                }),
            ),
            ClaudeAuditEvent::ThinkingText { index, fragment } => {
                ("thinking_text", index, Some(fragment.clone()), None)
            }
            ClaudeAuditEvent::Signature { index, fragment } => {
                ("signature", index, Some(fragment.clone()), None)
            }
            ClaudeAuditEvent::BlockStop { index } => ("block_stop", index, None, None),
            other => panic!("unmodeled audit {other:?}"),
        };
        sse::SseAudit {
            kind: kind.into(),
            index: u64::from(*index),
            fragment,
            block: block.map(str::to_owned),
        }
    }

    fn failure(error: &MessagesParseError) -> String {
        // Rig carries these failures to the loop only as text, so each modeled
        // cause must also be the one the loop's classifier reads from it.
        let report = gents_loop::error::ProviderReport::classify_failure_text(&error.to_string());
        let fail_closed_tool_use = || {
            assert!(
                report.fail_closed_tool_use && !report.refusal,
                "the loop reads a fail-closed tool_use: {error}"
            );
        };
        match error {
            MessagesParseError::ToolUse { .. } => {
                fail_closed_tool_use();
                "tool_use".into()
            }
            MessagesParseError::DuplicateToolUseId { id } => {
                fail_closed_tool_use();
                format!("duplicate_tool_use_id:{}", tool_number(id))
            }
            MessagesParseError::OverlappingToolUse { id } => {
                fail_closed_tool_use();
                format!("overlapping_tool_use:{}", tool_number(id))
            }
            MessagesParseError::MalformedToolUse { .. } => {
                fail_closed_tool_use();
                assert!(
                    error
                        .to_string()
                        .contains("fail-closed: malformed tool_use"),
                    "{error}"
                );
                "malformed_tool_use".into()
            }
            MessagesParseError::MalformedThinking { cause } => format!(
                "malformed_thinking:{}",
                match cause {
                    ThinkingParseCause::WrongBlock => "wrongBlock".to_owned(),
                    ThinkingParseCause::WrongIndex(index) => format!("wrongIndex:{index}"),
                    ThinkingParseCause::IncompleteBlock => "incompleteBlock".to_owned(),
                    ThinkingParseCause::SignatureOrder => "signatureOrder".to_owned(),
                    ThinkingParseCause::MissingSignature => "missingSignature".to_owned(),
                    ThinkingParseCause::Malformed(text) => panic!("unmodeled cause {text}"),
                }
            ),
            MessagesParseError::ProviderRefusal { request_id } => {
                let rendered = CompletionError::from(MessagesParseError::ProviderRefusal {
                    request_id: request_id.clone(),
                })
                .to_string();
                let report = gents_loop::error::ProviderReport::classify_failure_text(&rendered);
                assert!(
                    report.refusal && !report.fail_closed_tool_use,
                    "the loop reads the refusal as permanent: {rendered}"
                );
                format!("refusal:{request_id}")
            }
            other => panic!("unmodeled failure {other}"),
        }
    }

    gents_lean_vocab::families::claude_prompt_assembly_sse_dispatch::replay::<String>(
        |i: &sse::SseInput<String>| {
            let mut state = MessagesSseState::new(i.surface.iter().cloned().collect())
                .with_request_id(i.request_id.clone());
            let mut steps = Vec::new();
            for p in &i.payloads {
                let dispatched = state
                    .push_line_typed(&format!("data: {}", payload(p)))
                    .and_then(|mut events| {
                        events.extend(state.push_line_typed("")?);
                        Ok(events)
                    });
                match dispatched {
                    Ok(events) => steps.push(sse::SseStep {
                        emitted: events.iter().map(emitted).collect(),
                        audits: state.take_audit_events().iter().map(audit).collect(),
                    }),
                    Err(error) => {
                        return sse::SseOutcome {
                            steps,
                            finish: Vec::new(),
                            failure: Some(failure(&error)),
                        }
                    }
                }
            }
            match state.finish_typed() {
                Ok(events) => sse::SseOutcome {
                    steps,
                    finish: events.iter().map(emitted).collect(),
                    failure: None,
                },
                Err(error) => sse::SseOutcome {
                    steps,
                    finish: Vec::new(),
                    failure: Some(failure(&error)),
                },
            }
        },
    );
}

#[test]
fn claude_wire_start_cases_drive_native_parser() {
    use gents_lean_vocab::families::claude_prompt_assembly_wire_start as family;

    family::replay::<String, String, String, String, String>(|i| {
        let mut block = json!({"type":"thinking","thinking":i.thinking});
        if let Some(signature) = &i.signature {
            block["signature"] = match signature.kind.as_str() {
                "text" => json!(signature.text),
                "number" => json!(signature.number),
                other => panic!("unsupported modeled signature {other}"),
            };
        }
        let mut sse = format!(
            "data: {}\n\n",
            json!({"type":"content_block_start","index":i.index,"content_block":block})
        );
        for event in &i.later {
            let payload = match event.kind.as_str() {
                "signature_delta" => json!({
                    "type":"content_block_delta", "index":event.index,
                    "delta":{"type":"signature_delta", "signature":event.fragment.as_deref().expect("modeled signature")}
                }),
                "thinking_delta" => json!({
                    "type":"content_block_delta", "index":event.index,
                    "delta":{"type":"thinking_delta", "thinking":event.fragment.as_deref().expect("modeled thinking")}
                }),
                "content_stop" => json!({"type":"content_block_stop", "index":event.index}),
                other => panic!("unsupported modeled wire event {other}"),
            };
            sse.push_str(&format!("data: {payload}\n\n"));
        }
        match parse_messages_sse_typed(&sse, &RapidHashSet::new()) {
            Err(MessagesParseError::MalformedThinking {
                cause: ThinkingParseCause::Malformed("thinking start signature is not text"),
            }) => family::StartOutcome {
                kind: "wire_error".into(),
                error: None,
                content: Vec::new(),
            },
            Err(MessagesParseError::MalformedThinking {
                cause: ThinkingParseCause::SignatureOrder,
            }) => family::StartOutcome {
                kind: "content_error".into(),
                error: Some("signatureOrder".into()),
                content: Vec::new(),
            },
            Err(error) => panic!("unmodeled wire-start failure: {error}"),
            Ok(events) => family::StartOutcome {
                kind: "ok".into(),
                error: None,
                content: events
                    .iter()
                    .filter_map(|event| {
                        let RawStreamingChoice::Reasoning { content, .. } = event else {
                            return None;
                        };
                        let ReasoningContent::Text { text, signature } =
                            crate::llm::rig_compat::from_rig_reasoning_part(content)
                        else {
                            panic!("modeled text reasoning")
                        };
                        Some(family::StartBlock {
                            kind: "reasoning".into(),
                            value: None,
                            parts: Some(vec![family::StartPart {
                                kind: "text".into(),
                                payload: text,
                                signature,
                            }]),
                            id: None,
                            name: None,
                            arguments: None,
                        })
                    })
                    .collect(),
            },
        }
    });
}

#[test]
fn non_success_error_carries_status_request_id_and_bounded_body_prefix() {
    let err = non_success_error(
        reqwest::StatusCode::BAD_REQUEST,
        Some("req_123"),
        &body_prefix(b"  {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"temperature\"}}\n"),
    );
    let text = err.to_string();
    assert!(text.contains("HTTP 400 Bad Request"), "{text}");
    assert!(text.contains("(request-id req_123)"), "{text}");
    assert!(
        text.ends_with(
            " body={\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"temperature\"}}"
        ),
        "{text}"
    );

    let long = vec![b'x'; NON_SUCCESS_BODY_PREFIX_BYTES * 3];
    assert_eq!(body_prefix(&long).len(), NON_SUCCESS_BODY_PREFIX_BYTES);

    let bare = non_success_error(reqwest::StatusCode::TOO_MANY_REQUESTS, None, "").to_string();
    assert!(bare.ends_with("(request-id -)"), "{bare}");
    assert!(!bare.contains("body="), "{bare}");
}

#[tokio::test]
async fn transport_429_usage_cap_reports_reset_time() {
    let _guard = lock_fixtures_for_test().await;
    let url = crate::clients::provider_http::tests::one_shot_server(
        "429 Too Many Requests",
        &[
            ("retry-after", "6000"),
            ("anthropic-ratelimit-unified-status", "rejected"),
            ("anthropic-ratelimit-unified-reset", "1790354400"),
        ],
        r#"{"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed your account's rate limit. Please try again later."},"request_id":"req_011CeoNrgs1EJvDwA2YxXE7R"}"#,
    )
    .await;
    let bearer = crate::clients::claude_subscription::StaticBearer::new("access");
    let err = stream_messages_at(
        &url,
        "claude-sonnet-5",
        &echo_request(),
        &test_account(),
        RapidHashSet::new(),
        &bearer,
        None,
    )
    .await
    .err()
    .expect("429");
    let classified = crate::llm::rig_compat::classify_completion_error(
        &rig::agent::StreamingError::Completion(err),
    )
    .error;
    assert!(!classified.is_retryable());
    assert_eq!(
        classified.to_string(),
        "provider usage limit reached (resets at 2026-09-25T16:40:00Z): \
         This request would exceed your account's rate limit. Please try again later."
    );
    assert_eq!(bearer.invalidations(), 0);
}

pub(super) fn test_account() -> ClaudeAccountContext {
    ClaudeAccountContext {
        device_id: "cd".repeat(32),
        account_uuid: "acct-test".into(),
        subscriber: true,
        using_overage: false,
        api_key: false,
    }
}

fn captured_request<S>(
    account: &ClaudeAccountContext,
    bearer: &S,
    request: &CompletionRequest,
) -> String
where
    S: crate::auth::credential::BearerSource,
{
    let _guard = blocking_lock_fixtures_for_test();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            let (url, handle) = crate::auth::credential::test_support::one_shot_token_server(
                401,
                r#"{"type":"error","error":{"type":"authentication_error","message":"bad token"}}"#,
            )
            .await;
            let _ = stream_messages_at(
                &url,
                "claude-sonnet-5",
                request,
                account,
                RapidHashSet::new(),
                bearer,
                None,
            )
            .await;
            handle.await.expect("request")
        })
}

fn header_lines(request: &str) -> Vec<(String, String)> {
    request
        .lines()
        .skip(1)
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(": "))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.to_owned()))
        .collect()
}

/// The names of Claude Code's header set that the request carries, sorted. The wire order is
/// Bun's (sorted by code point, then its defaults), pinned in `wire_tests`.
fn claude_code_header_names(request: &str) -> Vec<String> {
    const CLAUDE_CODE_NAMES: &[&str] = &[
        "accept",
        "x-stainless-retry-count",
        "x-stainless-timeout",
        "x-stainless-lang",
        "x-stainless-package-version",
        "x-stainless-os",
        "x-stainless-arch",
        "x-stainless-runtime",
        "x-stainless-runtime-version",
        "anthropic-dangerous-direct-browser-access",
        "anthropic-version",
        "authorization",
        "x-api-key",
        "x-app",
        "user-agent",
        "x-claude-code-session-id",
        "x-claude-code-agent-did",
        "x-claude-code-parent-agent-did",
        "x-claude-code-request-class",
        "x-claude-code-agent-type",
        "x-claude-code-prompt-id",
        "content-type",
        "anthropic-beta",
        "x-client-request-id",
    ];
    let mut names: Vec<String> = header_lines(request)
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| CLAUDE_CODE_NAMES.contains(&name.as_str()))
        .collect();
    names.sort_unstable();
    names
}

fn sorted(mut names: Vec<&str>) -> Vec<&str> {
    names.sort_unstable();
    names
}

fn header_value(request: &str, name: &str) -> Option<String> {
    header_lines(request)
        .into_iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, value)| value)
}

fn request_with_context(context: &ClaudeRequestContext) -> CompletionRequest {
    let mut request = echo_request();
    request.additional_params = Some(json!({
        gents_loop::claude_messages_body::CLAUDE_REQUEST_PARAM: context.to_param().unwrap()
    }));
    request
}

fn context_of(
    cause: gents_claude::origin::Cause,
    root_client: bool,
    scheduled: bool,
) -> ClaudeRequestContext {
    ClaudeRequestContext {
        origin: gents_claude::origin::Origin {
            cause,
            root_client,
            scheduled,
            kind: gents_claude::origin::Kind::Turn,
            appends_system_prompt: true,
        },
        session_key: "session-wire".into(),
        prompt_key: "request-wire".into(),
        parent_session_id: None,
        agent: Some(gents_loop::claude_messages_body::ClaudeAgentIdentity {
            agent_did: "worker".into(),
            parent_agent_did: Some("lead".into()),
        }),
    }
}

const MAIN_HEADER_NAMES: &[&str] = &[
    "accept",
    "x-stainless-retry-count",
    "x-stainless-timeout",
    "x-stainless-lang",
    "x-stainless-package-version",
    "x-stainless-os",
    "x-stainless-arch",
    "x-stainless-runtime",
    "x-stainless-runtime-version",
    "anthropic-dangerous-direct-browser-access",
    "anthropic-version",
    "authorization",
    "x-app",
    "user-agent",
    "x-claude-code-session-id",
    "x-claude-code-request-class",
    "x-claude-code-prompt-id",
    "content-type",
    "anthropic-beta",
    "x-client-request-id",
];

#[test]
fn a_main_turn_sends_claude_codes_header_set() {
    let bearer = crate::clients::claude_subscription::StaticBearer::new("access");
    let context = context_of(gents_claude::origin::Cause::Client, true, false);
    let request = captured_request(&test_account(), &bearer, &request_with_context(&context));
    assert_eq!(
        claude_code_header_names(&request),
        sorted(MAIN_HEADER_NAMES.to_vec()),
        "{request}"
    );
    let ids = context.ids();
    assert_eq!(
        header_value(&request, "authorization").as_deref(),
        Some("Bearer access")
    );
    assert_eq!(header_value(&request, "x-app").as_deref(), Some("cli"));
    assert_eq!(
        header_value(&request, "x-claude-code-session-id"),
        Some(ids.session)
    );
    assert_eq!(
        header_value(&request, "x-claude-code-prompt-id"),
        Some(ids.prompt)
    );
    assert_eq!(
        header_value(&request, "x-claude-code-request-class").as_deref(),
        Some("main")
    );
    assert_eq!(
        header_value(&request, "x-stainless-retry-count").as_deref(),
        Some("0")
    );
    assert_eq!(
        header_value(&request, "x-stainless-timeout").as_deref(),
        Some("600")
    );
    assert_eq!(
        header_value(&request, "user-agent").as_deref(),
        Some("claude-cli/2.1.311 (external, cli)")
    );
    let betas = header_value(&request, "anthropic-beta").unwrap();
    assert!(
        betas.starts_with("claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14"),
        "{betas}"
    );
    assert!(uuid::Uuid::parse_str(&header_value(&request, "x-client-request-id").unwrap()).is_ok());
}

#[test]
fn claude_agent_header_cases_drive_the_sent_request() {
    use gents_claude::origin::{Cause, Kind};
    use gents_lean_vocab::families::claude_prompt_assembly_agent_header_set as headers;

    let bearer = crate::clients::claude_subscription::StaticBearer::new("access");
    gents_lean_vocab::families::claude_prompt_assembly_agent_header_set::replay::<Cause, Kind>(
        |i: &headers::AgentHeaderInput<Cause, Kind>| {
            let mut context = context_of(Cause::Client, true, false);
            context.origin = gents_lean_vocab::family::recode(&i.origin);
            if !i.has_parent {
                if let Some(agent) = context.agent.as_mut() {
                    agent.parent_agent_did = None;
                }
            }
            let request =
                captured_request(&test_account(), &bearer, &request_with_context(&context));
            let identity = context.agent.as_ref().expect("fixture agent identity");
            let agent_did = header_value(&request, "x-claude-code-agent-did");
            let parent = header_value(&request, "x-claude-code-parent-agent-did");
            assert!(agent_did
                .as_ref()
                .is_none_or(|did| *did == identity.agent_did));
            assert!(parent
                .as_ref()
                .is_none_or(|did| Some(did) == identity.parent_agent_did.as_ref()));
            let agent_type = header_value(&request, "x-claude-code-agent-type");
            let mut expected = MAIN_HEADER_NAMES.to_vec();
            expected.extend(
                [
                    (agent_did.is_some(), "x-claude-code-agent-did"),
                    (parent.is_some(), "x-claude-code-parent-agent-did"),
                    (agent_type.is_some(), "x-claude-code-agent-type"),
                ]
                .into_iter()
                .filter_map(|(present, name)| present.then_some(name)),
            );
            assert_eq!(
                claude_code_header_names(&request),
                sorted(expected),
                "the agent headers are the only additions to the main set: {request}"
            );
            headers::AgentHeaderObservation {
                agent_did: agent_did.is_some(),
                parent_agent_did: parent.is_some(),
                agent_type,
                request_class: header_value(&request, "x-claude-code-request-class")
                    .expect("first-party request class"),
                app: header_value(&request, "x-app").expect("x-app"),
            }
        },
    );
}

#[test]
fn usage_wiring_claude_live_messages_records_unified_headers() {
    let _guard = blocking_lock_fixtures_for_test();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            let db = crate::test_support::test_db(
                "claude-live-usage",
                gents_store::node_extension::Extensions::none(),
            )
            .await;
            let node = db.node.clone();
            let cache: std::sync::Arc<crate::usage_observation::UsageCache> =
                std::sync::Arc::default();
            let account = crate::usage_observation::UsageAccount::Backend {
                node_did: db.node_identity.did().to_owned(),
                provider: "ClaudeCliSubscription".into(),
                backend_id: "backend-usage-claude".into(),
                connection: "connection-claude".into(),
            };
            let url = crate::clients::provider_http::tests::one_shot_server(
                "200 OK",
                &[
                    ("anthropic-ratelimit-unified-5h-utilization", "0.25"),
                    ("anthropic-ratelimit-unified-5h-reset", "1790354400"),
                ],
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            )
            .await;
            let bearer = crate::clients::claude_subscription::StaticBearer::new("access-TEST");
            let _ = stream_messages_at(
                &url,
                "claude-sonnet-5",
                &echo_request(),
                &test_account(),
                RapidHashSet::new(),
                &bearer,
                Some(crate::usage_observation::UsageReporter::new(
                    cache.clone(),
                    node.clone(),
                    account.clone(),
                )),
            )
            .await;

            let stored = crate::clients::provider_http::tests::stored_usage_eventually(
                &cache, &node, &account,
            )
            .await
            .expect("usage recorded");
            let window = &stored.report.windows[0];
            assert_eq!((window.label.as_str(), window.used_pct), ("5h", 25.0));
        });
}

#[test]
fn a_scheduled_task_turn_is_a_background_print_session() {
    let bearer = crate::clients::claude_subscription::StaticBearer::new("access");
    let context = context_of(gents_claude::origin::Cause::TaskTrigger, false, true);
    let request = captured_request(&test_account(), &bearer, &request_with_context(&context));
    assert_eq!(
        claude_code_header_names(&request),
        sorted(MAIN_HEADER_NAMES.to_vec()),
        "{request}"
    );
    assert_eq!(header_value(&request, "x-app").as_deref(), Some("cli-bg"));
    assert_eq!(
        header_value(&request, "user-agent").as_deref(),
        Some("claude-cli/2.1.311 (external, sdk-cli, workload/cron)")
    );
    assert_eq!(
        header_value(&request, "x-claude-code-request-class").as_deref(),
        Some("main")
    );
}

#[test]
fn structured_output_schema_is_sent_next_to_the_reasoning_effort() {
    let mut request = echo_request();
    request.additional_params = Some(json!({
        (ADVERTISED_REASONING_EFFORTS_PARAM): ["low", "high"],
        "output_config": {"effort": "high"},
    }));
    let schema = schemars::json_schema!({
        "type": "object",
        "properties": {"goal": {"type": "string"}},
        "required": ["goal"],
    });
    request.output_schema = Some(schema.clone());
    let body = build_messages_body("claude-sonnet-5", &request).unwrap();
    assert_eq!(body["output_config"]["effort"], "high");
    assert_eq!(
        body["output_config"]["format"],
        json!({"type": "json_schema", "schema": schema.as_value()})
    );
}

#[test]
fn an_unsendable_structured_output_schema_is_an_error_not_a_dropped_field() {
    let mut request = echo_request();
    request.output_schema = Some(schemars::Schema::from(true));
    let error = build_messages_body("claude-sonnet-5", &request)
        .expect_err("a non-object schema cannot be sent as output_config.format");
    assert!(
        error.to_string().contains("structured output requested"),
        "{error}"
    );
}

#[test]
fn session_key_param_never_reaches_the_wire() {
    let mut request = echo_request();
    let plain = build_messages_body("claude-sonnet-5", &request).expect("body");
    request.additional_params = Some(json!({ SESSION_KEY_PARAM: "session-1" }));
    let keyed = build_messages_body("claude-sonnet-5", &request).expect("body");
    assert_ne!(plain, keyed, "the key selects the Claude session id");
    assert!(!keyed.to_string().contains(SESSION_KEY_PARAM));
    assert!(!keyed.to_string().contains("session-1"));
    assert_eq!(plain["messages"], keyed["messages"]);
}

#[test]
fn the_messages_uri_is_claude_codes_first_party_route() {
    assert_eq!(
        MESSAGES_URI,
        format!("{}{}", profile::BASE_URL, profile::MESSAGES_PATH)
    );
}

use gents_protocol::message::{
    AssistantContent, Message, ToolCall, ToolFunction, ToolResultContent, UserContent,
};
use serde_json::json;

use super::*;
use crate::test_support::doubles::{empty_trace, test_capture_context};

fn chat_body() -> Value {
    json!({
        "model": "test-model",
        "messages": [{"role": "user", "content": "hi"}],
        "tools": [{"type": "function", "function": {"name": "read_file"}}],
        "temperature": 0.2,
        "max_tokens": 512,
    })
}

fn components() -> RenderedRequestComponents {
    RenderedRequestComponents::from_provider_body(
        chat_body(),
        RenderedRequestSource::OpenAiChatCompletions,
    )
}

fn build(
    turn_index: usize,
    attempt: u32,
    trace: AssemblyTrace,
    components: RenderedRequestComponents,
) -> RenderedCompletionRequest {
    build_rendered_completion_request(
        &test_capture_context(),
        RenderedRequestSource::OpenAiChatCompletions,
        ProviderRoute {
            endpoint: Some("https://api.example.test".to_string()),
            route_path_sha256: None,
        },
        RenderedAttempt {
            capture_scope: "inference.1".to_string(),
            turn_index,
            attempt,
            assembly_trace: trace,
            admission_join: None,
        },
        components,
    )
    .expect("rendered request")
}

fn tool_result_message(id: &str, call_id: Option<&str>, text: &str) -> Message {
    Message::User {
        content: vec![match call_id {
            Some(call_id) => UserContent::tool_result_with_call_id(
                id,
                call_id.to_string(),
                vec![ToolResultContent::text(text)],
            ),
            None => UserContent::tool_result(id, vec![ToolResultContent::text(text)]),
        }],
    }
}

fn assistant_with_tool_call(id: Option<&str>, tool_call_id: &str) -> Message {
    Message::Assistant {
        request_id: None,
        id: id.map(str::to_string),
        content: vec![AssistantContent::ToolCall(ToolCall::new(
            tool_call_id.to_string(),
            ToolFunction {
                name: "read_file".to_string(),
                arguments: json!({"path": "a.txt"}),
            },
        ))],
    }
}

#[test]
fn canonical_hash_sorts_object_keys_and_digests_exact_bytes() {
    let left = json!({ "b": 1, "a": { "d": 2, "c": 3 } });
    let right = json!({ "a": { "c": 3, "d": 2 }, "b": 1 });
    assert_eq!(
        sha256_canonical_json(&left).unwrap(),
        sha256_canonical_json(&right).unwrap()
    );

    let value = json!({ "b": [3, {"z": 1, "a": 2}], "a": "x" });
    let bytes = canonical_json_string(&value).unwrap();
    assert_eq!(bytes, r#"{"a":"x","b":[3,{"a":2,"z":1}]}"#);
    assert_eq!(
        sha256_canonical_json(&value).unwrap(),
        format!("{:x}", Sha256::digest(bytes.as_bytes()))
    );
}

#[test]
fn capture_key_is_stable_prefixed_and_boundary_separated() {
    let key = capture_key("did:key:a", "session-1", "doc-1", "inference.1", 3, 2).unwrap();
    assert!(key.starts_with("rendered:v1:") && key.len() == "rendered:v1:".len() + 64);
    assert_eq!(
        key,
        capture_key("did:key:a", "session-1", "doc-1", "inference.1", 3, 2).unwrap()
    );
    for (a, b) in [
        (
            ("did:key:a", "s:1", "doc-1", "inference.1"),
            ("did:key:a:s", "1", "doc-1", "inference.1"),
        ),
        (
            ("did:key:a", "session", "r:1", "inference.1"),
            ("did:key:a", "session:r", "1", "inference.1"),
        ),
        (
            ("did:key:a", "session-1", "shared", "inference.1"),
            ("did:key:a", "session-2", "shared", "inference.1"),
        ),
        (
            ("did:key:a", "s", "req#compaction.1", "inference.1"),
            ("did:key:a", "s", "req", "compaction.1"),
        ),
    ] {
        assert_ne!(
            capture_key(a.0, a.1, a.2, a.3, 0, 0).unwrap(),
            capture_key(b.0, b.1, b.2, b.3, 0, 0).unwrap(),
        );
    }
}

#[test]
fn build_path_records_the_request_assembly_route() {
    let budgeted = build(0, 0, empty_trace(), components());
    let repaired = build(
        0,
        1,
        AssemblyTrace::from_effective_messages(AssemblyBuildPath::Repair, Vec::new()),
        components(),
    );

    assert_eq!(
        budgeted.assembly_trace.build_path,
        AssemblyBuildPath::Budgeted
    );
    assert_eq!(
        repaired.assembly_trace.build_path,
        AssemblyBuildPath::Repair
    );
    assert_ne!(budgeted.provenance_json, repaired.provenance_json);
}

#[test]
fn provenance_json_round_trips_to_a_captured_only_manifest() {
    let trace = AssemblyTrace::from_effective_messages(
        AssemblyBuildPath::Repair,
        vec![
            assistant_with_tool_call(Some("msg_abc"), "call-1"),
            tool_result_message("call-1", Some("fc_1"), "threaded bytes"),
        ],
    );
    let rendered = build(2, 1, trace.clone(), components());

    let manifest: ProvenanceManifest =
        serde_json::from_value(rendered.provenance_json.clone()).expect("manifest round-trip");

    assert_eq!(manifest.manifest_version, PROVENANCE_MANIFEST_VERSION);
    assert_eq!(manifest.status, ProvenanceStatus::CapturedOnly);
    assert!(!manifest.status_reason.is_empty());
    assert_eq!(manifest.capture_seam, CaptureSeam::TransportBody);
    assert_eq!(manifest.capture_scope, "inference.1");
    assert_eq!(manifest.assembly_trace, AssemblyTraceMetadata::from(&trace));
    assert_eq!(
        serde_json::from_value::<AssemblyTrace>(rendered.provenance_payload_json.clone())
            .expect("payload round-trip"),
        trace
    );
    assert_eq!(rendered.assembly_trace, trace);
}

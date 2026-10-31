use gents_protocol::message::{
    AssistantContent, Message, ToolCall, ToolFunction, ToolResultContent, UserContent,
};
use serde_json::json;

use super::*;

fn context() -> RenderedRequestContext {
    RenderedRequestContext {
        request_doc_id: "doc-1".to_string(),
        request_commit_cid: "bafy-request-commit".to_string(),
        request_id: "req-1".to_string(),
        node_did: "did:key:test".to_string(),
        requester_did: "did:key:requester".to_string(),
        agent_id: "behavior".to_string(),
        session_id: "session".to_string(),
        model_name: "test-model".to_string(),
        provider_family: None,
    }
}

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
        &context(),
        "inference.1",
        RenderedRequestSource::OpenAiChatCompletions,
        Some("https://api.example.test".to_string()),
        None,
        turn_index,
        attempt,
        trace,
        components,
        None,
    )
    .expect("rendered request")
}

fn empty_trace() -> AssemblyTrace {
    AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, Vec::new())
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
fn canonical_hash_sorts_object_keys() {
    let left = json!({ "b": 1, "a": { "d": 2, "c": 3 } });
    let right = json!({ "a": { "c": 3, "d": 2 }, "b": 1 });

    assert_eq!(
        sha256_canonical_json(&left).unwrap(),
        sha256_canonical_json(&right).unwrap()
    );
}

#[test]
fn component_hashes_digest_exactly_the_canonical_bytes() {
    let value = json!({ "b": [3, {"z": 1, "a": 2}], "a": "x" });

    let bytes = canonical_json_string(&value).unwrap();
    assert_eq!(bytes, r#"{"a":"x","b":[3,{"a":2,"z":1}]}"#);

    let digest = Sha256::digest(bytes.as_bytes());
    assert_eq!(
        sha256_canonical_json(&value).unwrap(),
        format!("{digest:x}")
    );
}

#[test]
fn capture_key_is_stable_and_prefixed() {
    let key = capture_key("did:key:a", "session-1", "doc-1", "inference.1", 3, 2).unwrap();
    assert!(key.starts_with("rendered:v1:"), "unexpected key {key}");
    assert_eq!(key.len(), "rendered:v1:".len() + 64);
    assert_eq!(
        key,
        capture_key("did:key:a", "session-1", "doc-1", "inference.1", 3, 2).unwrap()
    );
}

#[test]
fn every_capture_key_component_changes_the_key() {
    let base = capture_key("did:key:a", "session-1", "doc-1", "inference.1", 0, 0).unwrap();

    for varied in [
        capture_key("did:key:b", "session-1", "doc-1", "inference.1", 0, 0).unwrap(),
        capture_key("did:key:a", "session-2", "doc-1", "inference.1", 0, 0).unwrap(),
        capture_key("did:key:a", "session-1", "doc-2", "inference.1", 0, 0).unwrap(),
        capture_key("did:key:a", "session-1", "doc-1", "inference.2", 0, 0).unwrap(),
        capture_key("did:key:a", "session-1", "doc-1", "compaction.1", 0, 0).unwrap(),
        capture_key("did:key:a", "session-1", "doc-1", "inference.1", 1, 0).unwrap(),
        capture_key("did:key:a", "session-1", "doc-1", "inference.1", 0, 1).unwrap(),
    ] {
        assert_ne!(base, varied);
    }
}

#[test]
fn capture_key_does_not_collide_across_component_boundaries() {
    assert_ne!(
        capture_key("did:key:a", "s:1", "doc-1", "inference.1", 0, 0).unwrap(),
        capture_key("did:key:a:s", "1", "doc-1", "inference.1", 0, 0).unwrap(),
    );
    assert_ne!(
        capture_key("did:key:a", "session", "r:1", "inference.1", 0, 0).unwrap(),
        capture_key("did:key:a", "session:r", "1", "inference.1", 0, 0).unwrap(),
    );
    assert_ne!(
        capture_key("did:key:a", "session-1", "shared", "inference.1", 0, 0).unwrap(),
        capture_key("did:key:a", "session-2", "shared", "inference.1", 0, 0).unwrap(),
    );
    assert_ne!(
        capture_key("did:key:a", "s", "req#compaction.1", "inference.1", 0, 0).unwrap(),
        capture_key("did:key:a", "s", "req", "compaction.1", 0, 0).unwrap(),
    );
}

#[test]
fn the_summarizers_first_call_is_not_the_requests_first_turn() {
    assert_ne!(
        capture_key("did:key:a", "s", "req", "inference.1", 0, 0).unwrap(),
        capture_key("did:key:a", "s", "req", "compaction.1", 0, 0).unwrap(),
    );
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

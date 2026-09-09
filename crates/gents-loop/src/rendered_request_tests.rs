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
        agent_did: "did:key:test".to_string(),
        requester_did: "did:key:requester".to_string(),
        behavior_id: "behavior".to_string(),
        session_id: "session".to_string(),
        model_name: "test-model".to_string(),
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

/// The persisted bytes and the digest must come from the same encoder. If a
/// second serialization ever creeps into the sink, this is the test that
/// notices the digest no longer describes the stored string.
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
fn canonical_json_sorts_nested_arrays_of_objects() {
    let value = json!([{ "b": 1, "a": 2 }, { "d": 3, "c": 4 }]);
    assert_eq!(
        canonical_json_string(&value).unwrap(),
        r#"[{"a":2,"b":1},{"c":4,"d":3}]"#
    );
}

/// Reconstruction compares the *parsed* persisted string against a freshly
/// rendered request. That comparison is only meaningful if canonicalization
/// reorders keys and changes nothing else — array order, numeric form,
/// nulls, and escapes all have to survive the round trip.
#[test]
fn canonical_bytes_parse_back_to_the_same_value() {
    let value = json!({
        "model": "test-model",
        "messages": [
            { "role": "system", "content": "b\u{0007}\"quoted\"\n" },
            { "role": "user", "content": null },
        ],
        "tools": [],
        "temperature": 0.2,
        "max_tokens": 512,
        "nested": { "z": [1, 2, 3], "a": { "deep": true } },
    });

    let parsed: Value = serde_json::from_str(&canonical_json_string(&value).unwrap()).unwrap();
    assert_eq!(parsed, value);
    assert_eq!(parsed, canonical_json(&value));
}

/// Absent tools and tool choice remain explicit typed views of the captured
/// transport body.
#[test]
fn empty_tools_and_tool_choice_remain_explicit() {
    let rendered = build(
        0,
        0,
        empty_trace(),
        RenderedRequestComponents::from_provider_body(
            json!({"model": "test-model", "tools": [], "messages": []}),
            RenderedRequestSource::OpenAiChatCompletions,
        ),
    );

    assert_eq!(rendered.tools_json, json!([]));
    assert_eq!(rendered.tool_choice_json, Value::Null);
    assert_eq!(rendered.sampling_json["temperature"], Value::Null);
}

/// A Responses body names its message list `input`, and the derived views
/// have to follow the body rather than a configured wire API.
#[test]
fn responses_bodies_index_the_input_field() {
    let components = RenderedRequestComponents::from_provider_body(
        json!({
            "model": "gpt-5.2",
            "instructions": "hoisted",
            "input": [{"role": "user", "content": "hi"}],
            "max_output_tokens": 4096,
        }),
        RenderedRequestSource::OpenAiResponses,
    );
    assert_eq!(components.messages_json[0]["role"], "user");
    assert_eq!(components.sampling_json["max_tokens"], 4096);
}

/// Codex deletes `max_output_tokens`, `temperature`, and `top_p` from the
/// body. The row has to say `null`, not the value the loop assembled.
#[test]
fn a_stripped_sampling_parameter_reads_as_null() {
    let components = RenderedRequestComponents::from_provider_body(
        json!({ "model": "gpt-5.2", "input": [], "store": false }),
        RenderedRequestSource::OpenAiResponses,
    );
    assert_eq!(components.sampling_json["max_tokens"], Value::Null);
    assert_eq!(components.sampling_json["temperature"], Value::Null);
    assert_eq!(components.sampling_json["top_p"], Value::Null);
}

#[test]
fn rendered_completion_request_retains_transport_views() {
    let rendered = build(0, 0, empty_trace(), components());

    assert_eq!(rendered.request_id, "req-1");
    assert_eq!(rendered.capture_scope, "inference.1");
    assert_eq!(rendered.turn_index, 0);
    assert_eq!(rendered.attempt, 0);
    assert_eq!(rendered.requester_did, "did:key:requester");
    assert_eq!(rendered.capture_version, CAPTURE_VERSION);
    assert_eq!(
        rendered.source,
        RenderedRequestSource::OpenAiChatCompletions
    );
    assert_eq!(rendered.messages_json[0]["role"], "user");
    assert_eq!(rendered.tools_json[0]["function"]["name"], "read_file");
    assert_eq!(rendered.sampling_json["temperature"], 0.2);
    assert_eq!(rendered.sampling_json["max_tokens"], 512);
}

/// The payload survives capture as the durable source of truth.
#[test]
fn rendered_completion_request_retains_the_full_payload() {
    let rendered = build(0, 0, empty_trace(), components());

    assert_eq!(rendered.request_json, chat_body());
}

/// The `model_name` column reports the model the provider was asked for.
/// A behavior document edited between reconcile and send must not make the
/// row disagree with the body.
#[test]
fn model_name_comes_from_the_captured_body() {
    let rendered = build(
        0,
        0,
        empty_trace(),
        RenderedRequestComponents::from_provider_body(
            json!({"model": "wire-model", "messages": []}),
            RenderedRequestSource::OpenAiChatCompletions,
        ),
    );
    assert_eq!(rendered.model_name, "wire-model");

    let bodyless = build(
        0,
        0,
        empty_trace(),
        RenderedRequestComponents::from_provider_body(
            json!({"messages": []}),
            RenderedRequestSource::OpenAiChatCompletions,
        ),
    );
    assert_eq!(bodyless.model_name, "test-model");
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

/// Every component has to move the key. A dropped component silently merges
/// two provider attempts into one durable fact, which is precisely what
/// `capture_key_determines_request` forbids.
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

/// A delimited `"{a}:{b}"` encoding would make these one fact. `session_id`
/// is caller-supplied and unvalidated, so the encoding — not a convention —
/// has to rule the collision out. The scope rides inside the third
/// component as a nested array for exactly the same reason: a
/// `"{request_doc_id}#{scope}"` string would lose the component boundary.
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
    // A replicated document id appearing under two session contexts must
    // stay two facts rather than erasing the session boundary.
    assert_ne!(
        capture_key("did:key:a", "session-1", "shared", "inference.1", 0, 0).unwrap(),
        capture_key("did:key:a", "session-2", "shared", "inference.1", 0, 0).unwrap(),
    );
    // A document id component must not absorb another scope's boundary.
    assert_ne!(
        capture_key("did:key:a", "s", "req#compaction.1", "inference.1", 0, 0).unwrap(),
        capture_key("did:key:a", "s", "req", "compaction.1", 0, 0).unwrap(),
    );
}

/// The concrete collision this component exists to prevent: the request's
/// first turn and the summarizer's first call are both `(turn 0, attempt
/// 0)` of the same request in the same session.
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
    assert_eq!(manifest.assembly_trace, trace);
    assert_eq!(rendered.assembly_trace, trace);
}

/// `provenance_json` is what lands in the column, so its key order must not
/// depend on which workspace members turned on `serde_json/preserve_order`.
#[test]
fn provenance_json_is_canonically_ordered() {
    let rendered = build(0, 0, empty_trace(), components());

    assert_eq!(
        canonical_json(&rendered.provenance_json),
        rendered.provenance_json
    );
}

/// Version 1 declares `captured_only` positively. An absent field is never
/// the evidence — a reader must be able to see the claim, not infer it.
#[test]
fn version_one_provenance_never_claims_verification() {
    let rendered = build(0, 0, empty_trace(), components());

    assert_eq!(rendered.provenance_json["status"], "captured_only");
    assert_eq!(
        rendered.provenance_json["manifest_version"],
        PROVENANCE_MANIFEST_VERSION
    );
    assert!(rendered.provenance_json.get("assembly_trace").is_some());
}

/// The seam is recorded positively so a reader never has to guess whether
/// the bytes predate the ChatGPT-Codex and Grok body rewrites.
#[test]
fn provenance_names_the_seam_the_bytes_came_from() {
    let rendered = build(0, 0, empty_trace(), components());

    assert_eq!(rendered.provenance_json["capture_seam"], "transport_body");
    assert_eq!(rendered.provenance_json["capture_scope"], "inference.1");
}

/// Outside an admission scope (the one-shot situation) there is no join,
/// and its absence is an absent key — never a null or a placeholder.
#[test]
fn provenance_without_an_admission_scope_carries_no_join() {
    let rendered = build(0, 0, empty_trace(), components());

    assert!(rendered.provenance_json.get("admission").is_none());
    assert_eq!(
        rendered.provenance_json["manifest_version"],
        PROVENANCE_MANIFEST_VERSION
    );
}


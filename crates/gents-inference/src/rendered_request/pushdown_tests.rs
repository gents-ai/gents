use crate::rendered_request::{
    capture_key as derive_capture_key, AssemblyBuildPath, AssemblyTrace, ProvenanceManifest,
    RenderedCompletionRequest, RenderedRequestSource, CAPTURE_VERSION,
};
use serde_json::{json, Value};

fn node_did(id: u64) -> String {
    format!("did:key:z6Mk-node-{id}")
}

fn session_id(id: u64) -> String {
    format!("session-{id}")
}

fn request_doc_id(id: u64) -> String {
    format!("bae-request-doc-{id}")
}

fn rendered(
    node: u64,
    session: u64,
    request: u64,
    turn_index: usize,
    attempt: u32,
    request_json: Value,
) -> RenderedCompletionRequest {
    rendered_in_scope(
        node,
        session,
        request,
        CAPTURE_SCOPE,
        turn_index,
        attempt,
        request_json,
    )
}

const CAPTURE_SCOPE: &str = "inference.1";

fn rendered_in_scope(
    node: u64,
    session: u64,
    request: u64,
    capture_scope: &str,
    turn_index: usize,
    attempt: u32,
    request_json: Value,
) -> RenderedCompletionRequest {
    let node_did = node_did(node);
    let session_id = session_id(session);
    let request_doc_id = request_doc_id(request);
    let assembly_trace =
        AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, Vec::new());

    RenderedCompletionRequest {
        capture_key: derive_capture_key(
            &node_did,
            &session_id,
            &request_doc_id,
            capture_scope,
            turn_index,
            attempt,
        )
        .expect("capture key"),
        capture_version: CAPTURE_VERSION,
        request_doc_id,
        request_commit_cid: format!("bafy-request-commit-{request}"),
        request_id: format!("logical-request-{request}"),
        capture_scope: capture_scope.to_string(),
        turn_index,
        attempt,
        node_did,
        requester_did: "did:key:z6Mk-requester".to_string(),
        agent_did: "agent-1".to_string(),
        session_id,
        model_name: "test-model".to_string(),
        source: RenderedRequestSource::OpenAiChatCompletions,
        request_json,
        messages_json: json!([]),
        tools_json: json!([]),
        tool_choice_json: Value::Null,
        sampling_json: Value::Null,
        provenance_json: serde_json::to_value(ProvenanceManifest::captured_only(
            capture_scope.to_string(),
            None,
            None,
            assembly_trace.clone(),
        ))
        .expect("provenance manifest"),
        provenance_payload_json: serde_json::to_value(&assembly_trace).expect("provenance payload"),
        assembly_trace,
    }
}

fn capture_key(
    rendered: &RenderedCompletionRequest,
) -> (String, String, (String, String), usize, u32) {
    (
        rendered.node_did.clone(),
        rendered.session_id.clone(),
        (
            rendered.request_doc_id.clone(),
            rendered.capture_scope.clone(),
        ),
        rendered.turn_index,
        rendered.attempt,
    )
}

#[test]
fn rendered_capture_keys_follow_the_lean_model() {
    gents_lean_vocab::families::contracts_rendered_capture_key::replay(|pair| {
        let left = rendered(
            pair.left_node_did,
            pair.left_session_id,
            pair.left_request_id,
            usize::try_from(pair.left_turn_index).unwrap(),
            u32::try_from(pair.left_attempt).unwrap(),
            json!({"model": "test-model"}),
        );
        let right = rendered(
            pair.right_node_did,
            pair.right_session_id,
            pair.right_request_id,
            usize::try_from(pair.right_turn_index).unwrap(),
            u32::try_from(pair.right_attempt).unwrap(),
            json!({"model": "test-model"}),
        );
        let same_fact = left.capture_key == right.capture_key;
        assert_eq!(capture_key(&left) == capture_key(&right), same_fact);
        same_fact
    });
}

#[test]
fn the_capture_scope_separates_completion_loops_within_one_request() {
    let inference = rendered_in_scope(1, 1, 1, "inference.1", 0, 0, json!({"model": "m"}));
    let compaction = rendered_in_scope(1, 1, 1, "compaction.1", 0, 0, json!({"model": "m"}));
    let fallback = rendered_in_scope(
        1,
        1,
        1,
        "compaction_fallback.1",
        0,
        0,
        json!({"model": "m"}),
    );
    let compaction_again = rendered_in_scope(1, 1, 1, "compaction.2", 0, 0, json!({"model": "m"}));

    let keys = [&inference, &compaction, &fallback, &compaction_again];
    for (index, left) in keys.iter().enumerate() {
        for right in keys.iter().skip(index + 1) {
            assert_ne!(
                capture_key(left),
                capture_key(right),
                "scopes {} and {} must be separate facts",
                left.capture_scope,
                right.capture_scope
            );
            assert_ne!(
                left.capture_key, right.capture_key,
                "scopes {} and {} must derive different capture keys",
                left.capture_scope, right.capture_scope
            );
        }
    }
}

#[test]
fn a_request_doc_id_cannot_forge_another_scopes_key() {
    let forged = rendered_in_scope(1, 1, 1, "inference.1", 0, 0, json!({"model": "m"}));
    let honest = rendered_in_scope(1, 1, 1, "inference.1", 0, 0, json!({"model": "m"}));
    assert_eq!(forged.capture_key, honest.capture_key);

    let sneaky = derive_capture_key(
        &node_did(1),
        &session_id(1),
        &format!("{}#compaction.1", request_doc_id(1)),
        "inference.1",
        0,
        0,
    )
    .expect("capture key");
    let target = derive_capture_key(
        &node_did(1),
        &session_id(1),
        &request_doc_id(1),
        "compaction.1",
        0,
        0,
    )
    .expect("capture key");
    assert_ne!(sneaky, target);
}

#[test]
fn generated_capture_scope_cases_pin_the_shared_parser() {
    use crate::rendered_request::CaptureScope;

    let scope_cases = gents_lean_vocab::lean_capture_scope_cases();
    assert!(
        !scope_cases.is_empty(),
        "Lean emitted no capture scope cases"
    );
    for case in scope_cases {
        let parsed = case.label.parse::<CaptureScope>();
        if case.valid {
            let scope = parsed.unwrap_or_else(|error| {
                panic!("{label:?} must parse: {error}", label = case.label)
            });
            assert_eq!(scope.kind.as_str(), case.kind, "{:?}", case.label);
            assert_eq!(scope.seq, case.seq, "{:?}", case.label);
            assert_eq!(
                scope.to_string(),
                case.label,
                "label round trip must be exact"
            );
        } else {
            assert!(
                parsed.is_err(),
                "{label:?} must be rejected, never defaulted",
                label = case.label
            );
        }
    }
}

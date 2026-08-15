use crate::lean_vocab_test::lean_durable_reduction_cases;
use defra_node::EmbeddedNode;
use gents::llm::message::{
    AssistantContent, Message, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent,
    UserContent,
};
use gents::provider_context_reduction::{
    load_for_request, persist, reduction_key, rendered_capture_cites_reduction,
    NewProviderContextReduction, SourceBoundary,
};
use gents_protocol::rendered_request::{AssemblyBuildPath, AssemblyTrace, ProvenanceManifest};
use serde_json::Value;

fn checkpoint(value: u64) -> Vec<Message> {
    vec![Message::user(format!("checkpoint:{value}"))]
}

fn boundary(request_doc_id: &str, claim_commit: u64) -> SourceBoundary {
    SourceBoundary {
        boundary_version: 2,
        request_doc_id: request_doc_id.to_string(),
        request_commit_cid: format!("claim-cid-{claim_commit}"),
        canonical_through: None,
    }
}

fn open_pair_projection() -> (Vec<Message>, Vec<Message>) {
    let call = Message::Assistant {
        id: None,
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: "call-1".to_string(),
            call_id: Some("call-1".to_string()),
            function: ToolFunction {
                name: "read".to_string(),
                arguments: Value::Object(Default::default()),
            },
            signature: None,
            additional_params: None,
        })],
    };
    let result = Message::User {
        content: vec![UserContent::ToolResult(ToolResult {
            id: "call-1".to_string(),
            call_id: Some("call-1".to_string()),
            content: vec![ToolResultContent::Text(Text {
                text: "done".to_string(),
            })],
        })],
    };
    (vec![call], vec![result])
}

fn provenance(scope: &str, messages: Vec<Message>, keys: Vec<String>) -> String {
    serde_json::to_string(&ProvenanceManifest::captured_only(
        scope.to_string(),
        None,
        None,
        AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, messages)
            .with_reduction_keys(keys),
    ))
    .unwrap()
}

#[tokio::test]
async fn generated_durable_reduction_cases_pin_identity_and_persist_before_send() {
    let cases = lean_durable_reduction_cases();
    assert!(!cases.is_empty(), "Lean emitted no durable-reduction cases");

    for case in cases {
        let node = EmbeddedNode::builder().build().await.unwrap();
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let request_doc_id = format!("request-doc-{}", case.request_doc_id);
        let key = reduction_key(
            "did:key:agent",
            "session-11",
            &request_doc_id,
            case.turn_index,
            case.ordinal,
        )
        .unwrap();

        let mut parent_key = None;
        if case.ordinal > 1 {
            assert_eq!(case.ordinal, 2, "test fixture only models one predecessor");
            let predecessor_claim = case.claim_commit.saturating_sub(1);
            let predecessor_boundary = boundary(&request_doc_id, predecessor_claim);
            let predecessor_checkpoint = checkpoint(case.checkpoint.saturating_sub(1));
            let predecessor = persist(
                &node,
                NewProviderContextReduction {
                    agent_did: "did:key:agent",
                    requester_did: None,
                    session_id: "session-11",
                    request_id: "request-11",
                    request_doc_id: &request_doc_id,
                    request_commit_cid: &format!("claim-cid-{predecessor_claim}"),
                    reduction_index: 1,
                    turn_index: case.turn_index.saturating_sub(1),
                    parent_reduction_key: None,
                    producer_call: None,
                    source_boundary: &predecessor_boundary,
                    compacted_prefix: &[Message::user("older")],
                    retained_suffix: &predecessor_checkpoint,
                    checkpoint_messages: &predecessor_checkpoint,
                    summary: "",
                    original_tokens: 120,
                    compacted_tokens: 30,
                },
            )
            .await
            .unwrap();
            parent_key = Some(predecessor.reduction_key);
        }

        let mut prior_doc_id = None;
        if let Some(prior_checkpoint) = case.prior_checkpoint {
            let prior_claim = case.prior_claim_commit.unwrap_or(case.claim_commit);
            let prior_messages = checkpoint(prior_checkpoint);
            let prior_boundary = boundary(&request_doc_id, prior_claim);
            let row = persist(
                &node,
                NewProviderContextReduction {
                    agent_did: "did:key:agent",
                    requester_did: None,
                    session_id: "session-11",
                    request_id: "request-11",
                    request_doc_id: &request_doc_id,
                    request_commit_cid: &format!("claim-cid-{prior_claim}"),
                    reduction_index: case.ordinal,
                    turn_index: case.turn_index,
                    parent_reduction_key: None,
                    producer_call: None,
                    source_boundary: &prior_boundary,
                    compacted_prefix: &[Message::user("old")],
                    retained_suffix: &prior_messages,
                    checkpoint_messages: &prior_messages,
                    summary: "",
                    original_tokens: 100,
                    compacted_tokens: 20,
                },
            )
            .await
            .unwrap();
            prior_doc_id = Some(row.doc_id);
        }

        let intended_checkpoint = checkpoint(case.checkpoint);
        let (prefix, suffix) = if case.pair_closed {
            (vec![Message::user("old")], intended_checkpoint.clone())
        } else {
            open_pair_projection()
        };
        let intended_checkpoint = if case.pair_closed {
            intended_checkpoint
        } else {
            suffix.clone()
        };
        let intended_boundary = boundary(&request_doc_id, case.claim_commit);
        let request_commit_cid = format!("claim-cid-{}", case.claim_commit);
        let result = persist(
            &node,
            NewProviderContextReduction {
                agent_did: "did:key:agent",
                requester_did: None,
                session_id: "session-11",
                request_id: "request-11",
                request_doc_id: &request_doc_id,
                request_commit_cid: &request_commit_cid,
                reduction_index: case.ordinal,
                turn_index: case.turn_index,
                parent_reduction_key: parent_key.as_deref(),
                producer_call: None,
                source_boundary: &intended_boundary,
                compacted_prefix: &prefix,
                retained_suffix: &suffix,
                checkpoint_messages: &intended_checkpoint,
                summary: "",
                original_tokens: 100,
                compacted_tokens: 20,
            },
        )
        .await;

        let rust_outcome = match &result {
            Ok(row) if prior_doc_id.as_deref() == Some(row.doc_id.as_str()) => "idempotent",
            Ok(_) => "fresh",
            Err(_) if !case.pair_closed => "pair_open",
            Err(_) => "conflict",
        };
        assert_eq!(rust_outcome, case.outcome, "{} outcome drifted", case.name);

        let loaded = load_for_request(&node, &request_doc_id).await.unwrap();
        let durable_after = result.is_ok()
            && loaded.iter().any(|row| {
                row.reduction_key == key
                    && row.checkpoint_messages().unwrap() == intended_checkpoint
            });
        assert_eq!(
            durable_after, case.durable_after,
            "{} durability drifted",
            case.name
        );
        assert_eq!(
            result_is_send_permitted(rust_outcome, case.pair_closed),
            case.send_permitted,
            "{} send fence drifted",
            case.name
        );

        let cited_keys = |cites: bool| cites.then(|| vec![key.clone()]).unwrap_or_default();
        let inference_provenance = if case.inference_supported {
            provenance(
                "inference.1",
                intended_checkpoint.clone(),
                cited_keys(case.inference_cites),
            )
        } else {
            serde_json::json!({"manifest_version": 999}).to_string()
        };
        let inference = rendered_capture_cites_reduction(
            "inference.1",
            i64::try_from(case.turn_index).unwrap(),
            &inference_provenance,
            i64::try_from(case.turn_index).unwrap(),
            &key,
        );
        let title = rendered_capture_cites_reduction(
            "title.1",
            i64::try_from(case.turn_index).unwrap(),
            &provenance("title.1", intended_checkpoint, cited_keys(case.title_cites)),
            i64::try_from(case.turn_index).unwrap(),
            &key,
        );
        assert_eq!(
            inference || title,
            case.consumed,
            "{} consumption drifted",
            case.name
        );
    }

    assert!(!rendered_capture_cites_reduction(
        "malformed-scope",
        0,
        "{not-json",
        0,
        "reduction-key",
    ));
    assert!(!rendered_capture_cites_reduction(
        "inference.1",
        0,
        "{not-json",
        0,
        "reduction-key",
    ));
}

fn result_is_send_permitted(outcome: &str, pair_closed: bool) -> bool {
    matches!(outcome, "fresh" | "idempotent") && pair_closed
}

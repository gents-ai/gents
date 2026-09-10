use super::*;
use crate::identity::{AgentIdentity, KeyIdentity};
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
use gents_protocol::row::AgentRequestRow;
use serde_json::{json, Value};

async fn execute(node: &EmbeddedNode, query: &str) -> Value {
    let response = node.execute(query).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response.data.unwrap()
}

async fn persist_request(node: &EmbeddedNode, create: &AgentRequestCreate) -> AgentRequestRow {
    execute(node, &create.graphql_mutation().unwrap()).await;
    let data = execute(
        node,
        &format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            escape_graphql_string(&create.request_id),
            crate::request_admission::SIGNED_REQUEST_FIELDS,
        ),
    )
    .await;
    serde_json::from_value(data["AgentRequest"][0].clone()).unwrap()
}

#[tokio::test]
async fn generated_logical_output_obligation_cases_drive_signed_requests_and_durable_writes() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().logical_output_obligation_cases;
    assert_eq!(cases.len(), 11);
    for case in cases {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
        let automated = case["automated_root"].as_bool().unwrap();
        let mut root = AgentRequestCreate::base(
            "output-root",
            identity.did(),
            identity.did(),
            "output-behavior",
            "output-session",
            "Write the result",
            if automated {
                "scheduled"
            } else {
                "interactive"
            },
            "2026-09-05T00:00:00Z",
            if automated {
                AgentRequestAdmissionRecord::runtime_automated_trigger(
                    identity.did(),
                    "output-trigger",
                )
            } else {
                AgentRequestAdmissionRecord::local_self(identity.did())
            },
        );
        if automated {
            root.caused_by_trigger_kind = Some("event".into());
            root.caused_by_trigger_id = Some("output-trigger".into());
            root.caused_by_trigger_doc_id = Some("original-trigger-doc".into());
            root.caused_by_source_doc_id = Some("original-area-doc".into());
            root.caused_by_trigger_context =
                Some(json!({"source_fields":{"area_id":"area-a","expected_total":2}}).to_string());
        }
        crate::sign_agent_request_create(&identity, &mut root)
            .await
            .unwrap();
        let root_row = persist_request(&node, &root).await;
        let root_request = crate::watcher::AgentRequest::try_from(root_row.clone()).unwrap();
        let mut child = crate::lifecycle::queue::prepare_goal_continuation(
            &root_request,
            "output-behavior".into(),
            "output-goal",
            "Finish missing writes",
            1,
            false,
            "2026-09-05T00:00:01Z",
        )
        .unwrap();
        if !case["authenticated_child"].as_bool().unwrap() {
            child.caused_by_parent_request_doc_id = Some("wrong-physical-parent".into());
        }
        crate::sign_agent_request_create(&identity, &mut child)
            .await
            .unwrap();
        let child_row = persist_request(&node, &child).await;
        assert_eq!(
            child_row.caused_by_trigger_context,
            root_row.caused_by_trigger_context
        );
        assert_eq!(
            child_row.caused_by_source_doc_id,
            root_row.caused_by_source_doc_id
        );
        let request = crate::watcher::AgentRequest::try_from(child_row.clone()).unwrap();
        let mut outsider = AgentRequestCreate::base(
            "output-outsider",
            identity.did(),
            identity.did(),
            "output-behavior",
            "output-session",
            "Independent request",
            "interactive",
            "2026-09-05T00:00:02Z",
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        crate::sign_agent_request_create(&identity, &mut outsider)
            .await
            .unwrap();
        let outsider_row = persist_request(&node, &outsider).await;
        let mut stored = std::collections::HashSet::new();
        for write in case["writes"].as_array().unwrap() {
            let call = write["call_doc"].as_u64().unwrap();
            // Reobserving one physical call cannot create another DB document.
            if !stored.insert(call) {
                continue;
            }
            let row = match write["request_doc"].as_u64().unwrap() {
                10 => &root_row,
                20 => &child_row,
                30 => &outsider_row,
                other => panic!("unmapped request {other}"),
            };
            let expected = if !case["count_valid"].as_bool().unwrap() {
                if call == 100 {
                    2
                } else {
                    3
                }
            } else {
                case["expected_count"].as_u64().unwrap_or(2)
            };
            let mut lifecycle = crate::tool_call_lifecycle::ToolCallLifecycle::new(
                node.clone(),
                row.request_id.clone(),
                "output-session".into(),
                identity.did().into(),
                format!("output-call-{call}"),
                call as u32,
                if write["tool"] == 1 {
                    "write_result"
                } else {
                    "other_tool"
                }
                .into(),
                json!({"expected_total":expected}).to_string(),
                chrono::Utc::now() + chrono::Duration::minutes(5),
            )
            .with_request_doc_id(row.doc_id.clone());
            lifecycle.start_running().await.unwrap();
            if write["completed"].as_bool().unwrap() {
                lifecycle.complete("persisted output").await.unwrap();
            } else {
                lifecycle
                    .fail(
                        "write failed",
                        crate::tool_call_lifecycle::FailureClass::ToolReturnedError,
                    )
                    .await
                    .unwrap();
            }
        }
        let configured = vec![(
            "write_result".into(),
            crate::document_config::WriteToolOutputObligation {
                scope: if case["request_scope"].as_bool().unwrap() {
                    crate::document_config::WriteToolOutputObligationScope::Request
                } else {
                    crate::document_config::WriteToolOutputObligationScope::Trigger
                },
                minimum_writes: case["minimum"].as_u64().unwrap() as usize,
                expected_count_field: (case["expected_count"].is_number()
                    || !case["count_valid"].as_bool().unwrap())
                .then(|| "expected_total".into()),
            },
        )];
        let observed =
            match OutputObligationGate::for_request(node.clone(), &request, &configured).await {
                Err(error) => {
                    assert!(
                        !case["authenticated_child"].as_bool().unwrap(),
                        "{}: unexpected ancestry error {error:#}",
                        case["name"]
                    );
                    assert!(error.to_string().contains("parent is absent"), "{error:#}");
                    "reject"
                }
                Ok(None) => "complete",
                Ok(Some(gate)) => match gate.unmet().await {
                    Err(error) => {
                        assert!(
                            !case["count_valid"].as_bool().unwrap(),
                            "{}: unexpected gate error {error:#}",
                            case["name"]
                        );
                        assert!(error.to_string().contains("disagree"), "{error:#}");
                        "reject"
                    }
                    Ok(unmet) if unmet.is_empty() => "complete",
                    Ok(_) => "continue",
                },
            };
        assert_eq!(
            observed,
            case["expected_decision"].as_str().unwrap(),
            "{}",
            case["name"]
        );
        node.shutdown().await;
    }
}

// Malformed durable counts must fail the actual output gate.
#[tokio::test]
async fn dynamic_count_failures_are_observed_not_defaulted() {
    for (args, reason) in [
        (
            json!({"other_field": 1}).to_string(),
            "the write is missing the expected_count_field",
        ),
        // `canonical_positive_count` rejects leading zeroes, zero, and values
        // above the closed-set maximum: textual variants cannot smuggle a
        // second representation of the same count past the consistency check.
        (
            json!({"expected_total": "03"}).to_string(),
            "a leading-zero count is not canonical",
        ),
        (
            json!({"expected_total": 0}).to_string(),
            "zero is not a canonical positive count",
        ),
        (
            json!({"expected_total": -2}).to_string(),
            "a negative count is not a canonical positive integer",
        ),
    ] {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let gate = OutputObligationGate::new(
            node.clone(),
            "request-doc-count-fail-closed",
            vec![ActiveOutputObligation {
                tool_name: "write_result".to_string(),
                contract: crate::document_config::WriteToolOutputObligation {
                    scope: crate::document_config::WriteToolOutputObligationScope::Trigger,
                    minimum_writes: 1,
                    expected_count_field: Some("expected_total".to_string()),
                },
            }],
        );

        let mut lifecycle = crate::tool_call_lifecycle::ToolCallLifecycle::new(
            node.clone(),
            "request-count-fail-closed".to_string(),
            "session-count-fail-closed".to_string(),
            "did:test:count".to_string(),
            "call-count-1".to_string(),
            1,
            "write_result".to_string(),
            args,
            chrono::Utc::now() + chrono::Duration::minutes(1),
        )
        .with_request_doc_id(Some("request-doc-count-fail-closed".to_string()));
        lifecycle.start_running().await.unwrap();
        lifecycle.complete("persisted output").await.unwrap();

        let error = gate
            .unmet()
            .await
            .expect_err("a malformed durable count must fail the gate closed");
        assert!(
            error.to_string().contains("expected_count_field"),
            "{reason}: unexpected error {error:#}"
        );
        node.shutdown().await;
    }
}

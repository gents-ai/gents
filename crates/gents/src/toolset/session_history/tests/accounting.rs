use super::*;

#[tokio::test]
async fn captured_context_details_require_exact_owner_and_admission_identity() {
    let node = seeded_node().await;
    let mut context =
        load_session_inference_observation(&node, PIN_FIXED_DID, Some(PIN_FIXED_DID), "session-a")
            .await
            .unwrap()
            .latest_context
            .unwrap();
    context.accounting.estimator = "openai_chat_wire_json_bytes_div_4_v1".into();
    context.accounting.components.documents = 0;
    let body = json!({"messages":[{"role":"system","content":"private system"},{"role":"user","content":"hello"}],"tools":[{"type":"function"}]});
    context.accounting.components.messages =
        crate::provider_input::estimate_json(&body["messages"]).unwrap();
    let owner = node
        .execute(r#"{AgentRequest(filter:{request_id:{_eq:"request-a-new"}}){_docID}}"#)
        .await;
    let doc = owner.data.as_ref().unwrap()["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap();
    let mutation = format!(
        r#"mutation {{create_RenderedRequest(input: {{
            capture_key:"context-details", request_doc_id:"{}", request_id:"misleading-alias",
            node_did:"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7", session_id:"session-a", capture_scope:"inference.1",
            turn_index:{}, attempt:{}, capture_version:1, source:"openai_chat_completions",
            request_json:"{}", provenance_json:"{}"
        }}) {{_docID}}}}"#,
        escape_graphql_string(doc),
        context.accounting.turn_index,
        context.accounting.attempt,
        escape_graphql_string(&body.to_string()),
        escape_graphql_string(&json!({"admission":{"call_id":context.call_id}}).to_string())
    );
    let inserted = node.execute(&mutation).await;
    assert!(!inserted.has_errors(), "{:?}", inserted.errors);
    let details = load_session_context_details(
        &crate::config_client::ConfigAccess::local(node.clone()),
        PIN_FIXED_DID,
        Some(PIN_FIXED_DID),
        "session-a",
        &context,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(details.tool_definitions_count, 1);
    assert_eq!(details.message_count, 1);
    assert!(details.system_prompt_tokens > 0);
    for (agent, requester, session) in [
        ("did:key:foreign", Some(PIN_FIXED_DID), "session-a"),
        (
            "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
            Some("did:key:foreign"),
            "session-a",
        ),
        (PIN_FIXED_DID, Some(PIN_FIXED_DID), "session-b"),
    ] {
        assert!(load_session_context_details(
            &crate::config_client::ConfigAccess::local(node.clone()),
            agent,
            requester,
            session,
            &context
        )
        .await
        .unwrap()
        .is_none());
    }
    let mut missing_base_context = context.clone();
    missing_base_context.accounting.turn_index += 1;
    let missing_base_container = json!({
        "gents_capture_json": 1,
        "request_body": {
            "gents_lossless_json": 1,
            "kind": "object_delta",
            "base": {
                "doc_id": "missing-context-details-base",
                "field_commit_cid": "missing-cid",
                "depth": 0,
                "node_did": "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
                "requester_did": "",
                "session_id": "session-a",
                "source": "openai_chat_completions",
                "capture_scope": "inference.1"
            },
            "changed": {},
            "removed": []
        },
        "provenance_payload": {
            "gents_lossless_json": 1,
            "kind": "full",
            "value": {}
        }
    });
    let mutation = format!(
        r#"mutation {{create_RenderedRequest(input: {{
                capture_key:"context-details-missing-base", request_doc_id:"{}",
                request_id:"missing-base", node_did:"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
                session_id:"session-a", capture_scope:"inference.1",
                turn_index:{}, attempt:{}, capture_version:2,
                source:"openai_chat_completions", request_json:"{}", provenance_json:"{}"
            }}) {{_docID}}}}"#,
        escape_graphql_string(doc),
        missing_base_context.accounting.turn_index,
        missing_base_context.accounting.attempt,
        escape_graphql_string(&missing_base_container.to_string()),
        escape_graphql_string(&json!({"admission":{"call_id":context.call_id}}).to_string())
    );
    let inserted = node.execute(&mutation).await;
    assert!(!inserted.has_errors(), "{:?}", inserted.errors);
    assert!(load_session_context_details(
        &crate::config_client::ConfigAccess::local(node.clone()),
        "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
        Some(PIN_FIXED_DID),
        "session-a",
        &missing_base_context
    )
    .await
    .unwrap()
    .is_none());
    for (offset, key, request_json, provenance_json) in [
        (
            2,
            "context-details-malformed-provenance",
            json!({"messages":[]}).to_string(),
            "not-json".to_string(),
        ),
        (
            3,
            "context-details-malformed-messages",
            json!({"messages":{}}).to_string(),
            json!({"admission":{"call_id":context.call_id}}).to_string(),
        ),
    ] {
        let mut malformed_context = context.clone();
        malformed_context.accounting.turn_index += offset;
        let mutation = format!(
            r#"mutation {{create_RenderedRequest(input: {{
                    capture_key:"{key}", request_doc_id:"{}", request_id:"{key}",
                    node_did:"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7", session_id:"session-a",
                    capture_scope:"inference.1", turn_index:{}, attempt:{},
                    capture_version:1, source:"openai_chat_completions",
                    request_json:"{}", provenance_json:"{}"
                }}) {{_docID}}}}"#,
            escape_graphql_string(doc),
            malformed_context.accounting.turn_index,
            malformed_context.accounting.attempt,
            escape_graphql_string(&request_json),
            escape_graphql_string(&provenance_json),
        );
        let inserted = node.execute(&mutation).await;
        assert!(!inserted.has_errors(), "{:?}", inserted.errors);
        assert!(load_session_context_details(
            &crate::config_client::ConfigAccess::local(node.clone()),
            "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
            Some(PIN_FIXED_DID),
            "session-a",
            &malformed_context
        )
        .await
        .unwrap()
        .is_none());
    }
    context.call_id = "foreign-call".into();
    assert!(load_session_context_details(
        &crate::config_client::ConfigAccess::local(node.clone()),
        "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
        Some(PIN_FIXED_DID),
        "session-a",
        &context
    )
    .await
    .unwrap()
    .is_none());
}

#[tokio::test]
async fn ui_accounting_separates_compaction_usage_from_current_context() {
    let node = seeded_node().await;
    let before =
        load_session_inference_observation(&node, PIN_FIXED_DID, Some(PIN_FIXED_DID), "session-a")
            .await
            .unwrap();
    let response = node
        .execute(r#"{ AgentRequest(filter: {request_id: {_eq: "request-a-new"}}) {_docID} }"#)
        .await;
    let doc = response.data.as_ref().unwrap()["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap();
    for (id, kind, input, sequence) in [
        ("summarizer", "compaction", 9000, 2),
        ("reduced", "inference", 250, 3),
    ] {
        let mut accounting = before.latest_context.as_ref().unwrap().accounting.clone();
        accounting.estimated_input_tokens = input;
        let encoded = escape_graphql_string(&serde_json::to_string(&accounting).unwrap());
        let response = node.execute(&format!(r#"mutation {{ create_InferenceCall(input: {{
                call_id: "{id}", call_kind: "{kind}", backend_id: "backend-a", call_state: "completed", call_seq: {sequence}, request_id: "request-a-new",
                request_doc_id: "{}", node_did: "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7", queued_at: "2026-06-04T12:00:00Z",
                prompt_tokens: 10, completion_tokens: 5, cached_input_tokens: 0, usage_completeness: "complete", context_accounting_json: "{encoded}"
            }}) {{_docID}} }}"#, escape_graphql_string(doc))).await;
        crate::graphql::ensure_no_errors(&response, "context generation fixture").unwrap();
        let observed = load_session_inference_observation(
            &node,
            PIN_FIXED_DID,
            Some(PIN_FIXED_DID),
            "session-a",
        )
        .await
        .unwrap();
        assert_eq!(
            observed
                .latest_context
                .as_ref()
                .unwrap()
                .accounting
                .estimated_input_tokens,
            if kind == "compaction" { 650 } else { 250 }
        );
        assert!(observed.token_usage.charged_tokens > before.token_usage.charged_tokens);
    }
}

#[tokio::test]
async fn ui_accounting_uses_exact_requester_and_physical_request_ownership() {
    let node = seeded_node().await;
    for (agent, session, request) in [
        (" ", "session-a", "request-a-new"),
        (
            "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
            "",
            "request-a-new",
        ),
        (
            "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
            "session-a",
            "\t",
        ),
    ] {
        assert!(
            load_request_context_observation(&node, agent, None, session, request)
                .await
                .is_err()
        );
    }
    let baseline =
        load_session_inference_observation(&node, PIN_FIXED_DID, Some(PIN_FIXED_DID), "session-a")
            .await
            .unwrap();
    assert_eq!(baseline.token_usage.input_tokens, Some(100));
    assert_eq!(
        baseline
            .latest_context
            .as_ref()
            .unwrap()
            .accounting
            .estimated_input_tokens,
        650
    );
    assert_eq!(baseline.latest_completion_tokens, Some(20));
    assert_eq!(baseline.inference_turns, Some(1));
    let live = load_request_context_observation(
        &node,
        "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
        Some(PIN_FIXED_DID),
        "session-a",
        "request-a-new",
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(live.context, baseline.latest_context.clone().unwrap());
    assert_eq!(live.completion_tokens, Some(20));
    assert!(load_request_context_observation(
        &node,
        "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
        Some("not-an-owner"),
        "session-a",
        "request-a-new"
    )
    .await
    .unwrap()
    .is_none());

    let identities = tempfile::tempdir().unwrap();
    let foreign_identity =
        KeyIdentity::load_or_create(identities.path().join("foreign.key"), None).unwrap();
    let foreign_did = foreign_identity.did().to_owned();
    let mut foreign_request = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "foreign-requester",
        PIN_FIXED_DID,
        &foreign_did,
        "agent-a",
        "session-a",
        "foreign requester fixture",
        "interactive",
        "2026-06-04T11:59:00Z",
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&foreign_did),
    );
    crate::sign_agent_request_create(&foreign_identity, &mut foreign_request)
        .await
        .unwrap();
    let response = crate::config_client::ConfigAccess::write_local_response(
        &node,
        "test.session_history.foreign_request",
        &foreign_request.graphql_mutation().unwrap(),
    )
    .await
    .unwrap();
    crate::graphql::ensure_no_errors(&response, "foreign requester fixture").unwrap();
    let response = node
        .execute(r#"{ AgentRequest(filter: {request_id: {_eq: "foreign-requester"}}) {_docID} }"#)
        .await;
    let doc = response.data.as_ref().unwrap()["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap();
    let response = node.execute(&format!(r#"mutation {{ create_InferenceCall(input: {{
            call_id: "foreign-usage", call_seq: 1, call_kind: "inference", node_did: "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7", request_id: "request-a-new",
            request_doc_id: "{}", backend_id: "backend-a", call_state: "completed", prompt_tokens: 9000, completion_tokens: 1000, cached_input_tokens: 0, usage_completeness: "complete",
            queued_at: "2026-06-04T12:00:00Z"
        }}) {{_docID}} }}"#, escape_graphql_string(doc))).await;
    crate::graphql::ensure_no_errors(&response, "foreign usage fixture").unwrap();
    assert_eq!(
        load_session_inference_observation(&node, PIN_FIXED_DID, Some(PIN_FIXED_DID), "session-a")
            .await
            .unwrap(),
        baseline
    );
    assert_eq!(
        load_request_context_observation(
            &node,
            "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
            Some(PIN_FIXED_DID),
            "session-a",
            "request-a-new"
        )
        .await
        .unwrap()
        .unwrap(),
        live
    );
    let foreign = load_session_inference_observation(
        &node,
        "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
        Some(&foreign_did),
        "session-a",
    )
    .await
    .unwrap();
    assert_eq!(foreign.token_usage.input_tokens, Some(9000));
    assert!(foreign.latest_context.is_none());
    let absent = load_session_inference_observation(
        &node,
        "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
        Some("not-an-owner"),
        "session-a",
    )
    .await
    .unwrap();
    assert_eq!(absent.token_usage.model_calls, 0);
    assert!(absent.latest_context.is_none());
}

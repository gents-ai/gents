use super::artifact::{publish_receipt, validate_directory};
use super::validation::{capture_order, digest, rows, validate_body, validate_owner};
use super::*;
use gents::inference::rendered_request::{
    canonical_json_string, capture_key, commits, decode_capture_pair, ProvenanceManifest,
};
use gents::store::graphql::{created_doc_id, escape_graphql_string};
use serde_json::Value;

fn target(cap: u64) -> ExpectedCall {
    ExpectedCall {
        backend_id: "backend".into(),
        model: "model".into(),
        endpoint: "https://provider.invalid".into(),
        route_path: "/v1/chat/completions".into(),
        provider_family: "OpenAiCompatible".into(),
        source: "openai_chat_completions".into(),
        sampling: InferenceSampling {
            top_k: Some(17),
            seed: Some(42),
            ..Default::default()
        },
        output_token_cap: cap,
    }
}

fn request_expectation() -> ReceiptRequest {
    ReceiptRequest {
        node_did: "node".into(),
        requester_did: "requester".into(),
        agent_id: "agent".into(),
        session_id: "session".into(),
        request_id: "request".into(),
        request_doc_id: Some("physical".into()),
        lifecycle_state: "completed".into(),
        ordinary: target(1024),
        summary: Some(target(256)),
        require_summary: true,
    }
}

fn capture() -> Value {
    json!({"_docID":"capture", "request_doc_id":"physical", "request_id":"request",
            "node_did":"node", "requester_did":"requester", "agent_id":"agent",
            "session_id":"session", "capture_scope":"inference.1", "turn_index":0,
            "attempt":0, "request_commit_cid":"request-commit", "source":"openai_chat_completions",
            "model_name":"model"})
}

fn manifest() -> ProvenanceManifest {
    serde_json::from_value(json!({
        "manifest_version":4,"status":"captured_only","status_reason":"fixture",
        "capture_seam":"transport_body","capture_scope":"inference.1",
        "provider_endpoint":"https://provider.invalid","provider_family":"OpenAiCompatible",
        "provider_route_path_sha256":digest(b"/v1/chat/completions"),
        "admission":{"call_id":"call","call_seq":1},
        "assembly_trace":{"trace_version":4,"build_path":"budgeted",
            "effective_message_count":1,"reduction_keys":[],"context_accounting":null}
    }))
    .unwrap()
}

#[test]
fn openai_constructor_preserves_authored_references_and_omits_credentials() {
    let backend: InferenceBackend = serde_json::from_value(json!({
        "node_did":"node","backend_id":"backend","name":"fixture",
        "provider_kind":"OpenAiCompatible","endpoint":"https://provider.invalid/v1/",
        "auth":{"kind":"api_key","key":"do-not-export"}
    }))
    .unwrap();
    let profile = InferenceProfile {
        node_did: "node".into(),
        profile_id: "profile".into(),
        backend_id: "backend".into(),
        model_name: "model".into(),
        sampling_id: Some("sampling".into()),
        max_output_tokens: Some(1024),
        ..Default::default()
    };
    let sampling = InferenceSampling {
        node_did: "node".into(),
        sampling_id: "sampling".into(),
        top_k: Some(17),
        seed: Some(42),
        ..Default::default()
    };
    let expected = ExpectedCall::openai_chat(&backend, &profile, &sampling).unwrap();
    assert_eq!(expected.endpoint, "https://provider.invalid");
    assert_eq!(expected.route_path, "/v1/chat/completions");
    assert_eq!(expected.provider_family, "OpenAiCompatible");
    assert_eq!(expected.sampling, sampling);
    assert_eq!(expected.output_token_cap, 1024);
    assert!(!format!("{expected:?}").contains("do-not-export"));
    let mut wrong = profile.clone();
    wrong.max_output_tokens = None;
    assert!(ExpectedCall::openai_chat(&backend, &wrong, &sampling).is_err());
    wrong = profile.clone();
    wrong.backend_id = "sibling".into();
    assert!(ExpectedCall::openai_chat(&backend, &wrong, &sampling).is_err());
    wrong = profile.clone();
    wrong.sampling_id = Some("sibling".into());
    assert!(ExpectedCall::openai_chat(&backend, &wrong, &sampling).is_err());
    wrong = profile.clone();
    wrong.node_did = "sibling".into();
    assert!(ExpectedCall::openai_chat(&backend, &wrong, &sampling).is_err());
    let mut wrong = backend.clone();
    wrong.openai_wire_api = Some(OpenAiWireApi::Responses);
    assert!(ExpectedCall::openai_chat(&wrong, &profile, &sampling).is_err());
    wrong = backend.clone();
    wrong.endpoint = "https://secret@provider.invalid/v1".into();
    assert!(ExpectedCall::openai_chat(&wrong, &profile, &sampling).is_err());
    wrong = backend.clone();
    wrong.endpoint = "https://provider.invalid/v1?secret=x".into();
    assert!(ExpectedCall::openai_chat(&wrong, &profile, &sampling).is_err());
}

#[test]
fn authored_sampling_caps_and_routes_are_checked_without_exporting_content() {
    let body = json!({"model":"model","max_tokens":1024,"top_k":17,"seed":42,
            "messages":[{"role":"user","content":"private prompt"}],"headers":{"authorization":"secret"}});
    let observed = validate_body(&capture(), &body, &manifest(), &target(1024)).unwrap();
    let exported = observed.to_string();
    assert!(!exported.contains("private prompt") && !exported.contains("secret"));
    assert!(validate_body(&capture(), &body, &manifest(), &target(256)).is_err());
    for (field, value) in [
        ("top_k", json!(18)),
        ("seed", json!(43)),
        ("max_tokens", json!(1025)),
        ("temperature", json!(0.1)),
    ] {
        let mut wrong = body.clone();
        wrong[field] = value;
        assert!(
            validate_body(&capture(), &wrong, &manifest(), &target(1024)).is_err(),
            "{field}"
        );
    }
    let mut wrong_route = manifest();
    wrong_route.provider_route_path_sha256 = Some(digest(b"/other"));
    assert!(validate_body(&capture(), &body, &wrong_route, &target(1024)).is_err());
    let summary = json!({"model":"model","max_tokens":256,"top_k":17,"seed":42});
    assert!(validate_body(&capture(), &summary, &manifest(), &target(256)).is_ok());
}

#[test]
fn missing_sibling_scope_and_overflow_evidence_fail_closed() {
    assert!(rows(
        &json!({"data":{"RenderedRequest":[]}}),
        "RenderedRequest",
        4
    )
    .unwrap()
    .is_empty());
    assert!(rows(
        &json!({"data":{"RenderedRequest":[{},{}]}}),
        "RenderedRequest",
        1
    )
    .is_err());
    assert!(rows(
        &json!({"errors":[{"message":"unavailable"}],"data":{"RenderedRequest":[]}}),
        "RenderedRequest",
        4
    )
    .is_err());
    assert!(rows(&json!({"data":{}}), "RenderedRequest", 4).is_err());
    for field in [
        "request_doc_id",
        "request_id",
        "node_did",
        "requester_did",
        "agent_id",
        "session_id",
    ] {
        let mut sibling = capture();
        sibling[field] = json!("sibling");
        assert!(
            validate_owner(&sibling, &request_expectation(), "physical", true).is_err(),
            "{field}"
        );
        sibling.as_object_mut().unwrap().remove(field);
        assert!(
            validate_owner(&sibling, &request_expectation(), "physical", true).is_err(),
            "{field}"
        );
    }
    let mut absent = capture();
    absent.as_object_mut().unwrap().remove("attempt");
    assert!(capture_order(&absent).is_err());
    let mut zero = capture();
    zero["capture_scope"] = json!("inference.0");
    assert!(capture_order(&zero).is_err());
}

#[cfg(unix)]
#[test]
fn private_artifact_is_complete_exclusive_and_never_overwrites() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let bytes = br#"{"receipt_version":1}"#;
    let path = publish_receipt(dir.path(), "case-1", bytes).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(publish_receipt(dir.path(), "case-1", b"replacement").is_err());
    assert!(publish_receipt(dir.path(), "../escape", bytes).is_err());
    let linked = dir.path().join("linked");
    symlink(dir.path(), &linked).unwrap();
    assert!(validate_directory(&linked).is_err());
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o750)).unwrap();
    assert!(validate_directory(dir.path()).is_err());
}

#[tokio::test]
async fn canonical_reader_decodes_encoded_and_chunked_capture_evidence() {
    let home = gents::eval::runner::embedded::EmbeddedHome::create_temp("live-receipt")
        .await
        .unwrap();
    let node = home.node.clone();
    assert_eq!(node.node_identity_did(), Some(home.identity.did()));
    let access = ConfigAccess::local(node.clone());
    let body = json!({"model":"model","max_tokens":1024,"top_k":17,"seed":42,
            "messages":[{"role":"user","content":"private prompt"}]});
    let payload = json!({"trace_version":4,"build_path":"budgeted",
            "effective_message_count":1,"effective_messages":null,"assistant_message_ids":[],
            "threaded_tool_results":[],"reduction_keys":[],"context_accounting":null});
    let full = |value: &Value| json!({"gents_lossless_json":1,"kind":"full","value":value});
    let inline = json!({"gents_capture_json":1,"request_body":full(&body),"provenance_payload":full(&payload)}).to_string();
    assert_eq!(
        decode_capture_pair(&access, 2, &inline).await.unwrap(),
        (body.clone(), payload.clone())
    );
    async fn blocks(access: &ConfigAccess, value: &Value, prefix: &str) -> Value {
        let encoded = canonical_json_string(value).unwrap();
        let split = encoded.len() / 2;
        let mut entries = Vec::new();
        for (index, text) in [&encoded[..split], &encoded[split..]]
            .into_iter()
            .enumerate()
        {
            let query = format!(
                r#"mutation {{ create_RenderedRequestBlock(input: {{
                    content_key: "{}", payload: "{}", byte_len: {}
                }}) {{ _docID }} }}"#,
                digest(text.as_bytes()),
                escape_graphql_string(text),
                text.len()
            );
            let response = access
                .write("test.live_receipt_block", &query)
                .await
                .unwrap();
            let doc_id = created_doc_id(&response, "RenderedRequestBlock")
                .unwrap_or_else(|error| panic!("{prefix}-{index} block was not created: {error}"));
            let commit = commits::field_commit(access, &doc_id, "payload")
                .await
                .unwrap()
                .unwrap();
            entries.push(
                json!({"doc_id":doc_id,"content_key":digest(text.as_bytes()),
                    "field_commit_cid":commit.cid,"byte_len":text.len()}),
            );
        }
        json!({"gents_lossless_json":1,"kind":"manifest","blocks":entries})
    }
    let chunked = json!({"gents_capture_json":1,
            "request_body":blocks(&access, &body, "body").await,
            "provenance_payload":blocks(&access, &payload, "provenance").await})
    .to_string();
    let decoded = decode_capture_pair(&access, 3, &chunked).await.unwrap();
    assert_eq!(decoded, (body, payload));
    assert!(validate_body(&capture(), &decoded.0, &manifest(), &target(1024)).is_ok());

    let request = access
        .write(
            "test.live_receipt_request",
            r#"mutation {
            create_AgentRequest(input: {request_id: "request", node_did: "node",
                requester_did: "requester", agent_id: "agent", session_id: "session",
                lifecycle_state: "completed", created_at: "2026-10-31T00:00:00Z",
                terminalized_at: "2026-10-31T00:00:01Z"}) { _docID }
        }"#,
        )
        .await
        .unwrap();
    let physical = created_doc_id(&request, "AgentRequest").unwrap();
    let commit = commits::field_commit(&access, &physical, "lifecycle_state")
        .await
        .unwrap()
        .unwrap();
    let capture_key = capture_key("node", "session", &physical, "inference.1", 0, 0).unwrap();
    let query = format!(
        r#"mutation {{
            create_InferenceCall(input: {{call_id: "call", request_id: "request", request_doc_id: "{physical}",
                node_did: "node", agent_id: "agent", session_id: "session", call_seq: 1,
                model_name: "model", backend_id: "backend", call_kind: "inference", attempt: 1,
                call_state: "completed", started_at: "2026-10-31T00:00:00Z", ended_at: "2026-10-31T00:00:01Z"}}) {{ _docID }}
            create_RenderedRequest(input: {{capture_key: "{capture_key}", request_id: "request",
                request_doc_id: "{physical}", request_commit_cid: "{commit}", node_did: "node",
                requester_did: "requester", agent_id: "agent", session_id: "session", capture_scope: "inference.1",
                turn_index: 0, attempt: 0, capture_version: 3, model_name: "model", source: "openai_chat_completions",
                request_json: "{stored}", provenance_json: "{provenance}"}}) {{ _docID }}
        }}"#,
        commit = escape_graphql_string(&commit.cid),
        stored = escape_graphql_string(&chunked),
        provenance = escape_graphql_string(&serde_json::to_string(&manifest()).unwrap())
    );
    access
        .write("test.live_receipt_capture", &query)
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut expected = request_expectation();
    expected.request_doc_id = Some(physical.to_owned());
    expected.require_summary = false;
    let path = export_live_receipts(
        &access,
        dir.path(),
        "chunked",
        &[expected.clone()],
        ReceiptLimits::default(),
    )
    .await
    .unwrap();
    let receipt = std::fs::read_to_string(path).unwrap();
    assert!(!receipt.contains("private prompt"));
    let receipt: Value = serde_json::from_str(&receipt).unwrap();
    assert_eq!(
        receipt["requests"][0]["attempts"].as_array().unwrap().len(),
        1
    );
    assert_eq!(receipt["requests"][0]["attempts"][0]["call"]["attempt"], 1);
    assert_eq!(receipt["requests"][0]["attempts"][0]["attempt"], 0);
    assert!(export_live_receipts(
        &access,
        dir.path(),
        "small-capture",
        &[expected.clone()],
        ReceiptLimits {
            max_decoded_bytes_per_capture: 16,
            ..Default::default()
        }
    )
    .await
    .is_err());
    assert!(export_live_receipts(
        &access,
        dir.path(),
        "small-artifact",
        &[expected.clone()],
        ReceiptLimits {
            max_artifact_bytes: 16,
            ..Default::default()
        }
    )
    .await
    .is_err());
    expected.require_summary = true;
    assert!(export_live_receipts(
        &access,
        dir.path(),
        "missing-summary",
        &[expected.clone()],
        ReceiptLimits::default()
    )
    .await
    .is_err());
    expected.require_summary = false;
    expected.session_id = "sibling".into();
    assert!(export_live_receipts(
        &access,
        dir.path(),
        "sibling",
        &[expected.clone()],
        ReceiptLimits::default()
    )
    .await
    .is_err());
    expected.request_id = "absent".into();
    assert!(export_live_receipts(
        &access,
        dir.path(),
        "absent",
        &[expected],
        ReceiptLimits::default()
    )
    .await
    .is_err());
    access
        .write(
            "test.live_receipt_sibling",
            r#"mutation {
            create_AgentRequest(input: {request_id: "title-audit", node_did: "node",
                requester_did: "requester", agent_id: "agent", session_id: "session",
                lifecycle_state: "completed"}) { _docID }
        }"#,
        )
        .await
        .unwrap();
    let mut only_original = request_expectation();
    only_original.request_doc_id = Some(physical.to_owned());
    only_original.require_summary = false;
    let failure = export_live_receipts(
        &access,
        dir.path(),
        "unlisted-title-audit",
        &[only_original],
        ReceiptLimits::default(),
    )
    .await
    .unwrap_err();
    assert!(
        failure.to_string().contains("unlisted request"),
        "{failure:#}"
    );
    node.shutdown().await;
}

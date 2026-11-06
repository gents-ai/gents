use super::*;
use crate::inference::llm::message::{Text, ToolCall, ToolFunction};
use serde_json::json;
#[test]
fn failure_class_parser_accepts_only_canonical_persisted_values() {
    assert_eq!(
        failure_class_from_str("policyDenied"),
        Some(ToolFailureClass::PolicyDenied)
    );
    assert_eq!(failure_class_from_str("approvalDenied"), None);
    assert_eq!(failure_class_from_str("approval_denied"), None);
    assert_eq!(failure_class_from_str("tool_not_found"), None);
}

#[test]
fn persisted_failure_is_authoritative_over_success_looking_result_text() {
    let analysis = analyze_tool_call_with_persisted_outcome(
        "call_tool",
        r#"{"service_id":"x-data","tool_name":"query","arguments":{}}"#,
        "ordinary model-facing detail",
        ToolCallState::Failed,
        Some("serviceUnavailable"),
    );

    assert!(!analysis.tool_result_ok);
    assert_eq!(
        analysis.tool_failure_class,
        Some(ToolFailureClass::ServiceUnavailable)
    );
}

#[test]
fn completed_lifecycle_ignores_failure_shaped_result_text() {
    let analysis = analyze_tool_call_with_persisted_outcome(
        "read",
        r#"{"path":"README.md"}"#,
        "tool call failed: permission denied",
        ToolCallState::Completed,
        None,
    );

    assert!(analysis.tool_result_ok);
    assert_eq!(analysis.tool_failure_class, None);
    assert_eq!(analysis.tool_error, None);
}

#[test]
fn timeout_lifecycle_supplies_failure_class_when_absent() {
    let analysis = analyze_tool_call_with_persisted_outcome(
        "bash",
        r#"{"command":"sleep"}"#,
        "",
        ToolCallState::TimedOut,
        None,
    );

    assert!(!analysis.tool_result_ok);
    assert_eq!(
        analysis.tool_failure_class,
        Some(ToolFailureClass::External)
    );
}

#[test]
fn native_output_is_shape_only_and_does_not_override_lifecycle() {
    let result = concat!(
        "gents_exec: {\"ok\":false,\"status\":\"exit_nonzero\",",
        "\"command\":\"false\",\"exit_code\":2,\"timed_out\":false}\n",
        "stderr:\nfailed"
    );
    let analysis = analyze_tool_call_with_persisted_outcome(
        "bash",
        r#"{"command":"false"}"#,
        result,
        ToolCallState::Failed,
        Some("toolReturnedError"),
    );

    assert!(!analysis.tool_result_ok);
    let native = analysis.native_tool_output.expect("native output");
    assert!(!native.ok);
    assert_eq!(native.exit_code, Some(2));
    assert_eq!(native.status.as_deref(), Some("exit_nonzero"));
}

#[test]
fn call_tool_requires_object_arguments_without_string_coercion() {
    let analysis = analyze_tool_call_with_persisted_outcome(
        "call_tool",
        r#"{"service_id":"x-data","tool_name":"search","arguments":"{\"query\":\"gents\"}"}"#,
        "",
        ToolCallState::Failed,
        Some("argumentInvalid"),
    );

    assert_eq!(
        analysis.argument_parse_result,
        ArgumentParseResult::ArgumentsNotObject
    );
    assert_eq!(
        analysis.schema_validation_result,
        SchemaValidationResult::Failed
    );
    assert_eq!(analysis.validation_errors[0].path, "/arguments");
}

#[test]
fn extracts_raw_tool_call_json_from_persisted_assistant_message() {
    let message = Message::Assistant {
        request_id: None,
        id: None,
        content: vec![
            AssistantContent::Text(Text {
                text: "Checking.".to_string(),
            }),
            AssistantContent::ToolCall(ToolCall {
                raw_arguments: None,
                id: "internal-1".to_string(),
                call_id: Some("call-1".to_string()),
                function: ToolFunction {
                    name: "read".to_string(),
                    arguments: json!({ "path": "README.md" }),
                },
                signature: None,
                additional_params: None,
            }),
        ],
    };
    let tool_call =
        extract_raw_tool_call_json(&message, "internal-1", "read").expect("tool call json");

    assert_eq!(tool_call["id"], "internal-1");
    assert_eq!(tool_call["function"]["name"], "read");
    assert_eq!(tool_call["function"]["arguments"]["path"], "README.md");
    assert!(extract_raw_tool_call_json(&message, "unknown", "read").is_none());
    assert!(extract_raw_tool_call_json(&message, "call-1", "read").is_none());
    assert!(extract_raw_tool_call_json(&message, "internal-1", "other").is_none());
}

#[tokio::test]
async fn transcript_uses_the_callers_authenticated_access_and_preserves_denials() {
    use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
    use base64::Engine as _;
    use std::sync::{Arc, Mutex};

    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let app = Router::new()
        .route(
            "/api/v0/graphql",
            post(|State(seen): State<Arc<Mutex<Vec<String>>>>,
                  headers: HeaderMap,
                  Json(body): Json<Value>| async move {
                assert!(body["query"].as_str().unwrap().contains("AgentMessage"));
                let bearer = headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_owned();
                let mut calls = seen.lock().unwrap();
                calls.push(bearer);
                if calls.len() == 1 {
                    Json(json!({"data": {"AgentMessage": []}}))
                } else {
                    Json(json!({"errors": [{"message": "not allowed", "extensions": {"code": "PERMISSION_DENIED"}}]}))
                }
            }),
        )
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    let key_path = directory.path().join("reader.key");
    let identity = tokio::task::spawn_blocking(move || {
        crate::store::identity::KeyIdentity::load_or_create(key_path, None)
    })
    .await
    .unwrap()
    .unwrap();
    let did = crate::store::identity::NodeIdentity::did(&identity).to_owned();
    let access = crate::ConfigAccess::Graphql(
        crate::store::config_client::GraphqlEndpoint::as_principal(endpoint, &did)
            .with_client_signing_capability(
                crate::store::identity::NodeIdentity::client_signing_capability(&identity).unwrap(),
            )
            .unwrap(),
    );
    let read =
        || load_trace_transcript(&access, "session", "did:test:node", None, None, None, None);
    assert!(read().await.unwrap().is_empty());
    let error = read().await.expect_err("explicit permission denial");
    assert!(
        crate::store::config_client::TransactionFailure::config_client_error(&error)
            .is_some_and(crate::store::config_client::ConfigClientError::is_permission_denied)
    );
    server.abort();
    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 2);
    for bearer in calls.iter() {
        let payload = bearer
            .strip_prefix("Bearer ")
            .and_then(|token| token.split('.').nth(1))
            .expect("authenticated bearer");
        let claims: Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(payload)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(claims["iss"], did);
    }
}

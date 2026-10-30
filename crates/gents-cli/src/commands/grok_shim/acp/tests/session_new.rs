use super::*;

#[tokio::test]
async fn initialize_advertises_persisted_session_loading_and_gents_auth() {
    let (_staging, service) = test_service().await;
    let dispatch = service
        .handle_acp_payload(&request_payload(
            "initialize",
            json!({ "protocolVersion": 1, "clientInfo": { "name": "probe" } }),
        ))
        .await;
    assert!(dispatch.notifications.is_empty());
    let response = parse_response(dispatch.response.as_deref().expect("response line"));
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["agentCapabilities"]["loadSession"], true);
    assert_eq!(response["result"]["authMethods"][0]["id"], "gents.runtime");
    assert_eq!(
        response["result"]["authMethods"][0]["name"],
        "Gents runtime"
    );
}

#[tokio::test]
async fn local_billing_has_no_cloud_balance_or_subscription() {
    let (_staging, service) = test_service().await;
    let dispatch = service
        .handle_acp_payload(&request_payload("x.ai/billing", json!({})))
        .await;
    assert!(dispatch.notifications.is_empty());
    let response = parse_response(dispatch.response.as_deref().expect("response line"));
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(
        response["result"]["result"],
        json!({"config":null,"on_demand_enabled":false})
    );
}

#[tokio::test]
async fn workflows_list_is_routed() {
    let (_staging, service) = service_over(true).await;
    let config = &service.config;
    crate::commands::grok_shim::seed_test_agent_configuration(
        &config.node,
        &config.node_did,
        &config.agent_id,
        &config.agent_id,
        "grok-workflows-model",
        true,
    )
    .await;
    let dispatch = service
        .handle_acp_payload(&request_payload("x.ai/workflows/list", json!({})))
        .await;
    let response = parse_response(dispatch.response.as_deref().expect("response line"));
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"], json!({"result": {"workflows": []}}));
}

#[tokio::test]
async fn workflows_list_failure_is_a_shaped_internal_error() {
    let (_staging, service) = test_service().await;
    let dispatch = service
        .handle_acp_payload(&request_payload("x.ai/workflows/list", json!({})))
        .await;
    let response = parse_response(dispatch.response.as_deref().expect("response line"));
    assert_eq!(response["error"]["code"], -32603, "{response}");
    assert!(!response["error"]["message"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn authenticate_accepts_gents_runtime_and_rejects_others() {
    let (_staging, service) = test_service().await;
    let ok = service
        .handle_acp_payload(&request_payload(
            "authenticate",
            json!({ "methodId": "gents.runtime" }),
        ))
        .await;
    let response = parse_response(ok.response.as_deref().expect("response line"));
    assert_eq!(response["result"]["_meta"]["provider"], "gents");

    let rejected = service
        .handle_acp_payload(&request_payload(
            "authenticate",
            json!({ "methodId": "oauth" }),
        ))
        .await;
    let response = parse_response(rejected.response.as_deref().expect("response line"));
    assert_eq!(
        response["error"]["code"], -32602,
        "a caller-supplied auth method outside the advertised list is invalid params"
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("message")
            .contains("gents.runtime"),
        "rejection must name the only supported method"
    );
}

#[tokio::test]
async fn session_new_honors_preferred_id_and_nests_models() {
    let (_staging, service) = test_service().await;
    let dispatch = service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({
                "cwd": "/tmp",
                "mcpServers": [],
                "_meta": {
                    "sessionId": "grok-edge-preferred",
                    "modelId": "GLM-5.3-NVFP4",
                    "yoloMode": true,
                    "autoMode": false,
                },
            }),
        ))
        .await;
    assert_eq!(dispatch.notifications.len(), 1);
    let mcp_initialized = parse_response(&dispatch.notifications[0]);
    assert_eq!(
        mcp_initialized,
        json!({
            "jsonrpc": "2.0",
            "method": "_x.ai/mcp_initialized",
            "params": {
                "sessionId": "grok-edge-preferred",
                "mcpToolCount": 0,
                "elapsedMs": 0,
            },
        }),
        "session/new must emit the stock pager's exact MCP completion envelope"
    );
    let response = parse_response(dispatch.response.as_deref().expect("response line"));
    let result = &response["result"];
    assert_eq!(result["sessionId"], "grok-edge-preferred");
    assert_eq!(
        result["models"]["currentModelId"], "GLM-5.3-NVFP4",
        "session/new must return models.currentModelId, not a top-level key"
    );
    assert_eq!(
        result["models"]["availableModels"][0]["modelId"],
        "GLM-5.3-NVFP4"
    );
    assert_eq!(
        result["models"]["availableModels"][0]["meta"]["totalContextTokens"],
        262_144
    );
    assert!(result.get("availableModels").is_none());
    assert!(result.get("currentModelId").is_none());
    assert_eq!(result["_meta"]["yoloMode"], true);
    assert_eq!(result["_meta"]["autoMode"], false);
    assert_eq!(result["_meta"]["modelId"], "GLM-5.3-NVFP4");
}

#[tokio::test]
async fn session_new_derives_mode_capabilities_from_registration() {
    let (_staging, mut service) = test_service().await;
    service.register_client_identity(
        7,
        &crate::commands::grok_shim::server::Registration {
            client_type: "grok-pager".to_string(),
            mode: crate::commands::grok_shim::protocol::RegisterMode::Stdio,
            capabilities: crate::commands::grok_shim::protocol::ClientCapabilities {
                yolo_mode: true,
                auto_mode: false,
                terminal: false,
                ..Default::default()
            },
        },
    );

    let dispatch = service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({
                "cwd": "/tmp",
                "mcpServers": [],
                "_meta": { "sessionId": "grok-registered-session" },
            }),
        ))
        .await;
    let response = parse_response(dispatch.response.as_deref().expect("response line"));
    let result = &response["result"];
    assert_eq!(result["sessionId"], "grok-registered-session");
    assert_eq!(
        result["_meta"]["yoloMode"], true,
        "yoloMode must come from the registered capability, not the request"
    );
    assert_eq!(result["_meta"]["autoMode"], false);
    assert_eq!(
        result["_meta"]["clientTerminal"], false,
        "the reference pager registers terminal=false; the key must be present"
    );
}

#[tokio::test]
async fn session_new_meta_overrides_registration_but_defaults_from_it() {
    let (_staging, mut service) = test_service().await;
    service.register_client_identity(
        9,
        &crate::commands::grok_shim::server::Registration {
            client_type: "grok-pager".to_string(),
            mode: crate::commands::grok_shim::protocol::RegisterMode::Stdio,
            capabilities: crate::commands::grok_shim::protocol::ClientCapabilities {
                yolo_mode: false,
                auto_mode: true,
                terminal: true,
                ..Default::default()
            },
        },
    );
    let dispatch = service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({
                "cwd": "/tmp",
                "_meta": {
                    "sessionId": "grok-override-session",
                    "yoloMode": true,
                },
            }),
        ))
        .await;
    let response = parse_response(dispatch.response.as_deref().expect("response line"));
    let result = &response["result"];
    assert_eq!(
        result["_meta"]["yoloMode"], true,
        "the explicit request override must win"
    );
    assert_eq!(
        result["_meta"]["autoMode"], true,
        "an absent request key falls back to the registered capability"
    );
    assert_eq!(
        result["_meta"]["clientTerminal"], true,
        "clientTerminal must reflect the registered terminal capability"
    );
}

#[tokio::test]
async fn session_new_creates_exactly_one_session_and_zero_requests() {
    let (_staging, service) = test_service().await;
    service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({
                "cwd": "/tmp",
                "mcpServers": [],
                "_meta": { "sessionId": "grok-edge-docs" },
            }),
        ))
        .await;

    let node = service.config.node.clone();
    let query = r#"{
            AgentSession(filter: { session_id: { _eq: "grok-edge-docs" } }) {
                session_id agent_id node_did requester_did created_at closed_at
            }
            AgentRequest(filter: { session_id: { _eq: "grok-edge-docs" } }) { request_id }
        }"#
    .to_string();
    let response = node.execute(&query).await;
    ensure_no_errors(&response, "session document check").expect("query ok");
    let sessions = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentSession"))
        .and_then(Value::as_array)
        .expect("AgentSession array");
    assert_eq!(sessions.len(), 1, "exactly one AgentSession document");
    assert_eq!(sessions[0]["agent_id"], "did:test:grok-shim:default");
    assert_eq!(sessions[0]["node_did"], "did:test:grok-shim");
    assert_eq!(sessions[0]["requester_did"], "did:test:grok-shim");
    assert!(sessions[0]["created_at"].as_str().is_some());
    assert!(sessions[0]["closed_at"].is_null());
    let requests = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .expect("AgentRequest array");
    assert!(
        requests.is_empty(),
        "session/new must not fabricate AgentRequest rows"
    );
}

#[tokio::test]
async fn the_fixture_staging_tempdir_owns_and_cleans_up_the_node_storage() {
    let (staging, service) = test_service().await;
    let staging_root = staging.path().to_path_buf();
    let node_data = staging_root.join("node");
    assert!(
        node_data.exists(),
        "the node's storage directory must exist while the staging guard is held"
    );

    service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({
                "cwd": "/tmp",
                "mcpServers": [],
                "_meta": { "sessionId": "s-staging-cleanup" },
            }),
        ))
        .await;
    drop(service);
    drop(staging);
    assert!(
        !node_data.exists(),
        "dropping the staging guard must delete the node's storage directory"
    );
    assert!(
        !staging_root.exists(),
        "the whole staging directory must be cleaned up, not abandoned"
    );
}

#[tokio::test]
async fn session_new_writes_the_grok_record_the_resume_picker_needs() {
    let (_staging, mut service) = test_service().await;
    let home = tempfile::tempdir().expect("grok home");
    service.config.grok_home = Some(home.path().to_path_buf());
    let dispatch = service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({
                "cwd": "/Users/edjroz/Repos/gents",
                "mcpServers": [],
                "_meta": { "sessionId": "grok-resume-record" },
            }),
        ))
        .await;
    let response = parse_response(dispatch.response.as_deref().expect("response"));
    assert_eq!(response["result"]["sessionId"], "grok-resume-record");

    let path = home
        .path()
        .join("sessions/%2FUsers%2Fedjroz%2FRepos%2Fgents/grok-resume-record/summary.json");
    let record: Value = serde_json::from_slice(
        &std::fs::read(&path).expect("session/new writes the pager's session record"),
    )
    .expect("record is json");
    assert_eq!(record["info"]["id"], "grok-resume-record");
    assert_eq!(record["info"]["cwd"], "/Users/edjroz/Repos/gents");
    assert_eq!(record["chat_format_version"], 1);
}

#[tokio::test]
async fn session_new_is_idempotent_for_the_same_session_id() {
    let (_staging, service) = test_service().await;
    let params = json!({
        "cwd": "/tmp",
        "mcpServers": [],
        "_meta": { "sessionId": "grok-edge-idempotent" },
    });
    let first = service
        .handle_acp_payload(&request_payload("session/new", params.clone()))
        .await;
    let second = service
        .handle_acp_payload(&request_payload("session/new", params))
        .await;
    let first = parse_response(first.response.as_deref().expect("response"));
    let second = parse_response(second.response.as_deref().expect("response"));
    assert_eq!(first["result"]["sessionId"], second["result"]["sessionId"]);

    let node = service.config.node.clone();
    let query = r#"{
            AgentSession(filter: { session_id: { _eq: "grok-edge-idempotent" } }) { session_id }
        }"#
    .to_string();
    let response = node.execute(&query).await;
    ensure_no_errors(&response, "idempotency check").expect("query ok");
    let sessions = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentSession"))
        .and_then(Value::as_array)
        .expect("AgentSession array");
    assert_eq!(
        sessions.len(),
        1,
        "repeat session/new must not duplicate rows"
    );
}

#[tokio::test]
async fn session_new_rejects_an_existing_session_bound_to_a_different_agent() {
    let (_staging, service) = test_service().await;
    let node = service.config.node.clone();
    let seed = r#"mutation {
            create_AgentSession(input: {
                session_id: "grok-edge-foreign-agent",
                node_did: "did:test:foreign-agent",
                requester_did: "did:test:foreign-agent",
                agent_id: "did:test:grok-shim:default",
                created_at: "2026-08-31T22:46:45Z",
                closed_at: "2026-08-31T22:46:46Z"
            }) { _docID }
        }"#
    .to_string();
    let response = node.execute(&seed).await;
    ensure_no_errors(&response, "foreign session seed").expect("seed ok");

    let dispatch = service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({
                "cwd": "/tmp",
                "mcpServers": [],
                "_meta": { "sessionId": "grok-edge-foreign-agent" },
            }),
        ))
        .await;
    let response = parse_response(dispatch.response.as_deref().expect("response line"));
    assert_eq!(response["error"]["code"], -32603);
    let message = response["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("did:test:foreign-agent"),
        "the mismatch must name the session's immutable agent: {message}"
    );
    assert!(
        message.contains("node_did"),
        "the mismatch must name the immutable field: {message}"
    );

    let query = r#"{
            AgentSession(filter: { session_id: { _eq: "grok-edge-foreign-agent" } }) {
                node_did closed_at
            }
        }"#
    .to_string();
    let response = node.execute(&query).await;
    ensure_no_errors(&response, "foreign session check").expect("query ok");
    let sessions = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentSession"))
        .and_then(Value::as_array)
        .expect("AgentSession array");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["node_did"], "did:test:foreign-agent");
    assert_eq!(sessions[0]["closed_at"], "2026-08-31T22:46:46Z");
}

#[tokio::test]
async fn session_new_escapes_session_ids_in_queries() {
    let (_staging, service) = test_service().await;
    let hostile = r#"grok"\<script>-id"#;
    let dispatch = service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": hostile } }),
        ))
        .await;
    let response = parse_response(dispatch.response.as_deref().expect("response line"));
    assert_eq!(response["result"]["sessionId"], hostile);

    let node = service.config.node.clone();
    let query = format!(
        r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ session_id }} }}"#,
        escape_graphql_string(hostile)
    );
    let response = node.execute(&query).await;
    ensure_no_errors(&response, "hostile id check").expect("query ok");
    let sessions = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentSession"))
        .and_then(Value::as_array)
        .expect("AgentSession array");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["session_id"], hostile);
}

#[tokio::test]
async fn session_new_rejects_model_outside_bound_catalog() {
    let (_staging, service) = test_service().await;
    let dispatch = service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({
                "cwd": "/tmp",
                "mcpServers": [],
                "_meta": { "sessionId": "s-model", "modelId": "gpt-9" },
            }),
        ))
        .await;
    let response = parse_response(dispatch.response.as_deref().expect("response line"));
    assert_eq!(
        response["error"]["code"], -32602,
        "a requested model outside the bound catalog is invalid params"
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("message")
            .contains("bound catalog"),
        "rejection must name the bound catalog"
    );
    assert!(
        dispatch.notifications.is_empty(),
        "a rejected session/new must not clear the pager's MCP spinner"
    );
}

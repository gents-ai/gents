use std::sync::Arc;

use anyhow::Result;
use gents::defra_node::Node as EmbeddedNode;
use gents::eval::runner::embedded::EmbeddedHome;
use gents::store::graphql::escape_graphql_string;
use gents::{ensure_runtime_schemas, DocumentRuntimeOptions, Gents, NodeIdentity, ToolCeiling};
use serde_json::Value;

use crate::support::fixtures::{
    bind_default_agent_backend, test_agent, test_agent_with_identity, test_identity, test_node_for,
};
use crate::support::mock_endpoint::MockModelEndpoint;

#[tokio::test]
async fn document_runtime_uses_backend_specific_api_key_env_var() -> Result<()> {
    use std::ffi::OsString;
    use std::sync::LazyLock;

    static ENV_VAR_LOCK: LazyLock<tokio::sync::Mutex<()>> =
        LazyLock::new(|| tokio::sync::Mutex::new(()));

    struct TestEnvGuard {
        saved: Vec<(&'static str, Option<OsString>)>,
    }
    impl TestEnvGuard {
        fn new(names: &[&'static str]) -> Self {
            let saved = names
                .iter()
                .map(|name| (*name, std::env::var_os(name)))
                .collect();
            Self { saved }
        }
        fn set(&mut self, name: &'static str, value: &str) {
            unsafe {
                std::env::set_var(name, value);
            }
        }
    }
    impl Drop for TestEnvGuard {
        fn drop(&mut self) {
            for (name, value) in self.saved.iter().rev() {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    let _env_guard = ENV_VAR_LOCK.lock().await;
    let identity = Arc::new(test_identity("startup-probe-backend-auth"));
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await?,
    );
    ensure_runtime_schemas(node.as_ref()).await?;
    let mock_endpoint =
        MockModelEndpoint::start_with_required_bearer("default", Some("backend-key"))?;
    bind_default_agent_backend(
        &node,
        identity.did(),
        "backend-startup-auth",
        mock_endpoint.endpoint(),
    )
    .await;

    let escaped_backend_id = escape_graphql_string("backend-startup-auth");
    let mutation = format!(
        r#"mutation {{
            update_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                input: {{ auth: {{
                    kind: "environment",
                    variable: "GENTS_TEST_RUNTIME_BACKEND_KEY"
                }} }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let mut env = TestEnvGuard::new(&["GENTS_TEST_RUNTIME_BACKEND_KEY"]);
    env.set("GENTS_TEST_RUNTIME_BACKEND_KEY", "backend-key");
    let agent = Gents::from_default_agent_documents(
        node.clone(),
        identity.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await?;

    let default_agent = agent
        .agents()
        .iter()
        .find(|behavior| behavior.agent_did == agent.default_agent_did())
        .expect("document runtime should load the default behavior");
    assert_eq!(default_agent.completion_client_api_key()?, "backend-key");

    Ok(())
}

#[tokio::test]
async fn oneshot_refuses_admission_bound_to_another_node() -> Result<()> {
    use gents::store::local::backend_protection::BackendProtectionError;

    let admitted = EmbeddedHome::create_temp("oneshot-admitted").await?;
    let other = EmbeddedHome::create_temp("oneshot-other").await?;
    let behavior = test_agent("oneshot-wrong-node", "backend-oneshot-wrong-node", None);

    let error = gents::run_openai_oneshot_admitted(
        other.node.clone(),
        &behavior,
        "ping",
        &admitted.backend_admission(),
    )
    .await
    .expect_err("an admission bound to another node must not admit this one-shot");
    assert!(matches!(
        error.downcast_ref::<BackendProtectionError>(),
        Some(BackendProtectionError::WrongNode)
    ));
    Ok(())
}

#[tokio::test]
async fn admitted_openrouter_oneshot_completes_one_request_with_its_terminal_message() -> Result<()>
{
    use gents::BackendProviderKind;

    let db = crate::support::test_db_from_home(
        EmbeddedHome::create_temp("oneshot-admitted-openrouter").await?,
    );
    let fixture = crate::support::managed_agent_fixture(&db).await;
    let mut behavior = test_agent_with_identity(
        test_node_for(db.node_identity.clone(), fixture.binding.agent_did.clone()),
        fixture.agent.clone(),
    );
    let mock_endpoint = MockModelEndpoint::start_with_required_bearer(
        "openai/gpt-4o-mini",
        Some("openrouter-key"),
    )?;
    behavior.backend_id = Some(format!("{}:backend", fixture.binding.agent_did));
    behavior.backend_provider_kind = BackendProviderKind::OpenRouter;
    behavior.backend_endpoint = mock_endpoint.endpoint().to_string();
    behavior.backend_auth = gents::documents::BackendAuth::ApiKey {
        key: "openrouter-key".to_string(),
    };
    behavior.model_name = "openai/gpt-4o-mini".to_string();

    let result = gents::run_openai_oneshot_admitted(
        db.node.clone(),
        &behavior,
        "Say hello in one sentence.",
        &db.home.backend_admission(),
    )
    .await?;
    assert_eq!(result.response_text, "mock response");

    let projection = fixture
        .access
        .execute(
            r#"{
                AgentRequest { _docID node_did requester_did lifecycle_state terminal_output }
                AgentSession { observation }
            }"#,
        )
        .await?;
    let projection = &projection["data"];
    let request_rows = projection["AgentRequest"]
        .as_array()
        .expect("one-shot request rows");
    let [request_row] = request_rows.as_slice() else {
        panic!("one-shot creates exactly one request: {request_rows:?}");
    };
    assert_eq!(request_row["lifecycle_state"], "completed");
    let terminal: gents_protocol::output::TerminalOutput =
        serde_json::from_value(request_row["terminal_output"].clone())
            .expect("terminal output selection present on completed request");
    let gents_protocol::output::TerminalOutput::Message { message_doc_id } = terminal else {
        panic!("one-shot completion must select a published message");
    };
    let (header, message) = gents::session::load_canonical_message(
        &fixture.access,
        &message_doc_id,
        request_row["node_did"]
            .as_str()
            .expect("canonical request owner"),
        request_row["requester_did"].as_str(),
    )
    .await
    .expect("reconstruct terminal assistant message");
    assert_eq!(
        header.request_doc_id.as_deref(),
        request_row["_docID"].as_str(),
        "terminal message must belong to the completed request"
    );
    let gents_protocol::message::Message::Assistant { .. } = &message else {
        panic!("terminal output must reconstruct the assistant transcript row");
    };
    let body = gents_protocol::transcript::present_message(&message).body_markdown;
    assert!(body.contains("mock response"), "assistant body: {body:?}");
    assert_eq!(
        projection["AgentSession"][0]["observation"]["latest_request"]["lifecycle_state"],
        "completed"
    );

    let completion_request = mock_endpoint
        .recorded_requests()
        .into_iter()
        .find(|request| request.method == "POST" && request.path.ends_with("/chat/completions"))
        .expect("completion request should be recorded");
    let body: Value = serde_json::from_str(&completion_request.body)?;
    assert_eq!(body["provider"]["require_parameters"], true);
    assert_eq!(body["model"], "openai/gpt-4o-mini");
    Ok(())
}

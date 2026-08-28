use std::sync::Arc;

use anyhow::Result;
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{ensure_runtime_schemas, AgentIdentity, DocumentRuntimeOptions, Gents, ToolCeiling};
use serde_json::Value;

use crate::support::fixtures::{bind_default_behavior_backend, test_behavior, test_identity};
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
    bind_default_behavior_backend(
        node.as_ref(),
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
                input: {{ api_key_env_var: "GENTS_TEST_RUNTIME_BACKEND_KEY" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let mut env = TestEnvGuard::new(&["GENTS_TEST_RUNTIME_BACKEND_KEY"]);
    env.set("GENTS_TEST_RUNTIME_BACKEND_KEY", "backend-key");
    let agent = Gents::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await?;

    let default_behavior = agent
        .behaviors()
        .iter()
        .find(|behavior| behavior.behavior_id == agent.default_behavior_id())
        .expect("document runtime should load the default behavior");
    assert_eq!(default_behavior.completion_client_api_key()?, "backend-key");

    Ok(())
}

#[tokio::test]
async fn openrouter_oneshot_uses_provider_request_preferences() -> Result<()> {
    use gents::BackendProviderKind;

    let node = Arc::new(EmbeddedNode::builder().build().await?);
    ensure_runtime_schemas(node.as_ref()).await?;
    let mock_endpoint = MockModelEndpoint::start_with_required_bearer(
        "openai/gpt-4o-mini",
        Some("openrouter-key"),
    )?;
    let mut behavior = test_behavior("openrouter-oneshot", "backend-openrouter", None);
    behavior.backend_provider_kind = BackendProviderKind::OpenRouter;
    behavior.backend_endpoint = mock_endpoint.endpoint().to_string();
    behavior.backend_api_key = Some("openrouter-key".to_string());
    behavior.model_name = "openai/gpt-4o-mini".to_string();

    let result =
        gents::run_openai_oneshot(node.clone(), &behavior, "Say hello in one sentence.").await?;
    assert_eq!(result.response_text, "mock response");

    let projection = node
        .execute(
            r#"{
                AgentRequest { status lifecycle_state }
                AgentResponse { status content }
                AgentMessage(order: { sequence: ASC }) { role content }
                AgentConversation { status }
            }"#,
        )
        .await;
    assert!(!projection.has_errors(), "{:?}", projection.errors);
    assert_eq!(
        projection.data.as_ref().unwrap()["AgentRequest"][0]["status"],
        "completed"
    );
    assert_eq!(
        projection.data.as_ref().unwrap()["AgentRequest"][0]["lifecycle_state"],
        "completed"
    );
    assert_eq!(
        projection.data.as_ref().unwrap()["AgentResponse"][0]["status"],
        "complete"
    );
    assert_eq!(
        projection.data.as_ref().unwrap()["AgentResponse"][0]["content"],
        ""
    );
    let assistant = projection.data.as_ref().unwrap()["AgentMessage"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "assistant")
        .expect("one-shot assistant transcript row");
    assert!(assistant["content"]
        .as_str()
        .is_some_and(|content| content.contains("mock response")));
    assert_eq!(
        projection.data.as_ref().unwrap()["AgentConversation"][0]["status"],
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

use anyhow::Result;
use gents::runtime::embedded::EmbeddedHome;
use gents::{DocumentRuntimeOptions, Gents};
use gents_tools::surface::ToolCeiling;

use crate::support::fixtures::{bind_default_agent_backend, test_agent};
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
    let db = crate::support::test_db("startup-probe-backend-auth").await;
    let identity = db.node_identity.clone();
    let node = db.node.clone();
    let mock_endpoint =
        MockModelEndpoint::start_with_required_bearer("default", Some("backend-key"))?;
    bind_default_agent_backend(&db, "backend-startup-auth", mock_endpoint.endpoint()).await;

    let user = crate::support::load_managed_fixture_user(&db).await;
    crate::support::fixtures::set_backend_auth_as(
        &db.client_access(&user),
        db.node_identity.did(),
        "backend-startup-auth",
        serde_json::json!({"kind": "environment", "variable": "GENTS_TEST_RUNTIME_BACKEND_KEY"}),
    )
    .await;

    let mut env = TestEnvGuard::new(&["GENTS_TEST_RUNTIME_BACKEND_KEY"]);
    env.set("GENTS_TEST_RUNTIME_BACKEND_KEY", "backend-key");
    let agent = Gents::from_default_agent_documents(
        node.clone(),
        identity.clone(),
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await?;

    let default_agent = agent
        .agents()
        .iter()
        .find(|behavior| behavior.agent.agent_did == agent.default_agent_did())
        .expect("document runtime should load the default behavior");
    assert_eq!(
        default_agent.agent.completion_client_api_key()?,
        "backend-key"
    );

    Ok(())
}

#[tokio::test]
async fn oneshot_refuses_admission_bound_to_another_node() -> Result<()> {
    use gents_store::local::backend_protection::BackendProtectionError;

    let admitted = EmbeddedHome::create_temp("oneshot-admitted").await?;
    let other = EmbeddedHome::create_temp("oneshot-other").await?;
    let behavior = test_agent("oneshot-wrong-node", "backend-oneshot-wrong-node", None);

    let error = gents::run_openai_oneshot_admitted(
        other.node.clone(),
        &crate::support::fixtures::with_default_tools(behavior),
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

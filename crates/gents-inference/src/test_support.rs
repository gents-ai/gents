use gents_documents::test_support::{
    ensure_test_node, set_test_default_agent_as, test_agent_documents,
};
use gents_store::test_support::held_runtime_identity;

pub async fn capture_scripted_provider_request(
    request: &rig::completion::CompletionRequest,
    model: &str,
) -> Result<(), rig::completion::CompletionError> {
    use rig::http_client::HttpClientExt;
    let body = rig::providers::openai::completion::CompletionRequest::try_from((
        model.to_string(),
        request.clone(),
    ))
    .map_err(|error| rig::completion::CompletionError::ProviderError(error.to_string()))?;
    let mut body = serde_json::to_value(body)
        .map_err(|error| rig::completion::CompletionError::ProviderError(error.to_string()))?;
    body["stream"] = serde_json::Value::Bool(true);
    body["stream_options"] = serde_json::json!({ "include_usage": true });
    let inner = crate::rendered_request::transport::CountingInner::default();
    let transport =
        crate::rendered_request::transport::RenderedRequestCapturingHttpClient::new(inner.clone());
    let outbound = rig::http_client::Request::builder()
        .method("POST")
        .uri("https://scripted-provider.invalid/v1/chat/completions")
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(serde_json::to_vec(&body).map_err(
            |error| rig::completion::CompletionError::ProviderError(error.to_string()),
        )?))
        .map_err(|error| rig::completion::CompletionError::ProviderError(error.to_string()))?;
    transport
        .send_streaming(outbound)
        .await
        .map_err(|error| rig::completion::CompletionError::ProviderError(error.to_string()))?;
    assert_eq!(
        inner.send_count(),
        1,
        "capture must authorize scripted provider send"
    );
    Ok(())
}

pub async fn ensure_test_default_agent(
    node: &std::sync::Arc<defra_node::Node>,
    owner: &str,
) -> gents_protocol::agent_identity::AgentIdentityBinding {
    ensure_test_node(node, owner).await;
    let principal = gents_documents::ensure_node(node, owner).await.unwrap();
    if let Some(did) = principal.default_agent_did {
        let agent = gents_documents::load_agent(node, &did)
            .await
            .unwrap()
            .expect("fixture default agent exists");
        return gents_protocol::agent_identity::AgentIdentityBinding {
            agent_did: agent.agent_did,
            creation_nonce: agent.creation_nonce,
        };
    }
    let binding = install_test_agent(node, owner, "Default fixture agent").await;
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(owner).unwrap(),
    );
    set_test_default_agent_as(&access, owner, &binding.agent_did).await;
    binding
}

pub async fn install_test_agent(
    node: &std::sync::Arc<defra_node::Node>,
    owner: &str,
    display_name: &str,
) -> gents_protocol::agent_identity::AgentIdentityBinding {
    let node_did = node
        .node_identity_did()
        .expect("fixture node has an identity");
    let root = held_runtime_identity(node_did);
    let binding = gents_store::identity::provision_agent_identity(&root).unwrap();
    install_test_agent_binding(node, owner, display_name, &binding).await;
    binding
}

pub async fn install_test_agent_binding(
    node: &std::sync::Arc<defra_node::Node>,
    owner: &str,
    display_name: &str,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
) {
    ensure_test_node(node, owner).await;
    let actor = node
        .node_identity_did()
        .expect("fixture node has an identity");
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(actor).unwrap(),
    );
    install_test_agent_binding_with_access(owner, display_name, binding, &access).await;
}

pub async fn install_test_agent_binding_with_access(
    owner: &str,
    display_name: &str,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    access: &gents_store::config_client::ConfigAccess,
) {
    use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    let documents = test_agent_documents(owner, display_name, binding)
        .into_iter()
        .map(|(collection, value)| DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        })
        .collect();
    let plan = DesiredStateApplyPlan::new(documents).unwrap();
    access
        .transact_write("test.install_agent", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &crate::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
    let backend = format!("{}:backend", binding.agent_did);
    crate::backends::registry::set_backend_probe_status_as(access, owner, &backend, "healthy")
        .await
        .unwrap();
}

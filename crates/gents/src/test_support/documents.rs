/// Creates the fixture Node through the runtime owner's signed access.
/// `ensure_node` only loads; production Nodes are created by an authenticated client.
pub(crate) async fn ensure_test_node(node: &std::sync::Arc<defra_node::Node>, node_did: &str) {
    if crate::documents::load_node(node, node_did)
        .await
        .unwrap()
        .is_some()
    {
        return;
    }
    let access = crate::store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(node_did).unwrap(),
    );
    bootstrap_signed_test_node(&access, node_did).await;
}

pub async fn bootstrap_signed_test_node(
    access: &crate::store::config_client::ConfigAccess,
    node_did: &str,
) {
    use crate::documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};

    let node_input = serde_json::json!({"node_did": node_did});
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: crate::store::collection::Collection::Node,
        add: node_input.clone(),
        update: node_input,
    }])
    .unwrap();
    access
        .transact_write("test.signed_node", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &crate::inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
}

pub(crate) async fn set_test_default_agent_as(
    access: &crate::store::config_client::ConfigAccess,
    runtime_did: &str,
    agent_did: &str,
) {
    access
        .transact_write("test.set_default_agent", |txn| {
            Box::pin(async move {
                let (_, mut node) = crate::documents::read_desired_state_record_in_txn(
                    txn,
                    crate::store::collection::Collection::Node,
                    runtime_did,
                    runtime_did,
                )
                .await?
                .expect("fixture user must own one Node for its runtime");
                node["default_agent_did"] = serde_json::json!(agent_did);
                let plan = crate::documents::DesiredStateApplyPlan::new(vec![
                    crate::documents::DesiredStateApplyDocument {
                        collection: crate::store::collection::Collection::Node,
                        add: node.clone(),
                        update: node,
                    },
                ])?;
                crate::documents::apply_desired_state_plan(
                    txn,
                    &plan,
                    &crate::inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
}

/// Replaces one field of the fixture Node through its configuring User's
/// signed desired-state owner; Node is configuration the User signs.
pub(crate) async fn set_test_node_field_as(
    access: &crate::store::config_client::ConfigAccess,
    runtime_did: &str,
    field: &str,
    value: serde_json::Value,
) {
    access
        .transact_write("test.set_node_field", |txn| {
            let value = value.clone();
            Box::pin(async move {
                let (_, mut node) = crate::documents::read_desired_state_record_in_txn(
                    txn,
                    crate::store::collection::Collection::Node,
                    runtime_did,
                    runtime_did,
                )
                .await?
                .expect("fixture user must own one Node for its runtime");
                node[field] = value;
                let plan = crate::documents::DesiredStateApplyPlan::new(vec![
                    crate::documents::DesiredStateApplyDocument {
                        collection: crate::store::collection::Collection::Node,
                        add: node.clone(),
                        update: node,
                    },
                ])?;
                crate::documents::apply_desired_state_plan(
                    txn,
                    &plan,
                    &crate::inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
}

pub(crate) fn test_agent_documents(
    owner: &str,
    display_name: &str,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
) -> Vec<(crate::store::collection::Collection, serde_json::Value)> {
    use crate::store::collection::Collection;
    use serde_json::json;
    let agent_did = &binding.agent_did;
    let context = format!("{agent_did}:context");
    let tools = format!("{agent_did}:tools");
    let profile = format!("{agent_did}:inference");
    let backend = format!("{agent_did}:backend");
    vec![
        (
            Collection::Agent,
            json!({"node_did":owner,"agent_did":agent_did,"creation_nonce":binding.creation_nonce,"display_name":display_name,"context_id":context,"inference_profile_id":profile}),
        ),
        (
            Collection::AgentContext,
            json!({"node_did":owner,"context_id":context,"tools_id":tools}),
        ),
        (
            Collection::Tools,
            json!({"node_did":owner,"tools_id":tools}),
        ),
        (
            Collection::InferenceProfile,
            json!({"node_did":owner,"profile_id":profile,"backend_id":backend,"model_name":"test-model"}),
        ),
        (
            Collection::InferenceBackend,
            json!({"node_did":owner,"backend_id":backend,"name":"Test inference","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}}),
        ),
    ]
}

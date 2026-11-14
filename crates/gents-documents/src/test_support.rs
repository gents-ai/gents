pub mod event_cursor;

/// Admits every advertised profile: the provider rules live in the inference
/// owner, and documents-level fixtures publish documents without a provider.
pub struct AdmitAllProfiles;

#[async_trait::async_trait]
impl crate::inference_backend::AdvertisedProfilePolicy for AdmitAllProfiles {
    async fn validate_advertised_profile(
        &self,
        _txn: &gents_store::config_client::ConfigApplyTxn<'_>,
        _owner: &str,
        _backend: &crate::InferenceBackend,
        _profile: &crate::InferenceProfile,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Creates the fixture Node through the runtime owner's signed access.
/// `ensure_node` only loads; production Nodes are created by an authenticated client.
pub async fn ensure_test_node(node: &std::sync::Arc<defra_node::Node>, node_did: &str) {
    if crate::load_node(node, node_did).await.unwrap().is_some() {
        return;
    }
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(node_did).unwrap(),
    );
    bootstrap_signed_test_node(&access, node_did).await;
}

pub async fn bootstrap_signed_test_node(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
) {
    use crate::{DesiredStateApplyDocument, DesiredStateApplyPlan};

    let node_input = serde_json::json!({"node_did": node_did});
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: gents_store::collection::Collection::Node,
        add: node_input.clone(),
        update: node_input,
    }])
    .unwrap();
    access
        .transact_write("test.signed_node", |txn| {
            let plan = &plan;
            Box::pin(
                async move { crate::apply_desired_state_plan(txn, plan, &AdmitAllProfiles).await },
            )
        })
        .await
        .unwrap();
}

pub async fn set_test_default_agent_as(
    access: &gents_store::config_client::ConfigAccess,
    runtime_did: &str,
    agent_did: &str,
) {
    let selection =
        serde_json::to_value(crate::DefaultAgentSelection::new(runtime_did, agent_did)).unwrap();
    let plan = crate::DesiredStateApplyPlan::new(vec![crate::DesiredStateApplyDocument {
        collection: gents_store::collection::Collection::DefaultAgentSelection,
        add: selection.clone(),
        update: selection,
    }])
    .unwrap();
    access
        .transact_write("test.set_default_agent", |txn| {
            let plan = &plan;
            Box::pin(
                async move { crate::apply_desired_state_plan(txn, plan, &AdmitAllProfiles).await },
            )
        })
        .await
        .unwrap();
}

/// Replaces one field of the fixture Node through its configuring User's
/// signed desired-state owner; Node is configuration the User signs.
pub async fn set_test_node_field_as(
    access: &gents_store::config_client::ConfigAccess,
    runtime_did: &str,
    field: &str,
    value: serde_json::Value,
) {
    access
        .transact_write("test.set_node_field", |txn| {
            let value = value.clone();
            Box::pin(async move {
                let (_, mut node) = crate::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Node,
                    runtime_did,
                    runtime_did,
                )
                .await?
                .expect("fixture user must own one Node for its runtime");
                node[field] = value;
                let plan =
                    crate::DesiredStateApplyPlan::new(vec![crate::DesiredStateApplyDocument {
                        collection: gents_store::collection::Collection::Node,
                        add: node.clone(),
                        update: node,
                    }])?;
                crate::apply_desired_state_plan(txn, &plan, &AdmitAllProfiles).await
            })
        })
        .await
        .unwrap();
}

pub fn test_agent_documents(
    owner: &str,
    display_name: &str,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
) -> Vec<(gents_store::collection::Collection, serde_json::Value)> {
    use gents_store::collection::Collection;
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

/// Desired-state fixtures shared by the documents tests and the tests of the
/// layers above that publish documents through the apply owner.
pub mod desired_state_fixtures {
    use std::sync::Arc;

    use anyhow::{Context, Result};
    use defra_node::Node as EmbeddedNode;
    use gents_store::collection::Collection;
    use gents_store::config_client::ConfigAccess;
    use serde_json::{json, Value};

    use crate::{
        apply_desired_state_plan, config_projection, DesiredStateApplyCounts,
        DesiredStateApplyDocument, DesiredStateApplyPlan,
    };

    pub use crate::desired_state::selection::{apply_selection_changes, Change};
    pub use crate::desired_state::task_selection::apply_task_selections;

    /// A canonical document whose required fields the fixture does not exercise
    /// hold placeholders; `fields` override them.
    pub fn canonical(
        collection: Collection,
        node_did: &str,
        fields: serde_json::Value,
    ) -> serde_json::Value {
        let mut document = match collection {
            Collection::Agent => {
                json!({"creation_nonce":"nonce", "inference_profile_id":"profile"})
            }
            Collection::InferenceProfile => json!({"backend_id":"backend", "model_name":"model"}),
            Collection::Task => json!({"prompt_template":"work"}),
            _ => json!({}),
        };
        document["node_did"] = json!(node_did);
        document[collection.unique_field()] = json!("fixture");
        for (field, value) in fields.as_object().expect("fixture fields") {
            document[field] = value.clone();
        }
        config_projection(collection, Some(&document))
            .expect("canonical fixture")
            .1
            .expect("canonical fixture projection")
    }

    pub fn backend(owner: &str, id: &str) -> Value {
        json!({"node_did":owner,"backend_id":id,"name":"Local","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:8000/v1","auth":{"kind":"unauthenticated"}})
    }

    pub fn document(value: Value) -> DesiredStateApplyDocument {
        DesiredStateApplyDocument {
            collection: Collection::InferenceBackend,
            add: value.clone(),
            update: value,
        }
    }

    pub fn config(collection: Collection, value: Value) -> DesiredStateApplyDocument {
        DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        }
    }

    pub async fn register_config_schemas(node: &EmbeddedNode) -> Result<()> {
        register_config_schemas_dropping_unique_indexes(node, &[]).await
    }

    pub async fn register_config_schemas_dropping_unique_indexes(
        node: &EmbeddedNode,
        duplicate_collections: &[Collection],
    ) -> Result<()> {
        gents_store::test_support::acp_policies::install_identity_acp_policies(node).await?;
        for (name, schema) in gents_protocol::schemas::RUNTIME_COLLECTION_NAMES
            .iter()
            .zip(gents_protocol::schemas::RUNTIME_ALL)
            .chain(
                gents_protocol::schemas::ALL_COLLECTION_NAMES
                    .iter()
                    .zip(gents_protocol::schemas::ALL),
            )
        {
            // Runtime-only schemas (the backend observation has no Collection variant)
            // are installed alongside the configuration collections, as in production.
            if gents_protocol::schemas::RUNTIME_COLLECTION_NAMES.contains(name)
                || Collection::ALL
                    .iter()
                    .any(|collection| collection.graphql_type() == *name)
            {
                let mut schema = (*schema).to_owned();
                for collection in duplicate_collections {
                    if collection.graphql_type() == *name {
                        schema = schema.replace(
                            &format!(
                                "@index(fields: [\"node_did\", \"{}\"], unique: true)",
                                collection.unique_field()
                            ),
                            "",
                        );
                    }
                }
                node.add_collection(&schema).await?;
            }
        }
        Ok(())
    }

    /// Configuration writes are signed by the fixture node's own identity. Anonymous
    /// access cannot complete the relationship grants that Node and configuration
    /// publication queue, so it fails with `RelationshipActorRequired`.
    pub fn runtime_access(node: &Arc<EmbeddedNode>) -> Result<ConfigAccess> {
        let runtime = node
            .node_identity_did()
            .context("fixture node has no node identity")?;
        Ok(ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(runtime)?,
        ))
    }

    pub async fn apply(access: &ConfigAccess, docs: Vec<DesiredStateApplyDocument>) -> Result<()> {
        apply_with_counts(access, docs).await.map(|_| ())
    }

    /// Configuration writes require the owner's Node document. Publishes it for
    /// each owner the plan writes to that has none yet.
    pub async fn ensure_owner_nodes(
        access: &ConfigAccess,
        docs: &[DesiredStateApplyDocument],
    ) -> Result<()> {
        let owners: std::collections::BTreeSet<&str> = docs
            .iter()
            .filter(|document| !matches!(document.collection, Collection::Node))
            .filter_map(|document| document.add["node_did"].as_str())
            .collect();
        for owner in owners {
            let response = access
                .execute(&format!(
                    "{{ Node(filter: {{node_did: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
                    gents_store::graphql::escape_graphql_string(owner)
                ))
                .await?;
            if response["data"]["Node"]
                .as_array()
                .is_some_and(Vec::is_empty)
            {
                super::bootstrap_signed_test_node(access, owner).await;
            }
        }
        Ok(())
    }

    pub async fn apply_with_counts(
        access: &ConfigAccess,
        docs: Vec<DesiredStateApplyDocument>,
    ) -> Result<DesiredStateApplyCounts> {
        ensure_owner_nodes(access, &docs).await?;
        let plan = DesiredStateApplyPlan::new(docs)?;
        access
            .transact_write("test.desired.apply", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan, &super::AdmitAllProfiles).await
                })
            })
            .await
    }
}

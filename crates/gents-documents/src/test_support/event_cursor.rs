//! Event-source cursor fixtures shared by the documents and automation tests.
use crate::event_consumer::load_or_seed_for_source;
use crate::{apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan};
use anyhow::Result;
use gents_protocol::event_delivery::EventConsumer;
use gents_store::collection::Collection;
use gents_store::config_client::{ConfigAccess, NodePermission};
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use gents_store::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};
use serde_json::{json, Value};
use std::sync::Arc;

pub fn document(collection: Collection, mut value: Value) -> DesiredStateApplyDocument {
    value["node_did"] = json!(PIN_FIXED_DID);
    DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    }
}

pub fn source(collection: &str) -> DesiredStateApplyDocument {
    document(
        Collection::EventSource,
        json!({
            "event_source_id":"source", "source_collection":collection, "event_kind":"created"
        }),
    )
}

pub fn trigger(enabled: bool) -> DesiredStateApplyDocument {
    document(
        Collection::Trigger,
        json!({
            "trigger_id":"trigger", "task_id":"task", "enabled":enabled, "concurrency":"queued_serial",
            "source":{"kind":"event","event_source_id":"source"}
        }),
    )
}

pub async fn publish(access: &ConfigAccess, docs: Vec<DesiredStateApplyDocument>) -> Result<()> {
    let plan = DesiredStateApplyPlan::new(docs)?;
    access
        .transact_write("test.cursor.publish", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan, &super::AdmitAllProfiles)
                    .await
                    .map(|_| ())
            })
        })
        .await
}

pub struct Fixture {
    pub _home: tempfile::TempDir,
    pub node: Arc<defra_node::Node>,
    pub user: ConfigAccess,
    pub agent: ConfigAccess,
    pub agent_identity: Arc<dyn NodeIdentity>,
}

pub async fn fixture(enabled: bool) -> Result<Fixture> {
    let home = tempfile::tempdir()?;
    let root = pin_fixed_signing_identity(home.path());
    let user_binding = gents_store::identity::provision_agent_identity(&root)?;
    let agent_binding = gents_store::identity::provision_agent_identity(&root)?;
    let user_identity = gents_store::identity::resolve_agent_identity(&root, &user_binding)?;
    let agent_identity = gents_store::identity::resolve_agent_identity(&root, &agent_binding)?;
    let node = Arc::new(
        gents_store::test_support::node_builder()
            .with_node_identity_did(PIN_FIXED_DID)
            .with_node_acp_enabled()
            .build()
            .await?,
    );
    gents_store::schema::ensure_runtime_schemas(&node).await?;
    let runtime = ConfigAccess::local_as(node.clone(), identity::Did::new(PIN_FIXED_DID)?);
    for did in [user_identity.did(), agent_identity.did()] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
        ] {
            runtime
                .grant_node_permission(&identity::Did::new(did)?, permission)
                .await?;
        }
    }
    for permission in [
        NodePermission::CollectionPatch,
        NodePermission::CollectionGet,
    ] {
        runtime
            .grant_node_permission(&identity::Did::new(user_identity.did())?, permission)
            .await?;
    }
    let user = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(user_identity.did())?,
        user_identity.as_ref(),
    )?;
    let agent = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent_identity.did())?,
        agent_identity.as_ref(),
    )?;
    user.transact_write("test.cursor.node", |txn| {
        Box::pin(async move {
            let response = txn
                .execute_client_signed_create(
                    "mutation($input: NodeMutationInputArg!) {create_Node(input:$input){_docID}}",
                    &json!({"input":{"node_did":PIN_FIXED_DID}}),
                    gents_protocol::schemas::NODE,
                )
                .await?;
            let doc = gents_store::graphql::created_doc_id(&response, "Node")?;
            txn.add_actor_relationship(
                "Node",
                &doc,
                "runtime_reader",
                &identity::Did::new(PIN_FIXED_DID)?,
            )
            .await?;
            Ok::<_, anyhow::Error>(())
        })
    })
    .await?;
    user.add_collection(
        "type WorkA { label: String @immutable } type WorkB { label: String @immutable }",
    )
    .await?;
    publish(&user, vec![
        document(Collection::InferenceBackend, json!({"backend_id":"backend", "name":"Test", "provider_kind":"OpenAiCompatible", "endpoint":"http://127.0.0.1:8000/v1", "auth":{"kind":"unauthenticated"}})),
        document(Collection::InferenceProfile, json!({"profile_id":"profile", "backend_id":"backend", "model_name":"model"})),
        document(Collection::Agent, json!({"agent_did":agent_identity.did(), "creation_nonce":agent_binding.creation_nonce, "inference_profile_id":"profile"})),
        document(Collection::Task, json!({"task_id":"task", "agent_did":agent_identity.did(), "prompt_template":"work"})),
        source("WorkA"), trigger(enabled),
    ]).await?;
    Ok(Fixture {
        _home: home,
        node,
        user,
        agent,
        agent_identity,
    })
}

pub async fn create_source(access: &ConfigAccess, collection: &str, label: &str) -> Result<String> {
    gents_store::graphql::validate_collection_identifier(collection)?;
    access
        .transact_write("test.cursor.source", |txn| {
            Box::pin(async move {
                let field = format!("create_{collection}");
                let response = txn
                    .execute(&format!(
                        "mutation {{{field}(input: {{label: \"{}\"}}) {{_docID}}}}",
                        escape_graphql_string(label)
                    ))
                    .await?;
                Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(&response, collection)?)
            })
        })
        .await
}

pub fn trigger_consumer() -> EventConsumer {
    EventConsumer::Trigger {
        trigger_id: "trigger".into(),
    }
}

pub async fn cursor(access: &ConfigAccess, collection: &str) -> Result<String> {
    access
        .transact_write("test.cursor.saved", |txn| {
            Box::pin(async move {
                Ok(
                    load_or_seed_for_source(txn, PIN_FIXED_DID, &trigger_consumer(), collection)
                        .await?
                        .cursor
                        .after,
                )
            })
        })
        .await
}

use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;

pub fn signed_schema(collection: gents_store::collection::Collection) -> &'static str {
    let name = collection.graphql_type();
    gents_protocol::schemas::RUNTIME_COLLECTION_NAMES
        .iter()
        .zip(gents_protocol::schemas::RUNTIME_ALL)
        .chain(
            gents_protocol::schemas::ALL_COLLECTION_NAMES
                .iter()
                .zip(gents_protocol::schemas::ALL),
        )
        .find_map(|(schema_name, schema)| (*schema_name == name).then_some(*schema))
        .expect("fixture collection has a bundled schema")
}

pub async fn find_owned_doc(
    access: &gents_store::config_client::ConfigAccess,
    name: &str,
    field: &str,
    value: &str,
) -> String {
    let response = access
        .execute(&format!(
            r#"{{ {name}(filter: {{ {field}: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(value)
        ))
        .await
        .unwrap();
    response["data"][name][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned()
}

pub async fn update_signed_document(
    access: &gents_store::config_client::ConfigAccess,
    collection: gents_store::collection::Collection,
    doc_id: &str,
    input: serde_json::Value,
) {
    let name = collection.graphql_type();
    let doc_id = escape_graphql_string(doc_id);
    access
        .transact_write("test.update_replicated_configuration", |txn| {
            let input = &input;
            let doc_id = &doc_id;
            Box::pin(async move {
                txn.execute_client_signed_update(
                    &format!(r#"mutation($input: {name}MutationInputArg!) {{ update_{name}(docID: "{doc_id}", input: $input) {{ _docID }} }}"#),
                    &serde_json::json!({"input": input}),
                    signed_schema(collection),
                ).await
            })
        })
        .await
        .unwrap();
}

pub async fn delete_signed_document(
    access: &gents_store::config_client::ConfigAccess,
    collection: gents_store::collection::Collection,
    doc_id: &str,
) {
    access
        .transact_write("test.delete_replicated_configuration", |txn| {
            Box::pin(async move {
                txn.delete_configuration_document(collection.graphql_type(), doc_id)
                    .await
            })
        })
        .await
        .unwrap();
}

pub async fn grant_delete_permissions(
    node: &std::sync::Arc<defra_node::Node>,
    runtime: &dyn NodeIdentity,
    user: &dyn NodeIdentity,
) {
    use gents_store::config_client::{ConfigAccess, NodePermission};
    let admin = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
    let target = identity::Did::new(user.did()).unwrap();
    for permission in [
        NodePermission::DocumentDelete,
        NodePermission::DacRelationDelete,
    ] {
        admin
            .grant_node_permission(&target, permission)
            .await
            .unwrap();
    }
}

pub async fn patch_desired_document(
    access: &gents_store::config_client::ConfigAccess,
    collection: gents_store::collection::Collection,
    node_did: &str,
    id: &str,
    patch: serde_json::Value,
) {
    use gents_documents::apply_desired_state_plan;
    use gents_documents::{
        read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    access
        .transact_write("test.patch_desired_document", |txn| {
            let patch = &patch;
            Box::pin(async move {
                let (_, mut value) =
                    read_desired_state_record_in_txn(txn, collection, node_did, id)
                        .await?
                        .expect("fixture configuration exists");
                value
                    .as_object_mut()
                    .unwrap()
                    .extend(patch.as_object().unwrap().clone());
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                }])?;
                apply_desired_state_plan(txn, &plan, &crate::resolved::ProviderProfilePolicy).await
            })
        })
        .await
        .unwrap();
}

pub async fn seed_automation(
    access: &gents_store::config_client::ConfigAccess,
    documents: Vec<(gents_store::collection::Collection, serde_json::Value)>,
) {
    for (collection, input) in documents {
        let name = collection.graphql_type();
        let node_did = input["node_did"].as_str().unwrap();
        let response = access
            .execute(&format!(
                r#"{{ Node(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                escape_graphql_string(node_did)
            ))
            .await
            .unwrap();
        let node_doc = response["data"]["Node"][0]["_docID"].as_str().unwrap();
        let schema = signed_schema(collection);
        let created = access
            .transact_write("test.replicated_automation", |txn| {
                let input = &input;
                Box::pin(async move {
                    txn.execute_client_signed_create(
                        &format!("mutation($input: {name}MutationInputArg!) {{ create_{name}(input: $input) {{ _docID }} }}"),
                        &serde_json::json!({"input": input}),
                        schema,
                    ).await
                })
            })
            .await
            .expect("seed replicated automation document");
        let doc = gents_store::graphql::created_doc_id(&created, name).unwrap();
        access
            .add_resource_relationship(name, &doc, "parent", "nodes", node_doc)
            .await
            .unwrap();
    }
}

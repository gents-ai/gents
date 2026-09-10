use super::*;
use crate::config_client::ConfigAccess;
use defra_node::EmbeddedNode;
use serde_json::json;
use std::sync::Arc;

fn backend(owner: &str, id: &str) -> Value {
    json!({"agent_did":owner,"backend_id":id,"name":"Local","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:8000/v1","auth":{"kind":"unauthenticated"}})
}
fn document(value: Value) -> DesiredStateApplyDocument {
    DesiredStateApplyDocument {
        collection: Collection::InferenceBackend,
        add: value.clone(),
        update: value,
    }
}
async fn register_config_schemas(node: &EmbeddedNode) -> Result<()> {
    register_config_schemas_with_legacy_duplicates(node, &[]).await
}

// Simulate a pre-index/replicated malformed identity set in duplicate rejection
// tests. Ordinary publication tests retain the production unique indexes.
async fn register_config_schemas_with_legacy_duplicates(
    node: &EmbeddedNode,
    duplicate_collections: &[Collection],
) -> Result<()> {
    for (name, schema) in gents_protocol::schemas::RUNTIME_COLLECTION_NAMES
        .iter()
        .zip(gents_protocol::schemas::RUNTIME_ALL)
        .chain(
            gents_protocol::schemas::ALL_COLLECTION_NAMES
                .iter()
                .zip(gents_protocol::schemas::ALL),
        )
    {
        if Collection::ALL
            .iter()
            .any(|collection| collection.graphql_type() == *name)
        {
            let mut schema = (*schema).to_owned();
            for collection in duplicate_collections {
                if collection.graphql_type() == *name {
                    schema = schema.replace(
                        &format!(
                            "@index(fields: [\"agent_did\", \"{}\"], unique: true)",
                            collection.unique_field()
                        ),
                        "",
                    );
                }
            }
            node.add_schema(&schema).await?;
        }
    }
    Ok(())
}

async fn apply(access: &ConfigAccess, docs: Vec<DesiredStateApplyDocument>) -> Result<()> {
    let plan = DesiredStateApplyPlan::new(docs)?;
    access
        .transact("test.desired.apply", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
}

#[test]
fn every_canonical_collection_uses_derived_owner_and_identity_fields() {
    for collection in Collection::ALL {
        let (fields, _) = config_projection(collection, None).unwrap();
        assert!(fields.contains(&"agent_did"), "{collection:?}");
        assert!(
            fields.contains(&collection.unique_field()),
            "{collection:?}"
        );
        assert!(!fields.contains(&"_docID"), "{collection:?}");
    }
}

#[test]
fn plans_reject_unknown_fields_sparse_updates_and_scope_changes() {
    let mut value = backend("did:key:owner", "local");
    value["probe_status"] = "healthy".into();
    assert!(DesiredStateApplyPlan::new(vec![document(value)]).is_err());
    let mut doc = document(backend("did:key:owner", "local"));
    doc.update = json!({"enabled":false});
    assert!(DesiredStateApplyPlan::new(vec![doc]).is_err());
    let mut doc = document(backend("did:key:owner", "local"));
    doc.update["agent_did"] = "did:key:other".into();
    assert!(DesiredStateApplyPlan::new(vec![doc]).is_err());
    assert!(DesiredStateApplyPlan::new(vec![
        document(backend("did:key:a", "same")),
        document(backend("did:key:b", "same"))
    ])
    .is_ok());
    assert!(DesiredStateApplyPlan::new(vec![
        document(backend("did:key:a", "same")),
        document(backend("did:key:a", "same"))
    ])
    .is_err());
}

#[test]
fn commitments_do_not_reinterpret_authored_strings_as_json() {
    assert_ne!(
        desired_state_document_digest(&json!({"value":["{\"a\":1}"]})).unwrap(),
        desired_state_document_digest(&json!({"value":[{"a":1}]})).unwrap()
    );
}

#[test]
fn task_projection_excludes_runtime_updated_at_and_normalizes_defaults() {
    let desired = json!({
        "agent_did": "did:key:owner",
        "task_id": "run-once",
        "behavior_id": "operator",
        "prompt_template": "Run once"
    });
    let mut observed = desired.clone();
    observed["updated_at"] = "2026-09-10T18:16:43.843636Z".into();
    observed["enabled"] = true.into();
    observed["hooks"] = Value::Null;
    observed["tags"] = Value::Null;

    let desired = config_projection(Collection::Task, Some(&desired))
        .unwrap()
        .1
        .unwrap();
    let observed = config_projection(Collection::Task, Some(&observed))
        .unwrap()
        .1
        .unwrap();

    assert_eq!(desired, observed);
    assert_eq!(desired["enabled"], true);
    assert!(desired.get("updated_at").is_none());
}

#[tokio::test]
async fn replacement_resets_defaults_and_preserves_backend_observations() -> Result<()> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    register_config_schemas(&node).await?;
    let access = ConfigAccess::Local(node.clone());
    let mut initial = backend("did:key:owner", "local");
    initial["max_concurrent"] = 7.into();
    initial["enabled"] = false.into();
    initial["tags"] = json!(["old"]);
    apply(
        &access,
        vec![
            document(initial),
            document(backend("did:key:other", "local")),
        ],
    )
    .await?;
    access.write("test.desired.observe", r#"mutation { update_InferenceBackend(filter:{agent_did:{_eq:"did:key:owner"},backend_id:{_eq:"local"}},input:{probe_status:"healthy",catalogs:[{agent_did:null,observed_at:"2026-01-01T00:00:00Z",models:null}]}){_docID} }"#).await?;
    let before = node
        .execute("{InferenceBackend{_docID agent_did catalogs probe_status}}")
        .await;
    assert!(!before.has_errors());
    let before = before.data.unwrap();
    apply(&access, vec![document(backend("did:key:owner", "local"))]).await?;
    let after = node.execute("{InferenceBackend{_docID agent_did max_concurrent enabled tags catalogs probe_status}}").await;
    assert!(!after.has_errors());
    let after = after.data.unwrap();
    for row in after["InferenceBackend"].as_array().unwrap() {
        let prior = before["InferenceBackend"]
            .as_array()
            .unwrap()
            .iter()
            .find(|prior| prior["agent_did"] == row["agent_did"])
            .unwrap();
        assert_eq!(row["_docID"], prior["_docID"]);
        assert_eq!(row["catalogs"], prior["catalogs"]);
        assert_eq!(row["probe_status"], prior["probe_status"]);
        assert_eq!(row["enabled"], true);
        assert!(row["max_concurrent"].is_null());
        assert!(row["tags"].is_null());
    }
    Ok(())
}

#[tokio::test]
async fn ambiguous_scope_aborts_transaction_including_prior_staged_writes() -> Result<()> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    register_config_schemas_with_legacy_duplicates(&node, &[Collection::InferenceBackend]).await?;
    let access = ConfigAccess::Local(node.clone());
    apply(
        &access,
        vec![document(backend("did:key:owner", "z_duplicate"))],
    )
    .await?;
    let mut duplicate = backend("did:key:owner", "z_duplicate");
    duplicate["name"] = "Distinct document".into();
    access
        .write(
            "test.desired.duplicate",
            &format!(
                "mutation {{ create_InferenceBackend(input:{}){{_docID}} }}",
                gents_protocol::graphql::graphql_input_literal(&duplicate)?
            ),
        )
        .await?;
    assert!(apply(
        &access,
        vec![
            document(backend("did:key:owner", "a_staged")),
            document(backend("did:key:owner", "z_duplicate"))
        ]
    )
    .await
    .is_err());
    let response = node.execute("{InferenceBackend{backend_id name}}").await;
    assert!(!response.has_errors());
    let rows = response.data.unwrap();
    let rows = rows["InferenceBackend"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row["backend_id"] == "z_duplicate"));
    assert!(rows.iter().any(|row| row["name"] == "Distinct document"));
    Ok(())
}

fn config(collection: Collection, value: Value) -> DesiredStateApplyDocument {
    DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    }
}

fn cyclic_configuration(owner: &str) -> Vec<DesiredStateApplyDocument> {
    vec![
        document(backend(owner, "backend")),
        config(
            Collection::InferenceProfile,
            json!({"agent_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model"}),
        ),
        config(
            Collection::AgentBehavior,
            json!({"agent_did":owner,"behavior_id":"behavior","inference_profile_id":"profile","context_id":"context"}),
        ),
        config(
            Collection::AgentContext,
            json!({"agent_did":owner,"context_id":"context","tools_id":"tools"}),
        ),
        config(
            Collection::Tools,
            json!({"agent_did":owner,"tools_id":"tools","subagents":{"target_ids":["target"]}}),
        ),
        config(
            Collection::SubagentTarget,
            json!({"agent_did":owner,"target_id":"target","target_agent_did":owner,"behavior_id":"behavior","name":"worker"}),
        ),
    ]
}

#[tokio::test]
async fn retained_inbound_references_and_cycles_share_atomic_publication() -> Result<()> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    register_config_schemas(&node).await?;
    let access = ConfigAccess::Local(node.clone());
    let owner = "did:key:owner";
    let mut documents = cyclic_configuration(owner);
    documents.extend(cyclic_configuration("did:key:foreign"));
    apply(&access, documents).await?;
    access.write("test.observe", r#"mutation { update_InferenceBackend(filter:{agent_did:{_eq:"did:key:owner"}},input:{probe_status:"healthy"}){_docID} }"#).await?;
    let before = node.execute("{InferenceBackend{_docID agent_did name probe_status} AgentContext{_docID agent_did context_id}}").await.data;

    // The unchanged behavior still references context. The foreign same-label
    // context must not satisfy that link, and a staged backend edit rolls back.
    let mut edited = backend(owner, "backend");
    edited["name"] = "must roll back".into();
    let plan = DesiredStateApplyPlan::new(vec![document(edited)])?.with_removals(vec![(
        Collection::AgentContext,
        owner.into(),
        "context".into(),
    )])?;
    let error = access
        .transact("test.reject.removal", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("references missing AgentContext"));
    let after = node.execute("{InferenceBackend{_docID agent_did name probe_status} AgentContext{_docID agent_did context_id}}").await.data;
    assert_eq!(
        before, after,
        "failed publication preserves desired fields and observations"
    );

    // Removing the complete same-owner cycle is legal, regardless of order.
    let removals = cyclic_configuration(owner)
        .iter()
        .map(|doc| {
            (
                doc.collection,
                owner.to_owned(),
                doc.add[doc.collection.unique_field()]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            )
        })
        .collect();
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(removals)?;
    access
        .transact("test.remove.cycle", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await?;
    let remaining = node
        .execute("{AgentBehavior{agent_did} InferenceBackend{agent_did}}")
        .await;
    assert!(!remaining.has_errors());
    for collection in ["AgentBehavior", "InferenceBackend"] {
        let rows = remaining.data.as_ref().unwrap()[collection]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["agent_did"], "did:key:foreign");
    }
    Ok(())
}

#[tokio::test]
async fn replacement_checks_actual_update_and_unchanged_duplicate_rows() -> Result<()> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    register_config_schemas_with_legacy_duplicates(&node, &[Collection::AgentContext]).await?;
    let access = ConfigAccess::Local(node.clone());
    let owner = "did:key:owner";
    apply(&access, cyclic_configuration(owner)).await?;
    let mut replacement = cyclic_configuration(owner)
        .into_iter()
        .find(|doc| doc.collection == Collection::AgentBehavior)
        .unwrap();
    replacement.update["context_id"] = "absent".into();
    assert!(apply(&access, vec![replacement]).await.is_err());
    let row = node.execute("{AgentBehavior{context_id}}").await;
    assert_eq!(
        row.data.unwrap()["AgentBehavior"][0]["context_id"],
        "context"
    );

    access.write("test.duplicate.context", r#"mutation { create_AgentContext(input:{agent_did:"did:key:owner",context_id:"context",description:"duplicate"}){_docID} }"#).await?;
    let error = apply(
        &access,
        vec![document(backend(owner, "unrelated-new-backend"))],
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("multiple live AgentContext"));
    let rows = node
        .execute("{InferenceBackend{backend_id}}")
        .await
        .data
        .unwrap();
    assert_eq!(rows["InferenceBackend"].as_array().unwrap().len(), 1);
    Ok(())
}

#[test]
fn removals_require_unique_scoped_identities() {
    assert!(DesiredStateApplyPlan::new(Vec::new())
        .unwrap()
        .with_removals(vec![(Collection::Tools, "".into(), "tools".into())])
        .is_err());
    assert!(
        DesiredStateApplyPlan::new(vec![document(backend("owner", "backend"))])
            .unwrap()
            .with_removals(vec![(
                Collection::InferenceBackend,
                "owner".into(),
                "backend".into()
            )])
            .is_err()
    );
    assert!(DesiredStateApplyPlan::new(Vec::new())
        .unwrap()
        .with_removals(vec![
            (Collection::Tools, "owner".into(), "tools".into()),
            (Collection::Tools, "owner".into(), "tools".into())
        ])
        .is_err());
}

#[tokio::test]
async fn read_only_preflight_uses_actual_replacements_and_retained_inbound_links() -> Result<()> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    register_config_schemas(&node).await?;
    let access = ConfigAccess::Local(node.clone());
    let owner = "did:key:owner";
    apply(&access, cyclic_configuration(owner)).await?;
    let mut replacement = cyclic_configuration(owner)
        .into_iter()
        .find(|document| document.collection == Collection::AgentBehavior)
        .unwrap();
    replacement.update["context_id"] = "absent".into();
    let plan = DesiredStateApplyPlan::new(vec![replacement])?;
    assert!(access
        .transact("test.preview.replacement", |txn| {
            let plan = &plan;
            Box::pin(async move { validate_desired_state_plan(txn, plan).await })
        })
        .await
        .is_err());
    let removal = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        Collection::AgentContext,
        owner.into(),
        "context".into(),
    )])?;
    assert!(access
        .transact("test.preview.removal", |txn| {
            let plan = &removal;
            Box::pin(async move { validate_desired_state_plan(txn, plan).await })
        })
        .await
        .is_err());
    let addition = DesiredStateApplyPlan::new(vec![document(backend(owner, "preview-only"))])?;
    access
        .transact("test.preview.addition", |txn| {
            let plan = &addition;
            Box::pin(async move { validate_desired_state_plan(txn, plan).await })
        })
        .await?;
    let state = node.execute("{ AgentBehavior { context_id } AgentContext { context_id } InferenceBackend { backend_id } }").await;
    assert!(!state.has_errors(), "{:?}", state.errors);
    let data = state.data.unwrap();
    assert_eq!(data["AgentBehavior"][0]["context_id"], "context");
    assert_eq!(data["AgentContext"].as_array().unwrap().len(), 1);
    assert!(!data["InferenceBackend"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["backend_id"] == "preview-only"));
    Ok(())
}

#[tokio::test]
async fn canonical_replacement_preserves_revocation_and_attested_creation() {
    use crate::config_client::{apply_desired_state_plan, ConfigAccess, DesiredStateApplyPlan};
    use std::sync::Arc;
    let node = Arc::new(
        crate::defra_node::EmbeddedNode::builder()
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let access = ConfigAccess::Local(node.clone());
    let original: crate::document_config::PackConfig = serde_json::from_value(serde_json::json!({
        "agent_principal":{"agent_did":"binding-owner"},
        "chain_key_bindings":[{"agent_did":"binding-owner","binding_id":"signing","address":"0x1111111111111111111111111111111111111111","key_backend":"keyring","attestation":"signed-owner-binding","created_at":"2026-01-01T00:00:00Z","revoked_at":"2026-01-02T00:00:00Z"}]
    })).unwrap();
    let plan = DesiredStateApplyPlan::from_pack_config(&original).unwrap();
    access
        .transact("seed_revoked_binding", |txn| {
            let plan = plan.clone();
            Box::pin(async move { apply_desired_state_plan(txn, &plan).await.map(|_| ()) })
        })
        .await
        .unwrap();
    for proposed in [
        None,
        Some(serde_json::Value::Null),
        Some(serde_json::json!("")),
        Some(serde_json::json!(" \t ")),
    ] {
        let mut raw = serde_json::to_value(&original).unwrap();
        let binding = raw["chain_key_bindings"][0].as_object_mut().unwrap();
        binding.insert(
            "created_at".into(),
            serde_json::json!("2026-09-01T00:00:00Z"),
        );
        binding.insert("tags".into(), serde_json::json!(["updated"]));
        if let Some(proposed) = proposed {
            binding.insert("revoked_at".into(), proposed);
        } else {
            binding.remove("revoked_at");
        }
        let config = serde_json::from_value(raw).unwrap();
        let plan = DesiredStateApplyPlan::from_pack_config(&config).unwrap();
        access
            .transact("stale_binding_update", |txn| {
                let plan = plan.clone();
                Box::pin(async move { apply_desired_state_plan(txn, &plan).await.map(|_| ()) })
            })
            .await
            .unwrap();
        let read=node.execute(r#"{ChainKeyBinding(filter:{agent_did:{_eq:"binding-owner"},binding_id:{_eq:"signing"}}){created_at revoked_at tags}}"#).await;
        assert!(!read.has_errors(), "{:?}", read.errors);
        let rows = read.data.unwrap()["ChainKeyBinding"].clone();
        assert_eq!(
            rows,
            serde_json::json!([{"created_at":"2026-01-01T00:00:00Z","revoked_at":"2026-01-02T00:00:00Z","tags":["updated"]}])
        );
    }
    node.shutdown().await;
}

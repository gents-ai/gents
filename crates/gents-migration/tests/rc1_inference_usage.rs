use gents_migration::ensure_migrations;
use gents_protocol::schemas;

mod common;

const USAGE_FIELDS: &[&str] = &[
    "session_id",
    "model_name",
    "profile_id",
    "cache_creation_input_tokens",
    "reasoning_output_tokens",
    "provider_total_tokens",
    "usage_completeness",
];

#[tokio::test]
async fn rc1_inference_usage_upgrade_preserves_rows_and_field_ids() {
    let directory = tempfile::tempdir().unwrap();
    let owner = common::node_identity();
    let node = std::sync::Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(directory.path())
            .with_node_identity_did(owner.clone())
            .build()
            .await
            .unwrap(),
    );
    let previous = schemas::INFERENCE_CALL
        .lines()
        .filter(|line| {
            !USAGE_FIELDS
                .iter()
                .any(|name| line.trim_start().starts_with(&format!("{name}:")))
        })
        .collect::<Vec<_>>()
        .join("\n");
    node.add_collection(&previous).await.unwrap();
    let before = node.get_collection("InferenceCall").unwrap().unwrap();
    assert_eq!(
        before.version_id,
        "bafyreibbbdmb7au32traw5ufp6rqytb3jv63bmjflykfhize2u6bwvqiwe"
    );
    let created = node
        .execute(
            r#"mutation {
                add_InferenceCall(input: {
                    call_id: "retained", node_did: "owner", request_id: "request",
                    request_doc_id: "request-document", prompt_tokens: 100,
                    completion_tokens: 30, cached_input_tokens: 20
                }) {_docID}
            }"#,
        )
        .await;
    assert!(!created.has_errors(), "{created:?}");
    let document = created.data.unwrap()["add_InferenceCall"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    ensure_migrations(node.as_ref()).await.unwrap();
    let after = node.get_collection("InferenceCall").unwrap().unwrap();
    assert_eq!(before.collection_id, after.collection_id);
    let target = after.version_id.clone();
    for field in before.fields {
        let retained = after.fields.iter().find(|f| f.name == field.name).unwrap();
        assert_eq!(retained.id, field.id, "{} moved", field.name);
        assert_eq!(retained.kind, field.kind, "{} changed kind", field.name);
        assert_eq!(retained.immutable, field.immutable);
    }
    for name in ["session_id", "model_name", "profile_id"] {
        assert!(
            after
                .fields
                .iter()
                .find(|f| f.name == name)
                .unwrap()
                .immutable
        );
    }
    for name in ["session_id", "model_name", "profile_id"] {
        assert_eq!(
            after
                .indexes
                .iter()
                .filter(|index| {
                    !index.resolved_unique()
                        && index.fields.len() == 1
                        && index.fields[0].name == name
                        && !index.fields[0].descending
                })
                .count(),
            1,
            "missing or duplicate index for {name}",
        );
    }
    let read = node
        .execute(
            r#"{ InferenceCall {
                _docID call_id request_doc_id prompt_tokens completion_tokens cached_input_tokens
                session_id model_name profile_id cache_creation_input_tokens
                reasoning_output_tokens provider_total_tokens usage_completeness
            }}"#,
        )
        .await;
    assert!(!read.has_errors(), "{read:?}");
    let data = read.data.unwrap();
    let rows = data["InferenceCall"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let retained = &rows[0];
    assert_eq!(retained["_docID"], document);
    assert_eq!(retained["request_doc_id"], "request-document");
    assert_eq!(retained["prompt_tokens"], 100);
    assert_eq!(retained["completion_tokens"], 30);
    assert_eq!(retained["cached_input_tokens"], 20);
    for name in USAGE_FIELDS {
        assert!(retained[name].is_null(), "invented historical {name}");
    }
    assert_eq!(
        ensure_migrations(node.as_ref())
            .await
            .unwrap()
            .steps_applied,
        0
    );
    let created = node.execute(r#"mutation {
        add_InferenceCall(input: {
            call_id: "current", node_did: "owner", request_doc_id: "current-request",
            session_id: "current-session", model_name: "current-model", profile_id: "current-profile",
            cache_creation_input_tokens: 7, reasoning_output_tokens: 9,
            provider_total_tokens: 130, usage_completeness: "complete"
        }) {_docID}
    }"#).await;
    assert!(!created.has_errors(), "{created:?}");
    for field in ["session_id", "model_name", "profile_id"] {
        let changed = node.execute(&format!(r#"mutation {{
            update_InferenceCall(filter: {{call_id: {{_eq: "current"}}}}, input: {{{field}: "changed"}}) {{_docID}}
        }}"#)).await;
        assert!(changed.has_errors(), "{field} became mutable");
    }
    node.shutdown().await;
    drop(node);
    let reopened = defra_node::EmbeddedNode::builder()
        .data_path(directory.path())
        .with_node_identity_did(owner)
        .build()
        .await
        .unwrap();
    assert_eq!(ensure_migrations(&reopened).await.unwrap().steps_applied, 0);
    assert_eq!(
        reopened
            .get_collection("InferenceCall")
            .unwrap()
            .unwrap()
            .version_id,
        target
    );
    let read = reopened
        .execute(
            "{ InferenceCall { _docID call_id prompt_tokens session_id provider_total_tokens } }",
        )
        .await;
    assert!(!read.has_errors(), "{read:?}");
    let data = read.data.unwrap();
    let rows = data["InferenceCall"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let retained = rows
        .iter()
        .find(|row| row["call_id"] == "retained")
        .unwrap();
    assert_eq!(retained["_docID"], document);
    assert_eq!(retained["prompt_tokens"], 100);
    assert!(retained["session_id"].is_null());
    let current = rows.iter().find(|row| row["call_id"] == "current").unwrap();
    assert_eq!(current["session_id"], "current-session");
    assert_eq!(current["provider_total_tokens"], 130);
    reopened.shutdown().await;
}

#[tokio::test]
async fn fresh_inference_usage_keeps_its_canonical_root_and_single_indexes() {
    let node = common::fresh_node().await;
    node.add_collection(schemas::INFERENCE_CALL).await.unwrap();
    let before = node.get_collection("InferenceCall").unwrap().unwrap();
    ensure_migrations(node.as_ref()).await.unwrap();
    let after = node.get_collection("InferenceCall").unwrap().unwrap();
    assert_eq!(after.version_id, before.version_id);
    for name in ["session_id", "model_name", "profile_id"] {
        assert_eq!(
            after
                .indexes
                .iter()
                .filter(|index| {
                    !index.resolved_unique()
                        && index.fields.len() == 1
                        && index.fields[0].name == name
                        && !index.fields[0].descending
                })
                .count(),
            1
        );
    }
    node.shutdown().await;
}

#[tokio::test]
async fn incompatible_inference_usage_stays_refused_without_schema_changes() {
    let node = common::fresh_node().await;
    let incompatible =
        schemas::INFERENCE_CALL.replace("prompt_tokens: Int", "prompt_tokens: String");
    node.add_collection(&incompatible).await.unwrap();
    let before = node.get_collection("InferenceCall").unwrap().unwrap();
    let error = ensure_migrations(node.as_ref()).await.unwrap_err();
    assert!(matches!(
        error,
        gents_migration::Error::UnknownLineage { .. }
    ));
    let versions = node.get_all_collection_versions().await.unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].version_id, before.version_id);
    node.shutdown().await;
}

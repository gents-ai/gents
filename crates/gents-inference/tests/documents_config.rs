use gents_documents::*;

#[tokio::test]
async fn reference_loader_isolates_foreign_malformed_backends_and_rejects_owned_corruption() {
    use gents_store::identity::NodeIdentity;

    let signed_home = gents_documents::test_support::SignedHome::start(
        "reference-owner-isolation",
        gents_store::node_extension::Extensions::default(),
    )
    .await;
    let _binding = gents_inference::test_support::default_agent(&signed_home).await;
    let (db, _user, access) = signed_home.into_parts();
    let owner = db.node_identity.did();
    let foreign = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("reference-foreign-node")
    })
    .await
    .unwrap();
    assert_ne!(owner, foreign.did());
    gents_documents::test_support::bootstrap_signed_test_node(&access, foreign.did()).await;
    for (node_did, id, auth) in [
        (owner, "good", "unauthenticated"),
        (foreign.did(), "bad", "invalid-auth"),
    ] {
        let input = serde_json::json!({
            "node_did": node_did, "backend_id": id, "name": "fixture",
            "provider_kind": "OpenAiCompatible", "endpoint": "http://localhost:8000/v1",
            "auth_kind": auth, "enabled": true,
            "creation_nonce": uuid::Uuid::new_v4().to_string(),
        });
        access.transact_write("test.reference_fixture", |txn| {
            let input = &input;
            Box::pin(async move {
                let response = txn.execute_client_signed_create(
                    "mutation($input: InferenceBackendMetadataMutationInputArg!) { create_InferenceBackendMetadata(input: $input) { _docID } }",
                    &serde_json::json!({"input": input}),
                    gents_protocol::schemas::INFERENCE_BACKEND_METADATA,
                ).await?;
                let doc = gents_store::graphql::created_doc_id(&response, "InferenceBackendMetadata")?;
                gents_documents::publish_node_child_in_txn(
                    txn, "InferenceBackendMetadata", &doc, node_did,
                ).await?;
                Ok::<_, anyhow::Error>(())
            })
        }).await.unwrap();
    }
    access
        .transact_write("test.reference_owner_isolation", |txn| {
            Box::pin(async move {
                let refs = ConfigReferences::load_in_txn(txn, owner).await?;
                refs.validate()?;
                assert!(refs.documents().any(|((collection, id), _)| *collection
                    == gents_store::collection::Collection::InferenceBackend
                    && id == "good"));
                assert!(!refs.documents().any(|((_, id), _)| id == "bad"));
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    let response = access.execute(&format!(
        r#"{{ InferenceBackendMetadata(filter: {{node_did: {{_eq: "{}"}}, backend_id: {{_eq: "good"}}}}, limit: 2) {{ _docID }} }}"#,
        gents_store::graphql::escape_graphql_string(owner),
    )).await.unwrap();
    let [row] = response["data"]["InferenceBackendMetadata"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("owned backend must be unique");
    };
    let doc = gents_store::graphql::escape_graphql_string(row["_docID"].as_str().unwrap());
    let mutation = format!(
        r#"mutation($input: InferenceBackendMetadataMutationInputArg!) {{ update_InferenceBackendMetadata(docID: "{doc}", input: $input) {{ _docID }} }}"#,
    );
    access
        .transact_write("test.corrupt_owned_backend", |txn| {
            let mutation = &mutation;
            Box::pin(async move {
                txn.execute_client_signed_update(
                    mutation,
                    &serde_json::json!({"input": {"auth_kind": "invalid-auth"}}),
                    gents_protocol::schemas::INFERENCE_BACKEND_METADATA,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    let result = access
        .transact_write("test.reference_owned_corruption", |txn| {
            Box::pin(async move { ConfigReferences::load_in_txn(txn, owner).await?.validate() })
        })
        .await;
    assert!(
        result.is_err(),
        "owned malformed configuration must fail closed"
    );
}

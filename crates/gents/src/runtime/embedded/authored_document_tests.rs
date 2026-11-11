use gents_store::config_client::authored_document::*;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;
use serde_json::json;

#[tokio::test]
async fn authored_application_document_is_signed_by_the_held_user() -> anyhow::Result<()> {
    const SDL: &str = "type AuthoredDocumentFixture { name: String }";
    const WRONG: &str = "type AuthoredDocumentFixture { name: Int }";
    let (db, user, access, _) =
        crate::test_support::root::signed_test_node("authored-document").await;
    // Schema installation is node administration; the held User authors documents only.
    let admin = ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    );
    let plan = gents_store::config_client::preview_schema_install(&admin, SDL).await?;
    gents_store::config_client::apply_schema_install(&admin, SDL, &plan.artifact_digest).await?;
    let doc = create_authored_document(
        &access,
        "AuthoredDocumentFixture",
        &json!({"name": "created"}),
        DocumentSchemaSource::UserAuthored,
        SDL,
    )
    .await?;
    let escaped = gents_store::graphql::escape_graphql_string(&doc);
    let response = access.execute(&format!(
        r#"{{ _commits(docID: "{escaped}", filter: {{fieldName: {{_eq: "_C"}}}}, order: {{height: ASC}}, limit: 1) {{cid}} }}"#,
    )).await?;
    let cid = response["data"]["_commits"][0]["cid"]
        .as_str()
        .expect("genesis CID");
    let (block, signature) = db
        .node
        .authorized_signed_block_bytes(cid, Some(user.did()))
        .await?;
    let block = defra_core::block::Block::from_dag_cbor(&block)?;
    let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
    let signer = db::block::verify::verified_signature_signer_did(&block, &signature)
        .map_err(anyhow::Error::msg)?;
    assert_eq!(signer, user.did());
    assert_ne!(user.did(), db.node_identity.did());
    let cases = &gents_lean_vocab::lean_contract_snapshot().identity_client_document_creation_cases;
    for (case_name, source, schema) in [
        ("remote", DocumentSchemaSource::RemoteObservation, SDL),
        ("schema-mismatch", DocumentSchemaSource::UserAuthored, WRONG),
    ] {
        let model = cases.iter().find(|case| case["name"] == case_name).unwrap();
        assert_eq!(model["expected_route"], "refuse");
        assert!(
            create_authored_document(
                &access,
                "AuthoredDocumentFixture",
                &json!({"name": "denied"}),
                source,
                schema,
            )
            .await
            .is_err(),
            "{case_name}"
        );
    }
    let response = access
        .execute("{ AuthoredDocumentFixture { name } }")
        .await?;
    assert_eq!(
        response["data"]["AuthoredDocumentFixture"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    Ok(())
}

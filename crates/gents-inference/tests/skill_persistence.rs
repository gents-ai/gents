use gents_inference::test_support::document_fixtures::*;
#[tokio::test]
async fn skill_crud_mutations_round_trip() {
    let (db, user, access, _binding) = gents_inference::test_support::signed_test_node(
        "skill-crud",
        gents_store::node_extension::Extensions::default(),
    )
    .await;
    grant_delete_permissions(&db.node, db.node_identity.as_ref(), &user).await;
    seed_automation(
        &access,
        vec![(
            gents_store::collection::Collection::Skill,
            serde_json::json!({
                "skill_id": "s1", "node_did": db.node_identity.did(),
                "name": "S", "tool_refs": ["read_file"], "enabled": true,
            }),
        )],
    )
    .await;
    let doc = find_owned_doc(&access, "Skill", "skill_id", "s1").await;
    patch_desired_document(
        &access,
        gents_store::collection::Collection::Skill,
        db.node_identity.did(),
        "s1",
        serde_json::json!({"enabled": false}),
    )
    .await;
    let response = access
        .execute(r#"{ Skill(filter: { skill_id: { _eq: "s1" } }) { skill_id enabled } }"#)
        .await
        .unwrap();
    assert_eq!(
        response["data"]["Skill"][0]["enabled"].as_bool(),
        Some(false),
        "disable must persist"
    );
    delete_signed_document(&access, gents_store::collection::Collection::Skill, &doc).await;
    let response = access
        .execute(r#"{ Skill(filter: { skill_id: { _eq: "s1" } }) { _docID } }"#)
        .await
        .unwrap();
    assert!(response["data"]["Skill"].as_array().unwrap().is_empty());
}

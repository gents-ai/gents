use super::*;
use crate::eval::runner::embedded::executor::tests::{
    frozen_binding, materialized_digest, write_slot_pack,
};
use crate::eval::runner::executor::FixtureDocument;
use crate::DocumentRuntimeOptions;

#[tokio::test]
async fn installing_a_pack_binds_its_inference_slot_to_the_frozen_profile() {
    let dir = tempfile::tempdir().unwrap();
    let pack_dir = dir.path().join("pack");
    write_slot_pack(&pack_dir, "gents:inference-slot:primary");
    let home = EmbeddedHome::create_temp("slot-binding").await.unwrap();
    let workspace = dir.path().join("workspace");
    let spec = TrialSpec {
        pack_digest: materialized_digest(&pack_dir),
        pack_dir,
        inference: frozen_binding(json!("frozen-profile")),
        agent_id: "subject".into(),
        ..TrialSpec::empty_for_tests("t1")
    };

    install(&spec, &home, &workspace).await.unwrap();

    let response = home
        .node
        .execute("query { Agent { agent_id inference_profile_id } }")
        .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    let rows = response.data.as_ref().unwrap()["Agent"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["agent_id"], "subject");
    assert_eq!(
        rows[0]["inference_profile_id"], "frozen-profile",
        "the slot marker never reaches the trial home"
    );
    home.node.shutdown().await;
}

#[tokio::test]
async fn a_trial_subject_supplies_an_omitted_default_agent() {
    let dir = tempfile::tempdir().unwrap();
    let pack_dir = dir.path().join("pack");
    write_slot_pack(&pack_dir, "gents:inference-slot:primary");
    let path = pack_dir.join("pack_config.json");
    let config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(config["node"].get("default_agent_id").is_none());
    let bytes = serde_json::to_vec(&config).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let spec = TrialSpec {
        pack_digest: materialized_digest(&pack_dir),
        pack_dir,
        agent_id: "subject".into(),
        inference: frozen_binding(json!("frozen-profile")),
        ..TrialSpec::empty_for_tests("subject-default")
    };
    let home = EmbeddedHome::create_temp("subject-default").await.unwrap();
    install(&spec, &home, &dir.path().join("workspace"))
        .await
        .unwrap();

    let access = ConfigAccess::local(home.node.clone());
    let response = access
        .execute("query { Node { default_agent_id } }")
        .await
        .unwrap();
    assert_eq!(response["data"]["Node"][0]["default_agent_id"], "subject");
    let agent = crate::Gents::from_default_agent_documents(
        home.node.clone(),
        home.identity.clone(),
        DocumentRuntimeOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(agent.default_agent_id(), "subject");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    home.node.shutdown().await;
}

#[tokio::test]
async fn subject_schemas_are_installed_before_documents_without_fixture_copies() {
    let dir = tempfile::tempdir().unwrap();
    let pack_dir = dir.path().join("pack");
    write_slot_pack(&pack_dir, "gents:inference-slot:primary");
    let schema_path = "schemas/submission.graphql";
    let sdl = "type EvalSubmission { message: String }";
    std::fs::create_dir_all(pack_dir.join("schemas")).unwrap();
    std::fs::write(pack_dir.join(schema_path), sdl).unwrap();
    let manifest_path = pack_dir.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["schemas"] = json!([schema_path]);
    manifest["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!(schema_path));
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let config_path = pack_dir.join("pack_config.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["datastore_tool_surfaces"] = json!([{
        "surface_id": "submissions",
        "entries": [{
            "tool_name": "submit_message",
            "collection": "EvalSubmission",
            "description": "Record a submission.",
            "fields": [{"name": "message", "required": true}]
        }]
    }]);
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let spec = TrialSpec {
        pack_digest: materialized_digest(&pack_dir),
        pack_dir,
        agent_id: "subject".into(),
        inference: frozen_binding(json!("frozen-profile")),
        fixtures: TrialFixtures {
            documents: vec![FixtureDocument {
                collection: "EvalSubmission".into(),
                document: json!({"message": "from the fixture"}),
            }],
            ..Default::default()
        },
        ..TrialSpec::empty_for_tests("subject-schema")
    };
    let home = EmbeddedHome::create_temp("subject-schema").await.unwrap();
    let workspace = dir.path().join("workspace");
    install(&spec, &home, &workspace).await.unwrap();
    let access = ConfigAccess::local(home.node.clone());
    let response = access
        .execute("query { EvalSubmission { message } }")
        .await
        .unwrap();
    assert_eq!(
        response["data"]["EvalSubmission"],
        json!([{"message": "from the fixture"}])
    );

    let fixtures = TrialFixtures {
        schemas: vec![sdl.into()],
        ..Default::default()
    };
    install_fixtures(&access, &fixtures, &workspace, home.did())
        .await
        .unwrap();
    let incompatible = TrialFixtures {
        schemas: vec!["type EvalSubmission { message: Int }".into()],
        ..Default::default()
    };
    let error = install_fixtures(&access, &incompatible, &workspace, home.did())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("EvalSubmission"));
    assert_eq!(
        access
            .execute("query { EvalSubmission { message } }")
            .await
            .unwrap(),
        response
    );
    home.node.shutdown().await;
}

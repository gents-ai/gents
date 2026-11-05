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
    let home = EmbeddedHome::create_trial_temp("slot-binding")
        .await
        .unwrap();
    let workspace = dir.path().join("workspace");
    let spec = TrialSpec {
        pack_digest: materialized_digest(&pack_dir),
        pack_dir,
        inference: frozen_binding(json!("frozen-profile")),
        agent_slot: "subject".into(),
        ..TrialSpec::empty_for_tests("t1")
    };

    let agent_did = install(&spec, &home, &workspace).await.unwrap();

    let evaluator = ConfigAccess::local_as(
        home.node.clone(),
        ::identity::Did::new(&spec.evaluator_did).unwrap(),
    );
    let response = evaluator
        .execute("query { Agent { agent_did inference_profile_id } }")
        .await
        .unwrap();
    let rows = response["data"]["Agent"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["agent_did"], agent_did);
    assert_ne!(agent_did, home.did());
    assert_ne!(agent_did, spec.evaluator_did);
    assert_eq!(
        rows[0]["inference_profile_id"], "frozen-profile",
        "the slot marker never reaches the trial home"
    );

    let node_did = home.did().to_owned();
    let ownership = evaluator
        .transact_readonly("eval.test.node_and_agent_owners", |txn| {
            let agent_did = &agent_did;
            let node_did = &node_did;
            Box::pin(async move {
                let (node, _) = crate::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    Collection::Node,
                    node_did,
                    node_did,
                )
                .await?
                .unwrap();
                let (agent, _) = crate::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    Collection::Agent,
                    node_did,
                    agent_did,
                )
                .await?
                .unwrap();
                let node_owner = txn.document_owners("Node", &[node]).await?;
                let agent_owner = txn.document_owners("Agent", &[agent]).await?;
                Ok::<_, anyhow::Error>((node_owner, agent_owner))
            })
        })
        .await
        .unwrap();
    assert_eq!(
        ownership.0[0].owner.as_ref().map(::identity::Did::as_str),
        Some(spec.evaluator_did.as_str())
    );
    assert_eq!(
        ownership.1[0].owner.as_ref().map(::identity::Did::as_str),
        Some(spec.evaluator_did.as_str())
    );

    let locator = crate::eval::runner::executor::TrialLocator {
        trial_node_did: home.did().to_owned(),
        session_id: "eval-session".into(),
        home_hint: None,
    };
    let stage = crate::eval::runner::executor::StageSpec {
        stage_id: "opening".into(),
        prompt: "hello".into(),
        seed: None,
        deadline_secs: None,
        settle: false,
        continuation: None,
        review_previous: false,
        captures: Vec::new(),
    };
    crate::eval::runner::embedded::executor::submit::submit_stage(
        &home.node,
        &spec.evaluator_did,
        &locator,
        &agent_did,
        &stage,
        "eval-request",
    )
    .await
    .unwrap();
    let actor =
        ConfigAccess::local_as(home.node.clone(), ::identity::Did::new(&agent_did).unwrap());
    let session = actor
        .execute("{ AgentSession { session_id requester_did agent_did relationships_pending } }")
        .await
        .unwrap();
    assert_eq!(session["data"]["AgentSession"].as_array().unwrap().len(), 1);
    assert_eq!(
        session["data"]["AgentSession"][0]["session_id"],
        "eval-session"
    );
    assert_eq!(
        session["data"]["AgentSession"][0]["requester_did"],
        spec.evaluator_did
    );
    assert_eq!(session["data"]["AgentSession"][0]["agent_did"], agent_did);
    assert_eq!(
        session["data"]["AgentSession"][0]["relationships_pending"],
        false
    );
    let outsider_did = ::identity::Did::new("did:key:eval-outsider").unwrap();
    let outsider = ConfigAccess::local_as(home.node.clone(), outsider_did.clone());
    let denied = outsider
        .execute("{ AgentSession { session_id } }")
        .await
        .unwrap();
    assert!(denied["data"].get("AgentSession").is_none(), "{denied}");
    let owner =
        ConfigAccess::local_as(home.node.clone(), ::identity::Did::new(home.did()).unwrap());
    owner
        .grant_node_permission(
            &outsider_did,
            crate::store::config_client::NodePermission::DocumentRead,
        )
        .await
        .unwrap();
    let denied_by_document_acp = outsider
        .execute("{ AgentSession { session_id } }")
        .await
        .unwrap();
    assert_eq!(
        denied_by_document_acp["data"]["AgentSession"],
        serde_json::json!([])
    );
    let visible = crate::eval::runner::embedded::live::activity(&evaluator)
        .await
        .unwrap();
    let hidden = crate::eval::runner::embedded::live::activity(&outsider)
        .await
        .unwrap();
    assert_eq!(visible.requests, 1);
    assert_eq!(hidden.requests, 0);
    assert!(
        crate::eval::runner::embedded::observe::poll_request_with_access(
            &evaluator,
            "eval-request"
        )
        .await
        .unwrap()
        .is_some()
    );
    assert!(
        crate::eval::runner::embedded::observe::poll_request_with_access(&outsider, "eval-request")
            .await
            .unwrap()
            .is_none()
    );
    evaluator.write("eval.test.complete_request", r#"mutation { update_AgentRequest(filter: {request_id: {_eq: "eval-request"}}, input: {lifecycle_state: "completed"}) { _docID } }"#).await.unwrap();
    let terminal = crate::eval::runner::embedded::observe::await_terminal_with_access(
        &home.node,
        &evaluator,
        "eval-request",
        None,
        std::time::Duration::ZERO,
        &crate::eval::runner::embedded::observe::NoHook,
    )
    .await
    .unwrap();
    assert_eq!(
        terminal.terminal_state,
        Some(gents_protocol::request_lifecycle::RequestLifecycleState::Completed)
    );
    home.node.shutdown().await.expect("shutdown node");
}

#[tokio::test]
async fn a_trial_subject_supplies_an_omitted_default_agent() {
    let dir = tempfile::tempdir().unwrap();
    let pack_dir = dir.path().join("pack");
    write_slot_pack(&pack_dir, "gents:inference-slot:primary");
    let path = pack_dir.join("pack_config.json");
    let config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(config["node"].get("default_agent_did").is_none());
    let bytes = serde_json::to_vec(&config).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let spec = TrialSpec {
        pack_digest: materialized_digest(&pack_dir),
        pack_dir,
        agent_slot: "subject".into(),
        inference: frozen_binding(json!("frozen-profile")),
        ..TrialSpec::empty_for_tests("subject-default")
    };
    let home = EmbeddedHome::create_temp("subject-default").await.unwrap();
    let agent_did = install(&spec, &home, &dir.path().join("workspace"))
        .await
        .unwrap();

    let access = ConfigAccess::local(home.node.clone());
    let response = access
        .execute("query { Node { default_agent_did } }")
        .await
        .unwrap();
    assert_eq!(response["data"]["Node"][0]["default_agent_did"], agent_did);
    let agent = crate::Gents::from_default_agent_documents(
        home.node.clone(),
        home.identity.clone(),
        DocumentRuntimeOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(agent.default_agent_did(), agent_did);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    home.node.shutdown().await.expect("shutdown node");
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
        agent_slot: "subject".into(),
        inference: frozen_binding(json!("frozen-profile")),
        fixtures: TrialFixtures {
            documents: vec![FixtureDocument {
                collection: "EvalSubmission".into(),
                document: json!({"message": "from the fixture"}),
                session_readers: Vec::new(),
                session_agent_owned: false,
                expired_client_request: false,
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
    home.node.shutdown().await.expect("shutdown node");
}

#[tokio::test]
async fn session_fixtures_publish_only_declared_cross_agent_readers() {
    let dir = tempfile::tempdir().unwrap();
    let pack_dir = dir.path().join("pack");
    write_slot_pack(&pack_dir, "gents:inference-slot:primary");
    let path = pack_dir.join("pack_config.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["agents"].as_array_mut().unwrap().push(json!({
        "agent_slot": "partner", "display_name": "Partner", "inference_profile_id": "gents:inference-slot:primary"
    }));
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let path = pack_dir.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["inference_slots"][0]["agents"]
        .as_array_mut()
        .unwrap()
        .push(json!("partner"));
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let fixture = |session: &str, slot: &str, readers: Vec<String>| FixtureDocument {
        collection: "AgentSession".into(),
        document: json!({"node_did":"$trial", "requester_did": if slot == "partner" { Value::Null } else { json!("$evaluator") }, "session_id": session,
            "agent_did": format!("gents:agent-slot:{slot}"), "created_at":"2025-01-01T00:00:00Z"}),
        session_readers: readers,
        session_agent_owned: slot == "partner",
        expired_client_request: false,
    };
    let spec = TrialSpec {
        pack_digest: materialized_digest(&pack_dir),
        pack_dir,
        agent_slot: "subject".into(),
        inference: frozen_binding(json!("frozen-profile")),
        fixtures: TrialFixtures {
            documents: vec![
                fixture("owned", "subject", vec![]),
                fixture("shared", "partner", vec!["subject".into()]),
                fixture("private", "partner", vec![]),
            ],
            ..Default::default()
        },
        ..TrialSpec::empty_for_tests("session-fixtures")
    };
    let home = EmbeddedHome::create_trial_temp("session-fixtures")
        .await
        .unwrap();
    let subject = install(&spec, &home, &dir.path().join("workspace"))
        .await
        .unwrap();
    let evaluator = ConfigAccess::local_as(
        home.node.clone(),
        ::identity::Did::new(&spec.evaluator_did).unwrap(),
    );
    let agents = evaluator
        .execute("{ Agent { agent_did display_name } }")
        .await
        .unwrap();
    let partner = agents["data"]["Agent"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["display_name"] == "Partner")
        .unwrap()["agent_did"]
        .as_str()
        .unwrap();
    assert_ne!(subject, partner);
    let session_rows = evaluator
        .execute("{ AgentSession { _docID session_id } }")
        .await
        .unwrap();
    let shared_id = session_rows["data"]["AgentSession"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["session_id"] == "shared")
        .unwrap()["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    let owners = evaluator
        .transact_readonly("eval.test.fixture_owner", |txn| {
            let shared_id = &shared_id;
            Box::pin(async move {
                txn.document_owners("AgentSession", &[shared_id.clone()])
                    .await
            })
        })
        .await
        .unwrap();
    assert_eq!(
        owners[0].owner.as_ref().map(::identity::Did::as_str),
        Some(partner)
    );
    let visible = |did: String| {
        let node = home.node.clone();
        async move {
            let access = ConfigAccess::local_as(node, ::identity::Did::new(did).unwrap());
            let rows = access
                .execute("{ AgentSession { session_id relationships_pending } }")
                .await
                .unwrap();
            let mut ids = rows["data"]["AgentSession"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    assert_eq!(r["relationships_pending"], false);
                    r["session_id"].as_str().unwrap().to_owned()
                })
                .collect::<Vec<_>>();
            ids.sort();
            ids
        }
    };
    assert_eq!(visible(subject).await, ["owned", "shared"]);
    assert_eq!(visible(partner.to_owned()).await, ["private", "shared"]);
    let outsider = "did:key:eval-fixture-outsider";
    grant_trial_operations(&home, outsider).await.unwrap();
    assert!(visible(outsider.into()).await.is_empty());
    home.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn expired_request_fixtures_are_signed_published_and_expired_without_inference() {
    let transition = crate::lean_vocab_test::lean_request_transition_cases()
        .iter()
        .find(|case| case.from == "pending" && case.to == "dead")
        .expect("Lean models pending expiry");
    assert_eq!(transition.classification, "legal");
    assert_eq!(transition.action.as_deref(), Some("expire"));
    let dir = tempfile::tempdir().unwrap();
    let pack_dir = dir.path().join("pack");
    write_slot_pack(&pack_dir, "gents:inference-slot:primary");
    let session = FixtureDocument {
        collection: "AgentSession".into(),
        document: json!({"node_did":"$trial", "requester_did":"$evaluator", "session_id":"traffic-session", "agent_did":"gents:agent-slot:subject", "created_at":"2025-01-01T00:00:00Z"}),
        session_readers: vec![],
        session_agent_owned: false,
        expired_client_request: false,
    };
    let request = |request_id: &str| FixtureDocument {
        collection: "AgentRequest".into(),
        document: json!({"node_did":"$trial", "requester_did":"$evaluator", "session_id":"traffic-session", "agent_did":"gents:agent-slot:subject", "request_id": request_id, "purpose":"normal", "lifecycle_state":"dead", "created_at":"2025-02-01T00:00:00Z", "valid_until":"2025-02-02T00:00:00Z"}),
        session_readers: vec![],
        session_agent_owned: false,
        expired_client_request: true,
    };
    let spec = TrialSpec {
        pack_digest: materialized_digest(&pack_dir),
        pack_dir,
        agent_slot: "subject".into(),
        inference: frozen_binding(json!("frozen-profile")),
        fixtures: TrialFixtures {
            documents: vec![session, request("traffic-1"), request("traffic-2")],
            ..Default::default()
        },
        ..TrialSpec::empty_for_tests("expired-fixtures")
    };
    let home = EmbeddedHome::create_trial_temp("expired-fixtures")
        .await
        .unwrap();
    let agent = install(&spec, &home, &dir.path().join("workspace"))
        .await
        .unwrap();
    let subject = ConfigAccess::local_as(home.node.clone(), ::identity::Did::new(&agent).unwrap());
    let rows = subject.execute(&format!("{{ AgentRequest {{ {} relationships_pending claimed_at }} InferenceCall {{ _docID }} AgentToolCall {{ _docID }} }}", crate::SIGNED_REQUEST_FIELDS)).await.unwrap();
    let requests = rows["data"]["AgentRequest"].as_array().unwrap();
    assert_eq!(requests.len(), 2);
    let mut ids = Vec::new();
    for row in requests {
        assert_eq!(row["lifecycle_state"], transition.to);
        assert_eq!(row["relationships_pending"], false);
        assert!(row["claimed_at"].is_null());
        assert_eq!(row["requester_did"], spec.evaluator_did);
        assert_eq!(row["agent_did"], agent);
        let native: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(row.clone()).unwrap();
        crate::verify_request_receipt_signature(&native).unwrap();
        ids.push(native.doc_id.unwrap());
    }
    assert!(rows["data"]["InferenceCall"].as_array().unwrap().is_empty());
    assert!(rows["data"]["AgentToolCall"].as_array().unwrap().is_empty());
    let evaluator = ConfigAccess::local_as(
        home.node.clone(),
        ::identity::Did::new(&spec.evaluator_did).unwrap(),
    );
    let owners = evaluator
        .transact_readonly("eval.test.expired_request_owners", |txn| {
            let ids = &ids;
            Box::pin(async move { txn.document_owners("AgentRequest", ids).await })
        })
        .await
        .unwrap();
    assert!(owners
        .iter()
        .all(|owner| owner.owner.as_ref().map(::identity::Did::as_str)
            == Some(spec.evaluator_did.as_str())));
    let outsider = "did:key:expired-fixture-outsider";
    grant_trial_operations(&home, outsider).await.unwrap();
    let outsider =
        ConfigAccess::local_as(home.node.clone(), ::identity::Did::new(outsider).unwrap());
    assert_eq!(
        outsider.execute("{AgentRequest {_docID}}").await.unwrap()["data"]["AgentRequest"],
        json!([])
    );
    home.node.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "materializes all 5001 signed historical requests"]
async fn discover_old_fixture_publishes_full_expired_traffic() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/configurator_evals/sessions");
    let case: Value =
        serde_json::from_slice(&std::fs::read(root.join("cases/discover_old.json")).unwrap())
            .unwrap();
    let documents: Vec<FixtureDocument> =
        serde_json::from_value(case["fixtures"]["documents"].clone()).unwrap();
    assert_eq!(
        documents
            .iter()
            .filter(|fixture| fixture.expired_client_request)
            .count(),
        5001
    );
    let pack_dir = root.join("subject");
    let spec = TrialSpec {
        pack_digest: materialized_digest(&pack_dir),
        pack_dir,
        agent_slot: "engineer".into(),
        inference: frozen_binding(json!("frozen-profile")),
        fixtures: TrialFixtures {
            documents,
            ..Default::default()
        },
        ..TrialSpec::empty_for_tests("discover-old-full")
    };
    let dir = tempfile::tempdir().unwrap();
    let home = EmbeddedHome::create_trial_temp("discover-old-full")
        .await
        .unwrap();
    let agent = install(&spec, &home, &dir.path().join("workspace"))
        .await
        .unwrap();
    let subject = ConfigAccess::local_as(home.node.clone(), ::identity::Did::new(&agent).unwrap());
    let rows = subject.execute("{ AgentRequest { request_id lifecycle_state relationships_pending claimed_at } InferenceCall { _docID } AgentToolCall { _docID } }").await.unwrap();
    let requests = rows["data"]["AgentRequest"].as_array().unwrap();
    assert_eq!(requests.len(), 5001);
    let mut ids = std::collections::BTreeSet::new();
    for row in requests {
        assert!(ids.insert(row["request_id"].as_str().unwrap()));
        assert_eq!(row["lifecycle_state"], "dead");
        assert_eq!(row["relationships_pending"], false);
        assert!(row["claimed_at"].is_null());
    }
    assert!(rows["data"]["InferenceCall"].as_array().unwrap().is_empty());
    assert!(rows["data"]["AgentToolCall"].as_array().unwrap().is_empty());
    home.node.shutdown().await.unwrap();
}

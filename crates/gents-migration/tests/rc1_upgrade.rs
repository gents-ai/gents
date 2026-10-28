use gents_migration::ensure_migrations;
use gents_protocol::schemas;

mod common;
use common::fresh_node;

#[tokio::test]
async fn applied_pairing_generation_upgrade_preserves_routes_without_granting_authority() {
    let node = fresh_node().await;
    let previous = schemas::PEER_PAIRING_APPLIED
        .replace("    enrollment_request_digest: String\n", "")
        .replace("    enrollment_authorization_sequence: Int\n", "")
        .replace("    enrollment_authorization_expires_at: DateTime\n", "");
    node.add_collection(&previous).await.unwrap();
    let before = node.get_collection("PeerPairingApplied").unwrap().unwrap();
    assert_eq!(
        before.version_id,
        "bafyreifunn7vevp6b6rzg232gjfypp2lqviafe5now5ldlwo3na5nfinq4"
    );
    let created = node
        .execute(
            r#"mutation {
        add_PeerPairingApplied(input: {
            peer_id: "retained-peer", replicator_addresses: ["retained-address"],
            updated_at: "2026-10-28T00:00:00Z"
        }) { _docID }
    }"#,
        )
        .await;
    assert!(!created.has_errors(), "{created:?}");
    let identity = created.data.unwrap()["add_PeerPairingApplied"][0]["_docID"]
        .as_str()
        .expect("created route identity")
        .to_owned();
    let report = ensure_migrations(node.as_ref()).await.unwrap();
    assert_eq!(report.steps_applied, 1);
    let after = node.get_collection("PeerPairingApplied").unwrap().unwrap();
    assert_eq!(
        after.version_id,
        "bafyreifiujskeayi7adradcst2jit3srboodcd6ejy2m6gppwy2mv6k7ri"
    );
    for field in before.fields {
        assert_eq!(
            after
                .fields
                .iter()
                .find(|value| value.name == field.name)
                .unwrap()
                .id,
            field.id,
            "{} moved",
            field.name
        );
    }
    let read = node
        .execute(
            r#"{ PeerPairingApplied {
        _docID peer_id replicator_addresses updated_at enrollment_request_digest
        enrollment_authorization_sequence enrollment_authorization_expires_at
    } }"#,
        )
        .await;
    assert!(!read.has_errors(), "{read:?}");
    let data = read.data.unwrap();
    let row = &data["PeerPairingApplied"][0];
    assert_eq!(row["_docID"], identity);
    assert_eq!(row["peer_id"], "retained-peer");
    assert_eq!(
        row["replicator_addresses"],
        serde_json::json!(["retained-address"])
    );
    assert_eq!(row["updated_at"], "2026-10-28T00:00:00Z");
    for field in [
        "enrollment_request_digest",
        "enrollment_authorization_sequence",
        "enrollment_authorization_expires_at",
    ] {
        assert!(row[field].is_null(), "upgrade invented {field}");
    }
    assert_eq!(
        ensure_migrations(node.as_ref())
            .await
            .unwrap()
            .steps_applied,
        0
    );
    node.shutdown().await;
}

#[tokio::test]
async fn rc1_upgrade_preserves_rows_field_ids_and_restarts() {
    let node = fresh_node().await;
    let profile = schemas::INFERENCE_PROFILE.replace("    service_tier: String\n", "");
    let tool = schemas::AGENT_TOOL_CALL
        .replace("    background_requested_at: DateTime\n", "")
        .replace("    job_pin: JSON\n", "")
        .replace("    job_progress: JSON\n", "")
        .replace("    job_step_state: JSON\n", "");
    node.add_collection(&format!("{profile}\n{tool}"))
        .await
        .unwrap();
    let names = [
        schemas::INFERENCE_PROFILE_NAME,
        schemas::AGENT_TOOL_CALL_NAME,
    ];
    let before = names.map(|name| node.get_collection(name).unwrap().unwrap());
    assert_eq!(
        before[0].version_id,
        "bafyreiat2uhx24kbgbm54lz7nt56vtjjsfttkiwop2ntii7qn2xtceehxi"
    );
    assert_eq!(
        before[1].version_id,
        "bafyreiexmbjxfqyg6yhjst6r746spzge27f6mhh2moyrkawbmvtvba4mbi"
    );
    let created = node.execute(r#"mutation {
        add_InferenceProfile(input: {profile_id: "retained", node_did: "owner", context_window: 65536, max_output_tokens: 4096}) {_docID}
        add_AgentToolCall(input: {tool_call_key: "retained", request_id: "request", status: "completed", latency_ms: 37}) {_docID}
    }"#).await;
    assert!(!created.has_errors(), "{created:?}");
    let identities = created.data.unwrap();
    let [profile_id, tool_id] = ["add_InferenceProfile", "add_AgentToolCall"].map(|name| {
        let rows = identities[name]
            .as_array()
            .unwrap_or_else(|| panic!("{name} must return document rows: {identities:?}"));
        assert_eq!(rows.len(), 1, "{name} must create exactly one document");
        rows[0]["_docID"]
            .as_str()
            .expect("created document ID")
            .to_owned()
    });
    let report = ensure_migrations(node.as_ref()).await.unwrap();
    assert_eq!(report.steps_applied, 3);
    for (name, previous) in names.into_iter().zip(before) {
        let current = node.get_collection(name).unwrap().unwrap();
        for field in previous.fields {
            let retained = current
                .fields
                .iter()
                .find(|value| value.name == field.name)
                .unwrap();
            assert_eq!(retained.id, field.id, "{name}.{} moved", field.name);
        }
    }
    let read = node
        .execute(
            r#"{
        InferenceProfile {_docID profile_id context_window max_output_tokens service_tier}
        AgentToolCall {_docID tool_call_key status latency_ms job_pin job_progress job_step_state background_requested_at}
    }"#,
        )
        .await;
    assert!(!read.has_errors(), "{read:?}");
    let data = read.data.unwrap();
    assert_eq!(
        data["InferenceProfile"][0]["_docID"].as_str(),
        Some(profile_id.as_str())
    );
    assert_eq!(
        data["AgentToolCall"][0]["_docID"].as_str(),
        Some(tool_id.as_str())
    );
    assert_eq!(data["InferenceProfile"][0]["context_window"], 65536);
    assert_eq!(data["InferenceProfile"][0]["max_output_tokens"], 4096);
    assert_eq!(data["AgentToolCall"][0]["latency_ms"], 37);
    assert_eq!(data["AgentToolCall"][0]["status"], "completed");
    let rerun = ensure_migrations(node.as_ref()).await.unwrap();
    assert_eq!(rerun.steps_applied, 0);
    assert_eq!(rerun.steps_already_current, 3);
    node.shutdown().await;
}

#[tokio::test]
async fn current_tool_schema_adds_background_control_without_moving_fields() {
    let node = fresh_node().await;
    let previous = schemas::AGENT_TOOL_CALL.replace("    background_requested_at: DateTime\n", "");
    node.add_collection(&previous).await.unwrap();
    let before = node.get_collection("AgentToolCall").unwrap().unwrap();
    assert_eq!(
        before.version_id,
        "bafyreihfkiuhbdzdlmfqf6wm4q5xgrad4wl2zi4fvcj25co3kdjrktjv5a"
    );
    ensure_migrations(node.as_ref()).await.unwrap();
    let after = node.get_collection("AgentToolCall").unwrap().unwrap();
    assert_eq!(
        after.version_id,
        "bafyreifddp6p4viubjrzhgbrgyjnnyzuq2u5qzpcavw2rkkmrbgfltboza"
    );
    assert!(after
        .fields
        .iter()
        .any(|field| field.name == "background_requested_at"));
    for field in before.fields {
        assert_eq!(
            after
                .fields
                .iter()
                .find(|current| current.name == field.name)
                .unwrap()
                .id,
            field.id
        );
    }
    assert_eq!(
        ensure_migrations(node.as_ref())
            .await
            .unwrap()
            .steps_applied,
        0
    );
    node.shutdown().await;
}

#[tokio::test]
async fn apply_collections_declare_updated_at_without_moving_fields() {
    let node = fresh_node().await;
    let collections = [
        (
            schemas::AGENT_TARGET_NAME,
            schemas::AGENT_TARGET,
            "target_id",
            "bafyreierhwy2pombqxf2ufiqih275phvgdxj2w3h6e7wnuwme2tsllgoje",
        ),
        (
            schemas::TOOLS_NAME,
            schemas::TOOLS,
            "tools_id",
            "bafyreibrawpzq3wv3gm6ew2gd26k75rfi6xm64fp4x55njpfdics6xx26e",
        ),
        (
            schemas::CHAIN_KEY_BINDING_NAME,
            schemas::CHAIN_KEY_BINDING,
            "binding_id",
            "bafyreias5umbmxkuo6iguihj6tvovbuzgjscwj7wjcmz23pirgk7jo4uom",
        ),
        (
            schemas::ETH_TOOL_NAME,
            schemas::ETH_TOOL,
            "tool_id",
            "bafyreie24fvfqig33silif6nxrf4jfua2yokujyqbf4ydvstrifvhweaca",
        ),
        (
            schemas::CALLBACK_NAME,
            schemas::CALLBACK,
            "callback_id",
            "bafyreibijc44aul7l2m7kmvlojizbze7236sjixhi57osoqkevsreda5p4",
        ),
        (
            schemas::CALLBACK_MODULE_NAME,
            schemas::CALLBACK_MODULE,
            "module_id",
            "bafyreiceftkewzvdjuylk3djznmbw656qbopzikxneqq3v7krigoqsvqtu",
        ),
        (
            schemas::CALLBACK_BINDING_NAME,
            schemas::CALLBACK_BINDING,
            "binding_id",
            "bafyreigvksodxohqggry2u7dr65xak5kvlyp65rvfm62pgxvp4vlnfgody",
        ),
    ];
    let mut before = Vec::new();
    for (name, sdl, _, _) in collections {
        let previous = sdl.replace("    updated_at: DateTime\n", "");
        assert_ne!(previous, sdl, "{name} must declare updated_at");
        node.add_collection(&previous).await.unwrap();
        before.push(node.get_collection(name).unwrap().unwrap());
    }
    let report = ensure_migrations(node.as_ref()).await.unwrap();
    assert_eq!(report.steps_applied, collections.len());
    for ((name, _, key, target), previous) in collections.into_iter().zip(before) {
        let current = node.get_collection(name).unwrap().unwrap();
        assert_eq!(current.version_id, target, "{name}");
        for field in previous.fields {
            let retained = current
                .fields
                .iter()
                .find(|value| value.name == field.name)
                .unwrap();
            assert_eq!(retained.id, field.id, "{name}.{} moved", field.name);
        }
        let created = node
            .execute(&format!(
                r#"mutation {{ add_{name}(input: {{{key}: "stamped", node_did: "owner", updated_at: "2026-10-27T00:00:00.000000001Z"}}) {{ _docID }} }}"#
            ))
            .await;
        assert!(!created.has_errors(), "{name}: {created:?}");
    }
    let rerun = ensure_migrations(node.as_ref()).await.unwrap();
    assert_eq!(rerun.steps_applied, 0);
    node.shutdown().await;
}

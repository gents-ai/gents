use super::*;
use crate::agent::p2p_reconcile::{
    reconcile_peer_tick, EnrollmentEndpointEntry, GraphqlPairingStateStore, PairingStateStore,
};
use crate::support::enrollment::{
    authorize_enrollment_peer, signed_enrollment_offer, wait_for_peer_identity,
};

async fn call(tool: &P2pTool, value: Value) -> Value {
    let output = Tool::call(tool, serde_json::from_value(value).unwrap())
        .await
        .unwrap();
    serde_json::from_str(&output).unwrap()
}

#[tokio::test]
async fn native_pairing_sync_and_revoke_preserve_enrollment_and_scope() {
    let local = crate::support::test_p2p_db("engineer-p2p-local").await;
    let remote = crate::support::test_p2p_db("engineer-p2p-remote").await;
    let identity = local.node_identity.clone();
    for node in [&local.node, &remote.node] {
        ConfigAccess::local(node.clone())
            .add_schema("type DeploymentNote @branchable { text: String }")
            .await
            .unwrap();
    }
    let (peer_id, address) = wait_for_peer_identity(&remote.node).await;
    let enrollment = authorize_enrollment_peer(
        local.node.clone(),
        "engineer-p2p",
        "Engineer P2P",
        identity.clone(),
        remote.node_identity.clone(),
        &peer_id,
        &address,
    )
    .await;
    let tool = P2pTool::new(
        local.node.clone(),
        Some(identity.clone()),
        true,
        EndpointScope::<String, ()>::only_units(["DeploymentNote".into()]),
    );
    let options = json!({"peer_id":peer_id,"peer_did":remote.node_identity.did(),"collections":["DeploymentNote"]});
    let preview = call(
        &tool,
        json!({"argv":["pairings","preview"],"options":options}),
    )
    .await;
    assert!(preview["outcome"]["current"].is_null());
    let wrong_did = Tool::call(&tool, serde_json::from_value(json!({"argv":["pairings","apply"],"options":{"peer_id":peer_id,"peer_did":identity.did(),"collections":["DeploymentNote"]}})).unwrap()).await.unwrap_err();
    assert!(wrong_did.to_string().contains("signed enrollment"));
    assert!(tool.overlay(&peer_id).await.unwrap().is_none());
    let readonly = P2pTool::new(
        local.node.clone(),
        Some(identity.clone()),
        false,
        EndpointScope::<String, ()>::only_units(["DeploymentNote".into()]),
    );
    assert!(Tool::call(
        &readonly,
        serde_json::from_value(json!({"argv":["pairings","apply"],"options":options})).unwrap()
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("mutations are disabled"));
    let malformed = Tool::call(
        &tool,
        serde_json::from_value(
            json!({"argv":["network","get"],"options":{"peer_id":"/ip4/127.0.0.1/tcp/4001"}}),
        )
        .unwrap(),
    )
    .await
    .unwrap_err();
    assert!(malformed.to_string().contains("invalid transport peer ID"));
    assert!(malformed.to_string().contains("network"));
    let forbidden = Tool::call(&tool, serde_json::from_value(json!({"argv":["sync","documents"],"options":{"peer_id":peer_id,"collection":"OAuthCredential","doc_ids":["copied-credential-id"]}})).unwrap()).await.unwrap_err();
    assert!(forbidden.to_string().contains("protocol collections"));
    assert!(forbidden
        .to_string()
        .contains("changing grants or ACP cannot enable"));
    let applied = call(
        &tool,
        json!({"argv":["pairings","apply"],"options":options}),
    )
    .await;
    assert_eq!(applied["outcome"]["unchanged"], false);
    let retried = call(
        &tool,
        json!({"argv":["pairings","apply"],"options":options}),
    )
    .await;
    assert_eq!(retried["outcome"]["unchanged"], true);
    let projection = GraphqlEnrollmentStore::new(local.node.clone(), identity.clone())
        .load_projection()
        .await
        .unwrap();
    let active = projection
        .active
        .iter()
        .find(|a| a.request.request_id == enrollment.request_id)
        .unwrap();
    let entry = EnrollmentEndpointEntry {
        desired_id: peer_id.clone(),
        peer_id: peer_id.clone(),
        node_did: remote.node_identity.did().into(),
        address: address.clone(),
        request_digest: active.request.request_digest.clone(),
        authorization_sequence: active.revision.sequence,
        authorization_expires_at: active.revision.authorization_expires_at.clone(),
    };
    let store = GraphqlPairingStateStore::for_enrollment_materialization(
        local.node.clone(),
        identity.clone(),
        entry,
    );
    let admin = EmbeddedRemoteP2pAdmin::new(local.node.clone());
    let tick = reconcile_peer_tick(&admin, &store, &peer_id).await.unwrap();
    assert!(tick.live_route_matches, "{tick:?}");
    let inspected = call(
        &tool,
        json!({"argv":["network","get"],"options":{"peer_id":peer_id}}),
    )
    .await;
    assert_eq!(inspected["outcome"]["connected"], true, "{inspected}");
    let compact = call(
        &tool,
        json!({"argv":["pairings","get"],"options":{"peer_id":peer_id}}),
    )
    .await;
    assert_eq!(compact["outcome"]["desired"].as_array().unwrap().len(), 1);
    assert_eq!(compact["outcome"]["applied"].as_array().unwrap().len(), 1);
    assert_eq!(
        compact["observations"]["enrolled_peers"][0]["peer_id"],
        peer_id
    );
    assert_eq!(
        compact["observations"]["connected_peers"]["observed_count"],
        1
    );
    let replicators = compact["observations"]["replicators"]["items"]
        .as_array()
        .unwrap();
    assert!(!replicators.is_empty());
    assert!(replicators
        .iter()
        .all(|row| row.get("collections").is_none()
            && row.get("filters").is_none()
            && row["collection_count"].as_u64().is_some()));
    let detail = call(&tool, compact["observations"]["details_call"].clone()).await;
    assert!(detail["observations"]["replicators"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["collections"].is_array() && row.get("filters").is_some()));
    let absent = call(
        &tool,
        json!({"argv":["pairings","get"],"options":{"peer_id":"unrelated-peer"}}),
    )
    .await;
    assert_eq!(absent["outcome"]["desired"], json!([]));
    assert_eq!(absent["outcome"]["applied"], json!([]));
    assert_eq!(absent["observations"]["enrolled_peers"], json!([]));
    assert_eq!(absent["observations"]["replicators"]["observed_count"], 0);
    assert_eq!(
        absent["observations"]["connected_peers"]["observed_count"],
        0
    );
    assert!(store
        .load_applied(&peer_id)
        .await
        .unwrap()
        .state
        .collections
        .contains("DeploymentNote"));
    ConfigAccess::local(remote.node.clone()).write("test.p2p.note", "mutation {create_DeploymentNote(input: {text: \"Previous image remains pinned until review\"}) {_docID}}").await.unwrap();
    let document = ConfigAccess::local(remote.node.clone())
        .execute("{DeploymentNote(limit: 1) {_docID}}")
        .await
        .unwrap();
    let doc_id = document["data"]["DeploymentNote"][0]["_docID"]
        .as_str()
        .unwrap_or_else(|| panic!("missing created document: {document}"));
    let synced = call(&tool, json!({"argv":["sync","documents"],"options":{"peer_id":peer_id,"collection":"DeploymentNote","doc_ids":[doc_id]}})).await;
    assert_eq!(
        synced["outcome"]["missing_document_ids"],
        json!([]),
        "{synced}"
    );
    let read = ConfigAccess::local(local.node.clone())
        .execute(&format!(
            "{{DeploymentNote(filter: {{_docID: {{_eq: {}}}}}) {{text}}}}",
            quoted(doc_id)
        ))
        .await
        .unwrap();
    assert_eq!(
        read["data"]["DeploymentNote"][0]["text"],
        "Previous image remains pinned until review"
    );
    let absent_id = "bae-00000000-0000-5000-8000-000000000000";
    let partial = call(
        &tool,
        json!({"argv":["sync","documents"],"options":{"peer_id":peer_id,"collection":"DeploymentNote","doc_ids":[doc_id,absent_id]}}),
    )
    .await;
    assert_eq!(partial["outcome"]["observed_document_ids"], json!([doc_id]));
    assert_eq!(
        partial["outcome"]["missing_document_ids"],
        json!([absent_id])
    );
    assert!(partial["next_call"].is_object());
    call(
        &tool,
        json!({"argv":["pairings","revoke"],"options":{"peer_id":peer_id}}),
    )
    .await;
    reconcile_peer_tick(&admin, &store, &peer_id).await.unwrap();
    assert!(tool.overlay(&peer_id).await.unwrap().is_none());
    assert!(!store
        .load_applied(&peer_id)
        .await
        .unwrap()
        .state
        .collections
        .contains("DeploymentNote"));
    assert!(GraphqlEnrollmentStore::new(local.node.clone(), identity)
        .load_projection()
        .await
        .unwrap()
        .active
        .iter()
        .any(|e| e.request.request_id == enrollment.request_id));
}

#[tokio::test]
async fn node_requested_enrollment_is_approved_and_paired_by_the_offering_node() {
    let server = crate::support::test_p2p_db("engineer-enroll-server").await;
    let candidate = crate::support::test_p2p_db("engineer-enroll-candidate").await;
    for node in [&server.node, &candidate.node] {
        ConfigAccess::local(node.clone())
            .add_schema("type DeploymentNote @branchable { text: String }")
            .await
            .unwrap();
    }
    let (server_peer, _) = wait_for_peer_identity(&server.node).await;
    let (candidate_peer, _) = wait_for_peer_identity(&candidate.node).await;
    signed_enrollment_offer(
        &server.node,
        "engineer-enroll",
        "Engineer enrollment",
        server.node_identity.as_ref(),
        "challenge-engineer-enroll".into(),
    )
    .await;
    let scope = || EndpointScope::<String, ()>::only_units(["DeploymentNote".into()]);
    let server_tool = P2pTool::new(
        server.node.clone(),
        Some(server.node_identity.clone()),
        true,
        scope(),
    );
    let candidate_tool = P2pTool::new(
        candidate.node.clone(),
        Some(candidate.node_identity.clone()),
        true,
        scope(),
    );
    let server_readonly = P2pTool::new(
        server.node.clone(),
        Some(server.node_identity.clone()),
        false,
        scope(),
    );
    assert!(Tool::call(
        &server_readonly,
        serde_json::from_value(json!({"action":"enrollment_offer"})).unwrap()
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("mutations are disabled"));
    let offer = call(&server_tool, json!({"action":"enrollment_offer"})).await;
    assert_eq!(offer["outcome"]["peer_id"], server_peer);
    assert_eq!(offer["outcome"]["peer_did"], server.node_identity.did());
    assert!(offer["outcome"]["expires_at"].is_string());
    let offer_token = offer["outcome"]["offer_token"].as_str().unwrap();
    let request = json!({"action":"enrollment_request","offer_token":offer_token});
    let readonly = P2pTool::new(
        candidate.node.clone(),
        Some(candidate.node_identity.clone()),
        false,
        scope(),
    );
    assert!(
        Tool::call(&readonly, serde_json::from_value(request.clone()).unwrap())
            .await
            .unwrap_err()
            .to_string()
            .contains("mutations are disabled")
    );
    let requested = call(&candidate_tool, request.clone()).await;
    let request_id = requested["outcome"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(requested["outcome"]["peer_id"], server_peer);
    assert_eq!(requested["outcome"]["peer_did"], server.node_identity.did());
    let approve = requested["observations"]["approve_on_peer"].clone();
    let retried = call(&candidate_tool, request).await;
    assert_eq!(retried["outcome"]["request_id"], request_id);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let pending = call(&server_tool, json!({"action":"enrollment_pending"})).await;
        let listed = pending["outcome"]["pending"].as_array().unwrap();
        if let Some(entry) = listed.iter().find(|e| e["request_id"] == request_id) {
            assert_eq!(entry["peer_id"], candidate_peer);
            assert_eq!(entry["peer_did"], candidate.node_identity.did());
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "request never arrived: {pending}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let approved = call(&server_tool, approve).await;
    assert_eq!(approved["outcome"]["request_id"], request_id);
    let history = call(&server_readonly, json!({"action":"enrollment_history"})).await;
    let entry = &history["outcome"]["history"][0];
    assert_eq!(entry["request_id"], request_id, "{history}");
    assert_eq!(entry["state"], "approved");
    assert_eq!(entry["peer_did"], candidate.node_identity.did());
    assert_eq!(entry["decided_by"], server.node_identity.did());
    assert!(entry["decided_at"].is_string());
    let applied = call(
        &server_tool,
        json!({"action":"pairings_apply","peer_id":candidate_peer,"peer_did":candidate.node_identity.did(),"collections":["DeploymentNote"]}),
    )
    .await;
    assert_eq!(applied["outcome"]["unchanged"], false, "{applied}");
}

#[tokio::test]
async fn p2p_rejects_foreign_actor_and_preserves_another_overlays_owner() {
    let db = crate::support::test_p2p_db("engineer-p2p-owned").await;
    let access = ConfigAccess::local(db.node.clone());
    access.write("test.p2p.overlay", "mutation {create_DataPlanePairingDesired(input: {peer_id: \"foreign-route\", node_did: \"did:key:another\", source: \"another-owner\", collections: [\"DeploymentNote\"], replicator_addresses: null, template: \"app-collections\"}) {_docID}}").await.unwrap();
    let tool = P2pTool::new(
        db.node.clone(),
        Some(db.node_identity.clone()),
        true,
        EndpointScope::all(),
    );
    let result = Tool::call(
        &tool,
        serde_json::from_value(
            json!({"argv":["pairings","revoke"],"options":{"peer_id":"foreign-route"}}),
        )
        .unwrap(),
    )
    .await
    .unwrap_err();
    assert!(result.to_string().contains("another owner"));
    assert!(tool.overlay("foreign-route").await.unwrap().is_some());
    access.write("test.p2p.mixed", &format!("mutation {{create_DataPlanePairingDesired(input: {{peer_id: \"mixed-route\", node_did: {}, source: \"engineer\", collections: [\"DeploymentNote\", \"PrivateNote\"], replicator_addresses: null, template: \"app-collections\"}}) {{_docID}}}}", quoted(db.node_identity.did()))).await.unwrap();
    let scoped = P2pTool::new(
        db.node.clone(),
        Some(db.node_identity.clone()),
        true,
        EndpointScope::<String, ()>::only_units(["DeploymentNote".into()]),
    );
    let result = Tool::call(
        &scoped,
        serde_json::from_value(
            json!({"argv":["pairings","revoke"],"options":{"peer_id":"mixed-route"}}),
        )
        .unwrap(),
    )
    .await
    .unwrap_err();
    assert!(result.to_string().contains("outside the P2P grant"));
    assert!(scoped.overlay("mixed-route").await.unwrap().is_some());
    let key_dir = tempfile::tempdir().unwrap();
    let foreign = Arc::new(
        crate::identity::KeyIdentity::load_or_create(key_dir.path().join("foreign.key"), None)
            .unwrap(),
    );
    let tool = P2pTool::new(db.node.clone(), Some(foreign), true, EndpointScope::all());
    assert!(Tool::call(
        &tool,
        serde_json::from_value(json!({"argv":["status"]})).unwrap()
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("running node DID"));
}

#[tokio::test]
async fn help_and_scope_distinguish_capability_limits_from_collection_grants() {
    let db = crate::support::test_p2p_db("engineer-p2p-discovery").await;
    let tool = P2pTool::new(
        db.node.clone(),
        Some(db.node_identity.clone()),
        false,
        EndpointScope::<String, ()>::only_units(["DeploymentNote".into()]),
    );
    for (argv, resource) in [
        (json!(["help", "pairing"]), "pairings"),
        (json!(["pairings", "help"]), "pairings"),
        (json!(["help", "documents"]), "sync"),
    ] {
        let error = Tool::call(&tool, serde_json::from_value(json!({"argv":argv})).unwrap())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&format!("\"topic\":\"{resource}\"")),
            "{error}"
        );
    }
    let unknown = Tool::call(
        &tool,
        serde_json::from_value(json!({"argv":["replicators","list"]})).unwrap(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        unknown.contains("next call: p2p {\"action\":\"help\"}"),
        "{unknown}"
    );
    let scope = call(&tool, json!({"action":"help","topic":"scope"})).await;
    assert_eq!(scope["outcome"]["grant"]["mutations"], false);
    assert_eq!(
        scope["outcome"]["grant"]["application_collections"]["names"],
        json!(["DeploymentNote"])
    );
    assert!(scope["outcome"]["help"]
        .as_str()
        .unwrap()
        .contains("permanently excluded"));
    let forbidden = tool
        .checked_collections(
            json!({"collections":["OAuthCredential"]})
                .as_object()
                .unwrap(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(forbidden.contains("changing grants or ACP cannot enable"));
    assert!(!forbidden.contains("nonempty"));
    let denied = tool
        .checked_collections(json!({"collections":["PrivateNote"]}).as_object().unwrap())
        .await
        .unwrap_err()
        .to_string();
    assert!(denied.contains("outside this tool's P2P collection grant"));
    assert!(!denied.contains("never supported"));
    let empty = tool
        .checked_collections(json!({"collections":[]}).as_object().unwrap())
        .await
        .unwrap_err()
        .to_string();
    assert!(empty.contains("must contain application collection names"));
    assert!(!empty.contains("protocol"));
    let all = P2pTool::new(
        db.node.clone(),
        Some(db.node_identity.clone()),
        true,
        EndpointScope::all(),
    );
    for name in ["OAuthCredential", "AgentRequest", "InferenceBackend"] {
        assert!(all
            .checked_collections(json!({"collections":[name]}).as_object().unwrap())
            .await
            .is_err());
    }
    let status = call(&tool, json!({"action":"status"})).await;
    assert!(status["observations"]["sync"].get("pending_dags").is_some());
    assert!(status["observations"]["sync"]
        .get("car_served_cids")
        .is_none());
    let detailed = call(&tool, status["observations"]["details_call"].clone()).await;
    assert!(detailed["observations"]["sync"]
        .get("car_served_cids")
        .is_some());
}

#[tokio::test]
async fn typed_actions_are_the_closed_schema_and_argv_remains_accepted() {
    let db = crate::support::test_p2p_db("engineer-p2p-typed").await;
    let tool = P2pTool::new(
        db.node.clone(),
        Some(db.node_identity.clone()),
        true,
        EndpointScope::<String, ()>::only_units(["DeploymentNote".into()]),
    );
    let schema = Tool::definition(&tool, String::new()).await.parameters;
    assert_eq!(schema["required"], json!(["action"]));
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"]["action"]["enum"], json!(ACTIONS));
    assert!(schema["properties"].get("argv").is_none());
    for (name, property) in schema["properties"].as_object().unwrap() {
        assert!(
            property["description"].is_string(),
            "{name} has no description"
        );
    }
    for action in ACTIONS {
        assert!(
            Tool::definition(&tool, String::new())
                .await
                .description
                .contains(action),
            "{action} missing from the description"
        );
    }

    let typed = call(&tool, json!({"action":"status"})).await;
    let legacy = call(&tool, json!({"argv":["status"]})).await;
    assert_eq!(
        typed["outcome"]["listen_addresses"],
        legacy["outcome"]["listen_addresses"]
    );
    assert_eq!(
        typed["observations"]["details_call"],
        json!({"action":"status","details":true})
    );
    let help = call(&tool, json!({"action":"help","topic":"enrollment"})).await;
    assert!(help["outcome"]["help"]
        .as_str()
        .unwrap()
        .contains("enrollment_offer"));

    for (args, expected) in [
        (json!({"action":"enrollment_offers"}), "unknown action"),
        (json!({}), "action is required"),
        (json!({"action":"status","argv":["status"]}), "not both"),
        (
            json!({"action":"status","topic":"network"}),
            "topic applies only to action help",
        ),
        (
            json!({"action":"pairings_get","peer_id":"a","options":{"peer_id":"a"}}),
            "sent twice",
        ),
        (
            json!({"action":"enrollment_pending","request_id":"enroll-x"}),
            "unexpected field for action enrollment_pending",
        ),
        (
            json!({"action":"enrollment_approve"}),
            "request_id must be a nonempty string",
        ),
    ] {
        let error = Tool::call(&tool, serde_json::from_value(args.clone()).unwrap())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{args}: {error}");
        assert!(
            error.contains("next call: p2p {\"action\":\"help\""),
            "{args}: {error}"
        );
    }
}

#[tokio::test]
async fn deny_is_signed_and_a_shared_offer_names_the_single_use_rule() {
    let server = crate::support::test_p2p_db("engineer-deny-server").await;
    let first = crate::support::test_p2p_db("engineer-deny-first").await;
    let second = crate::support::test_p2p_db("engineer-deny-second").await;
    signed_enrollment_offer(
        &server.node,
        "engineer-deny",
        "Engineer deny",
        server.node_identity.as_ref(),
        "challenge-engineer-deny".into(),
    )
    .await;
    let scope = || EndpointScope::<String, ()>::only_units(["DeploymentNote".into()]);
    let tool = |db: &crate::support::TestDb| {
        P2pTool::new(
            db.node.clone(),
            Some(db.node_identity.clone()),
            true,
            scope(),
        )
    };
    let server_tool = tool(&server);
    let pending = |count: usize| {
        let server_tool = server_tool.clone();
        async move {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            loop {
                let listed = call(&server_tool, json!({"action":"enrollment_pending"})).await;
                if listed["outcome"]["pending"].as_array().unwrap().len() >= count
                    || std::time::Instant::now() > deadline
                {
                    return listed;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    };

    for valid_for_secs in [0, 3601] {
        let refused = Tool::call(
            &server_tool,
            serde_json::from_value(
                json!({"action":"enrollment_offer","valid_for_secs":valid_for_secs}),
            )
            .unwrap(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(refused.contains("between 1 and 3600 seconds"), "{refused}");
    }
    let offer = call(
        &server_tool,
        json!({"action":"enrollment_offer","valid_for_secs":1800}),
    )
    .await;
    let expires_at =
        chrono::DateTime::parse_from_rfc3339(offer["outcome"]["expires_at"].as_str().unwrap())
            .unwrap();
    let remaining = expires_at.with_timezone(&chrono::Utc) - chrono::Utc::now();
    assert!(
        remaining > chrono::Duration::minutes(29) && remaining <= chrono::Duration::minutes(30),
        "{offer}"
    );
    let token = offer["outcome"]["offer_token"].as_str().unwrap();
    let denied_id = call(
        &tool(&first),
        json!({"action":"enrollment_request","offer_token":token}),
    )
    .await["outcome"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    pending(1).await;
    let denied = call(
        &server_tool,
        json!({"action":"enrollment_deny","request_id":denied_id}),
    )
    .await;
    assert_eq!(denied["outcome"]["state"], "denied", "{denied}");
    let history = call(&server_tool, json!({"action":"enrollment_history"})).await;
    let entry = &history["outcome"]["history"][0];
    assert_eq!(entry["request_id"], denied_id, "{history}");
    assert_eq!(entry["state"], "denied");
    assert_eq!(entry["decided_by"], server.node_identity.did());
    let (first_peer, _) = wait_for_peer_identity(&first.node).await;
    let unenrolled = Tool::call(
        &server_tool,
        serde_json::from_value(json!({"action":"pairings_apply","peer_id":first_peer,"peer_did":first.node_identity.did(),"collections":["DeploymentNote"]})).unwrap(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        unenrolled.contains("each node enrolled in the other's network"),
        "{unenrolled}"
    );

    let shared = call(&server_tool, json!({"action":"enrollment_offer"})).await;
    let shared = shared["outcome"]["offer_token"].as_str().unwrap();
    let mut ids = Vec::new();
    for db in [&first, &second] {
        let requested = call(
            &tool(db),
            json!({"action":"enrollment_request","offer_token":shared}),
        )
        .await;
        ids.push(
            requested["outcome"]["request_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let error = loop {
        let error = Tool::call(
            &server_tool,
            serde_json::from_value(json!({"action":"enrollment_approve","request_id":ids[1]}))
                .unwrap(),
        )
        .await
        .unwrap_err()
        .to_string();
        if error.contains("challenge is reused") || std::time::Instant::now() > deadline {
            break error;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    assert!(error.contains("single use"), "{error}");
    assert!(
        error.contains("next call: p2p {\"action\":\"enrollment_offer\"}"),
        "{error}"
    );
}

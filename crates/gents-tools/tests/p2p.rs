use gents_loop::tool::Tool;
use gents_p2p::test_support::enrollment::{signed_enrollment_offer, wait_for_peer_identity};
use gents_tools::builtin::p2p::*;
use serde_json::{json, Value};
use std::sync::Arc;

async fn call(tool: &P2pTool, value: Value) -> Value {
    let output = Tool::call(tool, serde_json::from_value(value).unwrap())
        .await
        .unwrap();
    serde_json::from_str(&output).unwrap()
}

#[tokio::test]
async fn node_requested_enrollment_is_approved_by_the_offering_node() {
    let server = gents_session::test_support::test_p2p_db("engineer-enroll-server").await;
    let candidate = gents_session::test_support::test_p2p_db("engineer-enroll-candidate").await;
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
    let server_tool = P2pTool::new(
        server.node.clone(),
        Some(server.node_identity.clone()),
        true,
    );
    let candidate_tool = P2pTool::new(
        candidate.node.clone(),
        Some(candidate.node_identity.clone()),
        true,
    );
    let server_readonly = P2pTool::new(
        server.node.clone(),
        Some(server.node_identity.clone()),
        false,
    );
    assert!(Tool::call(
        &server_readonly,
        serde_json::from_value(json!({"action":"enrollment_offer"})).unwrap()
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("mutations are disabled"));
    assert!(Tool::call(
        &server_readonly,
        serde_json::from_value(json!({"action":"enrollment_renewal_policy", "auto_renew":true}))
            .unwrap()
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("mutations are disabled"));
    let policy = call(&server_tool, json!({"action":"enrollment_renewal_policy", "auto_renew":true, "lease_seconds": 60 * 86400})).await;
    assert_eq!(policy["outcome"]["auto_renew"], true);
    assert_eq!(policy["outcome"]["lease_seconds"], 60 * 86400);
    let disabled = call(
        &server_tool,
        json!({"action":"enrollment_renewal_policy", "auto_renew":false}),
    )
    .await;
    assert_eq!(disabled["outcome"]["auto_renew"], false);
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
    for invalid in [json!(0), json!(7_776_001), json!(-1), json!(1.5)] {
        let error = Tool::call(
            &server_tool,
            serde_json::from_value(json!({
                "action": "enrollment_approve", "request_id": request_id, "lease_seconds": invalid
            }))
            .unwrap(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("lease_seconds"), "{error}");
    }
    let mut approve = approve;
    approve["lease_seconds"] = json!(7_776_000);
    let refused = Tool::call(&server_tool, serde_json::from_value(approve).unwrap())
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains("no Agent identity"), "{refused}");
    gents_p2p::p2p_reconcile::GraphqlEnrollmentStore::new(
        server.node.clone(),
        server.node_identity.clone(),
    )
    .decide_request_with_lease(
        &request_id,
        gents_protocol::enrollment::EnrollmentDecisionKind::Approved,
        std::time::Duration::from_secs(7_776_000),
    )
    .await
    .unwrap();
    let history = call(&server_readonly, json!({"action":"enrollment_history"})).await;
    let entry = &history["outcome"]["history"][0];
    assert_eq!(entry["request_id"], request_id, "{history}");
    assert_eq!(entry["state"], "approved");
    assert_eq!(entry["peer_did"], candidate.node_identity.did());
    assert_eq!(entry["decided_by"], server.node_identity.did());
    assert!(entry["decided_at"].is_string());
    let decided =
        chrono::DateTime::parse_from_rfc3339(entry["decided_at"].as_str().unwrap()).unwrap();
    let expires =
        chrono::DateTime::parse_from_rfc3339(entry["authorization_expires_at"].as_str().unwrap())
            .unwrap();
    assert_eq!((expires - decided).num_seconds(), 7_776_000);
    // P2P carries no application pairing: the approved enrollment stands and
    // the removed pairing action is unknown.
    let refused = Tool::call(
        &server_tool,
        serde_json::from_value(json!({"action":"pairings_apply","peer_id":candidate_peer}))
            .unwrap(),
    )
    .await
    .unwrap_err();
    assert!(refused.to_string().contains("unknown action"), "{refused}");
}

#[tokio::test]
async fn p2p_administration_admits_as_the_lean_model() {
    let db = gents_session::test_support::test_p2p_db("engineer-p2p-owned").await;
    let (transport_peer, _) = wait_for_peer_identity(&db.node).await;
    let key_dir = tempfile::tempdir().unwrap();
    let foreign: Arc<dyn gents_store::identity::NodeIdentity> = Arc::new(
        gents_store::identity::KeyIdentity::load_or_create(
            key_dir.path().join("foreign.key"),
            None,
        )
        .unwrap(),
    );
    for case in gents_lean_vocab::families::p2p_administration_admit::cases::<
        String,
        String,
        String,
        String,
    >() {
        let input = &case.input;
        let identity = match input.actor.as_str() {
            "absent" => None,
            "node" => {
                Some(db.node_identity.clone() as Arc<dyn gents_store::identity::NodeIdentity>)
            }
            "foreign" => Some(foreign.clone()),
            other => panic!("unknown actor {other}"),
        };
        let tool = P2pTool::new(db.node.clone(), identity, true);
        let peer = match input.peer.as_str() {
            "transport_id" => transport_peer.clone(),
            "multiaddress" => "/ip4/127.0.0.1/tcp/4001".to_owned(),
            other => panic!("unknown peer {other}"),
        };
        let arguments = match input.command.as_str() {
            "help" => json!({"argv":["help"]}),
            "status" => json!({"argv":["status"]}),
            "network_get" => json!({"argv":["network","get"],"options":{"peer_id":peer}}),
            other => panic!("unknown command {other}"),
        };
        let outcome = match Tool::call(&tool, serde_json::from_value(arguments).unwrap()).await {
            Ok(_) => "answered",
            Err(error) => {
                let text = error.to_string();
                if text.contains("running node identity is unavailable") {
                    "identity_unavailable"
                } else if text.contains("requires the running node DID") {
                    "foreign_actor"
                } else if text.contains("invalid transport peer ID") {
                    assert!(
                        text.contains(r#"next call: p2p {"action":"network_list"}"#),
                        "{text}"
                    );
                    "malformed_peer"
                } else {
                    panic!("{input:?}: {text}")
                }
            }
        };
        case.check(outcome.to_string());
    }
}

#[tokio::test]
async fn help_and_scope_name_discovery_and_enrollment_as_the_only_capabilities() {
    let db = gents_session::test_support::test_p2p_db("engineer-p2p-discovery").await;
    let tool = P2pTool::new(db.node.clone(), Some(db.node_identity.clone()), false);
    for argv in [
        json!(["help", "pairings"]),
        json!(["help", "sync"]),
        json!(["help", "documents"]),
    ] {
        let error = Tool::call(&tool, serde_json::from_value(json!({"argv":argv})).unwrap())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("\"topic\":\"scope\""), "{error}");
    }
    for argv in [
        json!(["replicators", "list"]),
        json!(["pairings", "list"]),
        json!(["sync", "documents"]),
    ] {
        let unknown = Tool::call(&tool, serde_json::from_value(json!({"argv":argv})).unwrap())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            unknown.contains("next call: p2p {\"action\":\"help\"}"),
            "{unknown}"
        );
    }
    let scope = call(&tool, json!({"action":"help","topic":"scope"})).await;
    assert_eq!(scope["outcome"]["grant"], json!({"mutations": false}));
    assert!(scope["outcome"]["help"]
        .as_str()
        .unwrap()
        .contains("discovery and enrollment only"));
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
    let db = gents_session::test_support::test_p2p_db("engineer-p2p-typed").await;
    let tool = P2pTool::new(db.node.clone(), Some(db.node_identity.clone()), true);
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
            json!({"action":"network_get","peer_id":"a","options":{"peer_id":"a"}}),
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
    let server = gents_session::test_support::test_p2p_db("engineer-deny-server").await;
    let first = gents_session::test_support::test_p2p_db("engineer-deny-first").await;
    let second = gents_session::test_support::test_p2p_db("engineer-deny-second").await;
    signed_enrollment_offer(
        &server.node,
        "engineer-deny",
        "Engineer deny",
        server.node_identity.as_ref(),
        "challenge-engineer-deny".into(),
    )
    .await;
    let tool = |db: &gents_inference::test_support::TestDb| {
        P2pTool::new(db.node.clone(), Some(db.node_identity.clone()), true)
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

    for valid_for_secs in [0, 86401] {
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
        assert!(refused.contains("between 1 and 86400 seconds"), "{refused}");
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

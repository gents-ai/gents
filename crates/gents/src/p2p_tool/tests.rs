use super::*;
use crate::agent::p2p_reconcile::{
    reconcile_peer_tick, EnrollmentEndpointEntry, GraphqlPairingStateStore, PairingStateStore,
};
use crate::support::enrollment::{authorize_enrollment_peer, wait_for_peer_identity};

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
        ConfigAccess::Local(node.clone())
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
    ConfigAccess::Local(remote.node.clone()).write("test.p2p.note", "mutation {create_DeploymentNote(input: {text: \"Previous image remains pinned until review\"}) {_docID}}").await.unwrap();
    let document = ConfigAccess::Local(remote.node.clone())
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
    let read = ConfigAccess::Local(local.node.clone())
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
async fn p2p_rejects_foreign_actor_and_preserves_another_overlays_owner() {
    let db = crate::support::test_p2p_db("engineer-p2p-owned").await;
    let access = ConfigAccess::Local(db.node.clone());
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
            error.contains(&format!("[\"help\",\"{resource}\"]")),
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
        unknown.contains("next call: p2p {\"argv\":[\"help\"]}"),
        "{unknown}"
    );
    let scope = call(&tool, json!({"argv":["help","scope"]})).await;
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
    let status = call(&tool, json!({"argv":["status"]})).await;
    assert!(status["observations"]["sync"].get("pending_dags").is_some());
    assert!(status["observations"]["sync"]
        .get("car_served_cids")
        .is_none());
    let detailed = call(&tool, status["observations"]["details_call"].clone()).await;
    assert!(detailed["observations"]["sync"]
        .get("car_served_cids")
        .is_some());
}

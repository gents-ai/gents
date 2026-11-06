mod support;

use gents::defra_node;
use gents::runtime::agent::p2p_reconcile::{
    EmbeddedRemoteP2pAdmin, PairingFilters, RemoteP2pAdmin,
};
use gents::store::config_client::{ConfigAccess, NodePermission};
use gents::store::graphql::escape_graphql_string;
use gents::{KeyIdentity, NodeIdentity};
use std::{sync::Arc, time::Duration};

struct TestNode {
    node: Arc<defra_node::Node>,
    _directory: tempfile::TempDir,
}

fn client_http(endpoint: &str, identity: &KeyIdentity) -> ConfigAccess {
    ConfigAccess::Graphql(
        gents::store::config_client::GraphqlEndpoint::as_principal(endpoint, identity.did())
            .with_client_signing_capability(identity.client_signing_capability().unwrap())
            .unwrap(),
    )
}

async fn runtime_http_test_node(identity: Arc<KeyIdentity>) -> (TestNode, String) {
    let directory = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let node = Arc::new(
        defra_node::Node::builder()
            .data_path(directory.path())
            .with_node_identity_did(identity.did())
            .with_node_acp_enabled()
            .with_document_projector(
                gents::store::local::document_projector(
                    identity.clone(),
                    gents::tools::surface::ToolCeiling::meta_only(),
                )
                .unwrap(),
            )
            .with_p2p(support::test_p2p_config(
                &Default::default(),
                directory.path(),
            ))
            .with_http(defra_node::HttpConfig::with_listener(listener.into_std().unwrap()).unwrap())
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
    (
        TestNode {
            node,
            _directory: directory,
        },
        endpoint,
    )
}

#[tokio::test]
async fn private_client_journal_obeys_native_policy_and_refuses_exact_peer_pull() {
    let keys = tempfile::tempdir().unwrap();
    let owner =
        Arc::new(KeyIdentity::load_or_create(keys.path().join("client.key"), None).unwrap());
    let other = Arc::new(KeyIdentity::load_or_create(keys.path().join("other.key"), None).unwrap());
    let runtime = KeyIdentity::load_or_create(keys.path().join("runtime.key"), None).unwrap();
    let agent = KeyIdentity::load_or_create(keys.path().join("agent.key"), None).unwrap();
    let (source_test, endpoint) = runtime_http_test_node(owner.clone()).await;
    let source = &source_test.node;
    let actor = ConfigAccess::local_as(source.clone(), owner.did().parse().unwrap());
    for identity in [other.as_ref(), &runtime, &agent] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::CollectionGet,
        ] {
            actor
                .grant_node_permission(&identity.did().parse().unwrap(), permission)
                .await
                .unwrap();
        }
    }
    let created = actor.write("test.private_journal.create", r#"mutation {
        create_PendingChatSubmission(input: {request_id: "private-request", payload: "client secret"}) {
            _docID _version { cid }
        }
    }"#).await.unwrap();
    let rows = gents::store::graphql::mutation_documents(&created, "create_PendingChatSubmission")
        .unwrap()
        .unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    let doc_id = row["_docID"].as_str().unwrap();
    let doc_id_escaped = escape_graphql_string(doc_id);
    let cid = row["_version"][0]["cid"].as_str().unwrap();
    assert_eq!(
        source.verified_block_signer_did(cid).await.unwrap(),
        owner.did()
    );

    let snapshot = tokio::task::spawn_blocking(
        gents_lean_contract::load_contract_snapshot::<serde_json::Value>,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        snapshot["identity_private_journal_policy"]["policy_yaml"].as_str(),
        Some(gents_protocol::schemas::CLIENT_JOURNAL_POLICY),
    );
    let checks = snapshot["identity_private_journal_policy"]["checks"]
        .as_array()
        .unwrap();
    for case in checks {
        let identity = match case["role"].as_str().unwrap() {
            "owner" => Some(owner.as_ref()),
            "other" => Some(other.as_ref()),
            "runtime" => Some(&runtime),
            "agent" => Some(&agent),
            "anonymous" => None,
            role => panic!("unknown modeled journal actor: {role}"),
        };
        let permission = match case["action"].as_str().unwrap() {
            "read" => defra_node::DocumentPermission::Read,
            "update" => defra_node::DocumentPermission::Update,
            "delete" => defra_node::DocumentPermission::Delete,
            action => panic!("unknown modeled journal action: {action}"),
        };
        let native_identity = match identity {
            Some(key) => defra_node::AcpIdentity::Authenticated(key.did().parse().unwrap()),
            None => defra_node::AcpIdentity::Anonymous,
        };
        let allowed = source
            .document_acp()
            .check_doc_access(
                &native_identity,
                permission,
                gents_protocol::schemas::CLIENT_JOURNAL_POLICY_ID,
                gents_protocol::schemas::CLIENT_JOURNAL_POLICY_RESOURCE,
                doc_id,
            )
            .await
            .unwrap();
        assert_eq!(allowed, case["expected"].as_bool().unwrap(), "{case}");
        if case["action"] == "read" {
            let http = match identity {
                Some(key) => client_http(&endpoint, key),
                None => ConfigAccess::graphql(&endpoint),
            };
            let response = http.execute(&format!(r#"{{ PendingChatSubmission(filter: {{_docID: {{_eq: "{doc_id_escaped}"}}}}) {{ _docID payload }} }}"#)).await;
            let visible = match response {
                Ok(response) => response["data"]["PendingChatSubmission"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["_docID"] == doc_id && row["payload"] == "client secret"),
                Err(error) if identity.is_none() => {
                    assert!(error.to_string().contains("not authorized"), "{error:#}");
                    false
                }
                Err(error) => panic!("authenticated journal read failed: {error:#}"),
            };
            assert_eq!(visible, case["expected"].as_bool().unwrap(), "HTTP: {case}");
        }
    }
    let outsider = client_http(&endpoint, &other);
    let denied = outsider.write("test.private_journal.deny_update", &format!(r#"mutation {{ update_PendingChatSubmission(docID: "{doc_id_escaped}", input: {{payload: "stolen"}}) {{ _docID }} }}"#)).await.unwrap();
    let changed =
        gents::store::graphql::mutation_documents(&denied, "update_PendingChatSubmission")
            .unwrap()
            .unwrap();
    assert!(
        changed.is_empty(),
        "outsider changed a private journal: {denied}"
    );
    let owner_result = actor
        .execute(&format!(
            r#"{{ PendingChatSubmission(filter: {{_docID: {{_eq: "{doc_id_escaped}"}}}}) {{ payload }} }}"#
        ))
        .await
        .unwrap();
    assert_eq!(
        owner_result["data"]["PendingChatSubmission"][0]["payload"],
        "client secret"
    );

    let other_http = client_http(&endpoint, &other);
    let owner_http = client_http(&endpoint, &owner);
    let mut updates = other_http
        .subscribe("subscription { PendingChatSubmission { _docID payload } }")
        .await
        .unwrap();
    let mut owner_updates = owner_http
        .subscribe("subscription { PendingChatSubmission { _docID payload } }")
        .await
        .unwrap();
    actor
        .write(
            "test.private_journal.update",
            &format!(r#"mutation {{ update_PendingChatSubmission(docID: "{doc_id_escaped}", input: {{payload: "private update"}}) {{ _docID }} }}"#),
        )
        .await
        .unwrap();
    let observed = tokio::time::timeout(Duration::from_secs(10), owner_updates.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        observed["data"]["PendingChatSubmission"][0]["_docID"],
        doc_id
    );
    assert_eq!(
        observed["data"]["PendingChatSubmission"][0]["payload"],
        "private update"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(1), updates.recv())
            .await
            .is_err(),
        "another client received a private journal subscription event"
    );
    drop(updates);

    let (outsider_test, _) = runtime_http_test_node(other.clone()).await;
    let receiver = &outsider_test.node;
    let receiver_admin_access =
        ConfigAccess::local_as(receiver.clone(), other.did().parse().unwrap());
    for permission in [NodePermission::DocumentRead, NodePermission::CollectionGet] {
        receiver_admin_access
            .grant_node_permission(&owner.did().parse().unwrap(), permission)
            .await
            .unwrap();
    }
    receiver
        .document_acp()
        .register_doc_object(
            &owner.did().parse().unwrap(),
            gents_protocol::schemas::CLIENT_JOURNAL_POLICY_ID,
            gents_protocol::schemas::CLIENT_JOURNAL_POLICY_RESOURCE,
            doc_id,
        )
        .await
        .unwrap();
    let source_admin = EmbeddedRemoteP2pAdmin::new(source.clone());
    let receiver_admin = EmbeddedRemoteP2pAdmin::new(receiver.clone());
    let public = actor
        .write(
            "test.private_journal.public_probe",
            r#"mutation { create_PeerRegistry(input: {peer_id: "journal-public-probe", node_did: "did:key:probe", addresses: ["probe"]}) { _docID } }"#,
        )
        .await
        .unwrap();
    let public_id = gents::store::graphql::created_doc_id(&public, "PeerRegistry").unwrap();
    let public_id_escaped = escape_graphql_string(&public_id);
    let (_, source_address) = support::enrollment::wait_for_peer_identity(source).await;
    let (_, receiver_address) = support::enrollment::wait_for_peer_identity(receiver).await;
    let source_addresses = vec![source_address];
    let receiver_addresses = vec![receiver_address];
    receiver_admin.connect(&source_addresses).await.unwrap();
    support::p2p_waits::wait_for_connected_peer(receiver).await;
    source_admin
        .add_replicator(
            &receiver_addresses,
            &["PeerRegistry".into()],
            &PairingFilters::default(),
        )
        .await
        .unwrap();
    receiver
        .p2p()
        .unwrap()
        .sync_documents(
            "PeerRegistry",
            vec![public_id.clone()],
            Some(Duration::from_secs(5)),
        )
        .await
        .unwrap();
    let public_on_receiver = ConfigAccess::local_as(receiver.clone(), other.did().parse().unwrap())
        .execute(&format!(
            r#"{{ PeerRegistry(filter: {{_docID: {{_eq: "{public_id_escaped}"}}}}) {{ peer_id }} }}"#
        ))
        .await
        .unwrap();
    assert_eq!(
        public_on_receiver["data"]["PeerRegistry"][0]["peer_id"],
        "journal-public-probe"
    );
    let pull = receiver
        .p2p()
        .unwrap()
        .sync_documents(
            "PendingChatSubmission",
            vec![doc_id.to_owned()],
            Some(Duration::from_secs(5)),
        )
        .await;
    let owner_on_receiver = ConfigAccess::local_as(receiver.clone(), owner.did().parse().unwrap());
    let copied = owner_on_receiver
        .execute(&format!(
            r#"{{ PendingChatSubmission(filter: {{_docID: {{_eq: "{doc_id_escaped}"}}}}) {{ _docID }} }}"#
        ))
        .await
        .unwrap();
    assert!(
        copied["data"]["PendingChatSubmission"]
            .as_array()
            .unwrap()
            .is_empty(),
        "an unrelated transport peer fetched a private journal: {pull:?}"
    );
    assert!(
        receiver
            .authorized_signed_block_bytes(cid, Some(owner.did()))
            .await
            .is_err(),
        "an unrelated transport peer obtained the journal block: {pull:?}"
    );
    source.shutdown().await.unwrap();
    receiver.shutdown().await.unwrap();
}

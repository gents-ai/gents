use super::*;
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
async fn agent_pairing_overlay_has_native_owner_and_node_parent() -> anyhow::Result<()> {
    use crate::lean_vocab_test::lean_identity_household_cases;
    use crate::store::config_client::{
        GraphqlEndpoint, NodePermission, PairingObservation, PairingReadReason,
    };
    let model = lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .context("missing configuration model")?;
    let directory = tempfile::tempdir()?;
    let runtime_path = directory.path().join("runtime");
    let runtime = Arc::new(crate::KeyIdentity::load_or_create(&runtime_path, None)?);
    let runtime_key =
        crate::store::identity::load_file_identity(&runtime_path)?.private_key_bytes();
    let user = crate::KeyIdentity::load_or_create(directory.path().join("user"), None)?;
    let outsider = crate::KeyIdentity::load_or_create(directory.path().join("outsider"), None)?;
    let binding = crate::store::identity::provision_agent_identity(runtime.as_ref())?;
    let agent = crate::store::identity::resolve_agent_identity(runtime.as_ref(), &binding)?;
    let sibling_binding = crate::store::identity::provision_agent_identity(runtime.as_ref())?;
    let sibling =
        crate::store::identity::resolve_agent_identity(runtime.as_ref(), &sibling_binding)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(runtime.did())
            .with_signing_key(defra_node::SigningKey::Ed25519(runtime_key.clone()))
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .with_storage_backend(defra_node::StorageBackend::Regolith)
            .data_path(directory.path().join("database"))
            .build()
            .await?,
    );
    node.add_dac_policy_with_counter_as(
        &defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned())),
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await?;
    node.add_collection(gents_protocol::schemas::NODE).await?;
    node.add_collection(gents_protocol::schemas::DATA_PLANE_PAIRING_DESIRED)
        .await?;
    node.add_collection(gents_protocol::schemas::PEER_PAIRING_APPLIED)
        .await?;
    let runtime_access = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
    for actor in [user.did(), agent.did(), sibling.did(), outsider.did()] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
            NodePermission::CollectionGet,
        ] {
            runtime_access
                .grant_node_permission(&identity::Did::new(actor)?, permission)
                .await?;
        }
    }
    let http = |actor: &crate::KeyIdentity| -> anyhow::Result<ConfigAccess> {
        Ok(ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), actor.did())
                .with_client_signing_capability(actor.client_signing_capability()?)?,
        ))
    };
    let user_access = http(&user)?;
    let outsider_access = http(&outsider)?;
    let parent = user_access
        .write(
            "test.overlay.node",
            &format!(
                r#"mutation {{ create_Node(input: {{node_did:"{}"}}) {{_docID}} }}"#,
                escape_graphql_string(runtime.did())
            ),
        )
        .await?;
    let parent = gents_protocol::graphql::extract_mutation_doc_id(&parent, "Node")?;
    user_access
        .add_actor_relationship(
            "Node",
            &parent,
            "runtime_reader",
            &identity::Did::new(runtime.did())?,
        )
        .await?;
    let agent_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did())?,
        agent.as_ref(),
    )?;
    let sibling_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(sibling.did())?,
        sibling.as_ref(),
    )?;
    let tool = P2pTool::new(
        node.clone(),
        Some(runtime.clone()),
        true,
        EndpointScope::all(),
    )
    .with_invoking_agent(Some(agent.clone()));
    let applied = runtime_access
        .transact_write("test.tool.applied", |txn| {
            let actor = identity::Did::new(runtime.did()).unwrap();
            Box::pin(async move {
                let response = txn
                    .execute(&format!(
                        r#"mutation {{ create_PeerPairingApplied(input: {{
                peer_id: "peer", node_did: "{}", parent_linked: false
            }}) {{_docID}} }}"#,
                        escape_graphql_string(actor.as_str())
                    ))
                    .await?;
                let doc = gents_protocol::graphql::extract_mutation_doc_id(
                    &response,
                    "PeerPairingApplied",
                )?;
                crate::store::config_client::maybe_link_deferred_operation_in_txn(
                    txn,
                    "PeerPairingApplied",
                    &doc,
                    &actor,
                )
                .await?;
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await?;
    let sibling_tool = P2pTool::new(
        node.clone(),
        Some(runtime.clone()),
        true,
        EndpointScope::all(),
    )
    .with_invoking_agent(Some(sibling.clone()));
    let actor = identity::Did::new(agent.did())?;
    let policy_cases =
        &crate::lean_vocab_test::lean_contract_snapshot().identity_pairing_reader_policy_cases;
    let expected = |name: &str| {
        policy_cases
            .iter()
            .find(|case| case["name"] == name)
            .unwrap()["read"]
            .as_bool()
            .unwrap()
    };
    assert_eq!(
        !tool
            .rows("PeerPairingApplied", None, "_docID")
            .await?
            .is_empty(),
        expected("both-revoked")
    );
    user_access
        .set_pairing_reader(
            PairingObservation::Applied,
            &applied,
            PairingReadReason::ToolView,
            &actor,
            true,
        )
        .await?;
    assert_eq!(
        !tool
            .rows("PeerPairingApplied", None, "_docID")
            .await?
            .is_empty(),
        expected("tool-only")
    );
    assert_eq!(
        !sibling_tool
            .rows("PeerPairingApplied", None, "_docID")
            .await?
            .is_empty(),
        expected("sibling")
    );
    user_access
        .set_pairing_reader(
            PairingObservation::Applied,
            &applied,
            PairingReadReason::ToolView,
            &actor,
            false,
        )
        .await?;
    assert_eq!(
        !tool
            .rows("PeerPairingApplied", None, "_docID")
            .await?
            .is_empty(),
        expected("both-revoked")
    );
    let desired = json!({"peer_id":"peer", "node_did":runtime.did(), "actor_did":agent.did(),
        "collections":null, "replicator_addresses":null, "template":"app-collections", "source":OVERLAY_SOURCE});
    let fields = format!("{{peer_id: {}, node_did: {}, collections: null, replicator_addresses: null, template: \"app-collections\", source: {}}}", quoted("peer"), quoted(runtime.did()), quoted(OVERLAY_SOURCE));
    assert!(
        !tool
            .mutate_overlay("peer", Some(desired.clone()), Some(fields.clone()))
            .await?
    );
    let query = "{ DataPlanePairingDesired {_docID actor_did node_did template} }";
    let created = agent_access.execute(query).await?;
    let doc = created["data"]["DataPlanePairingDesired"][0]["_docID"]
        .as_str()
        .context("overlay ID")?
        .to_owned();
    let query = format!(
        r#"{{ DataPlanePairingDesired(filter: {{_docID: {{_eq:"{}"}}}}) {{_docID template}} }}"#,
        escape_graphql_string(&doc)
    );
    for revoked in [false, true] {
        if revoked {
            agent_access
                .delete_resource_relationship(
                    "DataPlanePairingDesired",
                    &doc,
                    "parent",
                    "nodes",
                    &parent,
                )
                .await?;
        }
        for case in model
            .pairing_overlay_cases
            .iter()
            .filter(|case| case["revoked"] == revoked)
        {
            let access = match case["actor"].as_str().unwrap() {
                "user" => &user_access,
                "runtime" => &runtime_access,
                "agent" => &agent_access,
                "sibling" => &sibling_access,
                "outsider" => &outsider_access,
                _ => unreachable!(),
            };
            let expected = case["expected"].as_bool().unwrap();
            let actual = if case["permission"] == "read" {
                !gents_protocol::graphql::graphql_rows_from_response(
                    &access.execute(&query).await?,
                    "DataPlanePairingDesired",
                )
                .is_empty()
            } else {
                let before = agent_access.execute(&query).await?;
                let mutation = format!(
                    r#"mutation {{ update_DataPlanePairingDesired(docID:"{}", input: {{template:"{}"}}) {{_docID}} }}"#,
                    escape_graphql_string(&doc),
                    escape_graphql_string(&format!(
                        "{}-{revoked}",
                        case["actor"].as_str().unwrap()
                    ))
                );
                let updated = match access.write("test.overlay.update", &mutation).await {
                    Ok(value) => !gents_protocol::graphql::graphql_rows_from_response(
                        &value,
                        "update_DataPlanePairingDesired",
                    )
                    .is_empty(),
                    Err(error) if error.is_permission_denied() || error.is_graphql_rejection() => {
                        false
                    }
                    Err(error) => return Err(error.into()),
                };
                if !expected {
                    assert_eq!(agent_access.execute(&query).await?, before);
                }
                updated
            };
            assert_eq!(actual, expected, "{case}");
        }
    }
    drop(tool);
    drop(sibling_tool);
    drop(agent_access);
    drop(sibling_access);
    drop(runtime_access);
    drop(user_access);
    drop(outsider_access);
    node.shutdown().await?;
    drop(node);
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let reopened = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(runtime.did())
            .with_signing_key(defra_node::SigningKey::Ed25519(runtime_key))
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .with_storage_backend(defra_node::StorageBackend::Regolith)
            .data_path(directory.path().join("database"))
            .build()
            .await?,
    );
    let tool = P2pTool::new(
        reopened.clone(),
        Some(runtime.clone()),
        true,
        EndpointScope::all(),
    )
    .with_invoking_agent(Some(agent.clone()));
    tool.mutate_overlay("peer", Some(desired), Some(fields))
        .await?;
    let user_access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
            .with_client_signing_capability(user.client_signing_capability()?)?,
    );
    assert!(gents_protocol::graphql::graphql_rows_from_response(
        &user_access.execute(&query).await?,
        "DataPlanePairingDesired"
    )
    .is_empty());
    reopened.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn native_pairing_sync_and_revoke_preserve_enrollment_and_scope() {
    let local = crate::support::test_p2p_db("engineer-p2p-local").await;
    let remote = crate::support::test_p2p_db("engineer-p2p-remote").await;
    let identity = local.node_identity.clone();
    for node in [&local.node, &remote.node] {
        ConfigAccess::local(node.clone())
            .add_collection("type DeploymentNote @branchable { text: String }")
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
    // Overlay reads are signed by the invoking Agent, never by the node.
    let agent_binding =
        crate::store::identity::provision_agent_identity(identity.as_ref()).unwrap();
    let agent =
        crate::store::identity::resolve_agent_identity(identity.as_ref(), &agent_binding).unwrap();
    // No projection pass runs here, so the node admin grants the baseline
    // every live Agent document projects.
    crate::store::identity::GrantSet::AgentBaseline
        .grant(
            &ConfigAccess::local_as(local.node.clone(), identity.did().parse().unwrap()),
            &agent_binding.agent_did.parse().unwrap(),
        )
        .await
        .unwrap();
    let tool = P2pTool::new(
        local.node.clone(),
        Some(identity.clone()),
        true,
        EndpointScope::<String, ()>::only_units(["DeploymentNote".into()]),
    )
    .with_invoking_agent(Some(agent));
    let options = json!({"peer_id":peer_id,"peer_did":remote.node_identity.did(),"collections":["DeploymentNote"]});
    // Application collections are refused for preview and apply alike.
    let preview = Tool::call(
        &tool,
        serde_json::from_value(json!({"argv":["pairings","preview"],"options":options})).unwrap(),
    )
    .await
    .unwrap_err();
    assert!(
        preview.to_string().contains("protocol collections"),
        "{preview}"
    );
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
        serde_json::from_value(json!({"argv":["pairings","revoke"],"options":{"peer_id":peer_id}}))
            .unwrap()
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
    // Application collections do not sync over P2P: pairing refuses them and
    // leaves enrollment and the overlay untouched.
    let refused = Tool::call(
        &tool,
        serde_json::from_value(json!({"argv":["pairings","apply"],"options":options})).unwrap(),
    )
    .await
    .unwrap_err();
    assert!(
        refused.to_string().contains("protocol collections"),
        "{refused}"
    );
    assert!(tool.overlay(&peer_id).await.unwrap().is_none());
    assert!(
        GraphqlEnrollmentStore::new(local.node.clone(), identity.clone())
            .load_projection()
            .await
            .unwrap()
            .active
            .iter()
            .any(|e| e.request.request_id == enrollment.request_id)
    );
}

#[tokio::test]
async fn node_requested_enrollment_is_approved_and_paired_by_the_offering_node() {
    let server = crate::support::test_p2p_db("engineer-enroll-server").await;
    let candidate = crate::support::test_p2p_db("engineer-enroll-candidate").await;
    for node in [&server.node, &candidate.node] {
        ConfigAccess::local(node.clone())
            .add_collection("type DeploymentNote @branchable { text: String }")
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
    // Application collections do not sync over P2P: the approved enrollment
    // stands, but pairing for DeploymentNote is refused.
    let refused = Tool::call(
        &server_tool,
        serde_json::from_value(json!({"action":"pairings_apply","peer_id":candidate_peer,"peer_did":candidate.node_identity.did(),"collections":["DeploymentNote"]})).unwrap(),
    )
    .await
    .unwrap_err();
    assert!(
        refused.to_string().contains("protocol collections"),
        "{refused}"
    );
}

#[tokio::test]
async fn p2p_rejects_foreign_actor_and_preserves_another_overlays_owner() {
    let db = crate::support::test_p2p_db("engineer-p2p-owned").await;
    let access = ConfigAccess::local(db.node.clone());
    let agent_binding =
        crate::store::identity::provision_agent_identity(db.node_identity.as_ref()).unwrap();
    let agent =
        crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &agent_binding)
            .unwrap();
    access.write("test.p2p.overlay", "mutation {create_DataPlanePairingDesired(input: {peer_id: \"foreign-route\", node_did: \"did:key:another\", source: \"another-owner\", collections: [\"DeploymentNote\"], replicator_addresses: null, template: \"app-collections\"}) {_docID}}").await.unwrap();
    let tool = P2pTool::new(
        db.node.clone(),
        Some(db.node_identity.clone()),
        true,
        EndpointScope::all(),
    )
    .with_invoking_agent(Some(agent.clone()));
    let result = Tool::call(
        &tool,
        serde_json::from_value(
            json!({"argv":["pairings","revoke"],"options":{"peer_id":"foreign-route"}}),
        )
        .unwrap(),
    )
    .await
    .unwrap_err();
    assert!(result.to_string().contains("another owner"), "{result}");
    assert!(tool.overlay("foreign-route").await.unwrap().is_some());
    access.write("test.p2p.mixed", &format!("mutation {{create_DataPlanePairingDesired(input: {{peer_id: \"mixed-route\", node_did: {}, source: \"engineer\", collections: [\"DeploymentNote\", \"PrivateNote\"], replicator_addresses: null, template: \"app-collections\"}}) {{_docID}}}}", quoted(db.node_identity.did()))).await.unwrap();
    let scoped = P2pTool::new(
        db.node.clone(),
        Some(db.node_identity.clone()),
        true,
        EndpointScope::<String, ()>::only_units(["DeploymentNote".into()]),
    )
    .with_invoking_agent(Some(agent.clone()));
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
        crate::store::identity::KeyIdentity::load_or_create(
            key_dir.path().join("foreign.key"),
            None,
        )
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
    // Application collections are refused before the grant is consulted, so an
    // in-grant or out-of-grant collection gets the same refusal.
    assert!(denied.contains("protocol collections"), "{denied}");
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

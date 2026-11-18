use super::*;
use crate::config_client::{
    client_authentication::runtime_authentication, runtime_endpoints::RuntimeEndpoints,
    ClientSigningCapability,
};
use anyhow::Context;
use defra_node::{
    EmbeddedVeraActor, EmbeddedVeraConfig, EmbeddedVeraDeployment, EmbeddedVeraPolicy,
};
use gents_protocol::client_authentication::ClientAuthentication;
use identity::{FullIdentity as _, RawIdentity};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct NoToolSources;

impl crate::local::ToolSourcePolicy for NoToolSources {
    fn node_operation_sources(
        &self,
        _: &serde_json::Value,
        _: &[serde_json::Value],
    ) -> anyhow::Result<Vec<crate::local::ProjectedOperationSource>> {
        Ok(Vec::new())
    }

    fn plugin_refs(
        &self,
        _: &serde_json::Value,
    ) -> anyhow::Result<Vec<crate::local::ProjectedPluginRef>> {
        Ok(Vec::new())
    }

    fn selected_tools(&self, _: &str, _: &serde_json::Value) -> Option<String> {
        None
    }
}

fn held_client() -> anyhow::Result<(RawIdentity, ClientSigningCapability)> {
    let key = RawIdentity::from_private_key(crypto::generate_ed25519()?)?;
    let capability = ClientSigningCapability::from_ed25519_key(&key.private_key_bytes())?;
    Ok((key, capability))
}

async fn credential(
    endpoint: &crate::config_client::GraphqlEndpoint,
    client: &ClientSigningCapability,
) -> anyhow::Result<ClientAuthorization> {
    Ok(ClientAuthorization::from_authenticated_bearer(
        client.did().parse()?,
        &endpoint
            .authorization()
            .await?
            .expect("held client authorization"),
    )?)
}

#[tokio::test]
async fn client_held_vera_access_can_be_approved_revoked_and_requested_again() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let runtime = Arc::new(crate::identity::KeyIdentity::load_or_create(
        directory.path().join("runtime.key"),
        None,
    )?);
    let (owner_key, owner) = held_client()?;
    let (candidate_key, candidate) = held_client()?;
    assert!(defra_core::signing::get_identity(owner.did()).is_none());
    assert!(defra_core::signing::get_identity(candidate.did()).is_none());
    let metadata = Arc::new(tokio::sync::RwLock::new(json!({})));
    let status = axum::Router::new().route(
        "/status",
        axum::routing::get({
            let metadata = metadata.clone();
            move || {
                let metadata = metadata.clone();
                async move { axum::Json(metadata.read().await.clone()) }
            }
        }),
    );
    let owner_staged = crate::config_client::OwnerStagedRoute::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}/api/v0/graphql", listener.local_addr()?);
    let keyring = Arc::new(defra_node::keyring::FileKeyring::open(
        directory.path().join("keys"),
        b"access-native-fixture",
    )?);
    let node = Arc::new(
        defra_node::Node::builder()
            .with_document_projector(crate::local::document_projector(
                runtime.clone(),
                Arc::new(NoToolSources),
            )?)
            .with_node_acp_owner(runtime.did())
            .with_signing_key(defra_node::SigningKey::Ed25519(
                runtime.identity.private_key_bytes(),
            ))
            .with_node_acp_enabled()
            .with_http(
                defra_node::HttpConfig::with_listener(listener)?
                    .with_authenticated_routes(status.merge(owner_staged.router())),
            )
            .with_embedded_vera(EmbeddedVeraConfig {
                data_dir: directory.path().join("vera"),
                deployment: EmbeddedVeraDeployment {
                    chain_id: 9413,
                    threshold: 1,
                    operator_keys: vec![
                        "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798".into(),
                    ],
                    created: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
                    policies: vec![EmbeddedVeraPolicy {
                        definition: gents_protocol::schemas::AGENT_OWNERSHIP_POLICY.into(),
                        counter: 0,
                        owner: runtime.did().into(),
                    }],
                },
                listen: "127.0.0.1:0".parse()?,
                start_deadline: Duration::from_secs(60),
                stop_deadline: Duration::from_secs(30),
                actor: EmbeddedVeraActor::Signer(Arc::new(
                    vera::vera_rs::Ed25519Signer::from_slice(
                        &runtime.identity.private_key_bytes(),
                    )?,
                )),
                rpc_listen: None,
                worker_dir: directory.path().join("worker"),
                keyring,
                tuning: vera::AcpTuning::default(),
            })
            .build()
            .await?,
    );
    owner_staged.serve(&node, runtime.clone());
    let runtime_access = ConfigAccess::local_as(node.clone(), runtime.did().parse()?);
    let authentication = runtime_authentication(&runtime_access).await?;
    assert!(matches!(authentication, ClientAuthentication::Vera { .. }));
    *metadata.write().await =
        json!({"node_did": runtime.did(), "client_authentication": authentication});
    for schema in [
        gents_protocol::schemas::NODE,
        gents_protocol::schemas::NETWORK_ENROLLMENT_GRANT,
        gents_protocol::schemas::CLIENT_RUNTIME_ACCESS_REQUEST,
        gents_protocol::schemas::PEER_REGISTRY,
    ] {
        runtime_access.add_collection(schema).await?;
    }
    crate::identity::GrantSet::OperatorUser
        .grant(&runtime_access, &owner.did().parse()?)
        .await?;
    let owner_endpoints = RuntimeEndpoints::new(owner.clone());
    let owner_endpoint = owner_endpoints.endpoint(runtime.did(), &url)?;
    let owner_access = ConfigAccess::Graphql(owner_endpoint.clone());
    let input = json!({"node_did": runtime.did(), "enabled": true});
    let response = owner_access.transact_write("test.client_access.node", |txn| {
        let input = &input;
        Box::pin(async move { txn.execute_client_signed_create(
            "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }", &json!({"input": input}), gents_protocol::schemas::NODE,
        ).await })
    }).await?;
    let node_doc = crate::graphql::created_doc_id(&response, "Node")?;
    owner_access
        .add_actor_relationship("Node", &node_doc, "runtime_reader", &runtime.did().parse()?)
        .await?;
    let registry = runtime_access
        .write(
            "test.transport.runtime_registry",
            &format!(
                r#"mutation {{ create_PeerRegistry(input: {{peer_id:"runtime-owned",node_did:"{}",addresses:["local"]}}) {{_docID}} }}"#,
                runtime.did()
            ),
        )
        .await?;
    let registry = crate::graphql::created_doc_id(&registry, "PeerRegistry")?;
    assert!(
        runtime_access
            .observe_actor_relationship(
                "PeerRegistry",
                &registry,
                "reader",
                &runtime.did().parse()?
            )
            .await?
    );
    runtime_access
        .write(
            "test.transport.update_registry",
            &format!(
                r#"mutation {{ update_PeerRegistry(docID:"{registry}",input:{{status:"updated"}}) {{_docID}} }}"#
            ),
        )
        .await?;
    assert!(
        !runtime_access
            .observe_actor_relationship(
                "PeerRegistry",
                &registry,
                "pending_parent",
                &runtime.did().parse()?
            )
            .await?
    );
    let candidate_endpoints = RuntimeEndpoints::new(candidate.clone());
    let candidate_endpoint = candidate_endpoints.endpoint(runtime.did(), &url)?;
    let candidate_access = ConfigAccess::Graphql(candidate_endpoint.clone());
    let service = ClientAccessOwner::new(node.clone(), runtime.clone(), Default::default());
    let now = Utc::now();
    let mut request = ClientAccessRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        node_did: runtime.did().into(),
        client_did: candidate.did().into(),
        label: "Native desktop".into(),
        issued_at: now.to_rfc3339(),
        expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
        signature: Vec::new(),
    };
    request.signature = candidate_key.sign(&request.signing_payload()?)?;
    let pending = service
        .request(
            credential(&candidate_endpoint, &candidate).await?,
            request.clone(),
        )
        .await
        .context("retain the candidate access request")?;
    assert_eq!(pending.state, ClientAccessState::Pending);
    let decision = DecideClientAccess {
        node_did: runtime.did().into(),
        request_id: request.request_id.clone(),
        decision: ClientAccessDecision::Approve,
    };
    assert!(service
        .decide(
            credential(&candidate_endpoint, &candidate).await?,
            decision.clone()
        )
        .await
        .is_err());
    let approved = service
        .decide(credential(&owner_endpoint, &owner).await?, decision.clone())
        .await
        .context("approve the retained client request as the Node owner")?;
    assert_eq!(approved.state, ClientAccessState::Approved);
    let query = Query::select(Selection::doc_id())
        .filter(Filter::eq(node_fields::NODE_DID, runtime.did()))
        .render();
    assert_eq!(
        candidate_access.read(&query).await?["data"]["Node"]
            .as_array()
            .expect("visible nodes")
            .len(),
        1
    );
    let revoked = service
        .decide(
            credential(&owner_endpoint, &owner).await?,
            DecideClientAccess {
                decision: ClientAccessDecision::Revoke,
                ..decision.clone()
            },
        )
        .await
        .context("revoke the approved client request")?;
    assert_eq!(revoked.state, ClientAccessState::Revoked);
    assert!(candidate_access.read(&query).await.is_err());
    let replayed = service
        .request(
            credential(&candidate_endpoint, &candidate).await?,
            request.clone(),
        )
        .await
        .context("read the revoked client request")?;
    assert_eq!(replayed.state, ClientAccessState::Revoked);
    request.request_id = uuid::Uuid::new_v4().to_string();
    request.signature = candidate_key.sign(&request.signing_payload()?)?;
    owner_access
        .add_actor_relationship(
            "Node",
            &node_doc,
            "administrator",
            &candidate.did().parse()?,
        )
        .await?;
    assert_eq!(
        service
            .request(
                credential(&candidate_endpoint, &candidate).await?,
                request.clone(),
            )
            .await?
            .state,
        ClientAccessState::Pending
    );
    let decision = DecideClientAccess {
        node_did: runtime.did().into(),
        request_id: request.request_id.clone(),
        decision: ClientAccessDecision::Approve,
    };
    service
        .decide(credential(&owner_endpoint, &owner).await?, decision.clone())
        .await?;
    service
        .decide(
            credential(&owner_endpoint, &owner).await?,
            DecideClientAccess {
                decision: ClientAccessDecision::Revoke,
                ..decision
            },
        )
        .await?;
    assert!(
        node.document_acp()
            .check_named_permission(
                &candidate.did().parse()?,
                "administrator",
                gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
                "nodes",
                &node_doc,
            )
            .await?
    );
    owner_access
        .delete_actor_relationship(
            "Node",
            &node_doc,
            "administrator",
            &candidate.did().parse()?,
        )
        .await?;
    owner_access
        .add_actor_relationship(
            "Node",
            &node_doc,
            "administrator",
            &identity::Did::wildcard(),
        )
        .await?;
    request.request_id = uuid::Uuid::new_v4().to_string();
    request.signature = candidate_key.sign(&request.signing_payload()?)?;
    service
        .request(
            credential(&candidate_endpoint, &candidate).await?,
            request.clone(),
        )
        .await?;
    let decision = DecideClientAccess {
        node_did: runtime.did().into(),
        request_id: request.request_id.clone(),
        decision: ClientAccessDecision::Approve,
    };
    service
        .decide(credential(&owner_endpoint, &owner).await?, decision.clone())
        .await?;
    owner_access
        .delete_actor_relationship(
            "Node",
            &node_doc,
            "administrator",
            &identity::Did::wildcard(),
        )
        .await?;
    assert!(
        node.document_acp()
            .check_named_permission(
                &candidate.did().parse()?,
                "administrator",
                gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
                "nodes",
                &node_doc,
            )
            .await?
    );
    service
        .decide(
            credential(&owner_endpoint, &owner).await?,
            DecideClientAccess {
                decision: ClientAccessDecision::Revoke,
                ..decision
            },
        )
        .await?;
    assert!(
        !node
            .document_acp()
            .check_named_permission(
                &candidate.did().parse()?,
                "administrator",
                gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
                "nodes",
                &node_doc,
            )
            .await?
    );
    assert!(defra_core::signing::get_identity(owner.did()).is_none());
    assert!(defra_core::signing::get_identity(candidate.did()).is_none());
    drop(owner_key);
    node.shutdown().await?;
    Ok(())
}

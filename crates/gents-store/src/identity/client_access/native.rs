use super::*;
use crate::config_client::{
    client_authentication::runtime_authentication, runtime_endpoints::RuntimeEndpoints,
    ClientSigningCapability,
};
use crate::test_support::doubles::FailingTupleChanges;
use crate::test_support::NoToolSources;
use anyhow::Context;
use defra_node::{EmbeddedVeraActor, EmbeddedVeraConfig, EmbeddedVeraDeployment};
use gents_protocol::client_authentication::ClientAuthentication;
use identity::{FullIdentity as _, RawIdentity};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
            .runtime_authorization()
            .await?
            .expect("held client authorization"),
    )?)
}

/// A runtime on embedded Vera serving its Node document, its User owner
/// and a candidate client holding its own key, with the access service.
struct VeraRuntime {
    _directory: tempfile::TempDir,
    runtime: Arc<crate::identity::KeyIdentity>,
    owner_key: RawIdentity,
    owner: ClientSigningCapability,
    candidate_key: RawIdentity,
    candidate: ClientSigningCapability,
    node: Arc<defra_node::Node>,
    url: String,
    owner_endpoint: crate::config_client::GraphqlEndpoint,
    owner_access: ConfigAccess,
    node_doc: String,
    candidate_endpoint: crate::config_client::GraphqlEndpoint,
    candidate_access: ConfigAccess,
    service: ClientAccessOwner,
    _endpoints: (RuntimeEndpoints, RuntimeEndpoints),
}

async fn vera_runtime() -> anyhow::Result<VeraRuntime> {
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
            let (metadata, runtime) = (metadata.clone(), runtime.clone());
            move |query: axum::extract::Query<rapidhash::RapidHashMap<String, String>>| {
                let (metadata, runtime) = (metadata.clone(), runtime.clone());
                async move {
                    let mut status = metadata.read().await.clone();
                    if let Some(nonce) = query
                        .get(gents_protocol::home_attestation::HOME_NONCE_QUERY)
                        .and_then(|nonce| {
                            gents_protocol::home_attestation::HomeNonce::parse(nonce).ok()
                        })
                    {
                        crate::identity::home_status::attest(
                            Some(runtime.as_ref()),
                            &nonce,
                            &mut status,
                        )
                        .await;
                    }
                    axum::Json(status)
                }
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
                gents_protocol::schemas::HomePolicyIds::embedded_vera(),
            )?)
            .with_node_acp_owner(runtime.did())
            .with_signing_key(defra_node::SigningKey::Ed25519(
                runtime.identity.private_key_bytes(),
            ))
            .with_node_acp_enabled()
            .with_http({
                let fence_route = owner_staged.clone();
                defra_node::HttpConfig::with_listener(listener)?
                    .with_authenticated_routes(status.merge(owner_staged.router()))
                    .with_router_transform(Arc::new(move |router| {
                        fence_route.wrap_relationship_fence(router)
                    }))
            })
            .with_embedded_vera(EmbeddedVeraConfig {
                data_dir: directory.path().join("vera"),
                deployment: EmbeddedVeraDeployment {
                    chain_id: 9413,
                    threshold: 1,
                    operator_keys: vec![
                        "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798".into(),
                    ],
                    created: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
                    policies: vec![crate::schema::catalog::vera_seed::seed(
                        &gents_protocol::schemas::BuiltInPolicy::Household,
                    )],
                },
                listen: "127.0.0.1:0".parse()?,
                start_deadline: Duration::from_secs(60),
                stop_deadline: Duration::from_secs(30),
                actor: EmbeddedVeraActor::Signer(Arc::new(
                    vera_delegation::Ed25519Signer::from_slice(
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
        runtime_access
            .add_collection(&crate::local::document_acp::node_policy_ids(&node).render(schema))
            .await?;
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
    {
        use crate::config_client::transport_publication::{publication_state, PublicationState};
        let txn = node.database().new_txn(true).await?;
        let state = publication_state(
            &txn.datastore()?,
            "PeerRegistry",
            &registry.parse()?,
            &runtime.did().parse()?,
        )
        .await?;
        txn.discard()?;
        assert_eq!(state, Some(PublicationState::Linked));
    }
    node.database().reconcile_document_projector().await?;
    let candidate_endpoints = RuntimeEndpoints::new(candidate.clone());
    let candidate_endpoint = candidate_endpoints.endpoint(runtime.did(), &url)?;
    let candidate_access = ConfigAccess::Graphql(candidate_endpoint.clone());
    let service = ClientAccessOwner::new(node.clone(), runtime.clone(), Default::default());
    Ok(VeraRuntime {
        _directory: directory,
        runtime,
        owner_key,
        owner,
        candidate_key,
        candidate,
        node,
        url,
        owner_endpoint,
        owner_access,
        node_doc,
        candidate_endpoint,
        candidate_access,
        service,
        _endpoints: (owner_endpoints, candidate_endpoints),
    })
}

#[tokio::test]
async fn client_held_vera_access_can_be_approved_revoked_and_requested_again() -> anyhow::Result<()>
{
    let VeraRuntime {
        _directory,
        runtime,
        owner_key,
        owner,
        candidate_key,
        candidate,
        node,
        url: _,
        owner_endpoint,
        owner_access,
        node_doc,
        candidate_endpoint,
        candidate_access,
        service,
        _endpoints,
    } = vera_runtime().await?;
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
                crate::local::document_acp::node_policy_ids(&node)
                    .id(gents_protocol::schemas::BuiltInPolicy::Household),
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
                crate::local::document_acp::node_policy_ids(&node)
                    .id(gents_protocol::schemas::BuiltInPolicy::Household),
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
                crate::local::document_acp::node_policy_ids(&node)
                    .id(gents_protocol::schemas::BuiltInPolicy::Household),
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

/// An approval whose grant fails at any of its tuples, after the Node
/// `administrator` relation was issued or later, leaves the client no
/// administrator relationship and no node permission, its request pending,
/// and a retried approval grants it access (Lean
/// `Identity.EnrollmentGrant.abandon`).
#[tokio::test]
async fn a_failed_approval_leaves_no_access_and_a_retry_approves() -> anyhow::Result<()> {
    let fixture = vera_runtime().await?;
    let member: identity::Did = fixture.candidate.did().parse()?;
    let approver = ConfigAccess::local_as(fixture.node.clone(), fixture.owner.did().parse()?)
        .with_client_authorization(credential(&fixture.owner_endpoint, &fixture.owner).await?)?;
    let runtime = ConfigAccess::local_as(fixture.node.clone(), fixture.runtime.did().parse()?);
    let administers = || async {
        fixture
            .node
            .document_acp()
            .check_named_permission(
                &member,
                "administrator",
                crate::local::document_acp::node_policy_ids(&fixture.node)
                    .id(gents_protocol::schemas::BuiltInPolicy::Household),
                "nodes",
                &fixture.node_doc,
            )
            .await
    };
    let direct_permissions = || async {
        Ok::<_, anyhow::Error>(
            runtime
                .explain_node_subject(&member)
                .await?
                .into_iter()
                .filter(|provenance| matches!(provenance.via, acp::nac::PermissionVia::Direct))
                .count(),
        )
    };
    for fail_at in 0..crate::identity::EnrollmentGrantTuple::derived().len() {
        let now = Utc::now();
        let mut request = ClientAccessRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            node_did: fixture.runtime.did().into(),
            client_did: fixture.candidate.did().into(),
            label: "Native desktop".into(),
            issued_at: now.to_rfc3339(),
            expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
            signature: Vec::new(),
        };
        request.signature = fixture.candidate_key.sign(&request.signing_payload()?)?;
        let candidate = || credential(&fixture.candidate_endpoint, &fixture.candidate);
        assert_eq!(
            fixture
                .service
                .request(candidate().await?, request.clone())
                .await?
                .state,
            ClientAccessState::Pending
        );
        let tuples = FailingTupleChanges {
            inner: crate::identity::enrollment_grant::NativeTuples {
                owner: &approver,
                nac_owner: &runtime,
                node_doc_id: &fixture.node_doc,
                member: &member,
            },
            fail_at,
            issues: Default::default(),
        };
        let failed = crate::identity::enrollment_grant::grant_with(
            &tuples,
            &approver,
            &runtime,
            &fixture.node_doc,
            &request.request_id,
            &member,
        )
        .await;
        assert!(
            matches!(failed, Err(EnrollmentGrantError::Issue { .. })),
            "{fail_at}: {failed:?}"
        );
        assert!(!administers().await?, "{fail_at}");
        assert_eq!(direct_permissions().await?, 0, "{fail_at}");
        assert_eq!(
            fixture
                .service
                .request(candidate().await?, request.clone())
                .await?
                .state,
            ClientAccessState::Pending,
            "{fail_at}"
        );
        let decision = DecideClientAccess {
            node_did: fixture.runtime.did().into(),
            request_id: request.request_id.clone(),
            decision: ClientAccessDecision::Approve,
        };
        let owner = || credential(&fixture.owner_endpoint, &fixture.owner);
        assert_eq!(
            fixture
                .service
                .decide(owner().await?, decision.clone())
                .await?
                .state,
            ClientAccessState::Approved,
            "{fail_at}"
        );
        assert!(administers().await?, "{fail_at}");
        assert!(direct_permissions().await? > 0, "{fail_at}");
        fixture
            .service
            .decide(
                owner().await?,
                DecideClientAccess {
                    decision: ClientAccessDecision::Revoke,
                    ..decision
                },
            )
            .await?;
        assert!(!administers().await?, "{fail_at}");
    }
    drop(fixture.owner_key);
    fixture.node.shutdown().await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Standing {
    Owner,
    Administrator,
    RuntimeReader,
    Unrelated,
}

/// What an approver observes of a member's exact Node `administrator`
/// relationship, through the approval path's own access, replays
/// `Identity.EnrollmentGrant.observeMemberAdministrator` on a live node: a
/// wildcard relation is never observed as the member's direct one, and only
/// an approver that may manage the relation observes it at all.
#[tokio::test]
async fn administrator_observation_replays_the_lean_model() -> anyhow::Result<()> {
    use crate::config_client::TransactionFailure as _;
    use gents_lean_vocab::families::identity_enrollment_grant_observe_member_administrator as family;
    let cases = family::cases::<Standing>();
    assert_eq!(cases.len(), 16);
    let fixture = vera_runtime().await?;
    let runtime = ConfigAccess::local_as(fixture.node.clone(), fixture.runtime.did().parse()?);
    let (approver_key, approver) = held_client()?;
    let approver_endpoints = RuntimeEndpoints::new(approver.clone());
    let approver_endpoint = approver_endpoints.endpoint(fixture.runtime.did(), &fixture.url)?;
    let approver_did: identity::Did = approver.did().parse()?;
    crate::identity::GrantSet::OperatorUser
        .grant(&runtime, &approver_did)
        .await?;
    let owner_approver = ConfigAccess::local_as(fixture.node.clone(), fixture.owner.did().parse()?)
        .with_client_authorization(credential(&fixture.owner_endpoint, &fixture.owner).await?)?;
    let other_approver = ConfigAccess::local_as(fixture.node.clone(), approver_did.clone())
        .with_client_authorization(credential(&approver_endpoint, &approver).await?)?;
    let member: identity::Did = fixture.candidate.did().parse()?;
    for case in cases {
        let mut relations = Vec::new();
        match case.input.standing {
            Standing::Administrator => relations.push(("administrator", approver_did.clone())),
            Standing::RuntimeReader => relations.push(("runtime_reader", approver_did.clone())),
            Standing::Owner | Standing::Unrelated => {}
        }
        if case.input.direct {
            relations.push(("administrator", member.clone()));
        }
        if case.input.wildcard {
            relations.push(("administrator", identity::Did::wildcard()));
        }
        for (relation, subject) in &relations {
            fixture
                .owner_access
                .add_actor_relationship("Node", &fixture.node_doc, relation, subject)
                .await?;
        }
        let observer = if case.input.standing == Standing::Owner {
            &owner_approver
        } else {
            &other_approver
        };
        let observed = match observer
            .observe_actor_relationship("Node", &fixture.node_doc, "administrator", &member)
            .await
        {
            Ok(direct) => Some(direct),
            Err(error) if error.permission_denied() => None,
            Err(error) => return Err(error).context(format!("{:?}", case.input)),
        };
        for (relation, subject) in relations.iter().rev() {
            fixture
                .owner_access
                .delete_actor_relationship("Node", &fixture.node_doc, relation, subject)
                .await?;
        }
        case.check(observed);
    }
    drop((approver_key, fixture.owner_key));
    fixture.node.shutdown().await?;
    Ok(())
}

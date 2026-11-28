use crate::node_scope::ContextCalls as _;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use defra_node::{EmbeddedVeraActor, EmbeddedVeraConfig, EmbeddedVeraDeployment};
use identity::RawIdentity;
use serde_json::json;

use super::*;
use crate::config_client::client_authentication::{runtime_authentication, ClientAuthorization};
use crate::config_client::runtime_endpoints::RuntimeEndpoints;
use crate::config_client::{ClientSigningCapability, GraphqlEndpoint};
use crate::identity::NodeIdentity as _;
use crate::test_support::doubles::AssignmentTestRuntime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provider {
    Local,
    Vera,
}

#[tokio::test]
async fn shutdown_cancels_pending_assignment_reads_and_runtime_callbacks(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let home = home(Provider::Vera).await?;
    let access = home.runtime()?;
    access
        .add_collection(
            &crate::local::document_acp::node_policy_ids(&home.node)
                .render(gents_protocol::schemas::CERTIFIED_ASSIGNMENT_STATE),
        )
        .await?;
    let scope = crate::config_client::assignment_receiver::AssignmentScope {
        policy: crate::local::document_acp::node_policy_ids(&home.node)
            .id(gents_protocol::schemas::BuiltInPolicy::Household)
            .to_owned(),
        resource: "nodes".into(),
        relation: "runtime_reader".into(),
        issuer: home.owner.did().into(),
        node_document: home.node_doc.clone(),
    };
    let runtime = Arc::new(AssignmentTestRuntime::default());
    runtime
        .block_active
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let ids = crate::subscription_source::collection_ids(&home.node, ["Node"])?;
    let (shutdown, stop) = tokio::sync::watch::channel(false);
    let watcher = tokio::spawn({
        let (access, scope, runtime, ids) =
            (access.clone(), scope.clone(), runtime.clone(), ids.clone());
        async move {
            access
                .watch_assignments(&scope, runtime.as_ref(), ids, stop)
                .await
        }
    });
    tokio::time::timeout(
        crate::test_support::STALL_CEILING,
        runtime.active_entered.notified(),
    )
    .await?;
    // A second watcher can shut down while the first still owns this scope.
    let (second_shutdown, stop) = tokio::sync::watch::channel(false);
    let second = access.watch_assignments(&scope, runtime.as_ref(), ids, stop);
    tokio::pin!(second);
    assert!(futures::poll!(&mut second).is_pending());
    second_shutdown.send(true)?;
    assert!(matches!(
        futures::poll!(&mut second),
        std::task::Poll::Ready(Ok(()))
    ));
    shutdown.send(true)?;
    tokio::time::timeout(crate::test_support::STALL_CEILING, watcher).await???;

    let (shutdown, mut stop) = tokio::sync::watch::channel(false);
    runtime
        .block_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let (entered, reading) = tokio::sync::oneshot::channel();
    let refresh =
        access.refresh_with_assignment_view(&scope, runtime.as_ref(), Some(&mut stop), async {
            entered.send(()).unwrap();
            std::future::pending().await
        });
    tokio::pin!(refresh);
    tokio::select! {
        _ = &mut refresh => panic!("refresh returned before the provider answered"),
        result = tokio::time::timeout(crate::test_support::STALL_CEILING, reading) => result??,
    }
    shutdown.send(true)?;
    assert!(
        tokio::time::timeout(crate::test_support::STALL_CEILING, refresh)
            .await??
            .is_none()
    );

    Ok(())
}

#[tokio::test]
async fn an_access_change_wakes_the_assignment_receiver() -> anyhow::Result<()> {
    exercise_assignment_withdrawal(None).await
}

#[tokio::test]
async fn the_first_observed_height_reconciles_a_withdrawal() -> anyhow::Result<()> {
    exercise_assignment_withdrawal(Some(events::ChannelBus::new())).await
}

async fn exercise_assignment_withdrawal(
    observer: Option<events::ChannelBus>,
) -> anyhow::Result<()> {
    use events::Bus as _;
    let home = home(Provider::Vera).await?;
    let access = home.runtime()?;
    access
        .add_collection(
            &crate::local::document_acp::node_policy_ids(&home.node)
                .render(gents_protocol::schemas::CERTIFIED_ASSIGNMENT_STATE),
        )
        .await?;
    let scope = crate::config_client::assignment_receiver::AssignmentScope {
        policy: crate::local::document_acp::node_policy_ids(&home.node)
            .id(gents_protocol::schemas::BuiltInPolicy::Household)
            .to_owned(),
        resource: "nodes".into(),
        relation: "runtime_reader".into(),
        issuer: home.owner.did().into(),
        node_document: home.node_doc.clone(),
    };
    let runtime = Arc::new(AssignmentTestRuntime::default());
    runtime.bodies.lock().await.insert(home.node_doc.clone());
    let ids = crate::subscription_source::collection_ids(&home.node, ["Node"])?;
    let (shutdown, stop) = tokio::sync::watch::channel(false);
    let watcher = tokio::spawn({
        let (access, runtime, scope) = (access.clone(), runtime.clone(), scope.clone());
        let subscriptions = observer.as_ref().map(|bus| {
            (
                crate::subscription_source::ChangeSubscription::from(
                    bus.subscribe_collection_changes(ids.clone()),
                ),
                bus.subscribe(&[events::EventName::AcpHeightAdvanced]),
            )
        });
        async move {
            match subscriptions {
                Some((changes, heights)) => {
                    access
                        .watch_assignment_events(&scope, runtime.as_ref(), changes, heights, stop)
                        .await
                }
                None => {
                    access
                        .watch_assignments(&scope, runtime.as_ref(), ids, stop)
                        .await
                }
            }
        }
    });
    async fn wait_active(runtime: &AssignmentTestRuntime, id: &str, active: bool) {
        loop {
            let changed = runtime.changed.notified();
            if runtime.active.lock().await.contains(id) == active {
                return;
            }
            tokio::time::timeout(crate::test_support::STALL_CEILING, changed)
                .await
                .expect("assignment runtime did not change");
        }
    }
    wait_active(&runtime, &home.node_doc, true).await;
    ConfigAccess::Graphql(home.owner_endpoint.clone())
        .delete_actor_relationship(
            "Node",
            &home.node_doc,
            "runtime_reader",
            &home.runtime.did().parse()?,
        )
        .await?;
    if let Some(observer) = observer.as_ref() {
        observer.publish(events::Message::acp_height_advanced(
            events::AcpHeightAdvancedData {
                height: 2,
                module_state_root: "first observed root".into(),
            },
        ));
    }
    wait_active(&runtime, &home.node_doc, false).await;
    shutdown.send(true)?;
    watcher.await??;
    Ok(())
}

#[tokio::test]
async fn a_vera_home_reads_only_its_own_certified_assignments() -> anyhow::Result<()> {
    let home = home_with_storage(Provider::Vera, true).await?;
    let access = home.runtime()?;
    let query = defra_node::AssignmentQuery {
        policy: crate::local::document_acp::node_policy_ids(&home.node)
            .id(gents_protocol::schemas::BuiltInPolicy::Household)
            .to_owned(),
        resource: "nodes".into(),
        relation: "runtime_reader".into(),
        subject: home.runtime.did().into(),
        issuer: home.owner.did().into(),
        not_before: None,
    };
    let certified = access.certified_assignments(query.clone()).await?;
    assert_eq!(certified.object_ids, std::slice::from_ref(&home.node_doc));
    access
        .add_collection(
            &crate::local::document_acp::node_policy_ids(&home.node)
                .render(gents_protocol::schemas::CERTIFIED_ASSIGNMENT_STATE),
        )
        .await?;
    let scope = crate::config_client::assignment_receiver::AssignmentScope {
        policy: query.policy.clone(),
        resource: query.resource.clone(),
        relation: query.relation.clone(),
        issuer: query.issuer.clone(),
        node_document: home.node_doc.clone(),
    };
    let running = AssignmentTestRuntime::default();
    running.bodies.lock().await.insert(home.node_doc.clone());
    let initial = access
        .refresh_assignments(&scope, &running)
        .await
        .map_err(|error| anyhow::anyhow!("initial assignment refresh: {error}"))?;
    assert_eq!(initial.timestamp, certified.timestamp);
    assert!(initial.may_run(&home.node_doc, true));
    assert!(!initial.may_run(&home.node_doc, false));
    assert!(running.active.lock().await.contains(&home.node_doc));
    let conflicting = defra_node::CertifiedAssignments {
        position: initial.position.clone(),
        object_ids: Vec::new(),
        ..certified.clone()
    };
    assert!(matches!(
        access
            .refresh_with_assignment_view(
                &scope,
                &running,
                None,
                std::future::ready(Ok(conflicting))
            )
            .await,
        Err(crate::config_client::assignment_receiver::ReceiverError::InvalidState(_))
    ));
    assert!(running.active.lock().await.contains(&home.node_doc));
    assert_eq!(
        access.load_assignments(&scope).await?,
        Some(initial.clone())
    );
    let other = defra_node::AssignmentQuery {
        subject: home.owner.did().into(),
        ..query.clone()
    };
    assert!(matches!(
        access.certified_assignments(other).await,
        Err(
            crate::config_client::assignments::AssignmentReadError::Provider(
                defra_node::Error::DacPolicy(db::DacPolicyError::AssignmentSubject)
            )
        )
    ));
    let owner_access = ConfigAccess::Graphql(home.owner_endpoint.clone());
    let input = json!({"node_did": home.owner.did(), "enabled": true});
    let response = owner_access
        .transact_write("test.assignment_replacement.node", |txn| {
            let input = &input;
            Box::pin(async move {
                txn.execute_client_signed_create(
                    "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                    &json!({"input": input}),
                    gents_protocol::schemas::NODE,
                )
                .await
            })
        })
        .await?;
    let replacement = crate::graphql::created_doc_id(&response, "Node")?;
    owner_access
        .add_actor_relationship(
            "Node",
            &replacement,
            "runtime_reader",
            &home.runtime.did().parse()?,
        )
        .await?;
    owner_access
        .delete_actor_relationship(
            "Node",
            &home.node_doc,
            "runtime_reader",
            &home.runtime.did().parse()?,
        )
        .await?;
    let withdrawn = access
        .certified_assignments(defra_node::AssignmentQuery {
            not_before: Some(certified.position.clone()),
            ..query
        })
        .await?;
    assert_eq!(withdrawn.object_ids, std::slice::from_ref(&replacement));
    assert!(withdrawn.position.height > certified.position.height);
    running
        .fail_stop
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(access.refresh_assignments(&scope, &running).await.is_err());
    assert_eq!(
        access.load_assignments(&scope).await?,
        Some(initial.clone())
    );
    assert!(running.active.lock().await.contains(&home.node_doc));
    running
        .fail_stop
        .store(false, std::sync::atomic::Ordering::SeqCst);
    running
        .block_after_stop
        .store(true, std::sync::atomic::Ordering::SeqCst);
    {
        // The previous stop failed. Its withdrawal must take effect even if
        // the authority cannot answer another read.
        let interrupted =
            access.refresh_with_assignment_view(&scope, &running, None, std::future::pending());
        tokio::pin!(interrupted);
        tokio::select! {
            result = &mut interrupted => panic!("refresh returned before interruption: {result:?}"),
            stopped = tokio::time::timeout(crate::test_support::STALL_CEILING, running.stopped.notified()) => stopped?,
        }
    }
    assert!(running.active.lock().await.is_empty());
    drop(access);
    let Home {
        _directory: directory,
        runtime,
        owner,
        node,
        _endpoints: endpoints,
        vera_config,
        ..
    } = home;
    node.shutdown().await?;
    drop(endpoints);
    drop(node);
    let reopened = Arc::new(
        defra_node::Node::builder()
            .data_path(directory.path().join("db"))
            .with_node_acp_owner(owner.did())
            .with_signing_key(defra_node::SigningKey::Ed25519(
                runtime.identity.private_key_bytes(),
            ))
            .with_node_acp_enabled()
            .with_embedded_vera(vera_config.expect("Vera configuration"))
            .build()
            .await?,
    );
    let access = ConfigAccess::local_as(reopened, runtime.did().parse()?);
    let recovered = access
        .load_assignments(&scope)
        .await?
        .expect("persisted assignment state");
    assert_eq!(recovered, initial);
    let replay = defra_node::CertifiedAssignments {
        position: initial.position.clone(),
        object_ids: initial.assigned.clone(),
        ..certified
    };
    assert!(matches!(
        access
            .refresh_with_assignment_view(&scope, &running, None, std::future::ready(Ok(replay)))
            .await,
        Err(crate::config_client::assignment_receiver::ReceiverError::Overtaken)
    ));
    assert!(running.active.lock().await.is_empty());
    let recovered = access.refresh_assignments(&scope, &running).await?;
    assert!(!recovered.may_run(&scope.node_document, true));
    assert!(!recovered.may_run(&replacement, false));
    running.bodies.lock().await.insert(replacement.clone());
    access.refresh_assignments(&scope, &running).await?;
    assert_eq!(
        running
            .active
            .lock()
            .await
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        [replacement]
    );
    Ok(())
}

#[tokio::test]
async fn an_agent_its_owner_places_is_a_certified_assignment_of_its_node() -> anyhow::Result<()> {
    let home = home(Provider::Vera).await?;
    let access = home.runtime()?;
    access
        .add_collection(
            &crate::local::document_acp::node_policy_ids(&home.node)
                .render(gents_protocol::schemas::AGENT),
        )
        .await?;
    let owner_access = ConfigAccess::Graphql(home.owner_endpoint.clone());
    let (_, agent_key) = held_client()?;
    let input = json!({
        "node_did": home.runtime.did(),
        "agent_did": agent_key.did(),
        "creation_nonce": "placement",
        "enabled": true,
    });
    let response = owner_access
        .transact_write("test.placement.agent", |txn| {
            let input = &input;
            Box::pin(async move {
                txn.execute_client_signed_create(
                    "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                    &json!({"input": input}),
                    gents_protocol::schemas::AGENT,
                )
                .await
            })
        })
        .await?;
    let agent = crate::graphql::created_doc_id(&response, "Agent")?;
    let node: identity::Did = home.runtime.did().parse()?;
    assert!(
        access
            .add_actor_relationship("Agent", &agent, "host", &node)
            .await
            .is_err(),
        "only the Agent's owner places it"
    );
    owner_access
        .add_actor_relationship("Agent", &agent, "host", &node)
        .await?;
    let query = defra_node::AssignmentQuery {
        policy: crate::local::document_acp::node_policy_ids(&home.node)
            .id(gents_protocol::schemas::BuiltInPolicy::Household)
            .to_owned(),
        resource: "agents".into(),
        relation: "host".into(),
        subject: home.runtime.did().into(),
        issuer: home.owner.did().into(),
        not_before: None,
    };
    let placed = access.certified_assignments(query.clone()).await?;
    assert_eq!(placed.object_ids, [agent]);
    let unissued = access
        .certified_assignments(defra_node::AssignmentQuery {
            issuer: home.runtime.did().into(),
            ..query
        })
        .await?;
    assert!(unissued.object_ids.is_empty());
    Ok(())
}

struct Home {
    _directory: tempfile::TempDir,
    runtime: Arc<crate::identity::KeyIdentity>,
    owner_key: RawIdentity,
    owner: ClientSigningCapability,
    owner_endpoint: GraphqlEndpoint,
    node: Arc<defra_node::Node>,
    node_doc: String,
    _endpoints: RuntimeEndpoints,
    vera_config: Option<EmbeddedVeraConfig>,
}

fn held_client() -> anyhow::Result<(RawIdentity, ClientSigningCapability)> {
    let key = RawIdentity::from_private_key(crypto::generate_ed25519()?)?;
    let capability = ClientSigningCapability::from_ed25519_key(&key.private_key_bytes())?;
    Ok((key, capability))
}

fn vera(
    directory: &std::path::Path,
    runtime: &crate::identity::KeyIdentity,
) -> anyhow::Result<EmbeddedVeraConfig> {
    Ok(EmbeddedVeraConfig {
        data_dir: directory.join("vera"),
        deployment: EmbeddedVeraDeployment {
            chain_id: 9414,
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
        actor: EmbeddedVeraActor::Signer(Arc::new(vera_delegation::Ed25519Signer::from_slice(
            &runtime.identity.private_key_bytes(),
        )?)),
        rpc_listen: None,
        worker_dir: directory.join("worker"),
        keyring: Arc::new(defra_node::keyring::FileKeyring::open(
            directory.join("keys"),
            b"enrollment-withdrawal-fixture",
        )?),
        tuning: vera::AcpTuning::default(),
    })
}

async fn home(provider: Provider) -> anyhow::Result<Home> {
    home_with_storage(provider, false).await
}

async fn home_with_storage(provider: Provider, persistent: bool) -> anyhow::Result<Home> {
    let directory = tempfile::tempdir()?;
    let runtime = Arc::new(crate::identity::KeyIdentity::load_or_create(
        directory.path().join("runtime.key"),
        None,
    )?);
    let (owner_key, owner) = held_client()?;
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
    let served: Arc<std::sync::OnceLock<std::sync::Weak<defra_node::Node>>> = Arc::default();
    let status = status.route(
        gents_protocol::owed_settlement::OWED_AWAIT_PATH,
        axum::routing::post({
            let served = served.clone();
            move |identity: defra_http::ExtractIdentity,
                  axum::Json(request): axum::Json<
                gents_protocol::owed_settlement::OwedAwaitRequest,
            >| {
                let served = served.clone();
                async move {
                    let node = served
                        .get()
                        .and_then(std::sync::Weak::upgrade)
                        .expect("the fixture's node serves the wait");
                    let wait = crate::config_client::owed::serve_await(
                        &node,
                        &identity.context(),
                        &request.transaction,
                    )
                    .await
                    .expect("the wait answers");
                    axum::Json(gents_protocol::owed_settlement::OwedAwaitReport { wait })
                }
            }
        }),
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}/api/v0/graphql", listener.local_addr()?);
    let grants = crate::config_client::OwnerStagedRoute::new();
    let fence_route = grants.clone();
    let mut builder = defra_node::Node::builder()
        .with_node_acp_owner(owner.did())
        .with_signing_key(defra_node::SigningKey::Ed25519(
            runtime.identity.private_key_bytes(),
        ))
        .with_node_acp_enabled()
        .with_http(
            defra_node::HttpConfig::with_listener(listener)?
                .with_authenticated_routes(status.merge(grants.router()))
                .with_router_transform(Arc::new(move |router| {
                    fence_route.wrap_relationship_fence(router)
                })),
        );
    if persistent {
        builder = builder.data_path(directory.path().join("db"));
    }
    let mut vera_config = None;
    if provider == Provider::Vera {
        let config = vera(directory.path(), &runtime)?;
        builder = builder.with_embedded_vera(config.clone());
        vera_config = Some(config);
    }
    let node = Arc::new(builder.build().await?);
    served
        .set(Arc::downgrade(&node))
        .expect("the fixture's node is set once");
    grants.serve(&node, runtime.clone());
    let as_owner = defra_core::RequestContext::default().with_identity(Some(owner.did().into()));
    for permission in crate::identity::node_runtime_permissions() {
        node.add_nac_actor_relationship_as(&as_owner, permission.as_str(), runtime.did())
            .await?;
    }
    if provider == Provider::Local {
        node.add_dac_policy_with_counter_as(
            &as_owner,
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
            0,
        )
        .await?;
    }
    let runtime_access = ConfigAccess::local_as(node.clone(), runtime.did().parse()?);
    *metadata.write().await = json!({
        "node_did": runtime.did(),
        "client_authentication": runtime_authentication(&runtime_access).await?,
    });
    for schema in [
        gents_protocol::schemas::NODE,
        gents_protocol::schemas::NETWORK_ENROLLMENT_GRANT,
    ] {
        runtime_access
            .add_collection(&crate::local::document_acp::node_policy_ids(&node).render(schema))
            .await?;
    }
    crate::identity::GrantSet::OperatorUser
        .grant(&runtime_access, &owner.did().parse()?)
        .await?;
    let endpoints = RuntimeEndpoints::new(owner.clone());
    let owner_endpoint = endpoints.endpoint(runtime.did(), &url)?;
    let owner_access = ConfigAccess::Graphql(owner_endpoint.clone());
    let input = json!({"node_did": runtime.did(), "enabled": true});
    let response = owner_access
        .transact_write("test.enrollment_withdrawal.node", |txn| {
            let input = &input;
            Box::pin(async move {
                txn.execute_client_signed_create(
                    "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                    &json!({"input": input}),
                    gents_protocol::schemas::NODE,
                )
                .await
            })
        })
        .await?;
    let node_doc = crate::graphql::created_doc_id(&response, "Node")?;
    owner_access
        .add_actor_relationship("Node", &node_doc, "runtime_reader", &runtime.did().parse()?)
        .await?;
    Ok(Home {
        _directory: directory,
        runtime,
        owner_key,
        owner,
        owner_endpoint,
        node,
        node_doc,
        _endpoints: endpoints,
        vera_config,
    })
}

impl Home {
    fn runtime(&self) -> anyhow::Result<ConfigAccess> {
        Ok(ConfigAccess::local_as(
            self.node.clone(),
            self.runtime.did().parse()?,
        ))
    }

    async fn authorization(&self) -> anyhow::Result<ClientAuthorization> {
        Ok(ClientAuthorization::from_authenticated_bearer(
            self.owner.did().parse()?,
            &self
                .owner_endpoint
                .runtime_authorization()
                .await?
                .expect("a held client authorizes its requests"),
        )?)
    }

    async fn owner(&self) -> anyhow::Result<ConfigAccess> {
        Ok(
            ConfigAccess::local_as(self.node.clone(), self.owner.did().parse()?)
                .with_client_authorization(self.authorization().await?)?,
        )
    }

    fn access_owner(&self) -> crate::identity::client_access::ClientAccessOwner {
        crate::identity::client_access::ClientAccessOwner::new(
            self.node.clone(),
            self.runtime.clone(),
            Default::default(),
        )
    }

    async fn administers(&self, member: &identity::Did) -> anyhow::Result<bool> {
        Ok(self
            .node
            .document_acp()
            .check_named_permission(
                member,
                "administrator",
                crate::local::document_acp::node_policy_ids(&self.node)
                    .id(gents_protocol::schemas::BuiltInPolicy::Household),
                "nodes",
                &self.node_doc,
            )
            .await?)
    }

    async fn direct_permissions(&self, member: &identity::Did) -> anyhow::Result<usize> {
        Ok(self
            .runtime()?
            .explain_node_subject(member)
            .await?
            .into_iter()
            .filter(|provenance| matches!(provenance.via, acp::nac::PermissionVia::Direct))
            .count())
    }

    async fn receipts(
        &self,
        member: &identity::Did,
    ) -> anyhow::Result<Vec<EnrollmentGrantReceipt>> {
        Ok(load_enrollment_grant_receipts(&self.runtime()?, member.as_str()).await?)
    }

    async fn approve(&self, request: &str, member: &identity::Did) -> anyhow::Result<()> {
        let owner = self.owner().await?;
        let runtime = self.runtime()?;
        grant_enrollment_authority(&owner, &runtime, &runtime, &self.node_doc, request, member)
            .await?;
        Ok(())
    }

    async fn withdraw_denial_as_node(
        &self,
        request: &str,
        member: &identity::Did,
    ) -> anyhow::Result<()> {
        let runtime = self.runtime()?;
        let receipts = self.receipts(member).await?;
        let withdrawal = EnrollmentGrantWithdrawal::plan(request, &receipts)?
            .expect("a live receipt to withdraw");
        withdrawal.revoke_node_permissions(&runtime).await?;
        runtime
            .transact_write("test.enrollment_withdrawal.revision", |txn| {
                let withdrawal = &withdrawal;
                Box::pin(async move { withdrawal.retire_owing_in_txn(txn).await })
            })
            .await?;
        Ok(())
    }

    async fn settle_as(
        &self,
        caller: ClientAuthorization,
    ) -> anyhow::Result<Vec<gents_protocol::owed_settlement::OwedStanding>> {
        Ok(self.access_owner().settle_withdrawals(caller).await?)
    }

    async fn shutdown(self) -> anyhow::Result<()> {
        drop(self.owner_key);
        self.node.shutdown().await?;
        Ok(())
    }
}

async fn enrolled_client() -> anyhow::Result<(RawIdentity, identity::Did)> {
    let (key, member) = held_client()?;
    Ok((key, member.did().parse()?))
}

async fn verify_denial_owed_until_owner_settles(
    home: &Home,
    provider: Provider,
) -> anyhow::Result<()> {
    let (_member_key, member) = enrolled_client().await?;
    home.approve("first", &member).await?;
    assert!(home.direct_permissions(&member).await? > 0, "{provider:?}");

    home.withdraw_denial_as_node("first", &member).await?;
    let receipts = home.receipts(&member).await?;
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt.withdrawn && receipt.owed)
            .count(),
        1,
        "{provider:?}: {receipts:?}"
    );
    assert!(
        administration_owed(member.as_str(), &receipts),
        "{provider:?}"
    );
    assert_eq!(home.direct_permissions(&member).await?, 0, "{provider:?}");
    assert!(home.administers(&member).await?, "{provider:?}");

    let (stranger_key, stranger) = held_client()?;
    let stranger_did: identity::Did = stranger.did().parse()?;
    crate::identity::GrantSet::OperatorUser
        .grant(&home.runtime()?, &stranger_did)
        .await?;
    let endpoints = RuntimeEndpoints::new(stranger.clone());
    let url = home.owner_endpoint.url().to_owned();
    let stranger_endpoint = endpoints.endpoint(home.runtime.did(), &url)?;
    let stranger_authorization = ClientAuthorization::from_authenticated_bearer(
        stranger_did,
        &stranger_endpoint
            .runtime_authorization()
            .await?
            .expect("a held client authorizes its requests"),
    )?;
    assert_eq!(
        home.settle_as(stranger_authorization).await?,
        [],
        "{provider:?}"
    );
    assert!(home.administers(&member).await?, "{provider:?}");
    assert!(
        administration_owed(member.as_str(), &home.receipts(&member).await?),
        "{provider:?}"
    );

    assert_eq!(
        home.settle_as(home.authorization().await?).await?,
        [gents_protocol::owed_settlement::OwedStanding::Settled],
        "{provider:?}"
    );
    assert!(!home.administers(&member).await?, "{provider:?}");
    assert!(
        !administration_owed(member.as_str(), &home.receipts(&member).await?),
        "{provider:?}"
    );
    assert_eq!(
        home.settle_as(home.authorization().await?).await?,
        [],
        "{provider:?}"
    );
    drop(stranger_key);
    Ok(())
}

#[tokio::test]
async fn a_denial_on_a_local_home_is_owed_until_the_owner_settles() -> anyhow::Result<()> {
    let home = home(Provider::Local).await?;
    verify_denial_owed_until_owner_settles(&home, Provider::Local).await?;

    let (_owed_key, owed_member) = enrolled_client().await?;
    home.approve("second", &owed_member).await?;
    let runtime = home.runtime()?;
    let gate = crate::identity::enrollment_decision_gate(&home.node);
    let withdrawal = gate.lock().await;
    let grant = runtime.grant_node_permission(&owed_member, NodePermission::DocumentRead);
    tokio::pin!(grant);
    assert!(futures::poll!(&mut grant).is_pending());
    home.withdraw_denial_as_node("second", &owed_member).await?;
    drop(withdrawal);
    let pending = |error: &ConfigClientError| {
        matches!(
            error,
            ConfigClientError::AdministrationRemovalPending { member: pending }
                if pending == owed_member.as_str()
        )
    };
    let direct = grant
        .await
        .expect_err("a direct permission waits for the owed removal");
    assert!(pending(&direct), "{direct:?}");
    let set = crate::identity::GrantSet::OperatorUser
        .grant(&runtime, &owed_member)
        .await
        .expect_err("a grant set waits for the owed removal");
    assert!(
        matches!(&set, crate::identity::GrantSetError::Grant { source, .. } if pending(source)),
        "{set:?}"
    );
    let remote = ConfigAccess::Graphql(home.owner_endpoint.clone());
    let refused = remote
        .grant_node_permission(&owed_member, NodePermission::DocumentRead)
        .await
        .expect_err("an HTTP grant also waits for settlement");
    assert!(
        matches!(
            refused,
            ConfigClientError::NodePermissionStatus {
                status: reqwest::StatusCode::CONFLICT,
                ..
            }
        ),
        "{refused:?}"
    );
    assert_eq!(home.direct_permissions(&owed_member).await?, 0);
    home.settle_as(home.authorization().await?).await?;
    remote
        .grant_node_permission(&owed_member, NodePermission::DocumentRead)
        .await?;
    assert_eq!(home.direct_permissions(&owed_member).await?, 1);

    let (_dep_key, dep_member) = enrolled_client().await?;
    let node_doc = home.node_doc.clone();
    let input = json!({
        "request_id": "shared",
        "member_did": dep_member.as_str(),
        "granted_at": "2026-11-20T00:00:00Z",
        "node_doc_id": node_doc,
        "parent_linked": true,
    });
    let ticket = home
        .runtime()?
        .transact_signed_conflict_rebuild("test.own_grant.share", |txn| {
            let (input, node_doc) = (&input, &node_doc);
            Box::pin(async move {
                let created = txn
                    .execute_client_signed_create(
                        "mutation($input: NetworkEnrollmentGrantMutationInputArg!) { create_NetworkEnrollmentGrant(input: $input) { _docID } }",
                        &json!({ "input": input }),
                        gents_protocol::schemas::NETWORK_ENROLLMENT_GRANT,
                    )
                    .await?;
                let doc = gents_protocol::graphql::extract_mutation_doc_id(
                    &created,
                    RECEIPT_COLLECTION,
                )
                .map_err(EnrollmentGrantError::RecordUnconfirmed)?;
                txn.add_resource_relationship(RECEIPT_COLLECTION, &doc, "parent", "nodes", node_doc)
                    .await?;
                Ok::<_, EnrollmentGrantError>(txn.owed_access_ticket()?)
            })
        })
        .await?;
    let wait = ticket
        .wait(crate::config_client::owed::OWN_GRANT_DEADLINE)
        .await?;
    assert_eq!(
        crate::config_client::owed::dependent_step(wait),
        crate::config_client::owed::DependentStep::Proceed,
        "{wait:?}"
    );
    ticket.require_settled().await?;
    let seen = load_enrollment_grant_receipts(&home.owner().await?, dep_member.as_str()).await?;
    assert!(
        seen.iter().any(|receipt| receipt.request_id == "shared"),
        "{seen:?}"
    );
    home.shutdown().await
}

#[tokio::test]
async fn a_denial_on_a_vera_home_is_owed_until_the_owner_settles() -> anyhow::Result<()> {
    let home = home(Provider::Vera).await?;
    verify_denial_owed_until_owner_settles(&home, Provider::Vera).await?;

    let owner = ConfigAccess::Graphql(home.owner_endpoint.clone());
    home.owner_endpoint.runtime_authorization().await?;
    let input = json!({"node_did": "did:key:z6MkOwedAwaitFixture", "enabled": true});
    let ticket = owner
        .transact_write("test.own_grant.http", |txn| {
            let input = &input;
            Box::pin(async move {
                txn.execute_client_signed_create(
                    "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                    &json!({"input": input}),
                    gents_protocol::schemas::NODE,
                )
                .await?;
                txn.owed_access_ticket()
            })
        })
        .await?;
    assert!(
        matches!(
            ticket,
            crate::config_client::owed::OwedAccessTicket::Http { .. }
        ),
        "a Vera home's client transaction waits through the runtime"
    );
    assert_eq!(
        ticket
            .wait(crate::config_client::owed::OWN_GRANT_DEADLINE)
            .await?,
        crate::config_client::owed::GrantWait::Settled
    );
    ticket.require_settled().await?;
    home.shutdown().await
}

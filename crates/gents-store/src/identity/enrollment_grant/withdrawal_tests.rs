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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provider {
    Local,
    Vera,
}

#[derive(Default)]
struct AssignmentTestRuntime {
    active: tokio::sync::Mutex<std::collections::BTreeSet<String>>,
    bodies: tokio::sync::Mutex<std::collections::BTreeSet<String>>,
    fail_stop: std::sync::atomic::AtomicBool,
    block_active: std::sync::atomic::AtomicBool,
    active_entered: tokio::sync::Notify,
    changed: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl crate::config_client::assignment_receiver::AssignmentRuntime for AssignmentTestRuntime {
    async fn active(&self) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        if self.block_active.load(std::sync::atomic::Ordering::SeqCst) {
            self.active_entered.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(self.active.lock().await.iter().cloned().collect())
    }

    async fn stop(&self, id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.fail_stop.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(std::io::Error::other("stop failed").into());
        }
        self.active.lock().await.remove(id);
        self.changed.notify_one();
        Ok(())
    }

    async fn has_verified_body(
        &self,
        id: &str,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.bodies.lock().await.contains(id))
    }

    async fn start_if_verified(
        &self,
        id: &str,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        if !self.bodies.lock().await.contains(id) {
            return Ok(false);
        }
        self.active.lock().await.insert(id.to_owned());
        self.changed.notify_one();
        Ok(true)
    }
}

#[tokio::test]
async fn shutdown_cancels_a_pending_assignment_runtime_callback(
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
    Ok(())
}

#[tokio::test]
async fn an_access_change_wakes_the_assignment_receiver() -> anyhow::Result<()> {
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
        async move {
            access
                .watch_assignments(&scope, runtime.as_ref(), ids, stop)
                .await
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
            .refresh_with_assignment_view(&scope, &running, conflicting)
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
    assert_eq!(access.load_assignments(&scope).await?, Some(initial));
    assert!(running.active.lock().await.contains(&home.node_doc));
    running
        .fail_stop
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let persisted = access
        .refresh_assignments(&scope, &running)
        .await
        .map_err(|error| anyhow::anyhow!("withdrawn assignment refresh: {error}"))?;
    assert!(!persisted.may_run(&home.node_doc, true));
    assert!(!persisted.may_run(&replacement, false));
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
    assert_eq!(recovered, persisted);
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
    let mut builder = defra_node::Node::builder()
        .with_node_acp_owner(owner.did())
        .with_signing_key(defra_node::SigningKey::Ed25519(
            runtime.identity.private_key_bytes(),
        ))
        .with_node_acp_enabled()
        .with_http(
            defra_node::HttpConfig::with_listener(listener)?
                .with_authenticated_routes(status.merge(grants.router())),
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

    async fn withdraw_as_node(
        &self,
        trigger: Trigger,
        member: &identity::Did,
    ) -> anyhow::Result<()> {
        let runtime = self.runtime()?;
        let receipts = self.receipts(member).await?;
        let withdrawal = match trigger {
            Trigger::Denied => EnrollmentGrantWithdrawal::plan("first", &receipts)?,
            Trigger::Revoked => {
                EnrollmentGrantWithdrawal::plan_membership(member.as_str(), &receipts)?
            }
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Denied,
    Revoked,
}

async fn enrolled_client() -> anyhow::Result<(RawIdentity, identity::Did)> {
    let (key, member) = held_client()?;
    Ok((key, member.did().parse()?))
}

async fn the_node_alone_changes_no_administration(provider: Provider) -> anyhow::Result<()> {
    let home = home(provider).await?;
    let (_member_key, member) = enrolled_client().await?;
    let (_other_key, other) = enrolled_client().await?;
    home.approve("first", &member).await?;
    assert!(home.administers(&member).await?, "{provider:?}");
    let runtime = home.runtime()?;
    for (subject, delete) in [(&member, true), (&other, false)] {
        let node_doc = home.node_doc.clone();
        let changed = runtime
            .transact_write("test.enrollment_withdrawal.node_alone", |txn| {
                let (node_doc, subject) = (&node_doc, subject);
                Box::pin(async move {
                    match delete {
                        true => {
                            txn.delete_actor_relationship(
                                "Node",
                                node_doc,
                                "administrator",
                                subject,
                            )
                            .await
                        }
                        false => {
                            txn.add_actor_relationship("Node", node_doc, "administrator", subject)
                                .await
                        }
                    }
                })
            })
            .await;
        assert!(
            changed
                .as_ref()
                .is_err_and(|error| error.is_permission_denied()),
            "{provider:?} delete={delete}: {changed:?}"
        );
    }
    assert!(home.administers(&member).await?, "{provider:?}");
    assert!(!home.administers(&other).await?, "{provider:?}");
    home.shutdown().await
}

#[tokio::test]
async fn the_node_alone_changes_no_administration_on_a_local_home() -> anyhow::Result<()> {
    the_node_alone_changes_no_administration(Provider::Local).await
}

#[tokio::test]
async fn the_node_alone_changes_no_administration_on_a_vera_home() -> anyhow::Result<()> {
    the_node_alone_changes_no_administration(Provider::Vera).await
}

async fn a_withdrawal_the_node_finds_due_is_owed_until_the_owner_settles(
    provider: Provider,
    trigger: Trigger,
) -> anyhow::Result<()> {
    let case = format!("{provider:?} {trigger:?}");
    let home = home(provider).await?;
    let (_member_key, member) = enrolled_client().await?;
    home.approve("first", &member).await?;
    if trigger == Trigger::Revoked {
        home.approve("renewal", &member).await?;
    }
    assert!(home.direct_permissions(&member).await? > 0, "{case}");

    home.withdraw_as_node(trigger, &member).await?;
    let receipts = home.receipts(&member).await?;
    let retired = match trigger {
        Trigger::Denied => 1,
        Trigger::Revoked => 2,
    };
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt.withdrawn && receipt.owed)
            .count(),
        retired,
        "{case}: {receipts:?}"
    );
    assert!(administration_owed(member.as_str(), &receipts), "{case}");
    assert_eq!(home.direct_permissions(&member).await?, 0, "{case}");
    assert!(home.administers(&member).await?, "{case}");

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
    assert_eq!(home.settle_as(stranger_authorization).await?, [], "{case}");
    assert!(home.administers(&member).await?, "{case}");
    assert!(
        administration_owed(member.as_str(), &home.receipts(&member).await?),
        "{case}"
    );

    assert_eq!(
        home.settle_as(home.authorization().await?).await?,
        [gents_protocol::owed_settlement::OwedStanding::Settled],
        "{case}"
    );
    assert!(!home.administers(&member).await?, "{case}");
    assert!(
        !administration_owed(member.as_str(), &home.receipts(&member).await?),
        "{case}"
    );
    assert_eq!(
        home.settle_as(home.authorization().await?).await?,
        [],
        "{case}"
    );
    drop(stranger_key);
    home.shutdown().await
}

#[tokio::test]
async fn a_denial_on_a_local_home_is_owed_until_the_owner_settles() -> anyhow::Result<()> {
    a_withdrawal_the_node_finds_due_is_owed_until_the_owner_settles(
        Provider::Local,
        Trigger::Denied,
    )
    .await
}

#[tokio::test]
async fn a_denial_on_a_vera_home_is_owed_until_the_owner_settles() -> anyhow::Result<()> {
    a_withdrawal_the_node_finds_due_is_owed_until_the_owner_settles(Provider::Vera, Trigger::Denied)
        .await
}

#[tokio::test]
async fn a_revocation_on_a_local_home_is_owed_until_the_owner_settles() -> anyhow::Result<()> {
    a_withdrawal_the_node_finds_due_is_owed_until_the_owner_settles(
        Provider::Local,
        Trigger::Revoked,
    )
    .await
}

#[tokio::test]
async fn a_revocation_on_a_vera_home_is_owed_until_the_owner_settles() -> anyhow::Result<()> {
    a_withdrawal_the_node_finds_due_is_owed_until_the_owner_settles(
        Provider::Vera,
        Trigger::Revoked,
    )
    .await
}

async fn a_reapproval_takes_the_owed_withdrawal_over(provider: Provider) -> anyhow::Result<()> {
    let home = home(provider).await?;
    let (_member_key, member) = enrolled_client().await?;
    home.approve("first", &member).await?;
    home.withdraw_as_node(Trigger::Denied, &member).await?;
    home.approve("second", &member).await?;
    let receipts = home.receipts(&member).await?;
    assert!(
        !administration_owed(member.as_str(), &receipts),
        "{provider:?}"
    );
    assert!(
        receipts.iter().any(|receipt| receipt.request_id == "second"
            && receipt.accounts(member.as_str(), EnrollmentGrantTuple::NodeAdministrator)),
        "{provider:?}: {receipts:?}"
    );
    assert_eq!(
        home.settle_as(home.authorization().await?).await?,
        [gents_protocol::owed_settlement::OwedStanding::Settled],
        "{provider:?}"
    );
    assert_eq!(
        home.settle_as(home.authorization().await?).await?,
        [],
        "{provider:?}"
    );
    assert!(home.administers(&member).await?, "{provider:?}");
    assert!(home.direct_permissions(&member).await? > 0, "{provider:?}");
    home.shutdown().await
}

#[tokio::test]
async fn a_reapproval_on_a_local_home_takes_the_owed_withdrawal_over() -> anyhow::Result<()> {
    a_reapproval_takes_the_owed_withdrawal_over(Provider::Local).await
}

#[tokio::test]
async fn a_reapproval_on_a_vera_home_takes_the_owed_withdrawal_over() -> anyhow::Result<()> {
    a_reapproval_takes_the_owed_withdrawal_over(Provider::Vera).await
}

struct Client {
    _key: RawIdentity,
    did: identity::Did,
    endpoint: GraphqlEndpoint,
    _endpoints: RuntimeEndpoints,
}

impl Client {
    async fn new(home: &Home) -> anyhow::Result<Self> {
        let (key, capability) = held_client()?;
        let did: identity::Did = capability.did().parse()?;
        crate::identity::GrantSet::OperatorUser
            .grant(&home.runtime()?, &did)
            .await?;
        let endpoints = RuntimeEndpoints::new(capability);
        let endpoint = endpoints.endpoint(home.runtime.did(), home.owner_endpoint.url())?;
        Ok(Self {
            _key: key,
            did,
            endpoint,
            _endpoints: endpoints,
        })
    }

    async fn authorization(&self) -> anyhow::Result<ClientAuthorization> {
        Ok(ClientAuthorization::from_authenticated_bearer(
            self.did.clone(),
            &self
                .endpoint
                .runtime_authorization()
                .await?
                .expect("a held client authorizes its requests"),
        )?)
    }
}

async fn an_administrator_settles_when_the_owner_is_away(provider: Provider) -> anyhow::Result<()> {
    let home = home(provider).await?;
    let (_member_key, member) = enrolled_client().await?;
    let administrator = Client::new(&home).await?;
    home.owner()
        .await?
        .add_actor_relationship("Node", &home.node_doc, "administrator", &administrator.did)
        .await?;
    home.approve("first", &member).await?;
    home.withdraw_as_node(Trigger::Denied, &member).await?;
    let owed = home
        .access_owner()
        .owed_removals(administrator.authorization().await?)
        .await?;
    assert_eq!(
        owed,
        [gents_protocol::owed_settlement::OwedRemoval {
            member_did: member.as_str().to_owned(),
            administrator_held: true,
        }],
        "{provider:?}"
    );
    assert_eq!(
        home.settle_as(administrator.authorization().await?).await?,
        [gents_protocol::owed_settlement::OwedStanding::Settled],
        "{provider:?}"
    );
    assert!(!home.administers(&member).await?, "{provider:?}");
    assert!(
        home.access_owner()
            .owed_removals(home.authorization().await?)
            .await?
            .is_empty(),
        "{provider:?}"
    );
    home.shutdown().await
}

#[tokio::test]
async fn an_administrator_settles_on_a_local_home_when_the_owner_is_away() -> anyhow::Result<()> {
    an_administrator_settles_when_the_owner_is_away(Provider::Local).await
}

#[tokio::test]
async fn an_administrator_settles_on_a_vera_home_when_the_owner_is_away() -> anyhow::Result<()> {
    an_administrator_settles_when_the_owner_is_away(Provider::Vera).await
}

async fn an_owed_member_is_refused_node_permissions(provider: Provider) -> anyhow::Result<()> {
    let home = home(provider).await?;
    let (_member_key, member) = enrolled_client().await?;
    home.approve("first", &member).await?;
    let runtime = home.runtime()?;
    let gate = crate::identity::enrollment_decision_gate(&home.node);
    let withdrawal = gate.lock().await;
    let grant = runtime.grant_node_permission(&member, NodePermission::DocumentRead);
    tokio::pin!(grant);
    assert!(futures::poll!(&mut grant).is_pending());
    home.withdraw_as_node(Trigger::Denied, &member).await?;
    drop(withdrawal);
    let pending = |error: &ConfigClientError| matches!(error, ConfigClientError::AdministrationRemovalPending { member: pending } if pending == member.as_str());
    let direct = grant
        .await
        .expect_err("a direct permission waits for the owed removal");
    assert!(pending(&direct), "{provider:?}: {direct:?}");
    let set = crate::identity::GrantSet::OperatorUser
        .grant(&runtime, &member)
        .await
        .expect_err("a grant set waits for the owed removal");
    assert!(
        matches!(&set, crate::identity::GrantSetError::Grant { source, .. } if pending(source)),
        "{provider:?}: {set:?}"
    );
    let remote = ConfigAccess::Graphql(home.owner_endpoint.clone());
    let refused = remote
        .grant_node_permission(&member, NodePermission::DocumentRead)
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
    assert_eq!(home.direct_permissions(&member).await?, 0, "{provider:?}");
    home.settle_as(home.authorization().await?).await?;
    remote
        .grant_node_permission(&member, NodePermission::DocumentRead)
        .await?;
    assert_eq!(home.direct_permissions(&member).await?, 1, "{provider:?}");
    home.shutdown().await
}

#[tokio::test]
async fn an_owed_member_is_refused_node_permissions_on_a_local_home() -> anyhow::Result<()> {
    an_owed_member_is_refused_node_permissions(Provider::Local).await
}

#[tokio::test]
async fn an_owed_member_is_refused_node_permissions_on_a_vera_home() -> anyhow::Result<()> {
    an_owed_member_is_refused_node_permissions(Provider::Vera).await
}

async fn a_deliberate_grant_closes_the_owed_removal(provider: Provider) -> anyhow::Result<()> {
    let home = home(provider).await?;
    let (_member_key, member) = enrolled_client().await?;
    home.approve("first", &member).await?;
    home.withdraw_as_node(Trigger::Denied, &member).await?;
    ConfigAccess::Graphql(home.owner_endpoint.clone())
        .add_actor_relationship("Node", &home.node_doc, "administrator", &member)
        .await?;
    assert!(
        !administration_owed(member.as_str(), &home.receipts(&member).await?),
        "{provider:?}"
    );
    assert_eq!(
        home.settle_as(home.authorization().await?).await?,
        [gents_protocol::owed_settlement::OwedStanding::Settled],
        "{provider:?}"
    );
    assert_eq!(
        home.settle_as(home.authorization().await?).await?,
        [],
        "{provider:?}"
    );
    assert!(home.administers(&member).await?, "{provider:?}");
    home.shutdown().await
}

#[tokio::test]
async fn a_deliberate_grant_on_a_local_home_closes_the_owed_removal() -> anyhow::Result<()> {
    a_deliberate_grant_closes_the_owed_removal(Provider::Local).await
}

#[tokio::test]
async fn a_deliberate_grant_on_a_vera_home_closes_the_owed_removal() -> anyhow::Result<()> {
    a_deliberate_grant_closes_the_owed_removal(Provider::Vera).await
}

async fn an_expiry_withdraws_unless_a_renewal_is_fresh(provider: Provider) -> anyhow::Result<()> {
    let home = home(provider).await?;
    let (_member_key, member) = enrolled_client().await?;
    home.approve("first", &member).await?;
    home.approve("renewal", &member).await?;
    let clock = lapse_clock(
        crate::local::document_acp::node_provider_time(&home.node),
        u64::try_from(chrono::Utc::now().timestamp())?,
    );
    assert!(
        !membership_lapsed(None, clock, &[clock - 60, clock + 600]),
        "{provider:?}"
    );
    assert!(home.direct_permissions(&member).await? > 0, "{provider:?}");
    assert!(
        membership_lapsed(None, clock, &[clock - 60, clock]),
        "{provider:?}"
    );
    home.withdraw_as_node(Trigger::Revoked, &member).await?;
    assert_eq!(home.direct_permissions(&member).await?, 0, "{provider:?}");
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
    home.shutdown().await
}

#[tokio::test]
async fn an_expiry_on_a_local_home_withdraws_unless_a_renewal_is_fresh() -> anyhow::Result<()> {
    an_expiry_withdraws_unless_a_renewal_is_fresh(Provider::Local).await
}

#[tokio::test]
async fn an_expiry_on_a_vera_home_withdraws_unless_a_renewal_is_fresh() -> anyhow::Result<()> {
    an_expiry_withdraws_unless_a_renewal_is_fresh(Provider::Vera).await
}

async fn a_removal_overtaking_a_takeover_is_restored(
    provider: Provider,
    takeover: Takeover,
) -> anyhow::Result<()> {
    let case = format!("{provider:?} {takeover:?}");
    let home = home(provider).await?;
    let (_member_key, member) = enrolled_client().await?;
    home.approve("first", &member).await?;
    home.withdraw_as_node(Trigger::Denied, &member).await?;
    match takeover {
        Takeover::Approval => home.approve("second", &member).await?,
        Takeover::DeliberateGrant => {
            home.owner()
                .await?
                .add_actor_relationship("Node", &home.node_doc, "administrator", &member)
                .await?
        }
    }
    let receipts = home.receipts(&member).await?;
    assert!(!administration_owed(member.as_str(), &receipts), "{case}");
    assert!(
        receipts.iter().any(|receipt| receipt.restore),
        "{case}: {receipts:?}"
    );
    home.owner()
        .await?
        .delete_actor_relationship("Node", &home.node_doc, "administrator", &member)
        .await?;
    assert!(!home.administers(&member).await?, "{case}");
    assert!(
        home.receipts(&member)
            .await?
            .iter()
            .any(|receipt| receipt.restore),
        "{case}"
    );
    let (stranger_key, stranger) = held_client()?;
    let stranger_did: identity::Did = stranger.did().parse()?;
    crate::identity::GrantSet::OperatorUser
        .grant(&home.runtime()?, &stranger_did)
        .await?;
    let endpoints = RuntimeEndpoints::new(stranger);
    let stranger_endpoint = endpoints.endpoint(home.runtime.did(), home.owner_endpoint.url())?;
    let stranger_authorization = ClientAuthorization::from_authenticated_bearer(
        stranger_did,
        &stranger_endpoint
            .runtime_authorization()
            .await?
            .expect("a held client authorizes its requests"),
    )?;
    assert_eq!(home.settle_as(stranger_authorization).await?, [], "{case}");
    assert!(!home.administers(&member).await?, "{case}");
    assert_eq!(
        home.settle_as(home.authorization().await?).await?,
        [gents_protocol::owed_settlement::OwedStanding::Settled],
        "{case}"
    );
    assert!(home.administers(&member).await?, "{case}");
    assert!(
        !home
            .receipts(&member)
            .await?
            .iter()
            .any(|receipt| receipt.restore),
        "{case}"
    );
    assert_eq!(
        home.settle_as(home.authorization().await?).await?,
        [],
        "{case}"
    );
    drop(stranger_key);
    home.shutdown().await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Takeover {
    Approval,
    DeliberateGrant,
}

#[tokio::test]
async fn a_removal_overtaking_an_approval_on_a_local_home_is_restored() -> anyhow::Result<()> {
    a_removal_overtaking_a_takeover_is_restored(Provider::Local, Takeover::Approval).await
}

#[tokio::test]
async fn a_removal_overtaking_an_approval_on_a_vera_home_is_restored() -> anyhow::Result<()> {
    a_removal_overtaking_a_takeover_is_restored(Provider::Vera, Takeover::Approval).await
}

#[tokio::test]
async fn a_removal_overtaking_a_deliberate_grant_on_a_local_home_is_restored() -> anyhow::Result<()>
{
    a_removal_overtaking_a_takeover_is_restored(Provider::Local, Takeover::DeliberateGrant).await
}

#[tokio::test]
async fn a_removal_overtaking_a_deliberate_grant_on_a_vera_home_is_restored() -> anyhow::Result<()>
{
    a_removal_overtaking_a_takeover_is_restored(Provider::Vera, Takeover::DeliberateGrant).await
}

async fn a_dependent_read_runs_only_once_its_grant_settled(
    provider: Provider,
) -> anyhow::Result<()> {
    let home = home(provider).await?;
    let (_member_key, member) = enrolled_client().await?;
    let node_doc = home.node_doc.clone();
    let input = json!({
        "request_id": "shared",
        "member_did": member.as_str(),
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
        "{provider:?}: {wait:?}"
    );
    ticket.require_settled().await?;
    let seen = load_enrollment_grant_receipts(&home.owner().await?, member.as_str()).await?;
    assert!(
        seen.iter().any(|receipt| receipt.request_id == "shared"),
        "{provider:?}: {seen:?}"
    );
    home.shutdown().await
}

#[tokio::test]
async fn a_dependent_read_on_a_local_home_runs_once_its_grant_settled() -> anyhow::Result<()> {
    a_dependent_read_runs_only_once_its_grant_settled(Provider::Local).await
}

#[tokio::test]
async fn a_dependent_read_on_a_vera_home_runs_once_its_grant_settled() -> anyhow::Result<()> {
    a_dependent_read_runs_only_once_its_grant_settled(Provider::Vera).await
}

#[tokio::test]
async fn a_client_waits_over_http_for_its_own_access_changes_on_a_vera_home() -> anyhow::Result<()>
{
    let home = home(Provider::Vera).await?;
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

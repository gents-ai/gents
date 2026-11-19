use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use defra_node::{
    EmbeddedVeraActor, EmbeddedVeraConfig, EmbeddedVeraDeployment, EmbeddedVeraPolicy,
};
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

/// A home as `FreshBackendStore::initialize` leaves it
/// (`Identity.NodeProvisioning.nacOwner`): the init User owns node access
/// control and the Node document; the Node holds `nodeRuntimePermissions` and
/// `runtime_reader` on its document.
struct Home {
    _directory: tempfile::TempDir,
    runtime: Arc<crate::identity::KeyIdentity>,
    owner_key: RawIdentity,
    owner: ClientSigningCapability,
    owner_endpoint: GraphqlEndpoint,
    node: Arc<defra_node::Node>,
    node_doc: String,
    _endpoints: RuntimeEndpoints,
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
            policies: vec![EmbeddedVeraPolicy {
                definition: gents_protocol::schemas::AGENT_OWNERSHIP_POLICY.into(),
                counter: 0,
                owner: runtime.did().into(),
            }],
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
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}/api/v0/graphql", listener.local_addr()?);
    let mut builder = defra_node::Node::builder()
        .with_node_acp_owner(owner.did())
        .with_signing_key(defra_node::SigningKey::Ed25519(
            runtime.identity.private_key_bytes(),
        ))
        .with_node_acp_enabled()
        .with_http(
            defra_node::HttpConfig::with_listener(listener)?.with_authenticated_routes(status),
        );
    if provider == Provider::Vera {
        builder = builder.with_embedded_vera(vera(directory.path(), &runtime)?);
    }
    let node = Arc::new(builder.build().await?);
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
        runtime_access.add_collection(schema).await?;
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
                gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
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

    /// The runtime enrollment store's withdrawal of a denied request or a
    /// revoked membership (`GraphqlEnrollmentStore::prepare_grant_withdrawal`
    /// and the revision's transaction), run as the Node.
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

/// Lean `node_alone_neither_grants_nor_withdraws_administration`: native
/// refuses the Node's own change of a member's Node `administrator`
/// relation, removal or grant, on either provider.
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

/// Lean `oweRevoke`, `oweRevokeMember` and `settle`: a withdrawal the Node
/// finds due retires the receipt owing the administration withdrawal and
/// withdraws every node permission at once; a caller that may not manage the
/// relation settles nothing; the owner's pass removes the relation and closes
/// the owed receipts.
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

/// Lean `reapproval_takes_the_owed_withdrawal_over`: approving the member
/// again while its withdrawal is owed records the relation in the new
/// receipt and closes the owed one, so the owner's pass keeps it.
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

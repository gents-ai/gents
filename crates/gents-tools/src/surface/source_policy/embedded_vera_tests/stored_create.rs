use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose, RequestScope,
};
use gents_protocol::schemas::{BuiltInPolicy, HomePolicyIds};
use gents_store::config_client::owed::settle_owed_while_online;
use gents_store::config_client::user_write::UserWriteRefusal;
use gents_store::config_client::{
    ClientSigningCapability, ConfigAccess, ConfigClientError, IdempotentTransactionRetry,
    NodePermission, StoredCreateCommands,
};
use gents_store::identity::{IdentityError, NodeIdentity, ServiceAccount};
use gents_store::local::backend_protection::FreshBackendStore;
use gents_store::local::document_acp::DocumentAcp;

use super::owed::{endpoint_for, InProcessPass};
use super::{client_key, home, start, Home};

const REQUEST_RESOURCE: &str = "delivered_requests";
const SESSION_DOC: &str = "bae-stored-create-session";
const AGENT_DOC: &str = "bae-stored-create-agent";

/// The User as its client holds it: a key in a signing capability that
/// DefraDB never holds.
pub(super) struct ClientUser(
    pub(super) ClientSigningCapability,
    pub(super) identity::RawIdentity,
);

async fn client_user(root: &std::path::Path, name: &str) -> anyhow::Result<ClientUser> {
    let capability = client_key(root, name).await?;
    let path = root.join(name);
    let raw = tokio::task::spawn_blocking(move || {
        gents_store::identity::load_or_create_file_identity(&path)
    })
    .await??;
    Ok(ClientUser(capability, raw))
}

#[async_trait::async_trait]
impl NodeIdentity for ClientUser {
    fn did(&self) -> &str {
        self.0.did()
    }

    async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, IdentityError> {
        use identity::FullIdentity as _;
        self.1
            .sign(payload)
            .map_err(|source| IdentityError::SignPayload {
                did: self.did().to_owned(),
                source,
            })
    }

    async fn verify(
        &self,
        did: &str,
        payload: &[u8],
        signature: &[u8],
    ) -> Result<bool, IdentityError> {
        gents_store::identity::verify_did_signature(did, payload, signature)
    }

    fn service_account(&self) -> Option<&ServiceAccount> {
        None
    }

    fn client_signing_capability(&self) -> Result<ClientSigningCapability, ConfigClientError> {
        Ok(self.0.clone())
    }
}

async fn request(
    home: &Home,
    user: &ClientUser,
    agent: &str,
    id: &str,
) -> anyhow::Result<AgentRequestCreate> {
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        id,
        RequestScope::new(home.node.did(), user.did(), agent, "stored-create-session"),
        "hello",
        "interactive",
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        AgentRequestAdmissionRecord::client(user.did()),
    );
    create.session_doc_id = Some(SESSION_DOC.into());
    create.agent_doc_id = Some(AGENT_DOC.into());
    create.admission.signature = user.sign(&create.signing_payload()).await?;
    Ok(create)
}

struct Prepared {
    create: AgentRequestCreate,
    evidence: serde_json::Value,
    doc_id: String,
}

async fn prepare(user: &ClientUser, create: AgentRequestCreate) -> anyhow::Result<Prepared> {
    let input = gents_store::config_client::client_request_input(&create)?;
    let evidence = gents_store::config_client::prepare_request_evidence(
        user,
        input.clone(),
        HomePolicyIds::embedded_vera(),
    )
    .await?;
    let doc_id = gents_store::config_client::verify_request_evidence(
        input,
        evidence.clone(),
        HomePolicyIds::embedded_vera(),
    )
    .await?
    .doc_id()
    .to_owned();
    Ok(Prepared {
        create,
        evidence,
        doc_id,
    })
}

/// The runtime's handling of a stored create: its working Agent admits the
/// create with the credentials the command carried and makes it, in one
/// transaction, before `before_commit` runs.
async fn stage<F, Fut>(
    node: &Arc<defra_node::Node>,
    agent: &Arc<dyn NodeIdentity>,
    prepared: &Prepared,
    credentials: &[String],
    before_commit: F,
) -> anyhow::Result<Result<String, UserWriteRefusal>>
where
    F: Fn() -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ()> + Send,
{
    let access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did())?,
        agent.as_ref(),
    )?;
    access
        .transact_idempotent(
            IdempotentTransactionRetry::Standard,
            "test.embedded_vera.stored_create",
            |txn| {
                let before_commit = &before_commit;
                Box::pin(async move {
                    let graph = Arc::new(
                        gents_store::config_client::verify_request_evidence(
                            gents_store::config_client::client_request_input(&prepared.create)?,
                            prepared.evidence.clone(),
                            HomePolicyIds::embedded_vera(),
                        )
                        .await?,
                    );
                    if let Err(refusal) =
                        txn.admit_stored_user_create(&prepared.create, graph.doc_id(), credentials)?
                    {
                        return Ok(Err(refusal));
                    }
                    let created = txn
                        .stage_verified_edit_request(
                            &prepared.create,
                            graph,
                            SESSION_DOC,
                            AGENT_DOC,
                        )
                        .await?;
                    before_commit().await;
                    Ok::<_, anyhow::Error>(Ok(created))
                })
            },
        )
        .await
}

async fn requests(node: &defra_node::Node, owner: &identity::Did) -> anyhow::Result<Vec<String>> {
    super::stored(node, owner, "AgentRequest", "request_id").await
}

async fn owner(node: &defra_node::Node, document: &str) -> anyhow::Result<Option<identity::Did>> {
    use acp::DocumentACP as _;
    Ok(node
        .vera_acp()
        .expect("the node decides document access through Vera")
        .get_doc_owner(
            gents_store::local::document_acp::node_policy_ids(node).id(BuiltInPolicy::Household),
            REQUEST_RESOURCE,
            document,
        )
        .await?)
}

async fn working_agent_reads(
    node: &defra_node::Node,
    agent: &str,
    document: &str,
) -> anyhow::Result<bool> {
    Ok(node
        .document_acp()
        .check_doc_access(
            &acp::Identity::Authenticated(identity::Did::new(agent)?),
            acp::DocumentPermission::Read,
            gents_store::local::document_acp::node_policy_ids(node).id(BuiltInPolicy::Household),
            REQUEST_RESOURCE,
            document,
        )
        .await?)
}

pub(super) struct Fixture {
    pub(super) _directory: tempfile::TempDir,
    pub(super) home: Home,
    pub(super) node: Arc<defra_node::Node>,
    pub(super) user: ClientUser,
    pub(super) stranger: ClientUser,
    pub(super) agent: Arc<dyn NodeIdentity>,
}

pub(super) async fn fixture(name: &str) -> anyhow::Result<Fixture> {
    let directory = tempfile::tempdir()?;
    let mut home = home(directory.path(), name).await?;
    let user = client_user(directory.path(), "user.key").await?;
    let stranger = client_user(directory.path(), "stranger.key").await?;
    home.owner = identity::Did::new(user.did())?;
    FreshBackendStore::observe(&home.data, &home.lock)
        .await?
        .initialize(&home.key, &home.node, &home.owner, DocumentAcp::Vera)
        .await?;
    let node = start(&home, DocumentAcp::Vera).await?;
    let binding = gents_store::identity::provision_agent_identity(home.node.as_ref())?;
    let agent = gents_store::identity::resolve_agent_identity(home.node.as_ref(), &binding)?;
    let as_node = ConfigAccess::local_as(node.clone(), identity::Did::new(home.node.did())?);
    for permission in [
        NodePermission::CollectionGet,
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
        NodePermission::DacRelationAdd,
    ] {
        as_node
            .grant_node_permission(&identity::Did::new(agent.did())?, permission)
            .await?;
    }
    Ok(Fixture {
        _directory: directory,
        home,
        node,
        user,
        stranger,
        agent,
    })
}

async fn nothing_before_commit() {}

#[tokio::test]
async fn a_stored_create_runs_under_the_request_bound_credentials_its_client_signed_into_it(
) -> anyhow::Result<()> {
    let Fixture {
        _directory,
        home,
        node,
        user,
        stranger,
        agent,
    } = fixture("stored-create").await?;
    assert!(defra_core::signing::get_identity(user.did()).is_none());
    let client = ConfigAccess::Graphql(endpoint_for(&node, &user.0)?);
    let audience = client
        .request_bound_audience()
        .await?
        .expect("a Vera home names the audience a stored create is bound for");
    assert!(audience.audience.genesis_id.is_some());

    let bare = prepare(&user, request(&home, &user, agent.did(), "bare").await?).await?;
    assert_eq!(
        stage(&node, &agent, &bare, &[], nothing_before_commit).await?,
        Err(UserWriteRefusal::DelegationRequired)
    );

    let foreign = prepare(&user, request(&home, &user, agent.did(), "foreign").await?).await?;
    let strangers = StoredCreateCommands::of_request(
        &foreign.create,
        &foreign.doc_id,
        HomePolicyIds::embedded_vera(),
    )?
    .mint(&stranger.0, &audience)?;
    assert_eq!(
        stage(&node, &agent, &foreign, &strangers, nothing_before_commit).await?,
        Err(UserWriteRefusal::IssuerNotSigner)
    );

    let elsewhere = prepare(
        &user,
        request(&home, &user, agent.did(), "elsewhere").await?,
    )
    .await?;
    let for_another = client
        .stored_create_credentials(&user, &foreign.create, &foreign.evidence)
        .await?;
    assert_eq!(
        stage(
            &node,
            &agent,
            &elsewhere,
            &for_another,
            nothing_before_commit
        )
        .await?,
        Err(UserWriteRefusal::DelegationForAnotherWrite)
    );
    assert_eq!(requests(&node, &home.owner).await?, Vec::<String>::new());

    let signed = prepare(&user, request(&home, &user, agent.did(), "signed").await?).await?;
    let credentials = client
        .stored_create_credentials(&user, &signed.create, &signed.evidence)
        .await?;
    assert_eq!(
        credentials.len(),
        4,
        "a registration and three relationships"
    );
    let created = stage(&node, &agent, &signed, &credentials, nothing_before_commit)
        .await?
        .expect("the create its client signed for is admitted");
    assert_eq!(created, signed.doc_id);
    assert_eq!(requests(&node, &home.owner).await?, ["signed"]);
    assert_eq!(
        owner(&node, &created).await?,
        Some(identity::Did::new(user.did())?)
    );
    assert!(working_agent_reads(&node, agent.did(), &created).await?);
    assert!(!working_agent_reads(&node, stranger.did(), &created).await?);
    let caller = defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    assert!(node.owed_acp_as(&caller, user.did()).await?.is_empty());
    assert!(defra_core::signing::get_identity(user.did()).is_none());

    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn a_stored_create_whose_credentials_lapse_before_it_is_sent_is_settled_by_its_client(
) -> anyhow::Result<()> {
    let Fixture {
        _directory,
        home,
        node,
        user,
        agent,
        ..
    } = fixture("stored-create-lapse").await?;
    let endpoint = endpoint_for(&node, &user.0)?;
    let audience = ConfigAccess::Graphql(endpoint.clone())
        .request_bound_audience()
        .await?
        .expect("a Vera home names its audience");

    let prepared = prepare(&user, request(&home, &user, agent.did(), "lapsed").await?).await?;
    let lifetime = Duration::from_secs(2);
    let minted = SystemTime::now();
    let credentials = StoredCreateCommands::of_request(
        &prepared.create,
        &prepared.doc_id,
        HomePolicyIds::embedded_vera(),
    )?
    .mint_with_lifetime_for_test(&user.0, &audience, lifetime)?;
    let created = stage(&node, &agent, &prepared, &credentials, || async {
        let lapsed = minted + lifetime + Duration::from_secs(2);
        while SystemTime::now() <= lapsed {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await?
    .expect("the create is admitted while its credentials are current");
    assert_eq!(requests(&node, &home.owner).await?, ["lapsed"]);
    assert_eq!(
        owner(&node, &created).await?,
        None,
        "its registration is owed"
    );
    let caller = defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    let owed = node.owed_acp_as(&caller, user.did()).await?;
    assert!(!owed.is_empty());
    assert!(owed
        .iter()
        .any(|entry| entry.state == acp::OwedState::AwaitingAuthority));

    let pass = InProcessPass::new(node.clone(), endpoint);
    settle_owed_while_online(&pass).await?;
    assert_eq!(
        owner(&node, &created).await?,
        Some(identity::Did::new(user.did())?)
    );
    assert!(working_agent_reads(&node, agent.did(), &created).await?);
    assert!(node.owed_acp_as(&caller, user.did()).await?.is_empty());

    node.shutdown().await?;
    Ok(())
}

/// What the runtime's public status reports of its node now, signed by its
/// node key.
struct NodeStatus(Arc<defra_node::Node>, Arc<dyn NodeIdentity>);

impl gents_store::config_client::ResolveAudience for NodeStatus {
    fn resolve<'a>(
        &'a self,
        nonce: &'a gents_protocol::home_attestation::HomeNonce,
    ) -> futures::future::BoxFuture<'a, Option<serde_json::Value>> {
        Box::pin(async move {
            Some(
                gents_store::test_support::signed_home_status(
                    self.1.as_ref(),
                    &gents_protocol::delegation_audience::ReportedDelegationAudience::Named(
                        gents_store::local::document_acp::node_delegation_audience(&self.0)?,
                    ),
                    gents_store::local::document_acp::node_provider_time(&self.0),
                    nonce,
                )
                .await,
            )
        })
    }
}

fn skewed(seconds: i64) -> Arc<dyn Fn() -> u64 + Send + Sync> {
    Arc::new(move || {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("a clock after the epoch")
            .as_secs();
        now.saturating_add_signed(seconds)
    })
}

#[tokio::test]
async fn a_stored_create_from_a_device_whose_clock_is_minutes_off_is_admitted() -> anyhow::Result<()>
{
    let Fixture {
        _directory,
        home,
        node,
        user,
        agent,
        ..
    } = fixture("stored-create-skew").await?;
    for (name, skew) in [("ahead", 7 * 60), ("behind", -12 * 60)] {
        let unread = ConfigAccess::Graphql(
            endpoint_for(&node, &user.0)?.with_device_clock_for_test(skewed(skew)),
        );
        let on_its_own_clock = prepare(
            &user,
            request(&home, &user, agent.did(), &format!("{name}-own")).await?,
        )
        .await?;
        let credentials = unread
            .stored_create_credentials(&user, &on_its_own_clock.create, &on_its_own_clock.evidence)
            .await?;
        assert_eq!(
            stage(
                &node,
                &agent,
                &on_its_own_clock,
                &credentials,
                nothing_before_commit
            )
            .await?,
            Err(UserWriteRefusal::DelegationRefused),
            "a device {name} that mints on its own clock is refused"
        );

        let client = ConfigAccess::Graphql(
            endpoint_for(&node, &user.0)?
                .with_audience_resolver(
                    home.node.did(),
                    Arc::new(NodeStatus(node.clone(), home.node.clone())),
                )
                .with_device_clock_for_test(skewed(skew)),
        );
        let signed = prepare(&user, request(&home, &user, agent.did(), name).await?).await?;
        let credentials = client
            .stored_create_credentials(&user, &signed.create, &signed.evidence)
            .await?;
        let created = stage(&node, &agent, &signed, &credentials, nothing_before_commit)
            .await?
            .unwrap_or_else(|refusal| {
                panic!("a device {name} that mints at its home's clock is admitted: {refusal}")
            });
        assert_eq!(
            owner(&node, &created).await?,
            Some(identity::Did::new(user.did())?)
        );
        assert!(working_agent_reads(&node, agent.did(), &created).await?);
    }
    assert_eq!(requests(&node, &home.owner).await?.len(), 2);
    let caller = defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    assert!(node.owed_acp_as(&caller, user.did()).await?.is_empty());

    node.shutdown().await?;
    Ok(())
}

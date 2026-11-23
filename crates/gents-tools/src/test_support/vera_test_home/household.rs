use std::sync::Arc;
use std::time::{Duration, Instant};

use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_protocol::schemas::BuiltInPolicy;
use gents_store::config_client::{ClientSigningCapability, ConfigAccess};
use gents_store::identity::KeyIdentity;
use gents_store::local::backend_protection::FreshBackendStore;
use gents_store::local::document_acp::DocumentAcp;

use super::{client_key, home, serve_with_owner_staged, Home};

/// How long a relationship change the provider owes may take to settle
/// before a test reports the last state it saw.
pub const SETTLE_CEILING: Duration = Duration::from_secs(60);

/// Both kinds of home, local first.
pub const PROVIDERS: [DocumentAcp; 2] = [DocumentAcp::Local, DocumentAcp::Vera];

/// A home as `gents init` leaves it: its init User owns the node access
/// control and the Node document, which names the Node its runtime reader,
/// and the runtime serves over HTTP.
pub struct Household {
    pub directory: tempfile::TempDir,
    pub home: Home,
    pub provider: DocumentAcp,
    pub node: Arc<defra_node::Node>,
    pub url: String,
    pub user: ClientSigningCapability,
    pub user_did: identity::Did,
    pub node_did: identity::Did,
    pub node_doc: String,
}

impl Household {
    pub async fn open(provider: DocumentAcp) -> anyhow::Result<Self> {
        let directory = tempfile::tempdir()?;
        let mut home = home(directory.path(), "household").await?;
        let user = client_key(directory.path(), "user.key").await?;
        let user_did = identity::Did::new(user.did())?;
        home.owner = user_did.clone();
        FreshBackendStore::observe(&home.data, &home.lock)
            .await?
            .initialize(&home.key, &home.node, &home.owner, provider)
            .await?;
        let (node, url) = serve_with_owner_staged(&home, provider).await?;
        let node_did = identity::Did::new(home.node.did())?;
        let mut household = Self {
            directory,
            home,
            provider,
            node,
            url,
            user,
            user_did,
            node_did,
            node_doc: String::new(),
        };
        let runtime = household.node_did.clone();
        let user = household.as_user()?;
        household.node_doc = user
            .transact_write("test.household.node", |txn| {
                let runtime = runtime.clone();
                Box::pin(async move {
                    let created = txn
                        .execute_client_signed_create(
                            "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                            &serde_json::json!({"input": {"node_did": runtime.as_str(), "enabled": true}}),
                            gents_protocol::schemas::NODE,
                        )
                        .await?;
                    Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(&created, "Node")?)
                })
            })
            .await?;
        user.add_actor_relationship("Node", &household.node_doc, "runtime_reader", &runtime)
            .await?;
        household
            .settled(&household.user_did, "the Node document's runtime reader")
            .await?;
        Ok(household)
    }

    pub fn is_vera(&self) -> bool {
        self.provider == DocumentAcp::Vera
    }

    /// The init User as its client acts: over HTTP, with the delegation its
    /// endpoint mints for a Vera home and an identity token otherwise.
    pub fn as_user(&self) -> anyhow::Result<ConfigAccess> {
        self.as_client(&self.user)
    }

    pub fn as_client(&self, client: &ClientSigningCapability) -> anyhow::Result<ConfigAccess> {
        let reported = gents_store::local::document_acp::status_reports(
            self.provider,
            gents_store::local::document_acp::node_delegation_audience(&self.node),
        );
        super::as_user(&self.url, client, reported)
    }

    /// The runtime's own Node identity in its process.
    pub fn as_node(&self) -> ConfigAccess {
        ConfigAccess::local_as(self.node.clone(), self.node_did.clone())
    }

    /// A key held in this process, as a runtime holds its own keys.
    pub async fn held_key(&self, name: &str) -> anyhow::Result<Arc<KeyIdentity>> {
        let path = self.directory.path().join(name);
        Ok(Arc::new(
            tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(path, None)).await??,
        ))
    }

    /// Another User whose client holds its key, admitted as an enrolled User
    /// is: the init User grants it `OperatorUser`.
    pub async fn client(&self, name: &str) -> anyhow::Result<ClientSigningCapability> {
        let client = client_key(self.directory.path(), name).await?;
        gents_store::identity::GrantSet::OperatorUser
            .grant(&self.as_user()?, &identity::Did::new(client.did())?)
            .await?;
        Ok(client)
    }

    /// A managed Agent derived from the runtime key, as the runtime creates one.
    pub fn derive_agent(&self) -> anyhow::Result<AgentIdentityBinding> {
        Ok(gents_store::identity::provision_agent_identity(
            self.home.node.as_ref(),
        )?)
    }

    pub fn agent_access(&self, binding: &AgentIdentityBinding) -> anyhow::Result<ConfigAccess> {
        let agent =
            gents_store::identity::resolve_agent_identity(self.home.node.as_ref(), binding)?;
        Ok(ConfigAccess::local_managed_agent(
            self.node.clone(),
            identity::Did::new(agent.did())?,
            agent.as_ref(),
        )?)
    }

    /// Create `binding`'s Agent document as the init User: its client signs
    /// the create in one transaction and grants it its Node `parent` and its
    /// own `executor` after the commit, as configuration apply queues them.
    pub async fn create_agent(&self, binding: &AgentIdentityBinding) -> anyhow::Result<String> {
        let user = self.as_user()?;
        let runtime = self.node_did.clone();
        let agent_doc = user
            .transact_write("test.household.agent", |txn| {
                let runtime = runtime.clone();
                Box::pin(async move {
                    let created = txn
                        .execute_client_signed_create(
                            "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                            &agent_input(&runtime, binding),
                            gents_protocol::schemas::AGENT,
                        )
                        .await?;
                    Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(&created, "Agent")?)
                })
            })
            .await?;
        user.add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &self.node_doc)
            .await?;
        user.add_actor_relationship(
            "Agent",
            &agent_doc,
            "executor",
            &identity::Did::new(&binding.agent_did)?,
        )
        .await?;
        self.settled(&self.user_did, "the Agent's relationships")
            .await?;
        Ok(agent_doc)
    }

    /// The Engineer as `gents init` provisions it: a User-created Agent that
    /// administers the Node and writes its own configuration
    /// (Lean `SelfConfig.CategoryGrants.engineerInitRelations`), and holds
    /// `PolicyDelegate`, without which the projection of an Agent it creates
    /// is refused on any home.
    pub async fn engineer(&self) -> anyhow::Result<(AgentIdentityBinding, String)> {
        let engineer = self.derive_agent()?;
        let engineer_doc = self.create_agent(&engineer).await?;
        let did = identity::Did::new(&engineer.agent_did)?;
        let user = self.as_user()?;
        user.add_actor_relationship("Node", &self.node_doc, "administrator", &did)
            .await?;
        user.add_actor_relationship("Agent", &engineer_doc, "config_writer", &did)
            .await?;
        gents_store::identity::GrantSet::PolicyDelegate
            .grant(&user, &did)
            .await?;
        self.settled(&self.user_did, "the Engineer's relationships")
            .await?;
        Ok((engineer, engineer_doc))
    }

    /// Create `binding`'s Agent document as the Agent `creator` acts, with
    /// the relationships configuration apply stages for an Agent a managed
    /// Agent creates, in the same transaction: the Node's owner and
    /// administrators administer it, it sits under the Node, and it executes
    /// as itself.
    pub async fn agent_creates_agent(
        &self,
        creator: &AgentIdentityBinding,
        binding: &AgentIdentityBinding,
    ) -> anyhow::Result<String> {
        let (runtime, node_doc) = (self.node_did.clone(), self.node_doc.clone());
        let agent_doc = self
            .agent_access(creator)?
            .transact_write("test.household.agent_creates_agent", |txn| {
                let (runtime, node_doc) = (runtime.clone(), node_doc.clone());
                Box::pin(async move {
                    let created = txn
                        .execute_with_variables(
                            "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                            &agent_input(&runtime, binding),
                        )
                        .await?;
                    let agent_doc = gents_store::graphql::created_doc_id(&created, "Agent")?;
                    gents_store::config_client::publish_node_managers_in_txn(
                        txn,
                        "Agent",
                        &agent_doc,
                        "administrator",
                        &node_doc,
                    )
                    .await?;
                    txn.add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
                        .await?;
                    txn.add_actor_relationship(
                        "Agent",
                        &agent_doc,
                        "executor",
                        &identity::Did::new(&binding.agent_did)?,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(agent_doc)
                })
            })
            .await?;
        self.settled(
            &identity::Did::new(&creator.agent_did)?,
            "the created Agent's relationships",
        )
        .await?;
        Ok(agent_doc)
    }

    /// Wait, bounded by [`SETTLE_CEILING`], until `actor` owes the provider
    /// nothing; fail with what it still owes. A local home owes nothing.
    pub async fn settled(&self, actor: &identity::Did, what: &str) -> anyhow::Result<()> {
        let owed = self.owed_until(actor, Vec::is_empty).await?;
        anyhow::ensure!(
            owed.is_empty(),
            "{what}: still owed after the ceiling: {owed:?}"
        );
        Ok(())
    }

    /// What `actor` owes the provider once `done` holds of it, or the last
    /// state seen at the ceiling.
    pub async fn owed_until(
        &self,
        actor: &identity::Did,
        done: impl Fn(&Vec<acp::OwedEntry>) -> bool,
    ) -> anyhow::Result<Vec<acp::OwedEntry>> {
        if !self.is_vera() {
            return Ok(Vec::new());
        }
        let context =
            defra_core::RequestContext::default().with_identity(Some(self.node_did.to_string()));
        let started = Instant::now();
        loop {
            let owed = self.node.owed_acp_as(&context, actor.as_str()).await?;
            if done(&owed) || started.elapsed() > SETTLE_CEILING {
                return Ok(owed);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Whether `actor` holds `permission` on a document, as the store decides.
    pub async fn holds(
        &self,
        actor: &identity::Did,
        permission: &str,
        resource: &str,
        document: &str,
    ) -> anyhow::Result<bool> {
        Ok(self
            .node
            .document_acp()
            .check_named_permission(
                actor,
                permission,
                gents_store::local::document_acp::node_policy_ids(&self.node)
                    .id(BuiltInPolicy::Household),
                resource,
                document,
            )
            .await?)
    }

    /// The first time `actor` holds `permission`, from `started`, bounded by
    /// [`SETTLE_CEILING`]; none if it never does.
    pub async fn holds_within(
        &self,
        started: Instant,
        actor: &identity::Did,
        permission: &str,
        resource: &str,
        document: &str,
    ) -> anyhow::Result<Option<Duration>> {
        loop {
            if self.holds(actor, permission, resource, document).await? {
                return Ok(Some(started.elapsed()));
            }
            if started.elapsed() > SETTLE_CEILING {
                return Ok(None);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    pub async fn reopen_for_replication(mut self) -> anyhow::Result<Self> {
        self.node.shutdown().await?;
        drop(self.node);
        (self.node, self.url) = super::serve_with(
            &self.home,
            self.provider,
            Some(gents_store::config_client::OwnerStagedRoute::new()),
            Some(&self.user_did),
        )
        .await?;
        Ok(self)
    }

    pub async fn shutdown(self) -> anyhow::Result<()> {
        self.node.shutdown().await?;
        Ok(())
    }
}

/// The text of an error chain, for reporting what a caller saw.
pub fn seen(error: &(dyn std::error::Error + 'static)) -> String {
    gents_protocol::error_chain::ErrorChain(error).to_string()
}

/// A short description of an owed entry: its relation, state and reason.
pub fn describe(entry: &acp::OwedEntry) -> String {
    format!("{:?} {:?} {:?}", entry.what, entry.state, entry.reason)
}

fn agent_input(runtime: &identity::Did, binding: &AgentIdentityBinding) -> serde_json::Value {
    serde_json::json!({"input": {
        "node_did": runtime.as_str(),
        "agent_did": binding.agent_did,
        "creation_nonce": binding.creation_nonce,
        "inference_profile_id": "fixture",
        "enabled": true,
    }})
}

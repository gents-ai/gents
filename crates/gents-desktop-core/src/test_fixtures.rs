//! A served runtime home for desktop tests, configured the way a desktop
//! configures its own runtime: the User client key is the node's native ACP
//! owner, it publishes the Node and provisions managed Agents through its
//! client-signed HTTP endpoint, and every Agent carries a DID the runtime
//! derived from a real `creation_nonce`.

use std::sync::Arc;

use gents::documents::apply_desired_state_plan;
use gents::eval::runner::embedded::EmbeddedHome;
use gents::store::collection::Collection;
use gents::store::config_client::{
    ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan, GraphqlEndpoint,
};
use gents::store::identity::{GrantSet, KeyIdentity, NodeIdentity};
use gents_protocol::agent_identity::AgentIdentityBinding;

/// A real runtime home served over authenticated DefraDB HTTP with node ACP.
pub struct ServedRuntimeHome {
    home: EmbeddedHome,
    user: Arc<dyn NodeIdentity>,
    _user_dir: Option<tempfile::TempDir>,
}

impl ServedRuntimeHome {
    /// A home whose User is a fresh client key and whose Node is published.
    pub async fn start() -> Self {
        let home = Self::start_unpublished().await;
        home.publish_node(serde_json::json!({})).await;
        home
    }

    /// [`Self::start`] before the User publishes the Node.
    pub async fn start_unpublished() -> Self {
        let user_dir = tempfile::tempdir().unwrap();
        let key = user_dir.path().join("user.key");
        let user: Arc<dyn NodeIdentity> = Arc::new(
            tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(key, None).unwrap())
                .await
                .unwrap(),
        );
        let mut home = Self::start_unpublished_for(user).await;
        home._user_dir = Some(user_dir);
        home
    }

    /// A home whose User is `user`, such as a desktop client's own key.
    /// `gents init` makes its User the native admin (`HouseholdOwner`); the
    /// User, not the runtime, then creates the Node.
    pub async fn start_unpublished_for(user: Arc<dyn NodeIdentity>) -> Self {
        let home = EmbeddedHome::create_temp("desktop-runtime").await.unwrap();
        assert_ne!(home.did(), user.did(), "the User is not the runtime");
        GrantSet::HouseholdOwner
            .grant(
                &ConfigAccess::local_as(
                    home.node.clone(),
                    ::identity::Did::new(home.did()).unwrap(),
                ),
                &::identity::Did::new(user.did()).unwrap(),
            )
            .await
            .unwrap();
        Self {
            home,
            user,
            _user_dir: None,
        }
    }

    /// Publishes the runtime's Node with `fields` as the User.
    pub async fn publish_node(&self, mut fields: serde_json::Value) {
        fields["node_did"] = self.node_did().into();
        self.apply_as_user("test.served_runtime.node", vec![(Collection::Node, fields)])
            .await;
    }

    pub fn node(&self) -> &Arc<defra_node::Node> {
        &self.home.node
    }

    /// The runtime's DID, which is also its Node document's `node_did`.
    pub fn node_did(&self) -> &str {
        self.home.did()
    }

    pub fn runtime(&self) -> &Arc<dyn NodeIdentity> {
        &self.home.identity
    }

    pub fn user(&self) -> &Arc<dyn NodeIdentity> {
        &self.user
    }

    /// The runtime's authenticated DefraDB GraphQL URL.
    pub fn graphql(&self) -> &str {
        self.home.graphql_endpoint().unwrap()
    }

    pub fn home_path(&self) -> &std::path::Path {
        self.home.path()
    }

    /// The User's endpoint in the shape `ClientCore::operator_access` builds.
    pub fn user_endpoint(&self) -> GraphqlEndpoint {
        GraphqlEndpoint::as_principal(self.graphql(), self.user.did())
            .with_client_signing_capability(self.user.client_signing_capability().unwrap())
            .unwrap()
            .with_client_signed_agent_creates()
    }

    pub fn user_access(&self) -> ConfigAccess {
        ConfigAccess::Graphql(self.user_endpoint())
    }

    /// The User's authenticated GraphQL without the client-signed create
    /// route: ordinary mutations a raw GraphQL client sends as the native admin.
    pub fn user_graphql_access(&self) -> ConfigAccess {
        ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(self.graphql(), self.user.did())
                .with_client_signing_capability(self.user.client_signing_capability().unwrap())
                .unwrap(),
        )
    }

    /// The runtime's own local access, for effects only the runtime performs
    /// (catalog observations, probe status), never for User configuration.
    pub fn runtime_access(&self) -> ConfigAccess {
        ConfigAccess::local_as(
            self.node().clone(),
            ::identity::Did::new(self.node_did()).unwrap(),
        )
    }

    /// Provisions a managed Agent identity through the runtime's HTTP route,
    /// as the desktop's `ClientCore::provision_agent_identity` does.
    pub async fn provision_agent(&self) -> AgentIdentityBinding {
        crate::agent_identity::provision(&self.user_endpoint(), self.node_did())
            .await
            .unwrap()
    }

    /// Provisions a managed Agent and publishes it as the User with the
    /// inference backend and profile a chat target needs.
    pub async fn publish_chat_agent(&self) -> AgentIdentityBinding {
        let binding = self.provision_agent().await;
        let node_did = self.node_did();
        self.apply_as_user(
            "test.served_runtime.chat_agent",
            vec![
                (
                    Collection::InferenceBackend,
                    serde_json::json!({
                        "node_did": node_did, "backend_id": "backend", "name": "Fixture",
                        "provider_kind": "OpenAiCompatible", "endpoint": "http://localhost:8000/v1",
                        "auth": {"kind": "unauthenticated"},
                    }),
                ),
                (
                    Collection::InferenceProfile,
                    serde_json::json!({
                        "node_did": node_did, "profile_id": "profile",
                        "backend_id": "backend", "model_name": "model",
                    }),
                ),
                (Collection::Agent, self.agent_value(&binding, "profile")),
            ],
        )
        .await;
        binding
    }

    /// The managed Agent's own signed local access, holding the baseline
    /// grants production projects from its Agent document.
    pub async fn agent_access(&self, binding: &AgentIdentityBinding) -> ConfigAccess {
        let agent =
            gents::store::identity::resolve_agent_identity(self.runtime().as_ref(), binding)
                .unwrap();
        GrantSet::AgentBaseline
            .grant(
                &self.runtime_access(),
                &::identity::Did::new(agent.did()).unwrap(),
            )
            .await
            .unwrap();
        ConfigAccess::local_managed_agent(
            self.node().clone(),
            agent.did().parse().unwrap(),
            agent.as_ref(),
        )
        .unwrap()
    }

    /// The published Agent document's `_docID`, as the User reads it.
    pub async fn agent_doc_id(&self, binding: &AgentIdentityBinding) -> String {
        let response = self
            .user_access()
            .execute(&format!(
                r#"{{ Agent(filter: {{agent_did: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
                gents::store::graphql::escape_graphql_string(&binding.agent_did)
            ))
            .await
            .unwrap();
        response["data"]["Agent"][0]["_docID"]
            .as_str()
            .expect("published Agent")
            .to_owned()
    }

    /// Creates `session_id` for `requester_did` as the managed Agent and
    /// relates it to that Agent, as the runtime's own sessions are owned.
    /// Returns its document.
    pub async fn agent_session(
        &self,
        binding: &AgentIdentityBinding,
        session_id: &str,
        requester_did: &str,
    ) -> String {
        let agent_doc = self.agent_doc_id(binding).await;
        let node_did = self.node_did().to_owned();
        let agent_did = binding.agent_did.clone();
        self.agent_access(binding)
            .await
            .transact_write("test.served_runtime.agent_session", |txn| {
                let (node_did, agent_did, agent_doc) = (&node_did, &agent_did, &agent_doc);
                Box::pin(async move {
                    let response = txn
                        .execute_with_variables(
                            "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) {_docID} }",
                            &serde_json::json!({"input": {
                                "node_did": node_did, "session_id": session_id,
                                "requester_did": requester_did, "agent_did": agent_did,
                                "created_at": "2026-10-24T00:00:00Z",
                                "relationships_pending": false,
                            }}),
                        )
                        .await?;
                    let session_doc =
                        gents::store::graphql::created_doc_id(&response, "AgentSession")?;
                    txn.add_resource_relationship(
                        "AgentSession",
                        &session_doc,
                        "parent",
                        "agents",
                        agent_doc,
                    )
                    .await?;
                    txn.add_actor_relationship(
                        "AgentSession",
                        &session_doc,
                        "working_agent",
                        &agent_did.parse()?,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(session_doc)
                })
            })
            .await
            .unwrap()
    }

    /// The canonical Agent document value for `binding` on this runtime.
    pub fn agent_value(
        &self,
        binding: &AgentIdentityBinding,
        profile_id: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "node_did": self.node_did(), "agent_did": binding.agent_did,
            "creation_nonce": binding.creation_nonce,
            "inference_profile_id": profile_id,
        })
    }

    /// A fresh DID that no Node or Agent on this store uses yet.
    pub async fn fresh_did(&self) -> String {
        let directory = tempfile::tempdir().unwrap();
        let key = directory.path().join("foreign.key");
        tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(key, None).unwrap())
            .await
            .unwrap()
            .did()
            .to_owned()
    }

    /// Publishes, as the User, a second Node scope on this store for a
    /// [`Self::fresh_did`], so scope checks have a same-named neighbour.
    /// Returns its DID.
    pub async fn publish_foreign_node(&self) -> String {
        let node_did = self.fresh_did().await;
        self.apply_as_user(
            "test.served_runtime.foreign_node",
            vec![(Collection::Node, serde_json::json!({"node_did": node_did}))],
        )
        .await;
        node_did
    }

    /// Applies `documents` in one desired-state plan as the User.
    pub async fn apply_as_user(
        &self,
        operation: &'static str,
        documents: Vec<(Collection, serde_json::Value)>,
    ) {
        let plan = DesiredStateApplyPlan::new(
            documents
                .into_iter()
                .map(|(collection, value)| DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                })
                .collect(),
        )
        .unwrap();
        self.user_access()
            .transact_write(operation, |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
            })
            .await
            .unwrap();
    }

    /// Routes `core` to this runtime as its co-hosted local runtime, so its
    /// operator access is the User's client-signed endpoint here.
    pub async fn connect(&self, core: &crate::client::ClientCore) {
        core.add_local_standard_peer_route_for_test(
            "Served Runtime",
            "127.0.0.1:56000/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb",
            self.node_did(),
            self.graphql(),
            &self.home_path().display().to_string(),
        )
        .await
        .unwrap();
    }

    pub async fn shutdown(self) {
        self.home.node.shutdown().await.unwrap();
    }
}

/// A proxy in front of a runtime's GraphQL endpoint that holds every request
/// whose body `held` selects until the test releases it, so a test can change
/// the runtime while that read is in flight. Each held or plain request gets
/// its own connection, so none bypasses inspection on a pooled one;
/// subscriptions stream through untouched.
pub struct GatedRuntimeProxy {
    url: String,
    held: tokio::sync::mpsc::UnboundedReceiver<HeldRequest>,
    task: tokio::task::JoinHandle<()>,
}

/// A request the proxy holds; dropping it without [`Self::release`] fails it.
pub struct HeldRequest {
    pub body: String,
    release: tokio::sync::oneshot::Sender<()>,
}

impl HeldRequest {
    pub fn release(self) {
        let _ = self.release.send(());
    }
}

impl GatedRuntimeProxy {
    pub async fn start(upstream_graphql: &str, held: fn(&str) -> bool) -> Self {
        let upstream = upstream_graphql
            .strip_prefix("http://")
            .and_then(|rest| rest.split('/').next())
            .expect("an http runtime GraphQL URL")
            .to_owned();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
        let (held_tx, held_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((client, _)) = accepted else { break };
                        connections.spawn(forward(client, upstream.clone(), held, held_tx.clone()));
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Self {
            url,
            held: held_rx,
            task,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// The next held request, in arrival order.
    pub async fn next(&mut self) -> HeldRequest {
        tokio::time::timeout(std::time::Duration::from_secs(15), self.held.recv())
            .await
            .expect("a held runtime request")
            .expect("proxy running")
    }
}

impl Drop for GatedRuntimeProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn forward(
    mut client: tokio::net::TcpStream,
    upstream: String,
    held: fn(&str) -> bool,
    held_tx: tokio::sync::mpsc::UnboundedSender<HeldRequest>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut request = Vec::new();
    let mut buffer = [0_u8; 8192];
    let (head_end, length) = loop {
        let Ok(read) = client.read(&mut buffer).await else {
            return;
        };
        if read == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..read]);
        if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or_default();
            break (end, length);
        }
    };
    while request.len() < head_end + 4 + length {
        let Ok(read) = client.read(&mut buffer).await else {
            return;
        };
        if read == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..read]);
    }
    let head = String::from_utf8_lossy(&request[..head_end]).to_ascii_lowercase();
    if !head.contains("accept: text/event-stream") {
        let line_end = request.windows(2).position(|w| w == b"\r\n").unwrap() + 2;
        request.splice(line_end..line_end, b"connection: close\r\n".iter().copied());
        let body = String::from_utf8_lossy(&request[request.len() - length..]).into_owned();
        if held(&body) {
            let (release, released) = tokio::sync::oneshot::channel();
            if held_tx.send(HeldRequest { body, release }).is_err() || released.await.is_err() {
                return;
            }
        }
    }
    let Ok(mut server) = tokio::net::TcpStream::connect(&upstream).await else {
        return;
    };
    if server.write_all(&request).await.is_err() {
        return;
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
}

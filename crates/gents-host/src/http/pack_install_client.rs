use anyhow::{ensure, Context, Result};
use futures_util::{SinkExt, StreamExt};
use gents_store::config_client::{
    prepare_pack_evidence_from_history, prepare_pack_relationship, prepare_pack_schema_approval,
    ClientSigningCapability, GraphqlEndpoint, PackApprovalError, PackApprovalScope, PackMutation,
    PackRelationship, PackReplayTransport,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, protocol::Message},
    MaybeTlsStream, WebSocketStream,
};

mod error;
pub use error::{PackSessionError, ReceiveError, SendError, SessionStep};

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ServerMessage {
    Ready {
        scope: PackApprovalScope,
        preparation: gents_pack::pack::HostedPackPreparation,
    },
    ReadResult {
        id: u64,
        result: Value,
    },
    ApproveGraph {
        id: u64,
        mutation: PackMutation,
        history: Option<Value>,
    },
    ApproveRelationship {
        id: u64,
        relationship: PackRelationship,
    },
    Applied {
        id: u64,
        result: Value,
    },
    Timestamp {
        id: u64,
        kind: String,
    },
    Finished {
        result: Value,
    },
    Previewed {
        result: Value,
    },
    Failed {
        class: gents_protocol::operations::pack::hosted::HostedInstallFailure,
        outcome: gents_protocol::operations::pack::hosted::HostedInstallOutcome,
        detail: Option<String>,
        documents: Option<gents_pack::pack::EditedDocumentRefusal>,
    },
}

impl ServerMessage {
    fn kind(&self) -> &'static str {
        match self {
            Self::Ready { .. } => "ready",
            Self::ReadResult { .. } => "read_result",
            Self::ApproveGraph { .. } => "approve_graph",
            Self::ApproveRelationship { .. } => "approve_relationship",
            Self::Applied { .. } => "applied",
            Self::Timestamp { .. } => "timestamp",
            Self::Finished { .. } => "finished",
            Self::Previewed { .. } => "previewed",
            Self::Failed { .. } => "failed",
        }
    }

    /// The error this message is when it does not answer `step`.
    fn refusal(self, step: SessionStep) -> PackSessionError {
        match self {
            Self::Failed {
                class,
                outcome,
                detail,
                documents,
            } => gents_pack::pack::HostedRefusal::new(class, outcome, detail, documents).into(),
            other => PackSessionError::Unexpected {
                step,
                received: other.kind(),
            },
        }
    }
}

type InstallSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A single runtime-owned transaction, driven by the client's pack interpreter.
pub struct PackInstallSession {
    socket: InstallSocket,
    endpoint: GraphqlEndpoint,
    presented_at: std::time::Instant,
    actor: ClientSigningCapability,
    scope: PackApprovalScope,
    preparation: gents_pack::pack::HostedPackPreparation,
    pending: Option<ServerMessage>,
    next_id: u64,
}

impl PackInstallSession {
    pub async fn start(
        endpoint: &GraphqlEndpoint,
        request: Value,
        node_did: &str,
        pack_digests: &[String],
    ) -> Result<Self, PackSessionError> {
        Self::open(
            endpoint,
            json!({"type": "start", "request": request}),
            node_did,
            pack_digests,
        )
        .await
    }

    /// Open a session whose first message is `first`, for the effects of
    /// `pack_digests` on `node_did`.
    pub async fn open(
        endpoint: &GraphqlEndpoint,
        first: Value,
        node_did: &str,
        pack_digests: &[String],
    ) -> Result<Self, PackSessionError> {
        let actor = endpoint
            .held_client_signing_capability()
            .map_err(|error| PackSessionError::Endpoint(error.into()))?
            .clone();
        let mut socket = connect(endpoint)
            .await
            .map_err(PackSessionError::Endpoint)?;
        let presented_at = std::time::Instant::now();
        send(&mut socket, first).await?;
        let (scope, preparation) = match receive(&mut socket).await? {
            ServerMessage::Ready { scope, preparation } => (scope, preparation),
            other => return Err(other.refusal(SessionStep::Start)),
        };
        if scope.node_did != node_did
            || scope.pack_digests != pack_digests
            || scope.nonce.is_empty()
        {
            return Err(PackSessionError::ScopeMismatch);
        }
        Ok(Self {
            socket,
            endpoint: endpoint.clone(),
            presented_at,
            actor,
            scope,
            preparation,
            pending: None,
            next_id: 1,
        })
    }

    pub fn preparation(&self) -> &gents_pack::pack::HostedPackPreparation {
        &self.preparation
    }

    /// Send `value`, first presenting a fresh delegation when the one the
    /// session holds is due (`Identity.OwedRegistration.refreshDue`).
    async fn send(&mut self, value: Value) -> Result<(), PackSessionError> {
        if self.endpoint.mints_delegations()
            && gents_store::config_client::owed::refresh_due(self.presented_at.elapsed())
        {
            let authorization = self
                .endpoint
                .authorization()
                .map_err(|error| PackSessionError::Endpoint(error.into()))?
                .ok_or_else(|| {
                    PackSessionError::Endpoint(anyhow::anyhow!("client authentication is required"))
                })?;
            self.presented_at = std::time::Instant::now();
            send(
                &mut self.socket,
                json!({"type": "delegation", "authorization": authorization}),
            )
            .await?;
        }
        Ok(send(&mut self.socket, value).await?)
    }

    pub async fn read(
        &mut self,
        document: &str,
        variables: &Value,
    ) -> Result<Value, PackSessionError> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(PackSessionError::IdsExhausted)?;
        self.send(json!({"type": "read", "id": id, "document": document, "variables": variables}))
            .await?;
        loop {
            match receive(&mut self.socket).await? {
                ServerMessage::ReadResult {
                    id: received,
                    result,
                } if received == id => return Ok(result),
                proposal @ (ServerMessage::ApproveGraph { .. }
                | ServerMessage::ApproveRelationship { .. }
                | ServerMessage::Timestamp { .. })
                    if self.pending.is_none() =>
                {
                    self.pending = Some(proposal)
                }
                other => return Err(other.refusal(SessionStep::Read)),
            }
        }
    }

    async fn proposal(&mut self) -> Result<ServerMessage, PackSessionError> {
        match self.pending.take() {
            Some(proposal) => Ok(proposal),
            None => Ok(receive(&mut self.socket).await?),
        }
    }

    pub async fn graph(&mut self, expected: &PackMutation) -> Result<Value, PackSessionError> {
        let (id, history) = match self.proposal().await? {
            ServerMessage::ApproveGraph {
                id,
                mutation,
                history,
            } if mutation == *expected => (id, history),
            ServerMessage::ApproveGraph { mutation, .. } => {
                return Err(PackSessionError::GraphDiffers {
                    collection: expected.collection().to_owned(),
                    doc_id: expected.doc_id().map(str::to_owned),
                    differences: expected.differences(&mutation),
                })
            }
            other => return Err(other.refusal(SessionStep::Graph)),
        };
        let ids = self
            .endpoint
            .home_policy_ids()
            .await
            .map_err(PackSessionError::HomePolicies)?;
        let evidence =
            prepare_pack_evidence_from_history(&self.actor, &self.scope, expected, history, ids)
                .await?;
        self.send(json!({"type": "graph_evidence", "id": id, "evidence": evidence}))
            .await?;
        self.applied(id).await
    }

    pub async fn relationship(
        &mut self,
        expected: &PackRelationship,
    ) -> Result<(), PackSessionError> {
        let id = match self.proposal().await? {
            ServerMessage::ApproveRelationship { id, relationship }
                if relationship == *expected =>
            {
                id
            }
            ServerMessage::ApproveRelationship { relationship, .. } => {
                return Err(PackSessionError::RelationshipDiffers {
                    collection: expected.collection.clone(),
                    doc_id: expected.doc_id.clone(),
                    differences: expected.differences(&relationship),
                })
            }
            other => return Err(other.refusal(SessionStep::Relationship)),
        };
        let evidence = prepare_pack_relationship(&self.actor, &self.scope, expected).await?;
        self.send(json!({"type": "relationship_evidence", "id": id, "evidence": evidence}))
            .await?;
        if !self.applied(id).await?.is_null() {
            return Err(PackSessionError::RelationshipResult);
        }
        Ok(())
    }

    async fn applied(&mut self, id: u64) -> Result<Value, PackSessionError> {
        match receive(&mut self.socket).await? {
            ServerMessage::Applied {
                id: received,
                result,
            } if received == id => Ok(result),
            other => Err(other.refusal(SessionStep::Effect)),
        }
    }

    async fn timestamp(&mut self, expected: &'static str) -> Result<String, PackSessionError> {
        let id = match self.proposal().await? {
            ServerMessage::Timestamp { id, kind } if kind == expected => id,
            ServerMessage::Timestamp { kind, .. } => {
                return Err(PackSessionError::TimestampDiffers {
                    expected,
                    proposed: kind,
                })
            }
            other => return Err(other.refusal(SessionStep::Timestamp)),
        };
        let value = gents_store::config_client::mint_recreate_identity_timestamp();
        self.send(json!({"type": "timestamp_value", "id": id, "value": value}))
            .await?;
        Ok(value)
    }

    async fn finish(&mut self) -> Result<Value, PackSessionError> {
        if self.pending.is_some() {
            return Err(PackSessionError::ExtraEffect);
        }
        self.send(json!({"type": "finish"})).await?;
        match receive(&mut self.socket).await? {
            ServerMessage::Finished { result } => Ok(result),
            other => Err(other.refusal(SessionStep::Finish)),
        }
    }
}

async fn connect(endpoint: &GraphqlEndpoint) -> Result<InstallSocket> {
    let mut url = reqwest::Url::parse(endpoint.url())?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "remote pack install requires an HTTP URL without credentials"
    );
    let prefix = url
        .path()
        .strip_suffix("/api/v0/graphql")
        .context("remote pack install requires a Gents runtime GraphQL endpoint")?
        .to_owned();
    url.set_path(&format!("{prefix}/packs/install/session"));
    url.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" })
        .map_err(|_| anyhow::anyhow!("invalid runtime WebSocket URL"))?;
    let mut upgrade = url.as_str().into_client_request()?;
    let authorization = endpoint
        .runtime_authorization()
        .await?
        .context("client authentication is required")?;
    let mut authorization =
        authorization.parse::<tokio_tungstenite::tungstenite::http::HeaderValue>()?;
    authorization.set_sensitive(true);
    upgrade.headers_mut().insert(
        tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
        authorization,
    );
    let (socket, _) = connect_async(upgrade)
        .await
        .context("connecting remote pack install")?;
    Ok(socket)
}

async fn send(socket: &mut InstallSocket, value: Value) -> Result<(), SendError> {
    let encoded = tokio::task::spawn_blocking(move || serde_json::to_string(&value))
        .await
        .map_err(SendError::Stopped)?
        .map_err(SendError::Encode)?;
    socket
        .send(Message::Text(encoded.into()))
        .await
        .map_err(SendError::Lost)
}

async fn receive(socket: &mut InstallSocket) -> Result<ServerMessage, ReceiveError> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                let bytes = text.len();
                return tokio::task::spawn_blocking(move || {
                    serde_json::from_str::<ServerMessage>(&text)
                })
                .await
                .map_err(ReceiveError::Decode)?
                .map_err(|error| ReceiveError::Unreadable {
                    bytes,
                    line: error.line(),
                    column: error.column(),
                });
            }
            Some(Ok(Message::Ping(_)))
            | Some(Ok(Message::Pong(_)))
            | Some(Ok(Message::Frame(_))) => {}
            Some(Ok(_)) | None => return Err(ReceiveError::Closed),
            Some(Err(error)) => return Err(ReceiveError::Lost(error)),
        }
    }
}

/// Serializes interpreter requests on the same authenticated session.
pub struct PackReplaySession {
    inner: tokio::sync::Mutex<PackReplayState>,
}

struct PackReplayState {
    session: Option<PackInstallSession>,
    result: Option<Value>,
    refusal: Option<gents_pack::pack::HostedRefusal>,
}

impl PackReplayState {
    /// The transport error the interpreter sees, keeping the runtime's typed
    /// refusal for the install to return.
    fn refused(&mut self, error: PackSessionError) -> PackApprovalError {
        if let PackSessionError::RuntimeFailed(refusal) = &error {
            self.refusal.get_or_insert_with(|| refusal.clone());
        }
        transport_error(error)
    }
}

impl PackReplaySession {
    pub fn new(session: PackInstallSession) -> Self {
        Self {
            inner: tokio::sync::Mutex::new(PackReplayState {
                session: Some(session),
                result: None,
                refusal: None,
            }),
        }
    }

    pub async fn result(&self) -> Result<Value, PackSessionError> {
        self.inner
            .lock()
            .await
            .result
            .take()
            .ok_or(PackSessionError::NoResult)
    }
}

fn transport_error(error: impl Into<PackSessionError>) -> PackApprovalError {
    PackApprovalError::Transport(format!("{:#}", anyhow::Error::from(error.into())))
}

#[async_trait::async_trait]
impl PackReplayTransport for PackReplaySession {
    async fn read(
        &self,
        document: &str,
        variables: &Value,
    ) -> std::result::Result<Value, PackApprovalError> {
        let mut state = self.inner.lock().await;
        state
            .session
            .as_mut()
            .ok_or(PackSessionError::Closed)
            .map_err(transport_error)?
            .read(document, variables)
            .await
            .map_err(|error| state.refused(error))
    }

    async fn apply_graph(
        &self,
        mutation: &PackMutation,
    ) -> std::result::Result<Value, PackApprovalError> {
        let mut state = self.inner.lock().await;
        state
            .session
            .as_mut()
            .ok_or(PackSessionError::Closed)
            .map_err(transport_error)?
            .graph(mutation)
            .await
            .map_err(|error| state.refused(error))
    }

    async fn apply_relationship(
        &self,
        relationship: &PackRelationship,
    ) -> std::result::Result<(), PackApprovalError> {
        let mut state = self.inner.lock().await;
        state
            .session
            .as_mut()
            .ok_or(PackSessionError::Closed)
            .map_err(transport_error)?
            .relationship(relationship)
            .await
            .map_err(|error| state.refused(error))
    }

    async fn next_timestamp(
        &self,
        kind: &'static str,
    ) -> std::result::Result<String, PackApprovalError> {
        let mut state = self.inner.lock().await;
        state
            .session
            .as_mut()
            .ok_or(PackSessionError::Closed)
            .map_err(transport_error)?
            .timestamp(kind)
            .await
            .map_err(|error| state.refused(error))
    }

    async fn commit(&self) -> std::result::Result<(), PackApprovalError> {
        let mut state = self.inner.lock().await;
        let result = state
            .session
            .as_mut()
            .ok_or(PackSessionError::Closed)
            .map_err(transport_error)?
            .finish()
            .await
            .map_err(|error| state.refused(error))?;
        state.result = Some(result);
        state.session.take();
        Ok(())
    }

    async fn abort(&self) -> std::result::Result<(), PackApprovalError> {
        let mut state = self.inner.lock().await;
        if let Some(mut session) = state.session.take() {
            send(&mut session.socket, json!({"type": "abort"}))
                .await
                .map_err(transport_error)?;
        }
        Ok(())
    }
}

pub async fn install_remote(
    endpoint: &GraphqlEndpoint,
    owner: &str,
    pack: &gents_pack::pack::resolved_pack::ResolvedPack,
    dependencies: &[gents_pack::pack::resolved_pack::ResolvedPack],
    args: &gents_pack::pack::install_flow::DocumentsInstallRequest,
    explicit: bool,
) -> Result<Value, gents_pack::pack::PackChangeError> {
    if !args.wire.is_empty() {
        return Err(anyhow::anyhow!(
            "a hosted pack install carries no wiring; its plugins are wired after it"
        )
        .into());
    }
    let request =
        super::pack_install::encode_install_request(pack, dependencies, args, owner, explicit)
            .await?;
    let digests = std::iter::once(pack)
        .chain(dependencies.iter())
        .map(|source| source.digest().to_owned())
        .collect::<Vec<_>>();
    let mut session = PackInstallSession::start(endpoint, request, owner, &digests)
        .await
        .map_err(refused)?;
    let preparation = session.preparation().clone();
    let access = gents_store::config_client::ConfigAccess::Graphql(endpoint.clone());
    let requested = gents_pack::operations::pack::parse_inference_slots(&args.inference_slots)?;
    let supplied = gents_pack::pack::install_flow::supplied_variables(
        &args.dependency_variables,
        &args.bind_agents,
    )?;
    let overrides = gents_pack::operations::pack::dependency_overrides(&supplied);
    let mut prepared = gents_pack::pack::prepare_hosted_document_pack_archive(
        &access,
        owner,
        pack.archive(),
        &gents_pack::pack::PackGraphInputs {
            dependencies: &dependencies
                .iter()
                .map(gents_pack::pack::resolved_pack::ResolvedPack::archive)
                .collect::<Vec<_>>(),
            requested: &requested,
            environment: &|name| {
                gents_pack::operations::pack::install_value(
                    gents_protocol::operations::pack::PackActor::User,
                    &supplied,
                    &args.environment,
                    name,
                )
            },
            overrides: &overrides,
        },
        &preparation,
    )
    .await?;
    gents_pack::pack::resolved_pack::ResolvedPack::record_graph_sources(
        &mut prepared,
        pack,
        dependencies,
    );
    let prepared = std::sync::Arc::new(prepared);
    let graph_archive = pack.shared_archive();
    let graph_prepared = prepared.clone();
    let graph = tokio::task::spawn_blocking(move || {
        gents_pack::pack::hosted_document_graph(&graph_archive, &graph_prepared, explicit)
    })
    .await
    .context("reconstructing the reviewed pack graph")??;
    let evidence = prepare_pack_schema_approval(&session.actor, &session.scope)
        .await
        .map_err(anyhow::Error::from)?;
    send(
        &mut session.socket,
        json!({"type": "schemas_ready", "evidence": evidence}),
    )
    .await
    .map_err(anyhow::Error::from)?;
    let replay = std::sync::Arc::new(PackReplaySession::new(session));
    let access = gents_store::config_client::ConfigAccess::Graphql(
        endpoint
            .clone()
            .with_pack_replay(replay.clone())
            .map_err(anyhow::Error::from)?,
    );
    let policy = gents_pack::operations::pack::drift_policy(args.edited);
    let applied = access
        .transact_write("pack.hosted_document_graph.replay", |txn| {
            let graph = &graph;
            Box::pin(async move {
                gents_pack::pack::apply_hosted_document_graph(
                    txn,
                    owner,
                    graph,
                    policy,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
                .map_err(gents_pack::pack::PackChangeError::into_anyhow)?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await;
    if let Err(error) = applied {
        return Err(match replay.inner.lock().await.refusal.take() {
            Some(refusal) => refusal.into(),
            None => error.into(),
        });
    }
    replay.result().await.map_err(refused)
}

/// A session error as the install's: the runtime's typed refusal by its own
/// variant.
fn refused(error: PackSessionError) -> gents_pack::pack::PackChangeError {
    match error {
        PackSessionError::RuntimeFailed(refusal) => refusal.into(),
        other => anyhow::Error::from(other).into(),
    }
}

/// Previews an install over the runtime's install session, which authorizes
/// the client's User to update the Node first. A preview writes nothing.
pub async fn preview_remote(
    endpoint: &GraphqlEndpoint,
    owner: &str,
    pack: &gents_pack::pack::resolved_pack::ResolvedPack,
    dependencies: &[gents_pack::pack::resolved_pack::ResolvedPack],
    args: &gents_pack::pack::install_flow::DocumentsInstallRequest,
    explicit: bool,
) -> Result<Value> {
    ensure!(args.preview, "a remote preview is a request with --preview");
    let request =
        super::pack_install::encode_install_request(pack, dependencies, args, owner, explicit)
            .await?;
    let mut socket = connect(endpoint).await?;
    send(&mut socket, json!({"type": "start", "request": request})).await?;
    match receive(&mut socket).await? {
        ServerMessage::Previewed { result } => Ok(result),
        other => Err(other.refusal(SessionStep::Start).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_store::identity::KeyIdentity;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    #[tokio::test]
    async fn refuses_a_graph_that_differs_from_the_interpreter() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut socket = accept_async(stream).await?;
            let _ = socket.next().await.context("missing start")??;
            socket.send(Message::Text(json!({
                "type": "ready",
                "scope": {"node_did": "did:key:node", "pack_digests": ["sha256:root"], "nonce": "test"},
                "preparation": {"agent_bindings": {}, "plugin_digests": {}, "operator_ux_digests": {}}
            }).to_string().into())).await?;
            socket.send(Message::Text(json!({
                "type": "approve_graph", "id": 1,
                "mutation": {"kind": "create", "collection": "Tools", "input": {"tools_id": "injected"}},
                "history": null
            }).to_string().into())).await?;
            let next = socket.next().await;
            anyhow::ensure!(
                !matches!(next, Some(Ok(Message::Text(_)))),
                "client sent evidence for a different graph"
            );
            Ok::<_, anyhow::Error>(())
        });
        let key_home = tempfile::tempdir()?;
        let key = KeyIdentity::load_or_create(key_home.path().join("user.key"), None)?
            .client_signing_capability()?;
        let endpoint = GraphqlEndpoint::as_principal(
            format!("http://{address}/api/v0/graphql"),
            key.did().to_owned(),
        )
        .with_client_signing_capability(key)?;
        let mut session = PackInstallSession::start(
            &endpoint,
            json!({}),
            "did:key:node",
            &["sha256:root".to_owned()],
        )
        .await?;
        let expected = PackMutation::Create {
            collection: "Tools".into(),
            input: json!({"tools_id": "reviewed"}),
        };
        let refused = session
            .graph(&expected)
            .await
            .expect_err("a graph the client did not plan is refused");
        let PackSessionError::GraphDiffers {
            collection,
            doc_id,
            differences,
        } = &refused
        else {
            panic!("{refused:#}");
        };
        assert_eq!((collection.as_str(), doc_id), ("Tools", &None));
        assert_eq!(
            differences,
            &vec![gents_store::config_client::PackMutationDifference::Field {
                path: "/tools_id".into(),
                constructed: Some(gents_store::config_client::ValueShape::Text { chars: 8 }),
                proposed: Some(gents_store::config_client::ValueShape::Text { chars: 8 }),
            }]
        );
        let text = refused.to_string();
        assert!(
            !text.contains("injected") && !text.contains("reviewed"),
            "{text}"
        );
        drop(session);
        server.await??;
        Ok(())
    }
    #[tokio::test]
    async fn read_can_pass_a_waiting_runtime_proposal() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut socket = accept_async(stream).await?;
            let _ = socket.next().await.context("missing start")??;
            socket.send(Message::Text(json!({
                "type": "ready",
                "scope": {"node_did": "did:key:node", "pack_digests": ["sha256:root"], "nonce": "test"},
                "preparation": {"agent_bindings": {}, "plugin_digests": {}, "operator_ux_digests": {}}
            }).to_string().into())).await?;
            socket
                .send(Message::Text(
                    json!({"type": "timestamp", "id": 42, "kind": "document"})
                        .to_string()
                        .into(),
                ))
                .await?;
            let request = socket.next().await.context("missing read")??;
            let Message::Text(request) = request else {
                anyhow::bail!("expected read");
            };
            let request: Value = serde_json::from_str(&request)?;
            anyhow::ensure!(request["type"] == "read", "expected read");
            socket
                .send(Message::Text(
                    json!({"type": "read_result", "id": request["id"], "result": {"data": {}}})
                        .to_string()
                        .into(),
                ))
                .await?;
            let response = socket.next().await.context("missing timestamp")??;
            let Message::Text(response) = response else {
                anyhow::bail!("expected timestamp response");
            };
            let response: Value = serde_json::from_str(&response)?;
            anyhow::ensure!(
                response["type"] == "timestamp_value"
                    && response["id"] == 42
                    && response["value"].is_string(),
                "invalid timestamp response"
            );
            Ok::<_, anyhow::Error>(())
        });
        let key_home = tempfile::tempdir()?;
        let key = KeyIdentity::load_or_create(key_home.path().join("user.key"), None)?
            .client_signing_capability()?;
        let endpoint = GraphqlEndpoint::as_principal(
            format!("http://{address}/api/v0/graphql"),
            key.did().to_owned(),
        )
        .with_client_signing_capability(key)?;
        let mut session = PackInstallSession::start(
            &endpoint,
            json!({}),
            "did:key:node",
            &["sha256:root".to_owned()],
        )
        .await?;
        assert_eq!(
            session.read("{ Node { _docID } }", &json!({})).await?,
            json!({"data": {}})
        );
        let _ = session.timestamp("document").await?;
        server.await??;
        Ok(())
    }

    fn ready() -> Value {
        json!({
            "type": "ready",
            "scope": {"node_did": "did:key:node", "pack_digests": ["sha256:root"], "nonce": "test"},
            "preparation": {"agent_bindings": {}, "plugin_digests": {}, "operator_ux_digests": {}}
        })
    }

    /// A runtime that answers the first message with `replies` (text frames
    /// sent as they are), then waits for the client to close.
    async fn scripted_runtime(
        replies: Vec<String>,
    ) -> Result<(
        GraphqlEndpoint,
        tokio::task::JoinHandle<Result<()>>,
        tempfile::TempDir,
    )> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut socket = accept_async(stream).await?;
            let _ = socket.next().await.context("missing start")??;
            if replies.is_empty() {
                socket.close(None).await?;
                return Ok(());
            }
            for reply in replies {
                socket.send(Message::Text(reply.into())).await?;
            }
            while let Some(Ok(message)) = socket.next().await {
                if matches!(message, Message::Close(_)) {
                    break;
                }
            }
            Ok::<_, anyhow::Error>(())
        });
        let key_home = tempfile::tempdir()?;
        let key = KeyIdentity::load_or_create(key_home.path().join("user.key"), None)?
            .client_signing_capability()?;
        let endpoint = GraphqlEndpoint::as_principal(
            format!("http://{address}/api/v0/graphql"),
            key.did().to_owned(),
        )
        .with_client_signing_capability(key)?;
        Ok((endpoint, server, key_home))
    }

    async fn start(endpoint: &GraphqlEndpoint) -> Result<PackInstallSession, PackSessionError> {
        PackInstallSession::start(
            endpoint,
            json!({}),
            "did:key:node",
            &["sha256:root".to_owned()],
        )
        .await
    }

    #[tokio::test]
    async fn the_first_reply_tells_an_unreadable_message_from_a_closed_connection() -> Result<()> {
        let unreadable = "not json sk-live-private".to_owned();
        let bytes = unreadable.len();
        let (endpoint, server, _key) = scripted_runtime(vec![unreadable]).await?;
        let error = start(&endpoint).await.err().expect("unreadable");
        assert!(
            matches!(
                error,
                PackSessionError::Receive(ReceiveError::Unreadable { bytes: got, .. }) if got == bytes
            ),
            "{error:#}"
        );
        assert!(!format!("{error:#}").contains("sk-live-private"));
        server.await??;

        let (endpoint, server, _key) = scripted_runtime(Vec::new()).await?;
        let error = start(&endpoint).await.err().expect("closed");
        assert!(
            matches!(error, PackSessionError::Receive(ReceiveError::Closed)),
            "{error:#}"
        );
        server.await??;
        Ok(())
    }

    #[tokio::test]
    async fn an_unexpected_first_message_is_named_by_its_type() -> Result<()> {
        let finished = json!({"type": "finished", "result": {"pack": "early"}}).to_string();
        let (endpoint, server, _key) = scripted_runtime(vec![finished]).await?;
        let error = start(&endpoint).await.err().expect("not ready");
        assert!(
            matches!(
                error,
                PackSessionError::Unexpected {
                    step: SessionStep::Start,
                    received: "finished"
                }
            ),
            "{error:#}"
        );
        server.await??;
        Ok(())
    }

    #[tokio::test]
    async fn a_relationship_the_client_did_not_plan_is_named_without_values() -> Result<()> {
        let relationship = |did: &str| PackRelationship {
            collection: "Agent".into(),
            doc_id: "bae-agent".into(),
            relation: "reader".into(),
            target: gents_store::config_client::PackRelationshipTarget::Actor { did: did.into() },
            delete: false,
        };
        let proposal = json!({"type": "approve_relationship", "id": 1,
            "relationship": relationship("did:key:injected")})
        .to_string();
        let (endpoint, server, _key) =
            scripted_runtime(vec![ready().to_string(), proposal]).await?;
        let mut session = start(&endpoint).await?;
        let error = session
            .relationship(&relationship("did:key:reviewed"))
            .await
            .expect_err("a relationship the client did not plan is refused");
        let PackSessionError::RelationshipDiffers {
            collection,
            doc_id,
            differences,
        } = &error
        else {
            panic!("{error:#}");
        };
        assert_eq!(
            (collection.as_str(), doc_id.as_str()),
            ("Agent", "bae-agent")
        );
        assert_eq!(differences.len(), 1, "{error}");
        let text = error.to_string();
        assert!(text.contains("/target/did"), "{text}");
        assert!(
            !text.contains("injected") && !text.contains("reviewed"),
            "{text}"
        );
        drop(session);
        server.await??;
        Ok(())
    }

    #[tokio::test]
    async fn a_runtime_refusal_is_its_class_and_next_step() -> Result<()> {
        use gents_protocol::operations::pack::hosted::{
            HostedInstallFailure, HostedInstallOutcome,
        };
        let failed =
            json!({"type": "failed", "class": "apply_rejected", "outcome": "not_installed"})
                .to_string();
        let (endpoint, server, _key) = scripted_runtime(vec![ready().to_string(), failed]).await?;
        let mut session = start(&endpoint).await?;
        let error = session
            .read("{ Node { _docID } }", &json!({}))
            .await
            .expect_err("the runtime refused");
        assert!(
            matches!(
                error,
                PackSessionError::RuntimeFailed(gents_pack::pack::HostedRefusal {
                    class: HostedInstallFailure::ApplyRejected,
                    outcome: HostedInstallOutcome::NotInstalled,
                    detail: None,
                    documents: None,
                })
            ),
            "{error:#}"
        );
        assert_eq!(
            error.to_string(),
            format!(
                "the runtime refused the install (not_installed, apply_rejected): {}",
                HostedInstallFailure::ApplyRejected.description()
            )
        );
        drop(session);
        server.await??;

        let quoting = json!({"type": "failed", "class": "apply_rejected",
            "outcome": "not_installed", "detail": "token sk-live-private", "documents": null})
        .to_string();
        let (endpoint, server, _key) = scripted_runtime(vec![quoting]).await?;
        let error = start(&endpoint).await.err().expect("refused");
        assert!(
            !format!("{error:#}").contains("sk-live-private"),
            "a class that shows no runtime text never shows it: {error:#}"
        );
        server.await??;

        let unset = json!({"type": "failed", "class": "unset_variables",
            "outcome": "not_installed", "detail": "names API_TOKEN", "documents": null})
        .to_string();
        let (endpoint, server, _key) = scripted_runtime(vec![unset]).await?;
        let error = start(&endpoint).await.err().expect("refused");
        assert!(
            format!("{error:#}").ends_with(": names API_TOKEN"),
            "{error:#}"
        );
        server.await??;
        Ok(())
    }
}

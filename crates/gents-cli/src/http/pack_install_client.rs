use anyhow::{bail, ensure, Context, Result};
use futures_util::{SinkExt, StreamExt};
use gents::store::config_client::{
    prepare_pack_evidence_from_history, prepare_pack_relationship, ClientSigningCapability,
    GraphqlEndpoint, PackApprovalError, PackApprovalScope, PackMutation, PackRelationship,
    PackReplayTransport,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, protocol::Message},
    MaybeTlsStream, WebSocketStream,
};

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ServerMessage {
    Ready {
        scope: PackApprovalScope,
        preparation: gents::pack::HostedPackPreparation,
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
    Failed {
        error: String,
        outcome: String,
    },
}

/// A single runtime-owned transaction, driven by the client's pack interpreter.
pub(crate) struct PackInstallSession {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    actor: ClientSigningCapability,
    scope: PackApprovalScope,
    preparation: gents::pack::HostedPackPreparation,
    pending: Option<ServerMessage>,
    next_id: u64,
}

impl PackInstallSession {
    pub(crate) async fn start(
        endpoint: &GraphqlEndpoint,
        request: Value,
        node_did: &str,
        pack_digests: &[String],
    ) -> Result<Self> {
        let actor = endpoint.held_client_signing_capability()?.clone();
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
            .authorization()?
            .context("client authentication is required")?;
        let mut authorization =
            authorization.parse::<tokio_tungstenite::tungstenite::http::HeaderValue>()?;
        authorization.set_sensitive(true);
        upgrade.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
            authorization,
        );
        let (mut socket, _) = connect_async(upgrade)
            .await
            .context("connecting remote pack install")?;
        send(&mut socket, json!({"type": "start", "request": request})).await?;
        let (scope, preparation) = match receive(&mut socket).await? {
            ServerMessage::Ready { scope, preparation } => (scope, preparation),
            ServerMessage::Failed { error, outcome } => {
                bail!("remote pack install failed ({outcome}): {error}")
            }
            _ => bail!("remote pack install returned an unexpected initial message"),
        };
        ensure!(
            scope.node_did == node_did
                && scope.pack_digests == pack_digests
                && !scope.nonce.is_empty(),
            "remote pack install scope does not match the reviewed artifacts and node"
        );
        Ok(Self {
            socket,
            actor,
            scope,
            preparation,
            pending: None,
            next_id: 1,
        })
    }

    pub(crate) fn preparation(&self) -> &gents::pack::HostedPackPreparation {
        &self.preparation
    }

    pub(crate) async fn read(&mut self, document: &str, variables: &Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .context("pack install request ID exhausted")?;
        send(
            &mut self.socket,
            json!({"type": "read", "id": id, "document": document, "variables": variables}),
        )
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
                ServerMessage::Failed { error, outcome } => {
                    bail!("remote pack install failed ({outcome}): {error}")
                }
                _ => bail!("unexpected remote pack install response to read"),
            }
        }
    }

    async fn proposal(&mut self) -> Result<ServerMessage> {
        match self.pending.take() {
            Some(proposal) => Ok(proposal),
            None => receive(&mut self.socket).await,
        }
    }

    pub(crate) async fn graph(&mut self, expected: &PackMutation) -> Result<Value> {
        let (id, history) = match self.proposal().await? {
            ServerMessage::ApproveGraph {
                id,
                mutation,
                history,
            } if mutation == *expected => (id, history),
            ServerMessage::Failed { error, outcome } => {
                bail!("remote pack install failed ({outcome}): {error}")
            }
            _ => bail!("runtime pack graph differs from the client's installation plan"),
        };
        let evidence =
            prepare_pack_evidence_from_history(&self.actor, &self.scope, expected, history).await?;
        send(
            &mut self.socket,
            json!({"type": "graph_evidence", "id": id, "evidence": evidence}),
        )
        .await?;
        self.applied(id).await
    }

    pub(crate) async fn relationship(&mut self, expected: &PackRelationship) -> Result<()> {
        let id = match self.proposal().await? {
            ServerMessage::ApproveRelationship { id, relationship }
                if serde_json::to_value(&relationship)? == serde_json::to_value(expected)? =>
            {
                id
            }
            ServerMessage::Failed { error, outcome } => {
                bail!("remote pack install failed ({outcome}): {error}")
            }
            _ => bail!("runtime pack relationship differs from the client's installation plan"),
        };
        let evidence = prepare_pack_relationship(&self.actor, &self.scope, expected).await?;
        send(
            &mut self.socket,
            json!({"type": "relationship_evidence", "id": id, "evidence": evidence}),
        )
        .await?;
        let result = self.applied(id).await?;
        ensure!(result.is_null(), "unexpected relationship result");
        Ok(())
    }

    async fn applied(&mut self, id: u64) -> Result<Value> {
        match receive(&mut self.socket).await? {
            ServerMessage::Applied {
                id: received,
                result,
            } if received == id => Ok(result),
            ServerMessage::Failed { error, outcome } => {
                bail!("remote pack install failed ({outcome}): {error}")
            }
            _ => bail!("unexpected remote pack install effect response"),
        }
    }

    async fn timestamp(&mut self, expected: &str) -> Result<String> {
        let id = match self.proposal().await? {
            ServerMessage::Timestamp { id, kind } if kind == expected => id,
            ServerMessage::Failed { error, outcome } => {
                bail!("remote pack install failed ({outcome}): {error}")
            }
            _ => bail!("runtime pack timestamp differs from the client installation plan"),
        };
        let value = gents::store::config_client::mint_recreate_identity_timestamp();
        send(
            &mut self.socket,
            json!({"type": "timestamp_value", "id": id, "value": value}),
        )
        .await?;
        Ok(value)
    }

    async fn finish(&mut self) -> Result<Value> {
        ensure!(
            self.pending.is_none(),
            "runtime proposed an unexpected extra pack effect"
        );
        send(&mut self.socket, json!({"type": "finish"})).await?;
        match receive(&mut self.socket).await? {
            ServerMessage::Finished { result } => Ok(result),
            ServerMessage::Failed { error, outcome } => {
                bail!("remote pack install failed ({outcome}): {error}")
            }
            _ => bail!("unexpected remote pack install completion response"),
        }
    }
}

async fn send(socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>, value: Value) -> Result<()> {
    let encoded = tokio::task::spawn_blocking(move || serde_json::to_string(&value))
        .await
        .context("encoding remote pack install message")??;
    socket
        .send(Message::Text(encoded.into()))
        .await
        .context("remote pack install connection lost; inspect the runtime before retrying")
}

async fn receive(socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>) -> Result<ServerMessage> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                return tokio::task::spawn_blocking(move || serde_json::from_str(&text))
                    .await
                    .context("decoding remote pack install message")?
                    .context("invalid remote pack install message");
            }
            Some(Ok(Message::Ping(_)))
            | Some(Ok(Message::Pong(_)))
            | Some(Ok(Message::Frame(_))) => {}
            Some(Ok(_)) | None => {
                bail!("remote pack install connection closed; inspect the runtime before retrying")
            }
            Some(Err(error)) => {
                return Err(error).context(
                    "remote pack install connection lost; inspect the runtime before retrying",
                )
            }
        }
    }
}

/// Serializes interpreter requests on the same authenticated session.
pub(crate) struct PackReplaySession {
    inner: tokio::sync::Mutex<PackReplayState>,
}

struct PackReplayState {
    session: Option<PackInstallSession>,
    result: Option<Value>,
}

impl PackReplaySession {
    pub(crate) fn new(session: PackInstallSession) -> Self {
        Self {
            inner: tokio::sync::Mutex::new(PackReplayState {
                session: Some(session),
                result: None,
            }),
        }
    }

    pub(crate) async fn result(&self) -> Result<Value> {
        self.inner
            .lock()
            .await
            .result
            .take()
            .context("remote pack install has no confirmed result")
    }
}

fn transport_error(error: anyhow::Error) -> PackApprovalError {
    PackApprovalError::Transport(format!("{error:#}"))
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
            .context("pack session is closed")
            .map_err(transport_error)?
            .read(document, variables)
            .await
            .map_err(transport_error)
    }

    async fn apply_graph(
        &self,
        mutation: &PackMutation,
    ) -> std::result::Result<Value, PackApprovalError> {
        let mut state = self.inner.lock().await;
        state
            .session
            .as_mut()
            .context("pack session is closed")
            .map_err(transport_error)?
            .graph(mutation)
            .await
            .map_err(transport_error)
    }

    async fn apply_relationship(
        &self,
        relationship: &PackRelationship,
    ) -> std::result::Result<(), PackApprovalError> {
        let mut state = self.inner.lock().await;
        state
            .session
            .as_mut()
            .context("pack session is closed")
            .map_err(transport_error)?
            .relationship(relationship)
            .await
            .map_err(transport_error)
    }

    async fn next_timestamp(
        &self,
        kind: &'static str,
    ) -> std::result::Result<String, PackApprovalError> {
        let mut state = self.inner.lock().await;
        state
            .session
            .as_mut()
            .context("pack session is closed")
            .map_err(transport_error)?
            .timestamp(kind)
            .await
            .map_err(transport_error)
    }

    async fn commit(&self) -> std::result::Result<(), PackApprovalError> {
        let mut state = self.inner.lock().await;
        let result = state
            .session
            .as_mut()
            .context("pack session is closed")
            .map_err(transport_error)?
            .finish()
            .await
            .map_err(transport_error)?;
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

pub(crate) async fn install_remote(
    endpoint: &GraphqlEndpoint,
    owner: &str,
    pack: &crate::commands::pack::PackSource,
    dependencies: &[crate::commands::pack::PackSource],
    args: &crate::cli::PackInstallArgs,
    explicit: bool,
) -> Result<Value> {
    ensure!(args.wire.is_empty(), "remote pack install does not yet support --wire with client-held signing; install the pack and wire it separately");
    let request =
        super::pack_install::encode_install_request(pack, dependencies, args, owner, explicit)
            .await?;
    let digests = std::iter::once(pack)
        .chain(dependencies.iter())
        .map(|source| source.digest().to_owned())
        .collect::<Vec<_>>();
    let mut session = PackInstallSession::start(endpoint, request, owner, &digests).await?;
    let preparation = session.preparation().clone();
    let access = gents::store::config_client::ConfigAccess::Graphql(endpoint.clone());
    let requested = crate::commands::pack::parse_inference_slot_bindings(&args.inference_slots)?;
    let prepared = gents::pack::prepare_hosted_document_pack_archive(
        &access,
        owner,
        pack.archive(),
        &requested,
        &dependencies
            .iter()
            .map(crate::commands::pack::PackSource::archive)
            .collect::<Vec<_>>(),
        &|name| std::env::var(name).ok(),
        &preparation,
    )
    .await?;
    let graph = gents::pack::hosted_document_graph(pack.archive(), &prepared, explicit)?;
    gents::pack::apply_prepared_dependency_schemas(&access, &prepared).await?;
    let temp = tempfile::tempdir()?;
    let root = pack.shared_archive();
    let path = temp.path().to_owned();
    tokio::task::spawn_blocking(move || gents::pack::materialize::materialize(&root, &path))
        .await
        .context("materializing the reviewed root pack")??;
    let schemas =
        crate::commands::schema::apply_pack_schemas_if_present(&access, temp.path()).await?;
    send(
        &mut session.socket,
        json!({"type": "schemas_ready", "schemas": schemas}),
    )
    .await?;
    let replay = std::sync::Arc::new(PackReplaySession::new(session));
    let access = gents::store::config_client::ConfigAccess::Graphql(
        endpoint.clone().with_pack_replay(replay.clone())?,
    );
    let policy = args.drift.policy();
    access
        .transact_write("pack.hosted_document_graph.replay", |txn| {
            let graph = &graph;
            Box::pin(async move {
                gents::pack::apply_hosted_document_graph(txn, owner, graph, policy).await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await?;
    replay.result().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::store::identity::{KeyIdentity, NodeIdentity};
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
        assert!(session.graph(&expected).await.is_err());
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
}

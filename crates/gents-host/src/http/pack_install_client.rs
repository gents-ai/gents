use anyhow::{bail, ensure, Context, Result};
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

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ServerMessage {
    Ready {
        scope: PackApprovalScope,
        preparation: gents_pack::pack::HostedPackPreparation,
        /// The runtime's build. Required: a runtime without it is an older
        /// build, refused by name rather than tolerated.
        build: gents_protocol::build_info::BuildInfo,
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
    /// A preview's answer, with the runtime's build so the client checks it
    /// exactly as it does at `ready`.
    Previewed {
        result: Value,
        build: gents_protocol::build_info::BuildInfo,
    },
    Failed {
        error: String,
        outcome: String,
        /// The failed step, from `pack_install::failure`. Absent from an older
        /// runtime, whose text is then shown by length only.
        #[serde(default)]
        class: Option<String>,
    },
}

impl ServerMessage {
    /// The message's type, for an error that names what arrived instead.
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
}

type InstallSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Open the runtime's install session and send the start request.
async fn open_session(
    endpoint: &GraphqlEndpoint,
    request: Value,
) -> Result<(InstallSocket, ClientSigningCapability)> {
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
    let mut start = serde_json::Map::new();
    start.insert("type".into(), "start".into());
    start.insert("request".into(), request);
    send(&mut socket, Value::Object(start)).await?;
    Ok((socket, actor))
}

/// The runtime's first answer, with every failure typed: a decode error, a
/// lost connection, an older runtime and the runtime's own refusal are each
/// their own error, and none of them repeats the runtime's text.
async fn first_reply(
    socket: &mut InstallSocket,
    client_build: &gents_protocol::build_info::BuildInfo,
) -> std::result::Result<(ServerMessage, usize), PackInstallHandshakeError> {
    match receive_sized(socket).await {
        // Every failure of this build's runtime carries a class, so one without
        // is an older runtime (its `InstallRequest` has `deny_unknown_fields`
        // and refuses the client's `client_build`). That is the "older
        // runtime" case, with its own next step, told by the missing class
        // rather than by the runtime's text.
        Ok((
            ServerMessage::Failed {
                error, class: None, ..
            },
            _,
        )) => Err(PackInstallHandshakeError::OlderRuntime {
            client: client_build.summary(),
            detail_bytes: error.len(),
        }),
        Ok((
            ServerMessage::Failed {
                error,
                outcome,
                class,
            },
            _,
        )) => Err(runtime_failure(&error, outcome, class.as_deref())),
        Ok(sized) => Ok(sized),
        // Only the decode error means "unreadable handshake". A closed or lost
        // connection is its own case and must not be reported as a build
        // problem.
        Err(error @ ReceiveFailure::Unreadable { .. }) => {
            Err(PackInstallHandshakeError::UnreadableHandshake {
                client: client_build.summary(),
                detail_bytes: format!("{error:#}").len(),
            })
        }
        Err(error @ ReceiveFailure::Lost(_)) => Err(PackInstallHandshakeError::ConnectionLost {
            detail_bytes: format!("{error:#}").len(),
        }),
    }
}

/// The build check at the handshake, on the client's side: a different build
/// is refused, and two dirty builds of one commit are accepted with a typed
/// warning the caller receives.
fn check_runtime_build(
    client_build: &gents_protocol::build_info::BuildInfo,
    runtime_build: &gents_protocol::build_info::BuildInfo,
) -> std::result::Result<Vec<PackInstallWarning>, PackInstallHandshakeError> {
    if let Some(refusal) = PackInstallHandshakeError::build_mismatch(client_build, runtime_build) {
        return Err(refusal);
    }
    let mut warnings = Vec::new();
    if client_build.dirty_warning(runtime_build).is_some() {
        warnings.push(PackInstallWarning::DirtyBuildPair {
            commit: client_build
                .git_sha
                .clone()
                .unwrap_or_else(|| "an unrecorded commit".to_owned()),
        });
    }
    Ok(warnings)
}

/// A `failed` message as an error the client can show: the runtime's text only
/// for a class that allows it, otherwise the class's own description. An
/// unclassified message (an older runtime) is shown by length only.
fn runtime_failure(error: &str, outcome: String, class: Option<&str>) -> PackInstallHandshakeError {
    use super::pack_install::failure::PackSessionFailureClass;
    match class.and_then(PackSessionFailureClass::from_wire) {
        Some(known) => PackInstallHandshakeError::RuntimeFailed {
            outcome,
            class: class.unwrap_or_default().to_owned(),
            reason: if known.shows_runtime_text() {
                error.to_owned()
            } else {
                known.description().to_owned()
            },
        },
        None => PackInstallHandshakeError::RuntimeFailed {
            outcome,
            class: "unclassified".to_owned(),
            reason: format!(
                "the runtime did not classify this failure, so its {} bytes of detail are not                  shown, since they can quote install values",
                error.len()
            ),
        },
    }
}

/// A single runtime-owned transaction, driven by the client's pack interpreter.
pub struct PackInstallSession {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    actor: ClientSigningCapability,
    scope: PackApprovalScope,
    preparation: gents_pack::pack::HostedPackPreparation,
    pending: Option<ServerMessage>,
    next_id: u64,
    /// What the handshake accepted on a weaker guarantee. Read by the caller
    /// after the install, so the desktop and library callers see it as well as
    /// the CLI.
    warnings: Vec<PackInstallWarning>,
}

impl PackInstallSession {
    pub async fn start(
        endpoint: &GraphqlEndpoint,
        request: Value,
        node_did: &str,
        pack_digests: &[String],
        client_build: &gents_protocol::build_info::BuildInfo,
    ) -> Result<Self> {
        let (mut socket, actor) = open_session(endpoint, request).await?;
        let (scope, preparation, runtime_build) =
            match first_reply(&mut socket, client_build).await? {
                (
                    ServerMessage::Ready {
                        scope,
                        preparation,
                        build,
                    },
                    _,
                ) => (scope, preparation, build),
                (other, bytes) => {
                    return Err(PackInstallHandshakeError::UnexpectedMessage {
                        kind: other.kind(),
                        detail_bytes: bytes,
                    }
                    .into())
                }
            };
        // The handshake is where a build mismatch is refused: a plan prepared by
        // two different builds is not the same plan, whatever it contains, and
        // finding that out after planning is too late to say so usefully. Two
        // dirty builds of one commit agree, and that is recorded as a typed
        // warning the desktop and library callers see, not printed here.
        let warnings = check_runtime_build(client_build, &runtime_build)?;
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
            warnings,
        })
    }

    /// What the handshake accepted on a weaker guarantee, for the caller to show
    /// or log. Empty on an ordinary install.
    pub fn warnings(&self) -> &[PackInstallWarning] {
        &self.warnings
    }

    pub fn preparation(&self) -> &gents_pack::pack::HostedPackPreparation {
        &self.preparation
    }

    pub async fn read(&mut self, document: &str, variables: &Value) -> Result<Value> {
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
                ServerMessage::Failed {
                    error,
                    outcome,
                    class,
                } => return Err(runtime_failure(&error, outcome, class.as_deref()).into()),
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

    pub async fn graph(&mut self, expected: &PackMutation) -> Result<Value> {
        let (id, history) = match self.proposal().await? {
            ServerMessage::ApproveGraph {
                id,
                mutation,
                history,
            } if mutation == *expected => (id, history),
            ServerMessage::Failed {
                error,
                outcome,
                class,
            } => return Err(runtime_failure(&error, outcome, class.as_deref()).into()),
            ServerMessage::ApproveGraph { mutation, .. } => {
                // A differing build is refused at the handshake, so both sides
                // here are the same build and the difference is in the plan.
                bail!(
                    "runtime pack graph differs from the client's installation plan: {}",
                    describe_mutation_disagreement(expected, &mutation)
                )
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

    pub async fn relationship(&mut self, expected: &PackRelationship) -> Result<()> {
        let id = match self.proposal().await? {
            ServerMessage::ApproveRelationship { id, relationship }
                if serde_json::to_value(&relationship)? == serde_json::to_value(expected)? =>
            {
                id
            }
            ServerMessage::Failed {
                error,
                outcome,
                class,
            } => return Err(runtime_failure(&error, outcome, class.as_deref()).into()),
            ServerMessage::ApproveRelationship { relationship, .. } => bail!(
                "runtime pack relationship differs from the client's installation plan: {}",
                describe_relationship_disagreement(expected, &relationship)?
            ),
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
            ServerMessage::Failed {
                error,
                outcome,
                class,
            } => Err(runtime_failure(&error, outcome, class.as_deref()).into()),
            _ => bail!("unexpected remote pack install effect response"),
        }
    }

    async fn timestamp(&mut self, expected: &str) -> Result<String> {
        let id = match self.proposal().await? {
            ServerMessage::Timestamp { id, kind } if kind == expected => id,
            ServerMessage::Failed {
                error,
                outcome,
                class,
            } => return Err(runtime_failure(&error, outcome, class.as_deref()).into()),
            _ => bail!("runtime pack timestamp differs from the client installation plan"),
        };
        let value = gents_store::config_client::mint_recreate_identity_timestamp();
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
            ServerMessage::Failed {
                error,
                outcome,
                class,
            } => Err(runtime_failure(&error, outcome, class.as_deref()).into()),
            _ => bail!("unexpected remote pack install completion response"),
        }
    }
}

async fn send(socket: &mut InstallSocket, value: Value) -> Result<()> {
    let encoded = tokio::task::spawn_blocking(move || serde_json::to_string(&value))
        .await
        .context("encoding remote pack install message")??;
    socket
        .send(Message::Text(encoded.into()))
        .await
        .context("remote pack install connection lost; inspect the runtime before retrying")
}

async fn receive(socket: &mut InstallSocket) -> Result<ServerMessage> {
    Ok(receive_sized(socket).await?.0)
}

/// Why no message arrived: one that could not be read, or a connection that
/// went away. Typed, so the handshake tells them apart by variant.
#[derive(Debug, thiserror::Error)]
enum ReceiveFailure {
    #[error("invalid remote pack install message ({bytes} bytes)")]
    Unreadable {
        bytes: usize,
        #[source]
        error: serde_json::Error,
    },
    #[error(transparent)]
    Lost(anyhow::Error),
}

/// A message and its size in bytes.
async fn receive_sized(
    socket: &mut InstallSocket,
) -> std::result::Result<(ServerMessage, usize), ReceiveFailure> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                let bytes = text.len();
                let decoded = tokio::task::spawn_blocking(move || serde_json::from_str(&text))
                    .await
                    .context("decoding remote pack install message")
                    .map_err(ReceiveFailure::Lost)?;
                return decoded
                    .map(|message| (message, bytes))
                    .map_err(|error| ReceiveFailure::Unreadable { bytes, error });
            }
            Some(Ok(Message::Ping(_)))
            | Some(Ok(Message::Pong(_)))
            | Some(Ok(Message::Frame(_))) => {}
            Some(Ok(_)) | None => {
                return Err(ReceiveFailure::Lost(anyhow::anyhow!(
                    "remote pack install connection closed; inspect the runtime before retrying"
                )))
            }
            Some(Err(error)) => {
                return Err(ReceiveFailure::Lost(anyhow::Error::new(error).context(
                    "remote pack install connection lost; inspect the runtime before retrying",
                )))
            }
        }
    }
}

/// Serializes interpreter requests on the same authenticated session.
pub struct PackReplaySession {
    inner: tokio::sync::Mutex<PackReplayState>,
    /// The handshake's warnings, kept outside the session because the session is
    /// consumed while the install runs.
    warnings: Vec<PackInstallWarning>,
}

struct PackReplayState {
    session: Option<PackInstallSession>,
    result: Option<Value>,
}

impl PackReplaySession {
    pub fn new(session: PackInstallSession) -> Self {
        // Taken before the session is consumed, so the caller can read them once
        // the install has finished.
        let warnings = session.warnings().to_vec();
        Self {
            inner: tokio::sync::Mutex::new(PackReplayState {
                session: Some(session),
                result: None,
            }),
            warnings,
        }
    }

    /// What the handshake accepted on a weaker guarantee, if anything.
    pub fn warnings(&self) -> &[PackInstallWarning] {
        &self.warnings
    }

    pub async fn result(&self) -> Result<Value> {
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

/// Install over the runtime's own install session, returning the runtime's
/// outcome **and** what the handshake accepted on a weaker guarantee.
///
/// The warnings are returned rather than printed here, so every caller sees
/// them: the CLI prints them, and the desktop and library callers can show or
/// log them without the install having decided for them.
pub async fn install_remote(
    endpoint: &GraphqlEndpoint,
    owner: &str,
    pack: &gents_pack::pack::resolved_pack::ResolvedPack,
    dependencies: &[gents_pack::pack::resolved_pack::ResolvedPack],
    args: &gents_pack::pack::install_flow::DocumentsInstallRequest,
    explicit: bool,
    client_build: gents_protocol::build_info::BuildInfo,
) -> Result<(Value, Vec<PackInstallWarning>)> {
    ensure!(args.wire.is_empty(), "remote pack install does not yet support --wire with client-held signing; install the pack and wire it separately on the owning node");
    let request = super::pack_install::encode_install_request(
        pack,
        dependencies,
        args,
        owner,
        explicit,
        client_build.clone(),
    )
    .await?;
    let digests = std::iter::once(pack)
        .chain(dependencies.iter())
        .map(|source| source.digest().to_owned())
        .collect::<Vec<_>>();
    let mut session =
        PackInstallSession::start(endpoint, request, owner, &digests, &client_build).await?;
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
        &requested,
        &dependencies
            .iter()
            .map(gents_pack::pack::resolved_pack::ResolvedPack::archive)
            .collect::<Vec<_>>(),
        &|name| {
            gents_pack::operations::pack::install_value(
                gents_protocol::operations::pack::PackActor::User,
                &supplied,
                name,
            )
        },
        &preparation,
        &overrides,
    )
    .await?;
    prepared
        .prepare_pin_moves(
            &access,
            owner,
            &args.move_pin_tools,
            &std::iter::once(pack.archive())
                .chain(dependencies.iter().map(|d| d.archive()))
                .collect::<Vec<_>>(),
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
    let evidence = prepare_pack_schema_approval(&session.actor, &session.scope).await?;
    send(
        &mut session.socket,
        json!({"type": "schemas_ready", "evidence": evidence}),
    )
    .await?;
    let replay = std::sync::Arc::new(PackReplaySession::new(session));
    let access = gents_store::config_client::ConfigAccess::Graphql(
        endpoint.clone().with_pack_replay(replay.clone())?,
    );
    let policy = gents_pack::operations::pack::drift_policy(args.edited);
    access
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
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await?;
    let result = replay.result().await?;
    Ok((result, replay.warnings().to_vec()))
}

/// Preview an install over the runtime's install session, so the build check
/// runs exactly as it does for an install: the runtime refuses a different
/// build before it opens the archives, and the client checks the runtime's
/// build in the answer. A preview writes nothing.
///
/// Returns the runtime's preview and what the build check accepted on a weaker
/// guarantee, as `install_remote` does.
pub async fn preview_remote(
    endpoint: &GraphqlEndpoint,
    owner: &str,
    pack: &gents_pack::pack::resolved_pack::ResolvedPack,
    dependencies: &[gents_pack::pack::resolved_pack::ResolvedPack],
    args: &gents_pack::pack::install_flow::DocumentsInstallRequest,
    explicit: bool,
    client_build: gents_protocol::build_info::BuildInfo,
) -> Result<(Value, Vec<PackInstallWarning>)> {
    ensure!(args.preview, "a remote preview is a request with --preview");
    let request = super::pack_install::encode_install_request(
        pack,
        dependencies,
        args,
        owner,
        explicit,
        client_build.clone(),
    )
    .await?;
    let (mut socket, _) = open_session(endpoint, request).await?;
    match first_reply(&mut socket, &client_build).await? {
        (ServerMessage::Previewed { result, build }, _) => {
            let warnings = check_runtime_build(&client_build, &build)?;
            Ok((result, warnings))
        }
        (other, bytes) => Err(PackInstallHandshakeError::UnexpectedMessage {
            kind: other.kind(),
            detail_bytes: bytes,
        }
        .into()),
    }
}

/// What the handshake accepted on a weaker guarantee than two clean builds of
/// one commit. The install went ahead; the caller decides how to show it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackInstallWarning {
    /// Both builds are dirty builds of one commit, so they were accepted as the
    /// same build on that commit and its core packs. The install is sound; the
    /// guarantee is weaker than two clean builds of the commit would give.
    ///
    /// Typed rather than printed inside the install, so the desktop and library
    /// callers see it too. The CLI prints it.
    DirtyBuildPair { commit: String },
}

impl PackInstallWarning {
    /// One line for a person, which the CLI prints and a UI can show.
    pub fn describe(&self) -> String {
        match self {
            Self::DirtyBuildPair { commit } => format!(
                "both this client and the runtime are dirty builds of {commit}, so they were \
                 accepted as the same build on the same commit and core packs; a clean install of \
                 each is the stronger check"
            ),
        }
    }
}

/// Why an install could not agree with the runtime, or why the runtime refused
/// it: a typed error whose variants each name the next step, because the
/// difference between them is exactly what an operator has to do about it. No
/// variant repeats text the runtime did not write from build facts alone.
#[derive(Debug, thiserror::Error)]
pub enum PackInstallHandshakeError {
    /// The class is known, the text is not shown: a decode error can quote the
    /// bytes it choked on, which on a runtime-side message means runtime
    /// values. The length is enough to tell "empty" from "long".
    #[error(
        "the runtime's install handshake was unreadable, which is what an older runtime looks like: \
         this client is `{client}`; run `gents runtime service update` to bring the runtime to this \
         build, or install from a client of the runtime's build ({detail_bytes} bytes of detail not \
         shown, since a decode error can quote runtime-side values)"
    )]
    UnreadableHandshake { client: String, detail_bytes: usize },

    #[error(
        "the connection to the runtime was lost during the install handshake ({detail_bytes} bytes \
         of detail not shown); the install did not happen, so retry it when the runtime is reachable"
    )]
    ConnectionLost { detail_bytes: usize },

    #[error(
        "the runtime predates this client's build fields ({detail_bytes} bytes of detail not shown, \
         since the runtime's message can quote values): this client is `{client}`; \
         run `gents runtime service update` to update the runtime, or install from a client of \
         the runtime's build"
    )]
    OlderRuntime { client: String, detail_bytes: usize },

    #[error(
        "this client and the runtime are different builds, so they cannot agree on an install: \
         {differences}; run `gents runtime service update`, or install from a client of the \
         runtime's build"
    )]
    BuildMismatch { differences: String },

    #[error(
        "this client cannot verify its own build identity because it records no commit, so a \
         mismatch with the runtime could go unnoticed: build from a git checkout \
         (`cargo build`, not a vendored tree) and retry ({client})"
    )]
    BuildUnverifiable { client: String },

    /// The runtime's own failure, by class. `reason` is the class's fixed
    /// description, or the runtime's text for the one class whose text holds
    /// build facts only (`failure::PackSessionFailureClass::shows_runtime_text`).
    #[error("the runtime refused the install ({outcome}, {class}): {reason}")]
    RuntimeFailed {
        outcome: String,
        class: String,
        reason: String,
    },

    #[error(
        "the runtime answered the install with `{kind}` ({detail_bytes} bytes), not the message \
         this client expected; run `gents runtime service update`, or install from a client of the \
         runtime's build"
    )]
    UnexpectedMessage {
        kind: &'static str,
        detail_bytes: usize,
    },
}

impl PackInstallHandshakeError {
    /// The refusal when the two builds differ, or `None` when they agree.
    pub fn build_mismatch(
        client: &gents_protocol::build_info::BuildInfo,
        runtime: &gents_protocol::build_info::BuildInfo,
    ) -> Option<Self> {
        // A build that records no commit cannot be compared: `None == None`
        // must never count as a match, so an unverifiable client is refused by
        // name instead of silently agreeing.
        if !client.is_verifiable() {
            return Some(Self::BuildUnverifiable {
                client: client.summary(),
            });
        }
        if !runtime.is_verifiable() {
            return Some(Self::BuildMismatch {
                differences: format!(
                    "this client is `{}` and the runtime is `{}`, which records no commit",
                    client.summary(),
                    runtime.summary()
                ),
            });
        }
        let differences = client.differences(runtime);
        if differences.is_empty() {
            None
        } else {
            Some(Self::BuildMismatch {
                differences: format!(
                    "this client is `{}` and the runtime is `{}`; {}",
                    client.summary(),
                    runtime.summary(),
                    differences.join("; ")
                ),
            })
        }
    }
}

fn describe_mutation_disagreement(
    constructed: &gents_store::config_client::PackMutation,
    proposed: &gents_store::config_client::PackMutation,
) -> String {
    use gents_store::config_client::PackMutationDiff as _;
    let differences = constructed.differences(proposed);
    let total = differences.len();
    // The operation, collection and document lead, so a reader knows which
    // mutation of the plan diverged before reading the field path.
    let target = format!(
        "{} {}",
        constructed.collection(),
        constructed.doc_id().unwrap_or("<new document>")
    );
    match differences.first() {
        Some(first) if total == 1 => format!("{target}: {}", first.describe()),
        Some(first) => format!(
            "{target}: {} (and {} more difference{})",
            first.describe(),
            total - 1,
            if total == 2 { "" } else { "s" }
        ),
        None => {
            format!("{target}: the plans agree field by field, so the difference is outside them")
        }
    }
}

/// The relationship counterpart. Relationships compare as JSON, so the first
/// differing path is found the same way, over the serialized form.
fn describe_relationship_disagreement(
    constructed: &gents_store::config_client::PackRelationship,
    proposed: &gents_store::config_client::PackRelationship,
) -> Result<String> {
    let constructed = serde_json::to_value(constructed)?;
    let proposed = serde_json::to_value(proposed)?;
    Ok(gents_store::config_client::describe_json_disagreement(
        &constructed,
        &proposed,
    ))
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
                "preparation": {"agent_bindings": {}, "plugin_digests": {}, "operator_ux_digests": {}},
                // Required: a runtime without it is an older build, refused.
                "build": {"version": "1.0.0-rc2", "git_sha": "aaaaaaa", "git_dirty": false, "core_packs": []}
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
            &gents_protocol::build_info::BuildInfo::from_parts(
                "1.0.0-rc2".to_owned(),
                Some("aaaaaaa".to_owned()),
                Some(false),
                Vec::new(),
            ),
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
                "preparation": {"agent_bindings": {}, "plugin_digests": {}, "operator_ux_digests": {}},
                // Required: a runtime without it is an older build, refused.
                "build": {"version": "1.0.0-rc2", "git_sha": "aaaaaaa", "git_dirty": false, "core_packs": []}
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
            &gents_protocol::build_info::BuildInfo::from_parts(
                "1.0.0-rc2".to_owned(),
                Some("aaaaaaa".to_owned()),
                Some(false),
                Vec::new(),
            ),
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

    /// Over the real socket: a runtime whose `ready` reports a different build
    /// is refused at the handshake, before it is asked for anything.
    #[tokio::test]
    async fn a_ready_from_a_different_build_is_refused_over_the_socket() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut socket = accept_async(stream).await?;
            let _ = socket.next().await.context("missing start")??;
            socket
                .send(Message::Text(
                    json!({
                        "type": "ready",
                        "scope": {"node_did": "did:key:node", "pack_digests": ["sha256:root"], "nonce": "test"},
                        "preparation": {"agent_bindings": {}, "plugin_digests": {}, "operator_ux_digests": {}},
                        // A different commit from the client's below.
                        "build": {"version": "1.0.0-rc2", "git_sha": "bbbbbbb", "git_dirty": false, "core_packs": []}
                    })
                    .to_string()
                    .into(),
                ))
                .await?;
            // The client must close without sending anything: refused before
            // planning.
            let next = socket.next().await;
            anyhow::ensure!(
                !matches!(next, Some(Ok(Message::Text(_)))),
                "the client planned against a runtime of a different build"
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
        let error = PackInstallSession::start(
            &endpoint,
            json!({}),
            "did:key:node",
            &["sha256:root".to_owned()],
            &gents_protocol::build_info::BuildInfo::from_parts(
                "1.0.0-rc2".to_owned(),
                Some("aaaaaaa".to_owned()),
                Some(false),
                Vec::new(),
            ),
        )
        .await
        .err()
        .expect("a different build is refused at the handshake");
        let text = error.to_string();
        assert!(
            text.contains("aaaaaaa") && text.contains("bbbbbbb"),
            "{text}"
        );
        assert!(text.contains("gents runtime service update"), "{text}");
        server.await??;
        Ok(())
    }

    /// The wiring the blocker was about: the process records its build, and a
    /// process that recorded none reports `UNVERIFIABLE` rather than a guess.
    #[test]
    fn the_recorded_runtime_build_is_what_the_handshake_reports() {
        let recorded = crate::http::pack_install::runtime_build();
        // Either a real record or the explicit unverifiable value -- never a
        // build whose commit was silently dropped.
        assert!(
            recorded.is_verifiable()
                || recorded == gents_protocol::build_info::BuildInfo::UNVERIFIABLE,
            "an unrecorded build says so: {recorded:?}"
        );
        if recorded == gents_protocol::build_info::BuildInfo::UNVERIFIABLE {
            assert!(!recorded.is_verifiable());
        }
    }

    /// The handshake is where a build mismatch is refused, before any planning:
    /// two builds cannot agree on a plan, so finding out later is too late to
    /// say so usefully.
    #[test]
    fn an_install_whose_builds_differ_is_refused_at_the_handshake() {
        let client = gents_protocol::build_info::BuildInfo::from_parts(
            "1.0.0-rc2".to_owned(),
            Some("aaaaaaa".to_owned()),
            Some(false),
            Vec::new(),
        );
        let runtime = gents_protocol::build_info::BuildInfo::from_parts(
            "1.0.0-rc2".to_owned(),
            Some("bbbbbbb".to_owned()),
            Some(false),
            Vec::new(),
        );
        // The plans would agree -- neither carries a pack -- so only the build
        // check can refuse this.
        let refusal = PackInstallHandshakeError::build_mismatch(&client, &runtime)
            .expect("two different builds are refused");
        let text = refusal.to_string();
        assert!(
            text.contains("aaaaaaa") && text.contains("bbbbbbb"),
            "{text}"
        );
        assert!(text.contains("gents runtime service update"), "{text}");
        assert!(
            PackInstallHandshakeError::build_mismatch(&client, &client).is_none(),
            "the same build is not refused"
        );
    }

    /// A build with no commit cannot be verified, and `None == None` must never
    /// count as a match -- the failure that let a stale CLI agree with a
    /// differing runtime.
    #[test]
    fn a_build_without_a_commit_is_refused_rather_than_matched() {
        let unverifiable = gents_protocol::build_info::BuildInfo::from_parts(
            "1.0.0-rc2".to_owned(),
            None,
            None,
            Vec::new(),
        );
        let two_of_them = PackInstallHandshakeError::build_mismatch(&unverifiable, &unverifiable)
            .expect("two unverifiable builds are not the same build");
        let text = two_of_them.to_string();
        assert!(text.contains("records no commit"), "{text}");
        assert!(
            text.contains("git checkout"),
            "the next step names how to fix it: {text}"
        );
        // And a verifiable client against an unverifiable runtime is refused too.
        let verifiable = gents_protocol::build_info::BuildInfo::from_parts(
            "1.0.0-rc2".to_owned(),
            Some("aaaaaaa".to_owned()),
            Some(false),
            Vec::new(),
        );
        assert!(
            PackInstallHandshakeError::build_mismatch(&verifiable, &unverifiable).is_some(),
            "an unverifiable runtime is not silently accepted"
        );
    }

    /// The older-runtime case is its own error with its own next step, and the
    /// connection and decode failures are not confused with each other.
    #[test]
    fn the_handshake_failures_are_distinguished_and_each_names_a_step() {
        let older = PackInstallHandshakeError::OlderRuntime {
            client: "gents 1.0.0-rc2 (aaaaaaa)".to_owned(),
            detail_bytes: 30,
        }
        .to_string();
        assert!(older.contains("gents runtime service update"), "{older}");
        assert!(older.contains("30 bytes"), "the length is stated: {older}");

        let lost = PackInstallHandshakeError::ConnectionLost { detail_bytes: 17 }.to_string();
        assert!(lost.contains("lost"), "{lost}");
        assert!(lost.contains("retry"), "{lost}");
        assert!(
            !lost.contains("older runtime"),
            "a lost connection is not a build problem: {lost}"
        );

        let unreadable = PackInstallHandshakeError::UnreadableHandshake {
            client: "gents 1.0.0-rc2 (aaaaaaa)".to_owned(),
            detail_bytes: 14,
        }
        .to_string();
        assert!(unreadable.contains("unreadable"), "{unreadable}");
        assert!(unreadable.contains("14 bytes"), "{unreadable}");
        assert!(
            !unreadable.contains("expected value"),
            "no verbatim runtime text leaks: {unreadable}"
        );
        // The message must not carry a run of spaces where a continuation broke.
        assert!(
            !unreadable.contains("  "),
            "no collapsed continuation: {unreadable}"
        );
    }

    fn build(sha: &str, dirty: bool) -> gents_protocol::build_info::BuildInfo {
        gents_protocol::build_info::BuildInfo::from_parts(
            "1.0.0-rc2".to_owned(),
            Some(sha.to_owned()),
            Some(dirty),
            Vec::new(),
        )
    }

    fn ready(build: &gents_protocol::build_info::BuildInfo) -> Value {
        json!({
            "type": "ready",
            "scope": {"node_did": "did:key:node", "pack_digests": ["sha256:root"], "nonce": "test"},
            "preparation": {"agent_bindings": {}, "plugin_digests": {}, "operator_ux_digests": {}},
            "build": build,
        })
    }

    /// A runtime that answers the start with `replies`, then waits for the
    /// client to close.
    async fn scripted_runtime(
        replies: Vec<Value>,
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
            for reply in replies {
                socket.send(Message::Text(reply.to_string().into())).await?;
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

    async fn start(
        endpoint: &GraphqlEndpoint,
        client: &gents_protocol::build_info::BuildInfo,
    ) -> Result<PackInstallSession> {
        PackInstallSession::start(
            endpoint,
            json!({}),
            "did:key:node",
            &["sha256:root".to_owned()],
            client,
        )
        .await
    }

    /// Two dirty builds of one commit are accepted, and the acceptance is in
    /// the install's result: the session carries it, and the replay session
    /// whose warnings `install_remote` returns keeps it after the session is
    /// consumed.
    #[tokio::test]
    async fn a_dirty_build_pair_reaches_the_install_result() -> Result<()> {
        let dirty = build("aaaaaaa", true);
        let (endpoint, server, _key) = scripted_runtime(vec![ready(&dirty)]).await?;
        let session = start(&endpoint, &dirty).await?;
        let expected = vec![PackInstallWarning::DirtyBuildPair {
            commit: "aaaaaaa".to_owned(),
        }];
        assert_eq!(session.warnings(), expected.as_slice());
        let replay = PackReplaySession::new(session);
        assert_eq!(replay.warnings(), expected.as_slice());
        assert!(replay.warnings()[0].describe().contains("aaaaaaa"));
        drop(replay);
        server.await??;

        // A clean pair carries no warning.
        let clean = build("aaaaaaa", false);
        let (endpoint, server, _key) = scripted_runtime(vec![ready(&clean)]).await?;
        assert!(start(&endpoint, &clean).await?.warnings().is_empty());
        server.await??;
        Ok(())
    }

    /// The runtime's `failed` text is shown only for a class whose text is a
    /// typed error of identifiers and build facts; every other class shows its
    /// own description, and an unclassified message is shown by length only.
    #[test]
    fn runtime_failure_text_passes_only_for_identifier_only_classes() {
        use crate::http::pack_install::failure::PackSessionFailureClass;
        let secret = "sk-live-0123456789";
        let quoted = format!("variable TOKEN resolved to {secret}");
        for class in PackSessionFailureClass::ALL {
            let code = serde_json::to_value(class).unwrap();
            let shown = runtime_failure(&quoted, "not_installed".into(), code.as_str()).to_string();
            if class.shows_runtime_text() {
                assert!(shown.contains(&quoted), "{shown}");
            } else {
                assert!(!shown.contains(secret), "{class:?}: {shown}");
                assert!(shown.contains(class.description()), "{shown}");
            }
            assert!(shown.contains("not_installed"), "{shown}");
        }
        for class in [None, Some("from_a_later_build")] {
            let shown = runtime_failure(&quoted, "unconfirmed".into(), class).to_string();
            assert!(!shown.contains(secret), "{shown}");
            assert!(shown.contains("unclassified"), "{shown}");
            assert!(
                shown.contains(&format!("{} bytes", quoted.len())),
                "{shown}"
            );
        }
    }

    /// After the handshake a refusal is typed too: the runtime's text is not
    /// what the operator sees for a class whose text can quote values.
    #[tokio::test]
    async fn a_refusal_after_the_handshake_shows_the_class_not_the_text() -> Result<()> {
        let clean = build("aaaaaaa", false);
        let secret = "sk-live-0123456789";
        let (endpoint, server, _key) = scripted_runtime(vec![
            ready(&clean),
            json!({"type": "failed", "outcome": "not_installed", "class": "apply_rejected",
                "error": format!("document rejected: token {secret}")}),
        ])
        .await?;
        let mut session = start(&endpoint, &clean).await?;
        let error = session
            .read("{ Node { _docID } }", &json!({}))
            .await
            .expect_err("the runtime refused");
        let shown = format!("{error:#}");
        assert!(!shown.contains(secret), "{shown}");
        assert!(shown.contains("apply_rejected"), "{shown}");
        drop(session);
        server.await??;
        Ok(())
    }

    /// A first message of the wrong type is named, with its real size.
    #[tokio::test]
    async fn an_unexpected_first_message_is_named_with_its_size() -> Result<()> {
        let finished = json!({"type": "finished", "result": {"pack": "early"}});
        let bytes = finished.to_string().len();
        let (endpoint, server, _key) = scripted_runtime(vec![finished]).await?;
        let error = start(&endpoint, &build("aaaaaaa", false))
            .await
            .err()
            .expect("an install is not finished before it is ready");
        let error = error
            .downcast_ref::<PackInstallHandshakeError>()
            .expect("typed");
        assert!(
            matches!(
                error,
                PackInstallHandshakeError::UnexpectedMessage { kind: "finished", detail_bytes }
                    if *detail_bytes == bytes
            ),
            "{error}"
        );
        assert!(error.to_string().contains(&format!("{bytes} bytes")));
        server.await??;
        Ok(())
    }
}

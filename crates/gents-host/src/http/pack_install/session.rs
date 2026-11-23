use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use futures_util::StreamExt;
use gents_pack::plugin::store::StoreWriter;
use gents_store::config_client::{
    ConfigAccess, ConfigApplyTxn, PackApprovalError, PackApprovalScope, PackEffectApprover,
    PackEvidence, PackMutation, PackRelationship, PackRelationshipEvidence, PackSchemaEvidence,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::failure::{RefusalIdentifiers, SessionFailure};
use super::{decode_install_request, delegated_install_access, refused, InstallRequest, MAX_BODY};
use gents_protocol::operations::pack::hosted::HostedInstallFailure as Class;

mod wire;
use crate::http::router::RuntimeHttpState;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ClientMessage {
    Start {
        request: InstallRequest,
    },
    /// Wire an installed plugin, the client signing each write.
    Wire {
        command: gents_pack::pack::plugin_wire::WireCommand,
    },
    SchemasReady {
        evidence: PackSchemaEvidence,
    },
    Read {
        id: u64,
        document: String,
        variables: Value,
    },
    GraphEvidence {
        id: u64,
        evidence: PackEvidence,
    },
    RelationshipEvidence {
        id: u64,
        evidence: PackRelationshipEvidence,
    },
    TimestampValue {
        id: u64,
        value: String,
    },
    Finish,
    Abort,
    /// The client presents a fresh delegation before the one it opened the
    /// session with lapses (`Identity.OwedRegistration.adopt`).
    Delegation {
        authorization: String,
    },
}

/// The session's access as its User and what keeps that access's
/// delegation current.
struct UserSession {
    access: ConfigAccess,
    refresher: Refresher,
}

/// Adopts the delegations a session's client presents again, for the writes
/// the session makes as its User.
#[derive(Clone)]
struct Refresher {
    node: Arc<defra_node::Node>,
    presented: Option<gents_store::config_client::user_write::PresentedDelegation>,
}

impl Refresher {
    fn adopt(&self, authorization: &str) -> Result<()> {
        let presented = self
            .presented
            .as_ref()
            .ok_or(gents_store::config_client::user_write::RefreshRefused)?;
        presented.refresh(&self.node, authorization)?;
        Ok(())
    }
}

pub async fn install_session(
    State(state): State<RuntimeHttpState>,
    identity: defra_http::ExtractIdentity,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(actor) = identity.did().cloned() else {
        return refused(StatusCode::UNAUTHORIZED, Class::NotAuthorized);
    };
    let access = match delegated_install_access(&state, &headers, actor.clone()) {
        Ok(access) => access,
        Err(_) => return refused(StatusCode::UNAUTHORIZED, Class::NotAuthorized),
    };
    if state.home.is_none() {
        return refused(StatusCode::SERVICE_UNAVAILABLE, Class::RuntimeUnavailable);
    }
    let Some(runtime) = state.activation_runtime.get() else {
        return refused(StatusCode::SERVICE_UNAVAILABLE, Class::RuntimeUnavailable);
    };
    let Some(store) = runtime.plugin_store_writer() else {
        return refused(StatusCode::SERVICE_UNAVAILABLE, Class::RuntimeUnavailable);
    };
    let node = runtime.observation_node().clone();
    let runtime_did = match identity::Did::new(&state.node_did) {
        Ok(did) => did,
        Err(_) => return refused(StatusCode::SERVICE_UNAVAILABLE, Class::RuntimeUnavailable),
    };
    if node.node_identity_did() != Some(runtime_did.as_str()) {
        return refused(StatusCode::SERVICE_UNAVAILABLE, Class::RuntimeUnavailable);
    }
    let runtime_access = ConfigAccess::local_as(node.clone(), runtime_did);
    let authorization =
        match gents_store::config_client::client_authentication::ClientAuthorization::from_http(
            &identity, &headers,
        ) {
            Ok(authorization) => authorization,
            Err(_) => return refused(StatusCode::UNAUTHORIZED, Class::NotAuthorized),
        };
    let presented = gents_store::config_client::user_write::PresentedDelegation::of_request(
        &node,
        &identity.context(),
        actor.as_str(),
    );
    let refresher = Refresher {
        node: node.clone(),
        presented: presented.clone(),
    };
    let user_access = match ConfigAccess::local_as(node, actor.clone())
        .with_client_authorization(authorization)
    {
        Ok(access) => access,
        Err(_) => return refused(StatusCode::FORBIDDEN, Class::NotAuthorized),
    };
    let user = UserSession {
        access: user_access.with_presented_delegation(presented),
        refresher,
    };
    upgrade
        .max_message_size(MAX_BODY.saturating_add(128))
        .max_frame_size(MAX_BODY.saturating_add(128))
        .on_upgrade(move |socket| async move {
            if let Err(error) = tokio::spawn(run(
                socket,
                state,
                store,
                access,
                user,
                runtime_access,
                actor,
            ))
            .await
            {
                tracing::error!(%error, "hosted pack session task failed");
            }
        })
}

async fn receive(socket: &mut WebSocket, refresher: &Refresher) -> Result<ClientMessage> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                match tokio::task::spawn_blocking(move || serde_json::from_str(&text)).await?? {
                    ClientMessage::Delegation { authorization } => {
                        refresher.adopt(&authorization)?
                    }
                    message => return Ok(message),
                }
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(_)) | None => anyhow::bail!("pack session closed"),
            Some(Err(error)) => return Err(error.into()),
        }
    }
}

async fn send(socket: &mut WebSocket, value: Value) -> Result<()> {
    let encoded = tokio::task::spawn_blocking(move || serde_json::to_string(&value)).await??;
    socket.send(Message::Text(encoded.into())).await?;
    Ok(())
}

struct SessionApprover {
    socket: Arc<Mutex<WebSocket>>,
    refresher: Refresher,
    next_id: AtomicU64,
    pending_id: AtomicU64,
}

impl SessionApprover {
    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    async fn wait(
        &self,
        txn: &ConfigApplyTxn<'_>,
        expected: &'static str,
        id: u64,
        proposal: Value,
    ) -> Result<ClientMessage, PackApprovalError> {
        let mut socket = self.socket.lock().await;
        send(&mut socket, proposal).await.map_err(transport)?;
        loop {
            match receive(&mut socket, &self.refresher)
                .await
                .map_err(transport)?
            {
                ClientMessage::Read {
                    id,
                    document,
                    variables,
                } => {
                    let result = txn
                        .pack_session_read(&document, &variables)
                        .await
                        .map_err(|error| PackApprovalError::Refused(format!("{error:#}")))?;
                    send(
                        &mut socket,
                        json!({"type":"read_result", "id":id, "result":result}),
                    )
                    .await
                    .map_err(transport)?;
                }
                message @ ClientMessage::GraphEvidence { id: got, .. }
                    if expected == "graph" && got == id =>
                {
                    return Ok(message)
                }
                message @ ClientMessage::RelationshipEvidence { id: got, .. }
                    if expected == "relationship" && got == id =>
                {
                    return Ok(message)
                }
                message @ ClientMessage::TimestampValue { id: got, .. }
                    if expected == "timestamp" && got == id =>
                {
                    return Ok(message)
                }
                ClientMessage::Abort => {
                    return Err(PackApprovalError::Refused(
                        "client aborted pack installation".into(),
                    ))
                }
                _ => {
                    return Err(PackApprovalError::Refused(
                        "unexpected pack session message".into(),
                    ))
                }
            }
        }
    }

    async fn applied(&self, result: Value) -> Result<(), PackApprovalError> {
        let id = self.pending_id.swap(0, Ordering::AcqRel);
        if id == 0 {
            return Err(PackApprovalError::Refused("no pending pack effect".into()));
        }
        send(
            &mut *self.socket.lock().await,
            json!({"type":"applied", "id":id, "result":result}),
        )
        .await
        .map_err(transport)
    }
}

fn transport(error: anyhow::Error) -> PackApprovalError {
    PackApprovalError::Transport(format!("{error:#}"))
}

#[async_trait::async_trait]
impl PackEffectApprover for SessionApprover {
    async fn approve_graph(
        &self,
        txn: &ConfigApplyTxn<'_>,
        mutation: &PackMutation,
        history: Option<Value>,
    ) -> Result<PackEvidence, PackApprovalError> {
        let id = self.id();
        let message = self
            .wait(
                txn,
                "graph",
                id,
                json!({"type":"approve_graph", "id":id, "mutation":mutation, "history":history}),
            )
            .await?;
        let ClientMessage::GraphEvidence { evidence, .. } = message else {
            unreachable!()
        };
        self.pending_id.store(id, Ordering::Release);
        Ok(evidence)
    }
    async fn approve_relationship(
        &self,
        txn: &ConfigApplyTxn<'_>,
        relationship: &PackRelationship,
    ) -> Result<PackRelationshipEvidence, PackApprovalError> {
        let id = self.id();
        let message = self
            .wait(
                txn,
                "relationship",
                id,
                json!({"type":"approve_relationship", "id":id, "relationship":relationship}),
            )
            .await?;
        let ClientMessage::RelationshipEvidence { evidence, .. } = message else {
            unreachable!()
        };
        self.pending_id.store(id, Ordering::Release);
        Ok(evidence)
    }
    async fn next_timestamp(
        &self,
        txn: &ConfigApplyTxn<'_>,
        kind: &'static str,
    ) -> Result<String, PackApprovalError> {
        let id = self.id();
        let message = self
            .wait(
                txn,
                "timestamp",
                id,
                json!({"type":"timestamp", "id":id, "kind":kind}),
            )
            .await?;
        let ClientMessage::TimestampValue { value, .. } = message else {
            unreachable!()
        };
        Ok(value)
    }
    async fn graph_applied(&self, result: &Value) -> Result<(), PackApprovalError> {
        self.applied(result.clone()).await
    }
    async fn relationship_applied(&self) -> Result<(), PackApprovalError> {
        self.applied(Value::Null).await
    }
    async fn client_finished(&self, txn: &ConfigApplyTxn<'_>) -> Result<(), PackApprovalError> {
        let mut socket = self.socket.lock().await;
        loop {
            match receive(&mut socket, &self.refresher)
                .await
                .map_err(transport)?
            {
                ClientMessage::Read {
                    id,
                    document,
                    variables,
                } => {
                    let result = txn
                        .pack_session_read(&document, &variables)
                        .await
                        .map_err(|error| PackApprovalError::Refused(format!("{error:#}")))?;
                    send(
                        &mut socket,
                        json!({"type":"read_result", "id":id, "result":result}),
                    )
                    .await
                    .map_err(transport)?;
                }
                ClientMessage::Finish => return Ok(()),
                ClientMessage::Abort => {
                    return Err(PackApprovalError::Refused(
                        "client aborted pack installation".into(),
                    ))
                }
                _ => {
                    return Err(PackApprovalError::Refused(
                        "unexpected pack session completion".into(),
                    ))
                }
            }
        }
    }
}

async fn run(
    socket: WebSocket,
    state: RuntimeHttpState,
    store: StoreWriter,
    access: ConfigAccess,
    user: UserSession,
    runtime_access: ConfigAccess,
    actor: identity::Did,
) {
    let socket = Arc::new(Mutex::new(socket));
    let result = run_inner(&socket, &state, store, access, user, runtime_access, actor).await;
    if let Err(failure) = result {
        failure.log();
        let _ = send(&mut *socket.lock().await, failure.message()).await;
    }
}

async fn run_inner(
    socket: &Arc<Mutex<WebSocket>>,
    state: &RuntimeHttpState,
    store: StoreWriter,
    access: ConfigAccess,
    user: UserSession,
    runtime_access: ConfigAccess,
    actor: identity::Did,
) -> std::result::Result<(), SessionFailure> {
    let UserSession {
        access: user_access,
        refresher,
    } = user;
    let home = store.home().to_owned();
    let runtime_did = identity::Did::new(&state.node_did)
        .map_err(SessionFailure::not_installed(Class::NotAuthorized))?;
    runtime_access
        .transact_readonly("pack.hosted_schema.authorize", |txn| {
            let runtime_did = &runtime_did;
            let actor = &actor;
            Box::pin(async move {
                gents_store::config_client::require_node_update_in_txn(txn, runtime_did, actor)
                    .await
                    .map_err(anyhow::Error::from)
            })
        })
        .await
        .map_err(SessionFailure::not_installed(Class::NotAuthorized))?;
    let first = receive(&mut *socket.lock().await, &refresher)
        .await
        .map_err(SessionFailure::not_installed(Class::InvalidRequest))?;
    let request = match first {
        ClientMessage::Start { request } => request,
        ClientMessage::Wire { command } => {
            return wire::run(socket, state, &store, &user_access, refresher, command).await
        }
        _ => {
            return Err(SessionFailure::not_installed(Class::InvalidRequest)(
                anyhow::anyhow!("pack session requires a start request"),
            ))
        }
    };
    if request.preview {
        let result = preview(state, home, access, request).await?;
        return send(
            &mut *socket.lock().await,
            json!({"type": "previewed", "result": result}),
        )
        .await
        .map_err(SessionFailure::not_installed(Class::PreviewFailed));
    }
    let mut received = async {
        anyhow::ensure!(
            request.wire.is_empty(),
            "hosted pack session cannot wire plugins"
        );
        let node_did = state.node_did.clone();
        tokio::task::spawn_blocking(move || decode_install_request(request, &node_did)).await?
    }
    .await
    .map_err(SessionFailure::not_installed(Class::InvalidRequest))?;
    let mut identifiers = RefusalIdentifiers::of(
        &std::iter::once(&received.root)
            .chain(received.dependencies.iter())
            .collect::<Vec<_>>(),
    );
    let failed = SessionFailure::not_installed::<anyhow::Error>(Class::PreparationFailed);
    let prepare = async {
        let requested =
            gents_pack::operations::pack::parse_inference_slots(&received.request.inference_slots)?;
        let supplied = gents_pack::pack::install_flow::supplied_variables(
            &received.request.dependency_variables,
            &received.request.bind_agents,
        )?;
        let overrides = gents_pack::operations::pack::dependency_overrides(&supplied);
        let environment = gents_pack::operations::pack::InstallEnvironment::HostedRuntime(
            std::mem::take(&mut received.request.installer_environment),
        );
        let mut prepared = match gents_pack::pack::prepare_document_pack_archive(
            &gents_pack::pack::InstallTarget {
                access: &access,
                owner: &state.node_did,
                plugin_home: Some(&store),
            },
            received.root.archive(),
            &gents_pack::pack::PackGraphInputs {
                dependencies: &received
                    .dependencies
                    .iter()
                    .map(|source| source.archive())
                    .collect::<Vec<_>>(),
                requested: &requested,
                environment: &|name| {
                    gents_pack::operations::pack::install_value(
                        gents_protocol::operations::pack::PackActor::User,
                        &supplied,
                        &environment,
                        name,
                    )
                },
                overrides: &overrides,
            },
            gents_pack::pack::InstalledVia::Operator,
        )
        .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                return Ok(Err(SessionFailure::change(
                    Class::PreparationFailed,
                    error,
                    &identifiers,
                )))
            }
        };
        gents_pack::pack::resolved_pack::ResolvedPack::record_graph_sources(
            &mut prepared,
            &received.root,
            &received.dependencies,
        );
        identifiers.ship(&prepared)?;
        gents_pack::pack::command_authority::require_command_task_grant(
            &access,
            &state.node_did,
            &format!(
                "{}/{}",
                received.root.manifest().metadata.namespace,
                received.root.manifest().name
            ),
            &prepared,
            received.request.grant_authority,
        )
        .await?;
        // The client grants after this install, from the rows of its plan.
        let planned = gents_tools::surface::planned_root_agents(
            &access,
            &state.node_did,
            received.root.manifest(),
            &prepared,
        )
        .await?;
        let prepared = Arc::new(prepared);
        let stage = gents_pack::pack::stage_hosted_pack(
            &state.node_did,
            &store,
            received.root.shared_archive(),
            prepared.clone(),
            received.request.grant_authority,
            received.request.explicit,
        )
        .await?;
        Ok::<_, anyhow::Error>(Ok((received, prepared, stage, planned)))
    }
    .await;
    let (received, prepared, stage, planned) = prepare.map_err(failed)??;
    let installed_plugins = stage.installed_plugins().to_vec();
    let materialized = if received.request.explicit {
        let cache_home = home.clone();
        let root = received.root.shared_archive();
        let dependencies = received
            .dependencies
            .iter()
            .map(|source| source.shared_archive())
            .collect::<Vec<_>>();
        tokio::task::spawn_blocking(move || {
            let mut leases = Vec::new();
            let mut dependency_assets = Vec::new();
            for dependency in &dependencies {
                if let Some((assets, lease)) =
                    gents_pack::pack::home_install::materialize_asset_pack(&cache_home, dependency)?
                {
                    dependency_assets.push((
                        format!(
                            "{}/{}",
                            dependency.manifest().metadata.namespace,
                            dependency.manifest().name
                        ),
                        assets,
                    ));
                    leases.push(lease);
                }
            }
            let assets = if let Some((assets, lease)) =
                gents_pack::pack::home_install::materialize_asset_pack(&cache_home, &root)?
            {
                leases.push(lease);
                assets
            } else {
                String::new()
            };
            Ok::<_, anyhow::Error>((assets, dependency_assets, leases))
        })
        .await
        .map_err(SessionFailure::not_installed(Class::PreparationFailed))?
        .map_err(SessionFailure::not_installed(Class::PreparationFailed))?
    } else {
        (String::new(), Vec::new(), Vec::new())
    };
    let scope = PackApprovalScope {
        node_did: state.node_did.clone(),
        pack_digests: std::iter::once(&received.root)
            .chain(received.dependencies.iter())
            .map(|source| source.digest().to_owned())
            .collect(),
        nonce: uuid::Uuid::new_v4().to_string(),
    };
    send(
        &mut *socket.lock().await,
        json!({"type":"ready", "scope":scope, "preparation":stage.preparation()}),
    )
    .await
    .map_err(SessionFailure::not_installed(Class::SchemaPreparation))?;
    let evidence = match tokio::time::timeout(
        std::time::Duration::from_secs(300),
        receive(&mut *socket.lock().await, &refresher),
    )
    .await
    .map_err(SessionFailure::not_installed(Class::SchemaPreparation))?
    .map_err(SessionFailure::not_installed(Class::SchemaPreparation))?
    {
        ClientMessage::SchemasReady { evidence } => evidence,
        _ => {
            return Err(SessionFailure::not_installed(Class::SchemaPreparation)(
                anyhow::anyhow!("client did not finish schema preparation"),
            ))
        }
    };
    gents_store::config_client::verify_pack_schema_approval(actor.as_str(), &scope, evidence)
        .map_err(SessionFailure::not_installed(Class::SchemaPreparation))?;
    // Read before any schema is applied: the collections this install
    // brings, which `pack remove --with-collections` later drops.
    let created_collections =
        gents_pack::pack::collections_install_creates(&runtime_access, received.root.archive())
            .await
            .map_err(SessionFailure::not_installed(Class::SchemaApplyFailed))?;
    gents_pack::pack::apply_prepared_dependency_schemas(&runtime_access, &prepared)
        .await
        .map_err(SessionFailure::not_installed(Class::SchemaApplyFailed))?;
    let schemas = if received.root.manifest().metadata.kind == gents_pack::pack::PackKind::Documents
    {
        let root = received.root.shared_archive();
        let temp = tokio::task::spawn_blocking(move || {
            let temp = tempfile::tempdir()?;
            gents_pack::pack::materialize::materialize(&root, temp.path())?;
            Ok::<_, anyhow::Error>(temp)
        })
        .await
        .map_err(SessionFailure::not_installed(Class::SchemaApplyFailed))?
        .map_err(SessionFailure::not_installed(Class::SchemaApplyFailed))?;
        gents_pack::pack::schema_apply::apply_pack_schemas_if_present(&runtime_access, temp.path())
            .await
            .map_err(SessionFailure::not_installed(Class::SchemaApplyFailed))?
    } else {
        None
    };
    let policy = gents_pack::operations::pack::drift_policy(
        gents_protocol::operations::pack::EditedDocuments::of_switches(
            received.request.overwrite,
            received.request.keep,
        ),
    );
    let approver = Arc::new(SessionApprover {
        socket: socket.clone(),
        refresher,
        next_id: AtomicU64::new(1),
        pending_id: AtomicU64::new(0),
    });
    let home_publication = if received.request.explicit {
        let coordinate = format!(
            "{}/{}",
            received.root.manifest().metadata.namespace,
            received.root.manifest().name
        );
        let record = gents_pack::pack::HomePackInstall {
            coordinate: coordinate.clone(),
            version: received.root.manifest().version.clone(),
            digest: received.root.digest().to_owned(),
            kind: received.root.manifest().metadata.kind.clone(),
            assets: materialized.0,
            dependency_assets: materialized.1,
            plugins: installed_plugins
                .iter()
                .filter(|plugin| {
                    plugin.owner_pack_coordinate.as_deref() == Some(coordinate.as_str())
                })
                .map(|plugin| gents_pack::pack::InstalledPackPlugin {
                    name: plugin.name.clone(),
                    digest: plugin.digest.clone(),
                })
                .collect(),
            ux: received.root.manifest().metadata.ux.clone(),
            installed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            installed_via: Some(gents_pack::pack::InstalledVia::Operator),
            source: Some(received.root.provenance()),
            source_history: Vec::new(),
            created_collections,
        };
        Some(gents_pack::pack::HostedHomePublication {
            home,
            record,
            leases: materialized.2,
        })
    } else {
        None
    };
    let reports = stage
        .apply(
            &user_access,
            &state.node_did,
            gents_pack::pack::HostedPackApproval { scope, approver },
            policy,
            home_publication,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await
        .map_err(|error| match error {
            gents_pack::pack::HostedPackStageError::Rejected(rejection) => {
                SessionFailure::rejection(rejection, &identifiers)
            }
            gents_pack::pack::HostedPackStageError::Unconfirmed(error) => {
                SessionFailure::unconfirmed(Class::ApplyUnconfirmed)(error)
            }
        })?;
    let report = gents_pack::pack::assemble_document_graph_report(&prepared, reports)
        .map_err(SessionFailure::unconfirmed(Class::ReceiptFailed))?;
    let manifest = received.root.manifest();
    let origin_tag = gents_pack::pack::pack_origin_tag(&format!(
        "{}/{}",
        manifest.metadata.namespace, manifest.name
    ))
    .map_err(SessionFailure::unconfirmed(Class::ReceiptFailed))?;
    let mut result = json!({"pack":received.root.manifest().name, "source":received.root.label(), "digest":received.root.digest(), "owner":state.node_did, "inference":prepared.inference, "dependency_inference":prepared.dependency_inference, "origin_tag":origin_tag, "dependencies":received.root.manifest().metadata.dependencies, "schemas":schemas, "apply":report});
    gents_pack::pack::installation_roots::attach_planned_root_agents(&mut result, &planned);
    send(
        &mut *socket.lock().await,
        json!({"type":"finished", "result":result}),
    )
    .await
    .map_err(SessionFailure::unconfirmed(Class::ReceiptFailed))?;
    Ok(())
}

/// A preview over the session, after the session authorized its User to
/// update the Node. It writes nothing.
async fn preview(
    state: &RuntimeHttpState,
    home: PathBuf,
    access: ConfigAccess,
    request: InstallRequest,
) -> std::result::Result<Value, SessionFailure> {
    let node_did = state.node_did.clone();
    let received = tokio::task::spawn_blocking(move || decode_install_request(request, &node_did))
        .await
        .map_err(SessionFailure::not_installed(Class::InvalidRequest))?
        .map_err(SessionFailure::not_installed(Class::InvalidRequest))?;
    if received.root.manifest().metadata.kind != gents_pack::pack::PackKind::Documents {
        return Err(SessionFailure::not_installed(Class::InvalidRequest)(
            anyhow::anyhow!("a hosted preview takes a documents pack"),
        ));
    }
    let identifiers = RefusalIdentifiers::of(
        &std::iter::once(&received.root)
            .chain(received.dependencies.iter())
            .collect::<Vec<_>>(),
    );
    super::install_owned(state.clone(), home, received, access)
        .await
        .map_err(|error| SessionFailure::change(Class::PreviewFailed, error, &identifiers))
}

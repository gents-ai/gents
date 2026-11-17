use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use futures_util::StreamExt;
use gents_store::config_client::{
    ConfigAccess, ConfigApplyTxn, PackApprovalError, PackApprovalScope, PackEffectApprover,
    PackEvidence, PackMutation, PackRelationship, PackRelationshipEvidence, PackSchemaEvidence,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::failure::{PackSessionFailureClass as Class, SessionFailure};
use super::{decode_install_request, delegated_install_access, refused, InstallRequest, MAX_BODY};
use crate::http::router::RuntimeHttpState;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ClientMessage {
    Start {
        request: InstallRequest,
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
}

pub async fn install_session(
    State(state): State<RuntimeHttpState>,
    identity: defra_http::ExtractIdentity,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(actor) = identity.did().cloned() else {
        return refused(
            StatusCode::UNAUTHORIZED,
            "Client authentication is required.",
        );
    };
    let access = match delegated_install_access(&state, &headers, actor.clone()) {
        Ok(access) => access,
        Err(error) => return refused(StatusCode::UNAUTHORIZED, error),
    };
    let Some(home) = state.home.as_ref().map(PathBuf::from) else {
        return refused(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime plugin store unavailable",
        );
    };
    let Some(runtime) = state.activation_runtime.get() else {
        return refused(StatusCode::SERVICE_UNAVAILABLE, "runtime is not ready");
    };
    let node = runtime.observation_node().clone();
    let runtime_did = match identity::Did::new(&state.node_did) {
        Ok(did) => did,
        Err(error) => return refused(StatusCode::SERVICE_UNAVAILABLE, error),
    };
    if node.node_identity_did() != Some(runtime_did.as_str()) {
        return refused(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime Node identity mismatch",
        );
    }
    let runtime_access = ConfigAccess::local_as(node.clone(), runtime_did);
    let user_access = ConfigAccess::local_as(node, actor.clone());
    upgrade
        .max_message_size(MAX_BODY.saturating_add(128))
        .max_frame_size(MAX_BODY.saturating_add(128))
        .on_upgrade(move |socket| async move {
            if let Err(error) = tokio::spawn(run(
                socket,
                state,
                home,
                access,
                user_access,
                runtime_access,
                actor,
            ))
            .await
            {
                tracing::error!(%error, "hosted pack session task failed");
            }
        })
}

async fn receive(socket: &mut WebSocket) -> Result<ClientMessage> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                return Ok(
                    tokio::task::spawn_blocking(move || serde_json::from_str(&text)).await??,
                );
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
            match receive(&mut socket).await.map_err(transport)? {
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
            match receive(&mut socket).await.map_err(transport)? {
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
    home: PathBuf,
    access: ConfigAccess,
    user_access: ConfigAccess,
    runtime_access: ConfigAccess,
    actor: identity::Did,
) {
    let socket = Arc::new(Mutex::new(socket));
    let result = run_inner(
        &socket,
        &state,
        home,
        access,
        user_access,
        runtime_access,
        actor,
    )
    .await;
    if let Err(failure) = result {
        // The class and outcome only: the chain can quote install values, so
        // it is neither sent nor logged (see `failure`).
        tracing::warn!(
            class = ?failure.class,
            outcome = failure.outcome,
            "hosted pack install failed"
        );
        let _ = send(&mut *socket.lock().await, failure.message()).await;
    }
}

async fn run_inner(
    socket: &Arc<Mutex<WebSocket>>,
    state: &RuntimeHttpState,
    home: PathBuf,
    access: ConfigAccess,
    user_access: ConfigAccess,
    runtime_access: ConfigAccess,
    actor: identity::Did,
) -> std::result::Result<(), SessionFailure> {
    let runtime_did = identity::Did::new(&state.node_did)
        .map_err(|error| SessionFailure::not_installed(Class::NotAuthorized)(error.into()))?;
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
    let request = async {
        let ClientMessage::Start { request } = receive(&mut *socket.lock().await).await? else {
            anyhow::bail!("pack session requires a start request");
        };
        Ok(request)
    }
    .await
    .map_err(SessionFailure::not_installed(Class::InvalidRequest))?;
    // The build check comes first, before the archives are even opened: a plan
    // prepared by two builds is not one plan, so neither an install nor a
    // preview proceeds, and nothing is written.
    if let Some(refusal) = super::refuse_client_build(&request.client_build) {
        return Err(SessionFailure::shown(
            Class::BuildMismatch,
            refusal.to_string(),
            "not_installed",
        ));
    }
    if request.preview {
        let result = preview(state, home, access, request).await?;
        send(
            &mut *socket.lock().await,
            json!({
                "type": "previewed",
                "result": result,
                // So the client checks the build too, as it does at `ready`.
                "build": super::runtime_build(),
            }),
        )
        .await
        .map_err(SessionFailure::not_installed(Class::PreviewFailed))?;
        return Ok(());
    }
    let received = async {
        anyhow::ensure!(
            request.wire.is_empty(),
            "hosted pack session cannot wire plugins"
        );
        let node_did = state.node_did.clone();
        Ok(
            tokio::task::spawn_blocking(move || decode_install_request(request, &node_did))
                .await??,
        )
    }
    .await
    .map_err(SessionFailure::not_installed(Class::InvalidRequest))?;
    let preparation_failed = SessionFailure::not_installed(Class::PreparationFailed);
    let prepare = async {
        let requested =
            gents_pack::operations::pack::parse_inference_slots(&received.request.inference_slots)?;
        let supplied = gents_pack::pack::install_flow::supplied_variables(
            &received.request.dependency_variables,
            &received.request.bind_agents,
        )?;
        let overrides = gents_pack::operations::pack::dependency_overrides(&supplied);
        let mut prepared = match gents_pack::pack::prepare_document_pack_archive(
            &access,
            &state.node_did,
            received.root.archive(),
            &requested,
            &received
                .dependencies
                .iter()
                .map(|source| source.archive())
                .collect::<Vec<_>>(),
            gents_pack::pack::InstalledVia::Operator,
            Some(&home),
            &|name| {
                gents_pack::operations::pack::install_value(
                    gents_protocol::operations::pack::PackActor::User,
                    &supplied,
                    name,
                )
            },
            &overrides,
        )
        .await
        {
            Ok(prepared) => prepared,
            // A plugin reference that does not resolve names identifiers only,
            // so the client is shown it as written.
            Err(gents_pack::pack::PackChangeError::PluginReference(unresolved)) => {
                return Ok(Err(SessionFailure::shown(
                    Class::PluginUnresolved,
                    unresolved.to_string(),
                    "not_installed",
                )));
            }
            Err(error) => return Err(error.into_anyhow()),
        };
        prepared
            .prepare_pin_moves(
                &access,
                &state.node_did,
                &received.request.move_pin_tools,
                &std::iter::once(received.root.archive())
                    .chain(received.dependencies.iter().map(|d| d.archive()))
                    .collect::<Vec<_>>(),
            )
            .await?;
        gents_pack::pack::resolved_pack::ResolvedPack::record_graph_sources(
            &mut prepared,
            &received.root,
            &received.dependencies,
        );
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
            &home,
            received.root.shared_archive(),
            prepared.clone(),
            received.request.grant_authority,
            received.request.explicit,
        )
        .await?;
        Ok::<_, anyhow::Error>(Ok((received, prepared, stage, planned)))
    }
    .await;
    let (received, prepared, stage, planned) = prepare.map_err(preparation_failed)??;
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
        .map_err(|error| SessionFailure::not_installed(Class::PreparationFailed)(error.into()))?
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
        json!({
            "type": "ready",
            "scope": scope,
            "preparation": stage.preparation(),
            // The other half of the handshake: which build proposed this plan.
            // The build this runtime is, supplied by the binary that has the
            // core packs (this crate carries none).
            "build": crate::http::pack_install::runtime_build(),
        }),
    )
    .await
    .map_err(SessionFailure::not_installed(Class::SchemaPreparation))?;
    let evidence = match tokio::time::timeout(
        std::time::Duration::from_secs(300),
        receive(&mut *socket.lock().await),
    )
    .await
    .map_err(|error| {
        SessionFailure::not_installed(Class::SchemaPreparation)(
            anyhow::Error::new(error).context("client schema preparation timed out"),
        )
    })?
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
        .map_err(|error| SessionFailure::not_installed(Class::SchemaPreparation)(error.into()))?;
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
        .map_err(|error| SessionFailure::not_installed(Class::SchemaApplyFailed)(error.into()))?
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
            gents_pack::pack::HostedPackStageError::Rejected(error) => {
                SessionFailure::not_installed(Class::ApplyRejected)(error)
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

/// A preview over the session: the same plan the one-shot route previews,
/// computed only after the build check has passed. A preview writes nothing.
async fn preview(
    state: &RuntimeHttpState,
    home: PathBuf,
    access: ConfigAccess,
    request: InstallRequest,
) -> std::result::Result<Value, SessionFailure> {
    let node_did = state.node_did.clone();
    let received = tokio::task::spawn_blocking(move || decode_install_request(request, &node_did))
        .await
        .map_err(|error| SessionFailure::not_installed(Class::InvalidRequest)(error.into()))?
        .map_err(SessionFailure::not_installed(Class::InvalidRequest))?;
    if received.root.manifest().metadata.kind != gents_pack::pack::PackKind::Documents {
        return Err(SessionFailure::not_installed(Class::InvalidRequest)(
            anyhow::anyhow!("a hosted preview takes a documents pack"),
        ));
    }
    super::install_owned(state.clone(), home, received, access)
        .await
        .map_err(SessionFailure::not_installed(Class::PreviewFailed))
}

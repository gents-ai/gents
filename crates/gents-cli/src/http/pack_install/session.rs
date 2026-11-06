use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use gents::store::config_client::{
    ConfigAccess, ConfigApplyTxn, PackApprovalError, PackApprovalScope, PackEffectApprover,
    PackEvidence, PackMutation, PackRelationship, PackRelationshipEvidence,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::{decode_install_request, delegated_install_access, refused, InstallRequest, MAX_BODY};
use crate::http::router::RuntimeHttpState;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ClientMessage {
    Start {
        request: InstallRequest,
    },
    SchemasReady {
        #[serde(default)]
        schemas: Value,
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

pub(crate) async fn install_session(
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
    let user_access = ConfigAccess::local_as(runtime.observation_node().clone(), actor);
    upgrade
        .max_message_size(MAX_BODY.saturating_add(128))
        .max_frame_size(MAX_BODY.saturating_add(128))
        .on_upgrade(move |socket| async move {
            if let Err(error) = tokio::spawn(run(socket, state, home, access, user_access)).await {
                tracing::error!(%error, "hosted pack session task failed");
            }
        })
}

async fn receive(socket: &mut WebSocket) -> Result<ClientMessage> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => return Ok(serde_json::from_str(&text)?),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(_)) | None => anyhow::bail!("pack session closed"),
            Some(Err(error)) => return Err(error.into()),
        }
    }
}

async fn send(socket: &mut WebSocket, value: Value) -> Result<()> {
    socket
        .send(Message::Text(serde_json::to_string(&value)?.into()))
        .await?;
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
            &mut self.socket.lock().await,
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
) {
    let socket = Arc::new(Mutex::new(socket));
    let result = run_inner(&socket, &state, home, access, user_access).await;
    if let Err((error, outcome)) = result {
        let _ = send(
            &mut socket.lock().await,
            json!({"type":"failed", "error":format!("{error:#}"), "outcome":outcome}),
        )
        .await;
    }
}

async fn run_inner(
    socket: &Arc<Mutex<WebSocket>>,
    state: &RuntimeHttpState,
    home: PathBuf,
    access: ConfigAccess,
    user_access: ConfigAccess,
) -> std::result::Result<(), (anyhow::Error, &'static str)> {
    let prepare = async {
        let ClientMessage::Start { request } = receive(&mut socket.lock().await).await? else {
            anyhow::bail!("pack session requires a start request");
        };
        anyhow::ensure!(
            !request.preview && request.wire.is_empty(),
            "hosted pack session cannot preview or wire plugins"
        );
        let received = decode_install_request(request, &state.node_did)?;
        let requested = crate::commands::pack::parse_inference_slot_bindings(
            &received.request.inference_slots,
        )?;
        let prepared = Arc::new(
            gents::pack::prepare_document_pack_archive(
                &access,
                &state.node_did,
                received.root.archive(),
                &requested,
                &received
                    .dependencies
                    .iter()
                    .map(|source| source.archive())
                    .collect::<Vec<_>>(),
                gents::pack::InstalledVia::Operator,
                Some(&home),
                &|name| std::env::var(name).ok(),
            )
            .await?,
        );
        let stage = gents::pack::stage_hosted_pack(
            &state.node_did,
            &home,
            received.root.archive(),
            prepared.clone(),
            received.request.grant_authority,
            received.request.explicit,
        )
        .await?;
        Ok::<_, anyhow::Error>((received, prepared, stage))
    }
    .await;
    let (received, prepared, stage) = prepare.map_err(|error| (error, "not_installed"))?;
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
                    gents::pack::home_install::materialize_asset_pack(&cache_home, dependency)?
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
                gents::pack::home_install::materialize_asset_pack(&cache_home, &root)?
            {
                leases.push(lease);
                assets
            } else {
                String::new()
            };
            Ok::<_, anyhow::Error>((assets, dependency_assets, leases))
        })
        .await
        .map_err(|error| (anyhow::Error::new(error), "not_installed"))?
        .map_err(|error| (error, "not_installed"))?
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
        &mut socket.lock().await,
        json!({"type":"ready", "scope":scope, "preparation":stage.preparation()}),
    )
    .await
    .map_err(|error| (error, "not_installed"))?;
    let schemas = match tokio::time::timeout(
        std::time::Duration::from_secs(300),
        receive(&mut socket.lock().await),
    )
    .await
    .map_err(|error| {
        (
            anyhow::Error::new(error).context("client schema preparation timed out"),
            "not_installed",
        )
    })?
    .map_err(|error| (error, "not_installed"))?
    {
        ClientMessage::SchemasReady { schemas } => schemas,
        _ => {
            return Err((
                anyhow::anyhow!("client did not finish schema preparation"),
                "not_installed",
            ))
        }
    };
    let policy = if received.request.overwrite {
        gents::pack::DriftPolicy::Overwrite
    } else if received.request.keep {
        gents::pack::DriftPolicy::Keep
    } else {
        gents::pack::DriftPolicy::Refuse
    };
    let approver = Arc::new(SessionApprover {
        socket: socket.clone(),
        next_id: AtomicU64::new(1),
        pending_id: AtomicU64::new(0),
    });
    let reports = stage
        .apply(&user_access, &state.node_did, scope, approver, policy)
        .await
        .map_err(|error| {
            let outcome = match &error {
                gents::pack::HostedPackStageError::Rejected(_) => "not_installed",
                gents::pack::HostedPackStageError::Unconfirmed(_) => "unconfirmed",
            };
            (anyhow::Error::new(error), outcome)
        })?;
    let report = gents::pack::assemble_document_graph_report(&prepared, reports)
        .map_err(|error| (error, "unconfirmed"))?;
    if received.request.explicit {
        let coordinate = format!(
            "{}/{}",
            received.root.manifest().metadata.namespace,
            received.root.manifest().name
        );
        let record = gents::pack::HomePackInstall {
            coordinate: coordinate.clone(),
            version: received.root.manifest().version.clone(),
            digest: received.root.digest().to_owned(),
            kind: gents::pack::PackKind::Documents,
            assets: materialized.0,
            dependency_assets: materialized.1,
            plugins: installed_plugins
                .iter()
                .filter(|plugin| {
                    plugin.owner_pack_coordinate.as_deref() == Some(coordinate.as_str())
                })
                .map(|plugin| gents::pack::InstalledPackPlugin {
                    name: plugin.name.clone(),
                    digest: plugin.digest.clone(),
                })
                .collect(),
            ux: received.root.manifest().metadata.ux.clone(),
            installed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            installed_via: Some(gents::pack::InstalledVia::Operator),
        };
        let leases = materialized.2;
        tokio::task::spawn_blocking(move || {
            let _leases = leases;
            gents::pack::record_after_install(
                &home,
                record,
                gents::pack::InstalledVia::Operator,
                true,
            )
        })
        .await
        .map_err(|error| (anyhow::Error::new(error), "unconfirmed"))?
        .map_err(|error| {
            (
                error.context("pack documents committed but home install record failed"),
                "unconfirmed",
            )
        })?;
    }
    let origin_tag = gents::pack::pack_origin_tag(&received.root.manifest().name)
        .map_err(|error| (error, "unconfirmed"))?;
    let result = json!({"pack":received.root.manifest().name, "source":received.root.label(), "digest":received.root.digest(), "owner":state.node_did, "inference":prepared.inference, "dependency_inference":prepared.dependency_inference, "origin_tag":origin_tag, "dependencies":received.root.manifest().metadata.dependencies, "schemas":schemas, "apply":report});
    send(
        &mut socket.lock().await,
        json!({"type":"finished", "result":result}),
    )
    .await
    .map_err(|error| (error, "unconfirmed"))?;
    Ok(())
}

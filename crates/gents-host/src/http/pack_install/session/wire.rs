use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use axum::extract::ws::WebSocket;
use gents_pack::pack::plugin_wire::{WireCommand, WirePlan};
use gents_store::config_client::{ConfigAccess, PackApprovalScope, PackEffectApprover as _};
use serde_json::json;
use tokio::sync::Mutex;

use super::{send, Class, Refresher, SessionApprover, SessionFailure};
use crate::http::router::RuntimeHttpState;

/// Wire new pins as the session's User in one runtime transaction; the
/// client signs each write the plans propose, and the runtime holds the
/// plugin store until the commit.
pub(super) async fn run(
    socket: &Arc<Mutex<WebSocket>>,
    state: &RuntimeHttpState,
    home: &Path,
    user_access: &ConfigAccess,
    refresher: Refresher,
    command: WireCommand,
) -> std::result::Result<(), SessionFailure> {
    let WireCommand { node_did, requests } = command;
    if node_did != state.node_did {
        return Err(SessionFailure::not_installed(Class::InvalidRequest)(
            anyhow::anyhow!("endpoint serves a different node"),
        ));
    }
    if requests.is_empty() || requests.iter().any(|request| request.preview || request.remove) {
        return Err(SessionFailure::not_installed(Class::InvalidRequest)(
            anyhow::anyhow!("a pack session wires new pins"),
        ));
    }
    let wire = WirePlan::read(user_access, &state.node_did, home, &requests)
        .await
        .map_err(SessionFailure::not_installed(Class::PreparationFailed))?;
    let scope = PackApprovalScope {
        node_did: state.node_did.clone(),
        pack_digests: wire.digests(),
        nonce: uuid::Uuid::new_v4().to_string(),
    };
    let preparation =
        json!({"agent_bindings": {}, "plugin_digests": {}, "operator_ux_digests": {}});
    send(
        &mut *socket.lock().await,
        json!({"type": "ready", "scope": scope, "preparation": preparation}),
    )
    .await
    .map_err(SessionFailure::not_installed(Class::SchemaPreparation))?;
    let approver = Arc::new(SessionApprover {
        socket: socket.clone(),
        refresher,
        next_id: AtomicU64::new(1),
        pending_id: AtomicU64::new(0),
    });
    let report = user_access
        .transact_verified_pack_with_approver("pack.wire.session", scope, approver.clone(), |txn| {
            let (wire, approver) = (&wire, &approver);
            Box::pin(async move {
                let interpreter = txn.approved_interpreter()?;
                let reports = wire
                    .apply(interpreter, gents_pack::pack::wiring::BatchWrite::Apply)
                    .await?;
                approver.client_finished(interpreter).await?;
                Ok::<_, anyhow::Error>(reports)
            })
        })
        .await
        .map_err(|failure| {
            if failure.unconfirmed {
                SessionFailure::unconfirmed(Class::ApplyUnconfirmed)(failure.error)
            } else {
                SessionFailure::not_installed(Class::ApplyRejected)(failure.error)
            }
        })?;
    let result = wire
        .outcomes(&report)
        .map_err(SessionFailure::unconfirmed(Class::ReceiptFailed))?;
    send(
        &mut *socket.lock().await,
        json!({"type": "finished", "result": result}),
    )
    .await
    .map_err(SessionFailure::unconfirmed(Class::ReceiptFailed))
}

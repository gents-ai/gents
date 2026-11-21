use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use axum::extract::ws::WebSocket;
use gents_pack::pack::plugin_wire::{wire_outcome, WireCommand, WirePlan};
use gents_store::config_client::{ConfigAccess, PackApprovalScope, PackEffectApprover as _};
use serde_json::json;
use tokio::sync::Mutex;

use super::{send, Class, Refresher, SessionApprover, SessionFailure};
use crate::http::router::RuntimeHttpState;

/// Wire a new pin as the session's User in the runtime's transaction; the
/// client signs each write the plan proposes, and the runtime holds the
/// plugin store until the commit.
pub(super) async fn run(
    socket: &Arc<Mutex<WebSocket>>,
    state: &RuntimeHttpState,
    home: &Path,
    user_access: &ConfigAccess,
    refresher: Refresher,
    command: WireCommand,
) -> std::result::Result<(), SessionFailure> {
    let WireCommand { node_did, request } = command;
    if node_did != state.node_did {
        return Err(SessionFailure::not_installed(Class::InvalidRequest)(
            anyhow::anyhow!("endpoint serves a different node"),
        ));
    }
    if request.preview || request.remove {
        return Err(SessionFailure::not_installed(Class::InvalidRequest)(
            anyhow::anyhow!("a pack session wires a new pin"),
        ));
    }
    let wire = WirePlan::read(user_access, &state.node_did, home, &request)
        .await
        .map_err(SessionFailure::not_installed(Class::PreparationFailed))?;
    let scope = PackApprovalScope {
        node_did: state.node_did.clone(),
        pack_digests: vec![wire.plugin.digest.clone()],
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
            let (plan, approver) = (&wire.plan, &approver);
            Box::pin(async move {
                let interpreter = txn.approved_interpreter()?;
                let report = plan
                    .apply(
                        interpreter,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await?;
                approver.client_finished(interpreter).await?;
                Ok::<_, anyhow::Error>(report)
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
    let result = wire_outcome(&request, &wire.plugin, &report)
        .map_err(SessionFailure::unconfirmed(Class::ReceiptFailed))?;
    send(
        &mut *socket.lock().await,
        json!({"type": "finished", "result": result}),
    )
    .await
    .map_err(SessionFailure::unconfirmed(Class::ReceiptFailed))
}

use std::sync::Arc;

use crate::client_runtime::ClientRuntime;
use gents_protocol::client_access::{ClientAccessDecision, ClientAccessState, DecideClientAccess};

#[derive(Debug, Clone)]
pub(crate) struct ManagedPairingTarget {
    pub(crate) node_name: String,
    pub(crate) node_did: String,
    pub(crate) graphql: String,
}

pub(crate) const MANAGED_PAIRING_CANCELLED: &str = "managed runtime connection was cancelled";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PairingFailure {
    #[error("{MANAGED_PAIRING_CANCELLED}")]
    Cancelled,
    #[cfg(feature = "tauri-host")]
    #[error("{0}")]
    Permanent(String),
    #[error("{0}")]
    Transient(String),
}

pub(crate) async fn managed_pairing_ready(
    core: &ClientRuntime,
    node_did: &str,
    _request_id: Option<&str>,
) -> Result<bool, PairingFailure> {
    let Some(access) = core.operator_access(node_did).ok() else {
        return Ok(false);
    };
    match access
        .read_view(&gents_documents::views::config::NodeList::new(node_did))
        .await
    {
        Ok(rows) => Ok(!rows.is_empty()),
        Err(error) if gents_store::config_client::TransactionFailure::permission_denied(&error) => {
            Ok(false)
        }
        Err(error) => Err(PairingFailure::Transient(
            gents_protocol::error_chain::ErrorChain(&error).to_string(),
        )),
    }
}

pub(crate) async fn ensure_managed_runtime_pairing(
    core: Arc<ClientRuntime>,
    node_home: &std::path::Path,
    target: &ManagedPairingTarget,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(), PairingFailure> {
    let connect = async {
        let connection = core
            .refresh_local_runtime(node_home, &target.node_name)
            .await?;
        anyhow::ensure!(
            connection.node_did == target.node_did && connection.graphql == target.graphql,
            "the local runtime identity or address changed"
        );
        if managed_pairing_ready(&core, &target.node_did, None).await? {
            return Ok::<_, anyhow::Error>(());
        }
        let receipt = if connection.access_request.is_some() {
            let receipt = core.runtime_access_status(&target.node_did).await?;
            anyhow::ensure!(
                receipt.state == ClientAccessState::Pending,
                "This client's previous access is no longer usable. Request access again."
            );
            receipt
        } else {
            core.request_runtime_access(connection).await?
        };
        let owner =
            gents_service::home_state::home_graphql_endpoint(node_home, target.graphql.clone())
                .await?;
        let approved = owner
            .decide_runtime_access(&DecideClientAccess {
                node_did: target.node_did.clone(),
                request_id: receipt.request_id,
                decision: ClientAccessDecision::Approve,
            })
            .await?;
        anyhow::ensure!(
            approved.state == ClientAccessState::Approved,
            "the Node did not approve this client's access"
        );
        core.refresh_node(&target.node_did).await?;
        Ok(())
    };
    tokio::select! {
        result = connect => result.map_err(|error| PairingFailure::Transient(format!("{error:#}"))),
        () = cancel.cancelled() => Err(PairingFailure::Cancelled),
    }
}

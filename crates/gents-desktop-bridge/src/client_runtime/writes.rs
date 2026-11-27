use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigAccess;

use super::ClientRuntime;
use gents::client_operations::SubmittedRequest;
use gents_observe::client_projection::observe::ObservedStore;
use gents_protocol::runtime_connection::{ConnectionRefusalCause, RuntimeConnection};

mod config_delete;
mod config_save;
mod manual_runs;
mod operator;
mod pending_queue;
mod request_patch;
mod requests;
mod sessions;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub struct RefreshedRequestProjection {
    pub requests: Vec<AgentRequestRow>,
    pub requester_did: Option<String>,
    pub selected_request_id: Option<String>,
    pub published_version: Option<u64>,
}

/// A committed submission is acknowledged independently of replica refresh.
/// Returning a read error here would invite a second durable submission.
async fn observe_submitted_request(
    store: &ObservedStore,
    receipt: SubmittedRequest,
    refresh: impl std::future::Future<Output = Result<super::StoreRefresh>>,
) -> SubmittedRequest {
    store.set_focused_request_id(Some(receipt.request_id.clone()));
    observe_committed_write(&receipt.node_did, "submit request", refresh).await;
    receipt
}

async fn observe_committed_write(
    node_did: &str,
    action: &str,
    refresh: impl std::future::Future<Output = Result<super::StoreRefresh>>,
) {
    if let Err(error) = refresh.await {
        tracing::warn!(
            target: "gents_desktop_core::writes",
            node_did = %node_did,
            action = %action,
            error = %gents_protocol::error_chain::ErrorChain(error.as_ref()),
            "desktop write saved; its refresh failed and the observer owns the node's next read"
        );
    }
}

/// A chat write refused before any request was saved. The cause is the Lean
/// `ClientChatReadiness.Cause` the fence observed at send time.
#[derive(Debug, thiserror::Error)]
pub enum ChatRouteRefusal {
    #[error("no saved deployment route owns node {node_did}; no request was saved ({cause})")]
    NoRoute {
        node_did: String,
        cause: ConnectionRefusalCause,
    },
    #[error("the selected deployment route is not ready; no request was saved ({cause})")]
    NotReady { cause: ConnectionRefusalCause },
}

impl ChatRouteRefusal {
    pub fn cause(&self) -> ConnectionRefusalCause {
        match self {
            Self::NoRoute { cause, .. } | Self::NotReady { cause } => *cause,
        }
    }
}

pub(super) fn chat_route_refusal(
    node_did: &str,
    connection: Option<&RuntimeConnection>,
) -> Option<ChatRouteRefusal> {
    match connection {
        None => Some(ChatRouteRefusal::NoRoute {
            node_did: node_did.to_owned(),
            cause: ConnectionRefusalCause::NoRoute,
        }),
        Some(connection) => connection
            .refusal(node_did)
            .map(|cause| ChatRouteRefusal::NotReady { cause }),
    }
}

pub(super) fn ensure_runtime_connection(
    node_did: &str,
    connection: Option<&RuntimeConnection>,
) -> Result<()> {
    match chat_route_refusal(node_did, connection) {
        Some(refusal) => Err(refusal.into()),
        None => Ok(()),
    }
}

fn connection_owning_node(
    records: &[RuntimeConnection],
    node_did: &str,
) -> Option<RuntimeConnection> {
    records
        .iter()
        .find(|record| record.node_did == node_did && record.validate())
        .cloned()
}

/// Why this desktop cannot reach the managed runtime that installed an app.
/// Every variant is a property of the saved runtime record, the app's home or
/// the client key, not of the runtime's current availability: the record's
/// endpoint is written when the runtime is connected and only replaced, never
/// cleared, so retrying does not heal any of them.
#[derive(Debug, thiserror::Error)]
pub enum LocalHomeAccessError {
    #[error("the local node that installed this app is not connected")]
    NotConnected,
    #[error("the local node that installed this app is not connected locally")]
    NotLocal,
    #[error("the installed app's runtime home is unavailable")]
    RuntimeHomeUnavailable(#[source] std::io::Error),
    #[error("the installed app's home is unavailable")]
    AppHomeUnavailable(#[source] std::io::Error),
    #[error("the app's local node belongs to a different home")]
    DifferentHome,
    #[error("the local node that installed this app has no operator connection")]
    NoOperatorConnection,
    #[error("the desktop's client key cannot sign requests to its runtime")]
    Signing(#[source] gents_store::config_client::ConfigClientError),
}

fn ux_local_runtime_record<'a>(
    records: &'a [RuntimeConnection],
    node_did: &str,
    home: &Path,
) -> std::result::Result<&'a RuntimeConnection, LocalHomeAccessError> {
    let record = records
        .iter()
        .find(|record| record.node_did == node_did)
        .ok_or(LocalHomeAccessError::NotConnected)?;
    let recorded_home = record
        .local_node_home
        .as_deref()
        .ok_or(LocalHomeAccessError::NotLocal)?;
    let recorded_home = std::fs::canonicalize(recorded_home)
        .map_err(LocalHomeAccessError::RuntimeHomeUnavailable)?;
    let home = std::fs::canonicalize(home).map_err(LocalHomeAccessError::AppHomeUnavailable)?;
    if recorded_home != home {
        return Err(LocalHomeAccessError::DifferentHome);
    }
    if record.graphql.is_empty() {
        return Err(LocalHomeAccessError::NoOperatorConnection);
    }
    Ok(record)
}

impl ClientRuntime {
    pub(super) fn session_queue_access(&self, node_did: &str) -> Result<ConfigAccess> {
        self.operator_graphql(node_did)
            .map(ConfigAccess::Graphql)
            .with_context(|| format!("runtime {node_did} has no authenticated database endpoint"))
    }

    pub(super) fn connection_for_write(&self, node_did: &str) -> Result<Option<RuntimeConnection>> {
        let records = self.directory.records();
        let record = connection_owning_node(&records, node_did.trim());
        if record.is_some() {
            return Ok(record);
        }
        if let Some(stale) = records
            .iter()
            .find(|record| record.node_did == node_did.trim())
        {
            ensure_runtime_connection(node_did, Some(stale))?;
        }
        Ok(None)
    }

    pub(super) async fn request_authority(
        &self,
        node_did: &str,
        record: Option<&RuntimeConnection>,
    ) -> Result<(
        Arc<dyn gents_store::identity::NodeIdentity>,
        AgentRequestAdmissionRecord,
        String,
    )> {
        let record = record.context("chat target has no current owned peer route")?;
        anyhow::ensure!(
            record.node_did == node_did,
            "request route belongs to another node"
        );
        let signer: Arc<dyn gents_store::identity::NodeIdentity> =
            Arc::new(self.node_identity.clone());
        Ok((
            signer,
            AgentRequestAdmissionRecord::client(self.node_identity.did()),
            self.node_identity.did().to_owned(),
        ))
    }

    pub fn requester_did(&self, _record: &RuntimeConnection) -> String {
        self.node_identity.did().to_owned()
    }

    async fn observe_committed_config(&self, node_did: &str, action: &str) {
        observe_committed_write(node_did, action, self.refresh_node(node_did)).await;
    }

    pub(super) fn clear_mutation_error(&self) {
        *self
            .last_mutation_error
            .write()
            .expect("mutation error lock poisoned") = None;
    }

    fn record_mutation_error(&self, operation: &str, error: anyhow::Error) -> anyhow::Error {
        let detail = gents_protocol::error_chain::ErrorChain(error.as_ref()).to_string();
        tracing::warn!(
            target: "gents_desktop_core::writes",
            operation = %operation,
            error = %detail,
            "desktop write failed"
        );
        crate::logging::append_desktop_error(self.paths.root(), operation, &detail);
        self.record_mutation_error_message(operation, &detail);
        error
    }

    fn record_mutation_error_message(&self, operation: &str, error: &dyn std::fmt::Display) {
        let message = format!("{operation} failed: {error}");
        *self
            .last_mutation_error
            .write()
            .expect("mutation error lock poisoned") = Some(message);
    }
}

#[cfg(test)]
mod access_classification;

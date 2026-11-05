use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use gents::store::config_client::ConfigAccess;
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::row::AgentRequestRow;

use super::super::mutations::SubmittedRequest;
use super::super::observe::ObservedStore;
use super::super::peer_directory::PeerRecord;
use super::ClientCore;
#[cfg(test)]
use super::ClientPeerStatus;

mod config_delete;
mod config_save;
mod manual_runs;
mod operator;
mod pending_queue;
mod pending_submissions;
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
    refresh: impl std::future::Future<Output = Result<u64>>,
) -> SubmittedRequest {
    store.set_focused_request_id(Some(receipt.request_id.clone()));
    if let Err(error) = refresh.await {
        store.request_resync(Some(&receipt.node_did));
        tracing::warn!(
            node_did = %receipt.node_did,
            session_id = %receipt.session_id,
            request_id = %receipt.request_id,
            error = %error,
            "submitted request awaits local projection refresh"
        );
    }
    receipt
}

pub(super) fn find_request_in_scope(
    rows: &[AgentRequestRow],
    request_id: &str,
    node_did: Option<&str>,
) -> Result<Option<AgentRequestRow>> {
    let mut matches = rows.iter().filter(|row| {
        row.request_id == request_id
            && node_did.is_none_or(|did| row.node_did.as_deref() == Some(did))
    });
    let Some(row) = matches.next() else {
        return Ok(None);
    };
    anyhow::ensure!(
        matches.next().is_none(),
        "request {request_id} is ambiguous across node scopes"
    );
    Ok(Some(row.clone()))
}

fn agent_did_for_write(requested_agent_did: Option<&str>) -> Option<String> {
    requested_agent_did
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

pub(super) fn ensure_peer_chat_ready_at(
    node_did: &str,
    peer_record: Option<&PeerRecord>,
    now: DateTime<Utc>,
) -> Result<()> {
    match peer_record {
        Some(record) if record.is_chat_ready_at(now) => Ok(()),
        Some(_) => bail!(
            "the selected deployment route is not ready; no request was saved (wait for pairing repair or inspect pairing status)"
        ),
        None => bail!("no saved deployment route owns node {node_did}; no request was saved"),
    }
}

fn peer_record_owning_node_at(
    records: &[PeerRecord],
    node_did: &str,
    now: DateTime<Utc>,
) -> Option<PeerRecord> {
    records
        .iter()
        .find(|record| record.node_did == node_did)
        .filter(|record| record.is_chat_ready_at(now))
        .cloned()
}

fn ux_local_runtime_record<'a>(
    records: &'a [PeerRecord],
    node_did: &str,
    home: &Path,
) -> Result<&'a PeerRecord> {
    let record = records
        .iter()
        .find(|record| record.node_did == node_did)
        .context("the local node that installed this app is not connected")?;
    let recorded_home = record
        .local_node_home
        .as_deref()
        .context("the local node that installed this app is not connected locally")?;
    let recorded_home = std::fs::canonicalize(recorded_home)
        .context("the installed app's runtime home is unavailable")?;
    let home = std::fs::canonicalize(home).context("the installed app's home is unavailable")?;
    anyhow::ensure!(
        recorded_home == home,
        "the app's local node belongs to a different home"
    );
    anyhow::ensure!(
        record.operator_graphql().is_some(),
        "the local node that installed this app has no operator connection"
    );
    Ok(record)
}

impl ClientCore {
    pub(super) fn session_queue_access(&self, node_did: &str) -> Result<ConfigAccess> {
        self.operator_graphql(node_did)
            .map(ConfigAccess::Graphql)
            .with_context(|| format!("runtime {node_did} has no authenticated database endpoint"))
    }

    pub(super) async fn peer_record_for_chat_write(
        &self,
        node_did: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<PeerRecord>> {
        let records = self.sync_state.records();
        let record = peer_record_owning_node_at(&records, node_did.trim(), now);
        if record.is_some() {
            return Ok(record);
        }

        let stale = records
            .iter()
            .find(|record| record.node_did == node_did.trim())
            .cloned();
        if let Some(stale) = stale {
            if stale.is_enrollment() && stale.pairing_ready {
                self.sync_state
                    .set_pairing_ready(&stale, false)
                    .await
                    .context("persisting expired enrollment write fence")?;
            }
            ensure_peer_chat_ready_at(node_did, Some(&stale), now)?;
        }
        Ok(None)
    }

    pub(super) async fn request_authority(
        &self,
        node_did: &str,
        record: Option<&PeerRecord>,
    ) -> Result<(
        Arc<dyn gents::store::identity::NodeIdentity>,
        AgentRequestAdmissionRecord,
        String,
    )> {
        let record = record.context("chat target has no current owned peer route")?;
        anyhow::ensure!(
            record.node_did == node_did,
            "request route belongs to another node"
        );
        let signer: Arc<dyn gents::store::identity::NodeIdentity> =
            Arc::new(self.node_identity.clone());
        Ok((
            signer,
            AgentRequestAdmissionRecord::client(self.node_identity.did()),
            self.node_identity.did().to_owned(),
        ))
    }

    pub fn requester_did(&self, _record: &PeerRecord) -> String {
        self.node_identity.did().to_owned()
    }

    async fn refresh_config_source(&self, source_node_did: &str) -> Result<u64> {
        match self.refresh_node(source_node_did).await? {
            Some(version) => Ok(version),
            None => self.refresh_store().await,
        }
    }

    #[cfg(test)]
    pub(super) fn update_peer_status(&self, status: ClientPeerStatus) {
        if let Some(expected) = self
            .sync_state
            .records()
            .into_iter()
            .find(|record| record.peer_id == status.peer_id)
        {
            self.sync_state.replace_peer(&expected, status);
        }
    }

    pub(super) fn clear_mutation_error(&self) {
        *self
            .last_mutation_error
            .write()
            .expect("mutation error lock poisoned") = None;
    }

    fn record_mutation_error(&self, operation: &str, error: anyhow::Error) -> anyhow::Error {
        let message = format!("{operation} failed: {error}");
        *self
            .last_mutation_error
            .write()
            .expect("mutation error lock poisoned") = Some(message);
        error
    }
}

use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, RwLock as StdRwLock};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents::config_client::ConfigAccess;
use gents::document_config::{Schedule, Task};
use gents::lifecycle::TriggerLineage;
use gents::NodeIdentity;
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::request_input::RequestInput;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_protocol::session::{SessionTitle, SessionTitleSource};
use gents_protocol::session_hydration::{
    decode_manifest_json, SessionHydrationCollection, SessionHydrationDocumentKey,
    SessionHydrationReceipt, SESSION_HYDRATION_RECEIPT_VERSION,
};
use serde::Deserialize;

use super::super::mutations::{self, PeerMutationResult, SubmitRequestOptions, SubmittedRequest};
use super::super::observe::ObservedStore;
use super::super::peer_directory::PeerRecord;
use super::super::query::load_chat_patch;
use super::super::store::{ClientStore, ClientStoreRows};
use super::bootstrap::normalize_required;
use super::p2p_ops;
use super::ClientCore;
#[cfg(test)]
mod hydration_progress_tests;
mod pending_submissions;

#[cfg(test)]
use super::ClientPeerStatus;
use crate::client::peer_directory::RemovalCause;

const REQUEST_PATCH_SIGNATURE_CAPACITY: usize = 2_048;

fn request_patch_cache_reusable(
    memoized: bool,
    same_signature: bool,
    fence_accepted: bool,
    rows_current: bool,
) -> bool {
    memoized && same_signature && fence_accepted && rows_current
}

#[cfg(test)]
mod request_patch_publication_tests {
    use super::request_patch_cache_reusable;
    use crate::client::observe::ObservedStore;
    use crate::client::store::{ClientStore, ClientStoreRows};
    use gents_protocol::request_lifecycle::RequestLifecycleState;
    use gents_protocol::row::AgentRequestRow;

    fn request_row(state: &str) -> AgentRequestRow {
        AgentRequestRow {
            doc_id: Some("request-doc".to_owned()),
            request_id: "request".to_owned(),
            node_did: Some("did:test:node".to_owned()),
            purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
            lifecycle_state: Some(match state {
                "pending" => RequestLifecycleState::Pending,
                "processing" => RequestLifecycleState::Processing,
                other => panic!("unknown test state {other}"),
            }),
            ..AgentRequestRow::default()
        }
    }

    fn request_patch(row: AgentRequestRow) -> ClientStore {
        ClientStore::from_rows(ClientStoreRows {
            requests: vec![row],
            ..ClientStoreRows::default()
        })
    }

    #[test]
    fn cached_rows_must_match_physical_rows_at_the_current_fence() {
        let (store, _) = ObservedStore::new(request_patch(request_row("pending")));
        let patch = request_patch(request_row("pending"));
        let captured = store.capture_read();
        assert_eq!(
            store.current_request_patch_read_status(&captured, &patch),
            (true, true)
        );

        store.merge_chat_patch(request_patch(request_row("processing")));
        let captured = store.capture_read();
        assert_eq!(
            store.current_request_patch_read_status(&captured, &patch),
            (true, false),
            "a current fence does not make an old physical request row current"
        );
    }

    #[test]
    fn generated_publication_cases_drive_request_patch_cache_admission() {
        let snapshot: serde_json::Value =
            gents_lean_contract::load_contract_snapshot().expect("Lean contract snapshot");
        let fixture = &snapshot["client_request_patch_publication_cases"];
        for case in fixture["reuse"].as_array().expect("reuse cases") {
            assert_eq!(
                request_patch_cache_reusable(
                    case["memoized"].as_bool().expect("memoized state"),
                    case["same_signature"]
                        .as_bool()
                        .expect("signature comparison"),
                    case["fence_accepted"].as_bool().expect("publication fence"),
                    case["rows_current"]
                        .as_bool()
                        .expect("physical row comparison"),
                ),
                case["expected_reuse"].as_bool().expect("Lean result"),
                "{}",
                case["name"].as_str().expect("case name")
            );
        }
        for case in fixture["fresh_read"].as_array().expect("fresh-read cases") {
            let (store, _) = ObservedStore::new(ClientStore::from_rows(ClientStoreRows::default()));
            let captured = store.capture_read();
            let fence_accepted = case["fence_accepted"].as_bool().expect("fence expectation");
            if !fence_accepted {
                store.merge_chat_patch(request_patch(request_row("processing")));
            }
            let published = case["patch_available"]
                .as_bool()
                .expect("fresh patch availability")
                .then(|| {
                    store.merge_chat_patch_at_read(&captured, request_patch(request_row("pending")))
                })
                .flatten()
                .is_some();
            let action = match (case["patch_available"].as_bool().unwrap(), published) {
                (false, false) => "no_patch",
                (true, true) => "merge_fresh",
                (true, false) => "reject_candidate",
                (false, true) => panic!("missing patch was published"),
            };
            assert_eq!(
                action,
                case["expected_action"].as_str().expect("Lean action"),
                "{}",
                case["name"].as_str().expect("case name")
            );
        }
    }
}

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

/// A validated applied receipt is the command result. The local projection is
/// a separate replica observation; if it is unavailable, the selected-session
/// refresh and normal observer path reconcile it without inviting replay.
async fn merge_accepted_edit_projection(
    access: &ConfigAccess,
    store: &ObservedStore,
    receipt: gents_protocol::session_input_edit::SessionInputEditReceipt,
    selected_doc_ids: &[String],
    node_did: &str,
    session_id: &str,
    requester_did: &str,
) -> gents_protocol::session_input_edit::SessionInputEditReceipt {
    let result = async {
        let mut doc_ids = selected_doc_ids.to_vec();
        doc_ids.extend(receipt.request_doc_ids.iter().cloned());
        let mut unique =
            rapidhash::RapidHashSet::with_capacity_and_hasher(doc_ids.len(), Default::default());
        doc_ids.retain(|doc_id| unique.insert(doc_id.clone()));
        if doc_ids.is_empty() {
            return anyhow::Result::<()>::Ok(());
        }
        let captured = store.capture_read();
        let rows = crate::client::load_request_documents_on(access, &doc_ids).await?;
        for row in &rows {
            anyhow::ensure!(
                row.node_did.as_deref() == Some(node_did)
                    && row.session_id.as_deref() == Some(session_id)
                    && row.requester_did.as_deref() == Some(requester_did),
                "pending edit request patch crossed its node, session, or requester scope"
            );
        }
        let patch = ClientStore::from_rows(ClientStoreRows {
            requests: rows,
            ..ClientStoreRows::default()
        });
        if store.merge_chat_patch_at_read(&captured, patch).is_none() {
            store.request_resync(Some(node_did));
            anyhow::bail!("accepted edit projection read became stale before publication");
        }
        anyhow::Result::<()>::Ok(())
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(
            command_id = %receipt.command_id,
            node_did,
            session_id,
            error = %error,
            "accepted pending edit awaits local projection refresh"
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

fn request_in_scope(
    rows: &[AgentRequestRow],
    request_id: &str,
    node_did: Option<&str>,
) -> Result<AgentRequestRow> {
    find_request_in_scope(rows, request_id, node_did)?
        .with_context(|| format!("request {request_id} not found"))
}

fn required_peer_generation<'a>(name: &str, value: Option<&'a str>) -> Result<&'a str> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{name} is missing from the current peer authority"))
}

fn row_matches_source(
    sources: &[Option<String>],
    index: usize,
    source_node_did: &str,
    is_remote_source: bool,
) -> bool {
    match sources.get(index).and_then(|source| source.as_deref()) {
        Some(source) => source == source_node_did,
        None => !is_remote_source,
    }
}

fn retain_sourced_rows<T>(
    rows: &mut Vec<T>,
    sources: &mut Vec<Option<String>>,
    source_node_did: &str,
    is_remote_source: bool,
    should_delete: impl Fn(&T) -> bool,
) {
    let previous_rows = std::mem::take(rows);
    let previous_sources = std::mem::take(sources);

    for (index, row) in previous_rows.into_iter().enumerate() {
        if should_delete(&row)
            && row_matches_source(&previous_sources, index, source_node_did, is_remote_source)
        {
            continue;
        }
        rows.push(row);
        sources.push(previous_sources.get(index).cloned().unwrap_or_default());
    }
}

fn chat_patch_signature(patch: &ClientStore) -> (usize, usize, u64) {
    let rows = patch.row_count();
    match serde_json::to_vec(&patch.to_rows()) {
        Ok(bytes) => {
            let mut hasher = rapidhash::fast::RapidHasher::default();
            bytes.hash(&mut hasher);
            let mut byte_len = bytes.len();
            for row in &patch.transcript_messages {
                row.doc_id.hash(&mut hasher);
                match serde_json::to_vec(&row.message) {
                    Ok(encoded) => {
                        byte_len = byte_len.saturating_add(encoded.len());
                        encoded.hash(&mut hasher);
                    }
                    Err(_) => return (rows, 0, 0),
                }
            }
            for row in &patch.output_segments {
                row.doc_id.hash(&mut hasher);
                match serde_json::to_vec(&row.segment) {
                    Ok(encoded) => {
                        byte_len = byte_len.saturating_add(encoded.len());
                        encoded.hash(&mut hasher);
                    }
                    Err(_) => return (rows, 0, 0),
                }
            }
            (rows, byte_len, hasher.finish())
        }
        Err(_) => (rows, 0, 0),
    }
}

fn agent_id_for_write(requested_agent_id: Option<&str>) -> Option<String> {
    requested_agent_id
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
    pub async fn refresh_local_standard_peer(
        &self,
        node_home: &Path,
        label: &str,
    ) -> Result<PeerRecord> {
        let discovery = crate::local_runtime::discover_standard_runtime(node_home).await?;
        self.persist_local_standard_peer(
            label,
            &discovery.p2p_listen_address,
            &discovery.node_did,
            &discovery.graphql,
            &node_home.display().to_string(),
        )
        .await
    }

    pub async fn persist_local_standard_peer(
        &self,
        label: &str,
        addr: &str,
        node_did: &str,
        graphql: &str,
        node_home: &str,
    ) -> Result<PeerRecord> {
        self.sync_state
            .upsert_local_standard_peer(
                normalize_required("label", label)?,
                normalize_required("addr", addr)?,
                normalize_required("node_did", node_did)?,
                normalize_required("graphql", graphql)?,
                normalize_required("node_home", node_home)?,
            )
            .await
    }

    pub async fn dismiss_mailbox_item(&self, doc_id: &str) -> Result<gents::mailbox::MailboxItem> {
        let result =
            gents::mailbox::dismiss_mailbox_item(&self.node, doc_id, self.node_identity.did())
                .await;
        match result {
            Ok(item) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                Ok(item)
            }
            Err(error) => Err(self.record_mutation_error("dismiss mailbox item", error)),
        }
    }

    pub async fn submit_request(
        &self,
        session_id: &str,
        node_did: &str,
        content: &str,
        agent_id: Option<&str>,
    ) -> Result<SubmittedRequest> {
        self.submit_request_with_options(
            session_id,
            node_did,
            content,
            agent_id,
            SubmitRequestOptions::default(),
        )
        .await
    }

    pub async fn submit_request_with_options(
        &self,
        session_id: &str,
        node_did: &str,
        content: &str,
        agent_id: Option<&str>,
        mut options: SubmitRequestOptions,
    ) -> Result<SubmittedRequest> {
        if let Some(intent) = options.caller_intent.as_ref() {
            anyhow::ensure!(
                intent.node_did == node_did,
                "caller submission belongs to another node"
            );
            if let Some(receipt) = self.recover_caller_submission(intent).await? {
                return Ok(receipt);
            }
        }
        let snapshot = self.store.snapshot();
        let peer_record = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer_record.as_ref(), Utc::now())?;
        let (signer, admission, requester_did) = self
            .request_authority(node_did, peer_record.as_ref())
            .await?;
        self.ensure_sender_owns_session(node_did, session_id, &requester_did)?;
        // Queue preparation captures assignment intent before signing; a later
        // replacement must not change an already prepared unassigned message.
        if options.input.queue.is_none() {
            options.input = self
                .prepare_goal_input(node_did, session_id, options.input)
                .await?;
        }
        crate::local_runtime::ensure_request_input_supported(peer_record.as_ref(), &options.input)
            .await?;
        let agent_id = agent_id_for_write(agent_id);
        match mutations::submit_request(
            &self.node,
            snapshot.as_ref(),
            &self.session_queue_access(node_did),
            session_id,
            node_did,
            &requester_did,
            signer.as_ref(),
            admission,
            content,
            agent_id.as_deref(),
            options,
        )
        .await
        {
            Ok(result) => {
                let result =
                    observe_submitted_request(&self.store, result, self.refresh_store()).await;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    action = "chat_submit",
                    row_id = %result.request_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.submit_failure(error)),
        }
    }

    /// Refuse a send into a session another requester owns before writing it.
    /// Core refuses such a request at claim time, and a refused request is
    /// never observed under the session's owner, so the send would otherwise
    /// wait for an observation that doesn't come. A session's owner is written
    /// once, so the snapshot's row is current; a session the snapshot doesn't
    /// hold is left to core's admission.
    fn ensure_sender_owns_session(
        &self,
        node_did: &str,
        session_id: &str,
        sender_did: &str,
    ) -> Result<()> {
        let store = self.store.snapshot();
        let Some(session) = store
            .sessions
            .iter()
            .find(|row| row.session_id == session_id && row.node_did == node_did)
        else {
            return Ok(());
        };
        match super::super::query::session_unsendable_reason(session, sender_did) {
            Some(reason) => Err(crate::error::BridgeError::new(
                crate::error::BridgeErrorCode::SessionNotOwned,
                reason,
            )
            .into()),
            None => Ok(()),
        }
    }

    pub(super) fn session_queue_access(&self, node_did: &str) -> ConfigAccess {
        self.operator_graphql(node_did)
            .map(ConfigAccess::Graphql)
            .unwrap_or_else(|| ConfigAccess::local(self.node_arc()))
    }

    pub async fn prepare_user_message_input(
        &self,
        node_did: &str,
        session_id: &str,
        input: gents_protocol::request_input::RequestInput,
    ) -> Result<gents_protocol::request_input::RequestInput> {
        self.prepare_user_message_input_with_delivery(
            node_did,
            session_id,
            input,
            gents_protocol::request_input::QueueDelivery::Steer,
        )
        .await
    }

    pub async fn prepare_user_message_input_with_delivery(
        &self,
        node_did: &str,
        session_id: &str,
        input: gents_protocol::request_input::RequestInput,
        delivery: gents_protocol::request_input::QueueDelivery,
    ) -> Result<gents_protocol::request_input::RequestInput> {
        let input = self.prepare_goal_input(node_did, session_id, input).await?;
        gents::lifecycle::prepare_user_message_input(
            &self
                .operator_observation_graphql(node_did)
                .map(ConfigAccess::Graphql)
                .unwrap_or_else(|| ConfigAccess::local(self.node_arc())),
            node_did,
            session_id,
            input,
            delivery,
        )
        .await
    }

    pub async fn pending_user_queue(
        &self,
        node_did: &str,
        session_id: &str,
    ) -> Result<gents::lifecycle::PendingQueueSnapshot> {
        Ok(self
            .pending_user_queue_projection(node_did, session_id, &[])
            .await?
            .pending)
    }

    pub async fn pending_user_queue_projection(
        &self,
        node_did: &str,
        session_id: &str,
        observed_doc_ids: &[String],
    ) -> Result<gents::lifecycle::PendingQueueProjection> {
        let record = self
            .sync_state
            .records()
            .into_iter()
            .find(|record| record.node_did == node_did)
            .context("no saved deployment route owns this node")?;
        gents::lifecycle::pending_user_queue_projection(
            &self
                .operator_observation_graphql(node_did)
                .map(ConfigAccess::Graphql)
                .unwrap_or_else(|| ConfigAccess::local(self.node_arc())),
            node_did,
            session_id,
            &self.requester_did(&record),
            observed_doc_ids,
        )
        .await
    }

    pub async fn edit_pending_queue(
        &self,
        node_did: &str,
        session_id: &str,
        edit: gents::lifecycle::PendingQueueEdit,
    ) -> Result<gents_protocol::session_input_edit::SessionInputEditReceipt> {
        self.apply_pending_queue_edit(node_did, session_id, edit, None, None)
            .await
    }

    pub async fn steer_pending_message(
        &self,
        node_did: &str,
        session_id: &str,
        expected_request_doc_ids: Vec<String>,
        request_doc_id: &str,
        request_id: Option<&str>,
    ) -> Result<gents_protocol::session_input_edit::SessionInputEditReceipt> {
        let snapshot = self.pending_user_queue(node_did, session_id).await?;
        let edit = gents::lifecycle::pending_steer_edit(
            &snapshot,
            expected_request_doc_ids,
            request_doc_id,
        )?;
        self.apply_pending_queue_edit(node_did, session_id, edit, Some(request_doc_id), request_id)
            .await
    }

    async fn apply_pending_queue_edit(
        &self,
        node_did: &str,
        session_id: &str,
        edit: gents::lifecycle::PendingQueueEdit,
        steer: Option<&str>,
        request_id: Option<&str>,
    ) -> Result<gents_protocol::session_input_edit::SessionInputEditReceipt> {
        let peer = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer.as_ref(), Utc::now())?;
        let (signer, admission, _) = self.request_authority(node_did, peer.as_ref()).await?;
        let selected_doc_ids = edit.selected_request_doc_ids.clone();
        let access = self.session_queue_access(node_did);
        let result = match steer {
            Some(request_doc_id) => {
                gents::lifecycle::submit_pending_user_steer(
                    &access,
                    signer.as_ref(),
                    admission,
                    Some(self.local_peer_id()),
                    node_did,
                    session_id,
                    edit,
                    request_doc_id,
                    request_id,
                )
                .await
            }
            None => {
                gents::lifecycle::submit_pending_user_edit(
                    &access,
                    signer.as_ref(),
                    admission,
                    Some(self.local_peer_id()),
                    node_did,
                    session_id,
                    edit,
                )
                .await
            }
        };
        match result {
            Ok(receipt) => {
                let requester_did = peer
                    .as_ref()
                    .map(|record| self.requester_did(record))
                    .unwrap_or_else(|| self.node_identity.did().to_string());
                let receipt = merge_accepted_edit_projection(
                    &self.session_queue_access(node_did),
                    self.store.as_ref(),
                    receipt,
                    &selected_doc_ids,
                    node_did,
                    session_id,
                    &requester_did,
                )
                .await;
                self.clear_mutation_error();
                Ok(receipt)
            }
            Err(error) => Err(self.record_mutation_error("edit pending messages", error)),
        }
    }

    pub async fn request_output(
        &self,
        node_did: &str,
        session_id: &str,
        request_id: &str,
    ) -> Result<Option<gents::session::CanonicalRequestOutput>> {
        let node_did = normalize_required("node_did", node_did)?;
        let session_id = normalize_required("session_id", session_id)?;
        let request_id = normalize_required("request_id", request_id)?;
        let Some(projection) = self
            .refresh_local_request(node_did, session_id, Some(request_id), None, &[])
            .await?
        else {
            return Ok(None);
        };
        let Some(row) = find_request_in_scope(&projection.requests, request_id, Some(node_did))?
        else {
            return Ok(None);
        };
        anyhow::ensure!(
            row.session_id.as_deref() == Some(session_id),
            "request output crossed the selected session scope"
        );
        let refs = projection.requests.iter().collect::<Vec<_>>();
        let answer = super::super::store::request_answer_owner(&refs, &row);
        anyhow::ensure!(
            answer.node_did.as_deref() == Some(node_did)
                && answer.session_id.as_deref() == Some(session_id)
                && answer.requester_did == row.requester_did,
            "request answer crossed its admitted scope"
        );
        if !answer.is_terminal() || gents::lifecycle::folded_into(answer).is_some() {
            return Ok(None);
        }
        let access = match self.operator_observation_graphql(node_did) {
            Some(graphql) => ConfigAccess::Graphql(graphql),
            None => self
                .store_access
                .lock()
                .await
                .as_ref()
                .context("local store access is unavailable")?
                .clone(),
        };
        gents::session::observe_request_output(&access, answer)
            .await
            .map(Some)
    }

    pub async fn request_timeline(
        &self,
        node_did: &str,
        request_id: &str,
    ) -> Result<gents::run_timeline::RunTimeline> {
        normalize_required("node_did", node_did)?;
        let request_id = normalize_required("request_id", request_id)?;
        let access = gents::config_client::ConfigAccess::local(self.node_arc());
        let timeline = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            gents::run_timeline_fetch::load_run_timeline(&access, request_id),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out loading timeline for {request_id}"))?
        .map_err(|error| anyhow::anyhow!("{}", strip_cli_operator_hints(&error.to_string())))?;
        Ok(timeline)
    }

    pub async fn network_status(&self) -> NetworkStatus {
        let local_peer_id = p2p_ops::p2p_local_peer_id(&self.p2p).await;
        let listen_addresses = p2p_ops::p2p_listen_addresses(&self.p2p).await;
        let connected_peers = p2p_ops::p2p_connected_peers(&self.p2p).await;
        let replicators = p2p_ops::p2p_get_replicators(&self.p2p).await;
        let saved_peers = self.sync_state.records();

        NetworkStatus {
            local_peer_id: local_peer_id.map_err(|error| error.to_string()),
            listen_addresses: listen_addresses.map_err(|error| error.to_string()),
            connected_peers: connected_peers.map_err(|error| error.to_string()),
            replicators: replicators
                .map(|rows| {
                    rows.into_iter()
                        .map(|info| NetworkReplicator {
                            peer_id: info.id,
                            address: info.address,
                            collections: info.collections,
                            status: info.status,
                            last_status_change: info.last_status_change,
                        })
                        .collect()
                })
                .map_err(|error| error.to_string()),
            saved_peers,
        }
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
        Arc<dyn gents::identity::NodeIdentity>,
        AgentRequestAdmissionRecord,
        String,
    )> {
        let record = record.context("chat target has no current owned peer route")?;
        if record.is_enrollment() {
            let request_id = required_peer_generation(
                "enrollment_request_id",
                record.enrollment_request_id.as_deref(),
            )?;
            let digest = required_peer_generation(
                "enrollment_request_digest",
                record.enrollment_request_digest.as_deref(),
            )?;
            let admin_did = required_peer_generation(
                "enrollment_admin_did",
                record.enrollment_admin_did.as_deref(),
            )?;
            let sequence = record
                .enrollment_authorization_sequence
                .filter(|value| *value > 0)
                .context("enrollment authorization sequence is missing")?;
            let expires_at = required_peer_generation(
                "enrollment_authorization_expires_at",
                record.enrollment_authorization_expires_at.as_deref(),
            )?;
            let signer: Arc<dyn gents::identity::NodeIdentity> =
                Arc::new(self.node_identity.clone());
            let admission = AgentRequestAdmissionRecord::enrollment(
                self.node_identity.did(),
                request_id,
                digest,
                admin_did,
                sequence,
                expires_at,
            );
            return Ok((signer, admission, self.requester_did(record)));
        }
        anyhow::ensure!(
            record.source.as_deref() == Some("local-standard") && record.node_did == node_did,
            "chat target is not owned by enrollment or a local standard runtime"
        );
        let signer: Arc<dyn gents::identity::NodeIdentity> =
            match gents::identity::RegisteredIdentity::from_registered_did(node_did, None) {
                Ok(identity) => Arc::new(identity),
                Err(_) => {
                    let home = required_peer_generation(
                        "local_node_home",
                        record.local_node_home.as_deref(),
                    )?;
                    crate::local_runtime::load_standard_runtime_identity(Path::new(home))?
                }
            };
        anyhow::ensure!(
            signer.did() == node_did,
            "local request signer does not own target node"
        );
        Ok((
            signer,
            AgentRequestAdmissionRecord::local_self(node_did),
            self.requester_did(record),
        ))
    }

    /// The DID this client sends as on `record`'s node.
    pub fn requester_did(&self, record: &PeerRecord) -> String {
        if record.is_enrollment() {
            self.node_identity.did().to_string()
        } else {
            record.node_did.clone()
        }
    }

    /// Terminal reads refresh operator configuration omitted by the client P2P route.
    /// Catalog replacement must precede the final request publication to retain page rows.
    pub async fn refresh_local_request(
        &self,
        node_did: &str,
        session_id: &str,
        request_id: Option<&str>,
        fallback_request_id: Option<&str>,
        page_request_doc_ids: &[String],
    ) -> Result<Option<RefreshedRequestProjection>> {
        let node_did = node_did.trim();
        let session_id = session_id.trim();
        let request_id = request_id.map(str::trim).filter(|id| !id.is_empty());
        let fallback_request_id = fallback_request_id
            .map(str::trim)
            .filter(|id| !id.is_empty());
        if node_did.is_empty() || session_id.is_empty() {
            return Ok(None);
        }

        let mut captured = self.store.capture_read();
        let (access, source) = match self.operator_observation_graphql(node_did) {
            Some(graphql) => (ConfigAccess::Graphql(graphql), "operator"),
            None => (
                self.store_access
                    .lock()
                    .await
                    .as_ref()
                    .context("local store access is unavailable")?
                    .clone(),
                "local",
            ),
        };
        let mut patch = load_chat_patch(
            &access,
            node_did,
            session_id,
            request_id,
            fallback_request_id,
            page_request_doc_ids,
        )
        .await?;
        let rows = patch.row_count();
        if rows == 0 {
            return Ok(None);
        }
        let mut signature = chat_patch_signature(&patch);
        let project = |patch: &ClientStore| {
            let requester_did = patch
                .sessions
                .first()
                .and_then(|session| session.requester_did.clone());
            let latest_request_id = patch
                .sessions
                .first()
                .and_then(|session| session.observation.as_ref())
                .and_then(|observation| observation.latest_request.as_ref())
                .map(|latest| latest.request_id.clone());
            let legacy_fallback = patch
                .sessions
                .first()
                .filter(|session| session.observation.is_none())
                .and_then(|_| fallback_request_id.map(str::to_owned));
            RefreshedRequestProjection {
                requests: patch.requests.clone(),
                requester_did,
                selected_request_id: request_id
                    .map(str::to_owned)
                    .or(latest_request_id)
                    .or(legacy_fallback),
                published_version: None,
            }
        };
        let cache_key_for = |projection: &RefreshedRequestProjection| {
            let requester = projection.requester_did.as_deref().unwrap_or("<null>");
            let request = projection
                .selected_request_id
                .as_deref()
                .unwrap_or("<none>");
            format!("{source}\0{node_did}\0{session_id}\0{requester}\0{request}")
        };
        let mut projection = project(&patch);
        let mut cache_key = cache_key_for(&projection);
        {
            let mut signatures = self.request_patch_signatures.lock().await;
            let cached_signature = signatures.get(&cache_key) == Some(&signature);
            if cached_signature {
                let (fence_accepted, rows_current) = self
                    .store
                    .current_request_patch_read_status(&captured, &patch);
                if request_patch_cache_reusable(
                    cached_signature,
                    true,
                    fence_accepted,
                    rows_current,
                ) {
                    return Ok(Some(projection));
                }
                signatures.remove(&cache_key);
                if !fence_accepted {
                    self.store.request_resync(Some(node_did));
                    return Ok(None);
                }
            }
        }

        let terminal = projection
            .selected_request_id
            .as_deref()
            .and_then(|request_id| patch.request_row(request_id))
            .is_some_and(|row| row.is_terminal());
        if terminal {
            self.refresh_node(node_did).await?;
            captured = self.store.capture_read();
            patch = load_chat_patch(
                &access,
                node_did,
                session_id,
                request_id,
                fallback_request_id,
                page_request_doc_ids,
            )
            .await?;
            if patch.row_count() == 0 {
                return Ok(None);
            }
            projection = project(&patch);
            cache_key = cache_key_for(&projection);
            signature = chat_patch_signature(&patch);
        }
        let (rows, bytes, _hash) = signature;
        let Some(version) = self.store.merge_chat_patch_at_read(&captured, patch) else {
            self.store.request_resync(Some(node_did));
            return Ok(None);
        };
        let mut signatures = self.request_patch_signatures.lock().await;
        if signatures.len() >= REQUEST_PATCH_SIGNATURE_CAPACITY {
            signatures.clear();
        }
        signatures.insert(cache_key, signature);
        drop(signatures);
        tracing::debug!(
            target: "gents_desktop_core::replication",
            request_id = projection.selected_request_id.as_deref().unwrap_or_default(),
            node_did,
            version,
            rows,
            bytes,
            terminal,
            source,
            "desktop selected request patch merged"
        );
        Ok(Some(RefreshedRequestProjection {
            published_version: Some(version),
            ..projection
        }))
    }

    /// A client replica cannot establish an idle source or complete history;
    /// editing requires the runtime's authenticated transaction endpoint.
    pub async fn fork_session_before_message(
        &self,
        node_did: &str,
        session_id: &str,
        message_key: &str,
        client_did: &str,
        child_session_id: &str,
    ) -> Result<String> {
        anyhow::ensure!(
            client_did == self.node_identity.did(),
            "the saved edit belongs to another native client"
        );
        let node_did = normalize_required("node_did", node_did)?;
        let session_id = normalize_required("session_id", session_id)?;
        anyhow::ensure!(
            child_session_id == child_session_id.trim(),
            "child_session_id must retain its exact canonical identity"
        );
        let child_session_id = normalize_required("child_session_id", child_session_id)?;
        anyhow::ensure!(!message_key.trim().is_empty(), "message_key is required");
        let peer = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer.as_ref(), Utc::now())?;
        let requester = self.requester_did(
            peer.as_ref()
                .context("chat target has no current owned peer route")?,
        );
        let access = self.operator_graphql(node_did)
            .map(ConfigAccess::Graphql)
            .context("Editing a message requires a direct runtime connection. Reconnect the local runtime and try again.")?;
        gents::session::fork_edit_on(
            &access,
            gents::session::ForkParams {
                source_session_id: session_id,
                boundary: gents::session::ForkBoundary::BeforeMessageKey(message_key),
                caller_node_did: node_did,
                caller_requester_did: Some(&requester),
                target_agent_id: None,
            },
            child_session_id,
            client_did,
        )
        .await
        .map_err(Into::into)
    }

    pub async fn set_session_archived(
        &self,
        node_did: &str,
        session_id: &str,
        expected_revision: u64,
        archived: bool,
    ) -> Result<gents_protocol::session::SessionArchiveReceipt> {
        let access = self.operator_access(node_did)?;
        let result = gents::session::set_session_archived_on(
            &access,
            node_did,
            session_id,
            &self.node_identity,
            expected_revision,
            archived,
        )
        .await;
        match result {
            Ok(receipt) => {
                if self.refresh_node(node_did).await?.is_none() {
                    self.refresh_store().await?;
                }
                self.clear_mutation_error();
                Ok(receipt)
            }
            Err(error) => Err(self.record_mutation_error(
                if archived {
                    "archive session"
                } else {
                    "restore session"
                },
                error,
            )),
        }
    }

    pub async fn rename_session(
        &self,
        node_did: &str,
        session_id: &str,
        title: &str,
    ) -> Result<()> {
        let snapshot = self.store.snapshot();
        let result = mutations::rename_session(
            &self.node,
            snapshot.as_ref(),
            node_did,
            self.node_identity.did(),
            session_id,
            title,
        )
        .await;
        match result {
            Ok(()) => {
                if self.refresh_node(node_did).await?.is_none() {
                    self.refresh_store().await?;
                }
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    action = "chat_rename",
                    row_id = %session_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("rename session", error)),
        }
    }

    pub async fn delete_skill(&self, skill_id: &str, source_node_did: &str) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted = mutations::delete_skill_on(&access, source_node_did, skill_id).await?;
            if deleted == 0 {
                bail!("no Skill document with skill_id {skill_id:?} for {source_node_did}");
            }
            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                let refresh_result = self.refresh_config_source(source_node_did).await;
                complete_confirmed_delete(
                    self.store.as_ref(),
                    &self.last_mutation_error,
                    refresh_result,
                    "delete skill",
                    "config_skill_delete",
                    skill_id,
                    |rows| prune_deleted_skill_rows(rows, source_node_did, skill_id),
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("delete skill", error)),
        }
    }

    pub async fn delete_task(&self, task_id: &str, source_node_did: &str) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted = mutations::delete_task_on(&access, source_node_did, task_id).await?;
            if deleted == 0 {
                bail!("no Task document with task_id {task_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete task",
            "config_task_delete",
            task_id,
            source_node_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.tasks,
                    &mut rows.task_source_node_dids,
                    source_node_did,
                    false,
                    |row| row.task_id == task_id,
                );
            },
        )
        .await
    }

    pub async fn delete_schedule(&self, schedule_id: &str, source_node_did: &str) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted =
                mutations::delete_schedule_on(&access, source_node_did, schedule_id).await?;
            if deleted == 0 {
                bail!("no Schedule document with schedule_id {schedule_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete schedule",
            "config_schedule_delete",
            schedule_id,
            source_node_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.schedules,
                    &mut rows.schedule_source_node_dids,
                    source_node_did,
                    false,
                    |row| row.schedule_id == schedule_id,
                );
            },
        )
        .await
    }

    pub async fn delete_trigger(&self, trigger_id: &str, source_node_did: &str) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted =
                mutations::delete_trigger_on(&access, source_node_did, trigger_id).await?;
            if deleted == 0 {
                bail!("no Trigger document with trigger_id {trigger_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete trigger",
            "config_trigger_delete",
            trigger_id,
            source_node_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.triggers,
                    &mut rows.trigger_source_node_dids,
                    source_node_did,
                    false,
                    |row| row.trigger_id == trigger_id,
                );
            },
        )
        .await
    }

    pub async fn delete_inference_backend(
        &self,
        backend_id: &str,
        source_node_did: &str,
    ) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted =
                mutations::delete_inference_backend_on(&access, source_node_did, backend_id)
                    .await?;
            if deleted == 0 {
                bail!("no InferenceBackend document with backend_id {backend_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete inference backend",
            "config_backend_delete",
            backend_id,
            source_node_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.inference_backends,
                    &mut rows.inference_backend_source_node_dids,
                    source_node_did,
                    false,
                    |row| row.backend_id == backend_id,
                );
            },
        )
        .await
    }

    pub async fn delete_inference_profile(
        &self,
        profile_id: &str,
        source_node_did: &str,
    ) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted =
                mutations::delete_inference_profile_on(&access, source_node_did, profile_id)
                    .await?;
            if deleted == 0 {
                bail!("no InferenceProfile document with profile_id {profile_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete inference profile",
            "config_profile_delete",
            profile_id,
            source_node_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.inference_profiles,
                    &mut rows.inference_profile_source_node_dids,
                    source_node_did,
                    false,
                    |row| row.profile_id == profile_id,
                );
            },
        )
        .await
    }

    pub async fn delete_tools(&self, tools_id: &str, source_node_did: &str) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted = mutations::delete_tools_on(&access, source_node_did, tools_id).await?;
            if deleted == 0 {
                bail!("no Tools document with tools_id {tools_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete tools",
            "config_tools_delete",
            tools_id,
            source_node_did,
            |rows| {
                rows.tools
                    .retain(|row| row.tools_id != tools_id || row.node_did != source_node_did);
            },
        )
        .await
    }

    pub async fn delete_tool_service(&self, service_id: &str, source_node_did: &str) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted =
                mutations::delete_tool_service_registry_on(&access, source_node_did, service_id)
                    .await?;
            if deleted == 0 {
                bail!("no ToolServiceRegistry document with service_id {service_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete tool service",
            "config_tool_service_delete",
            service_id,
            source_node_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.tool_service_registries,
                    &mut rows.tool_service_registry_source_node_dids,
                    source_node_did,
                    false,
                    |row| row.service_id == service_id,
                );
            },
        )
        .await
    }

    pub async fn delete_agent(&self, agent_id: &str, source_node_did: &str) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted = mutations::delete_agent_on(&access, source_node_did, agent_id).await?;
            if deleted == 0 {
                bail!("no Agent document with agent_id {agent_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete agent",
            "config_agent_delete",
            agent_id,
            source_node_did,
            |rows| {
                rows.agents
                    .retain(|row| row.agent_id != agent_id || row.node_did != source_node_did);
            },
        )
        .await
    }

    pub async fn delete_context(&self, context_id: &str, source_node_did: &str) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted =
                mutations::delete_agent_context_on(&access, source_node_did, context_id).await?;
            if deleted == 0 {
                bail!("no AgentContext document with context_id {context_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete context",
            "config_context_delete",
            context_id,
            source_node_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.contexts,
                    &mut rows.context_source_node_dids,
                    source_node_did,
                    false,
                    |row| row.context_id == context_id,
                );
            },
        )
        .await
    }

    async fn finish_automation_delete(
        &self,
        result: Result<()>,
        action_label: &str,
        action: &str,
        row_id: &str,
        source_node_did: &str,
        prune: impl FnOnce(&mut ClientStoreRows),
    ) -> Result<()> {
        match result {
            Ok(()) => {
                let refresh_result = self.refresh_config_source(source_node_did).await;
                complete_confirmed_delete(
                    self.store.as_ref(),
                    &self.last_mutation_error,
                    refresh_result,
                    action_label,
                    action,
                    row_id,
                    prune,
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error(action_label, error)),
        }
    }

    async fn refresh_config_source(&self, source_node_did: &str) -> Result<u64> {
        match self.refresh_node(source_node_did).await? {
            Some(version) => Ok(version),
            None => self.refresh_store().await,
        }
    }

    pub async fn resend_request(&self, stale_request_id: &str) -> Result<SubmittedRequest> {
        let selected_node_did = self.selected_node_did();
        self.resend_request_in_scope(stale_request_id, selected_node_did.as_deref())
            .await
    }

    pub fn request_in_scope(
        &self,
        request_id: &str,
        node_did: Option<&str>,
    ) -> Result<AgentRequestRow> {
        request_in_scope(&self.store.snapshot().requests, request_id, node_did)
    }

    pub async fn resend_request_in_scope(
        &self,
        stale_request_id: &str,
        node_did: Option<&str>,
    ) -> Result<SubmittedRequest> {
        let snapshot = self.store.snapshot();
        let stale = request_in_scope(&snapshot.requests, stale_request_id, node_did)?;
        let node_did = stale
            .node_did
            .as_deref()
            .context("stale request has no node_did")?;
        let peer_record = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer_record.as_ref(), Utc::now())?;
        let (signer, admission, requester_did) = self
            .request_authority(node_did, peer_record.as_ref())
            .await?;
        let result = mutations::resend_request(
            &self.node,
            snapshot.as_ref(),
            &self.session_queue_access(node_did),
            stale_request_id,
            node_did,
            &requester_did,
            signer.as_ref(),
            admission,
        )
        .await;
        match result {
            Ok(result) => {
                let result =
                    observe_submitted_request(&self.store, result, self.refresh_store()).await;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    action = "chat_resend",
                    row_id = %result.request_id,
                    stale_request_id = %stale_request_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error("resend request", error)),
        }
    }

    pub async fn interrupt_request(&self, request_id: &str) -> Result<()> {
        let snapshot = self.store.snapshot();
        let selected_node_did = self.selected_node_did();
        let mut requests = snapshot
            .requests
            .iter()
            .filter(|request| request.request_id == request_id)
            .filter(|request| {
                selected_node_did
                    .as_deref()
                    .is_none_or(|selected| request.node_did.as_deref() == Some(selected))
            });
        let request = requests
            .next()
            .with_context(|| format!("request {request_id} is absent from the selected node"))?;
        anyhow::ensure!(
            requests.next().is_none(),
            "request {request_id} is ambiguous across the selected node scope"
        );
        let node_did = request
            .node_did
            .as_deref()
            .context("request has no node_did")?;
        let peer_record = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer_record.as_ref(), Utc::now())?;
        match mutations::interrupt_request(&self.node, request_id).await {
            Ok(()) => {
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    action = "chat_interrupt",
                    row_id = %request_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("interrupt request", error)),
        }
    }

    pub async fn retry_request(&self, parent: &AgentRequestRow) -> Result<SubmittedRequest> {
        let snapshot = self.store.snapshot();
        let node_did = parent
            .node_did
            .as_deref()
            .context("retry parent has no node_did")?;
        let peer_record = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer_record.as_ref(), Utc::now())?;
        crate::local_runtime::ensure_request_input_supported(
            peer_record.as_ref(),
            &parent.input.clone().unwrap_or_default(),
        )
        .await?;
        let (signer, admission, requester_did) = self
            .request_authority(node_did, peer_record.as_ref())
            .await?;
        let result = mutations::retry_request(
            &self.node,
            snapshot.as_ref(),
            parent,
            &requester_did,
            signer.as_ref(),
            admission,
        )
        .await;
        match result {
            Ok(result) => {
                let result =
                    observe_submitted_request(&self.store, result, self.refresh_store()).await;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    action = "chat_retry",
                    row_id = %result.request_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error("retry request", error)),
        }
    }

    pub async fn rename_peer(&self, peer_id: &str, label: &str) -> Result<()> {
        let peer_id = normalize_required("peer_id", peer_id)?;
        let label = normalize_required("label", label)?;
        let record = self
            .sync_state
            .records()
            .iter()
            .find(|record| record.peer_id == peer_id)
            .cloned()
            .with_context(|| format!("peer {peer_id} not found"))?;
        let mut renamed = record.clone();
        renamed.label = label.to_string();
        self.sync_state
            .replace_record(&record, renamed)
            .await?
            .with_context(|| format!("peer {peer_id} changed while it was being renamed"))?;
        self.clear_mutation_error();
        Ok(())
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

    pub async fn remove_peer(&self, peer_id: &str) -> Result<PeerMutationResult> {
        let peer_id = normalize_required("peer_id", peer_id)?;
        let removal = self
            .route_manager
            .remove_peer(&self.sync_state, peer_id, RemovalCause::Operator)
            .await?;
        let removed = removal.record;
        let warning = match removal.cleanup_error {
            None => None,
            Some(error) => {
                let warning = format!(
                    "deployment hidden locally; route teardown is pending and will retry: {error}"
                );
                tracing::warn!(
                    target: "gents_desktop_core::peer",
                    peer_id = %removed.peer_id,
                    label = %removed.label,
                    error = %error,
                    "desktop deployment removal queued for retry"
                );
                Some(warning)
            }
        };

        self.clear_mutation_error();
        tracing::info!(
            target: "gents_desktop_core::peer",
            peer_id = %removed.peer_id,
            label = %removed.label,
            "desktop deployment removed"
        );
        Ok(PeerMutationResult {
            peer_id: removed.peer_id,
            label: removed.label,
            addr: removed.addr,
            connected: false,
            warning,
        })
    }

    pub fn operator_graphql(
        &self,
        node_did: &str,
    ) -> Option<gents::config_client::GraphqlEndpoint> {
        self.sync_state
            .records()
            .iter()
            .find(|record| record.node_did == node_did)
            .and_then(crate::local_runtime::operator_endpoint)
    }

    pub fn operator_observation_graphql(
        &self,
        node_did: &str,
    ) -> Option<gents::config_client::GraphqlEndpoint> {
        self.sync_state
            .records()
            .iter()
            .find(|record| record.node_did == node_did)
            .and_then(|record| self.options.operator_identity_interaction.endpoint(record))
    }

    pub fn operator_signer(
        &self,
        node_did: &str,
    ) -> Result<std::sync::Arc<dyn gents::identity::NodeIdentity>> {
        let record = self
            .sync_state
            .records()
            .into_iter()
            .find(|record| record.node_did == node_did)
            .context("no runtime record for this node")?;
        crate::local_runtime::operator_signer(&record)
    }

    pub fn operator_access(&self, node_did: &str) -> Result<ConfigAccess> {
        let record = self
            .sync_state
            .records()
            .into_iter()
            .find(|record| record.node_did == node_did);
        match record {
            Some(record) if record.operator_graphql().is_some() => {
                crate::local_runtime::operator_endpoint(&record)
                    .map(ConfigAccess::Graphql)
                    .context("local standard runtime has no operator GraphQL endpoint")
            }
            Some(_) => anyhow::bail!(
                "managed node {node_did} has no operator GraphQL endpoint; refusing a desktop-replica configuration fallback"
            ),
            None => Ok(ConfigAccess::local(self.node_arc())),
        }
    }

    pub fn local_home_operator_access(
        &self,
        node_did: &str,
        home: &std::path::Path,
    ) -> Result<ConfigAccess> {
        let records = self.sync_state.records();
        let record = ux_local_runtime_record(&records, node_did, home)?;
        crate::local_runtime::operator_endpoint(record)
            .map(ConfigAccess::Graphql)
            .context("the local node that installed this app has no operator connection")
    }

    pub async fn save_agent(&self, row: &gents::AgentDocument) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        let result = mutations::upsert_agent_on(&access, row).await;
        match result {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "agent",
                    row_id = %row.agent_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save agent", error)),
        }
    }

    pub async fn patch_config_components(
        &self,
        node_did: &str,
        patches: &[(
            gents::config_client::patch::SelfConfigTarget,
            String,
            gents::config_client::patch::SelfConfigPatch,
        )],
    ) -> Result<()> {
        let access = self.operator_access(node_did)?;
        match mutations::patch_config_components_on(&access, node_did, patches).await {
            Ok(()) => {
                self.refresh_config_source(node_did).await?;
                self.clear_mutation_error();
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("patch config components", error)),
        }
    }

    pub async fn read_config_editor(
        &self,
        node_did: &str,
        target: gents::config_client::patch::SelfConfigTarget,
        id: &str,
    ) -> Result<mutations::ConfigEditorDocument> {
        let access = self
            .operator_graphql(node_did)
            .map(ConfigAccess::Graphql)
            .context("Connect this local runtime before editing its configuration.")?;
        mutations::read_config_editor_on(&access, node_did, target, id).await
    }

    pub async fn read_backend_authentication(
        &self,
        node_did: &str,
        id: &str,
    ) -> Result<(mutations::ConfigEditorRevision, gents::InferenceBackend)> {
        let access = self
            .operator_graphql(node_did)
            .map(ConfigAccess::Graphql)
            .context("Connect this local runtime before changing authentication.")?;
        mutations::read_backend_authentication_on(&access, node_did, id).await
    }

    pub async fn save_backend_authentication(
        &self,
        node_did: &str,
        id: &str,
        revision: &mutations::ConfigEditorRevision,
        auth: &gents::document_config::BackendAuth,
    ) -> Result<()> {
        let access = self
            .operator_graphql(node_did)
            .map(ConfigAccess::Graphql)
            .context("Connect this local runtime before changing authentication.")?;
        mutations::save_backend_authentication_on(&access, node_did, id, revision, auth).await?;
        if self.refresh_config_source(node_did).await.is_err() {
            tracing::warn!("Authentication saved; client observation will resynchronize");
            self.store.request_resync(Some(node_did));
        }
        Ok(())
    }

    pub async fn save_config_editor(
        &self,
        node_did: &str,
        target: gents::config_client::patch::SelfConfigTarget,
        id: &str,
        revision: &mutations::ConfigEditorRevision,
        content: &str,
    ) -> Result<Option<mutations::ConfigEditorDocument>> {
        let access = self
            .operator_graphql(node_did)
            .map(ConfigAccess::Graphql)
            .context("Connect this local runtime before editing its configuration.")?;
        let saved =
            mutations::save_config_editor_on(&access, node_did, target, id, revision, content)
                .await?;
        if self.refresh_config_source(node_did).await.is_err() {
            tracing::warn!("Configuration saved; client observation will resynchronize");
            self.store.request_resync(Some(node_did));
        }
        Ok(saved)
    }

    pub async fn apply_config_components(
        &self,
        document: &gents::document_config::PackConfig,
    ) -> Result<()> {
        let access = self.operator_access(&document.node.node_did)?;
        match mutations::apply_config_components_on(&access, document).await {
            Ok(()) => {
                self.refresh_config_source(&document.node.node_did).await?;
                self.clear_mutation_error();
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("apply config components", error)),
        }
    }

    pub async fn save_node(&self, row: &gents::document_config::Node) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        let result = mutations::upsert_node_on(&access, row).await;
        match result {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "node",
                    row_id = %row.node_did,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save node", error)),
        }
    }

    pub async fn set_default_agent(&self, node_did: &str, agent_id: &str) -> Result<()> {
        let access = self.operator_access(node_did)?;
        match mutations::set_default_agent_on(&access, node_did, agent_id).await {
            Ok(()) => {
                self.refresh_config_source(node_did).await?;
                self.clear_mutation_error();
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("set default agent", error)),
        }
    }

    pub async fn save_backend(&self, row: &gents::InferenceBackend) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        match mutations::upsert_inference_backend_on(&access, row).await {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "backend",
                    row_id = %row.backend_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save backend", error)),
        }
    }

    pub async fn save_tools(&self, row: &gents::Tools) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        let result = mutations::upsert_tools_on(&access, row).await;
        match result {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "tools",
                    row_id = %row.tools_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save tools", error)),
        }
    }

    pub async fn save_tool_service_registry(
        &self,
        row: &gents::document_config::ToolServiceRegistry,
    ) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        match mutations::upsert_tool_service_registry_on(&access, row).await {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "tool_service_registry",
                    row_id = %row.service_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save tool service registry", error)),
        }
    }

    pub async fn select_agent_model(
        &self,
        node_did: &str,
        agent_id: &str,
        backend_id: &str,
        model_name: &str,
        reasoning_effort: Option<gents::config::ReasoningEffort>,
        service_tier: Option<gents::config::ServiceTier>,
    ) -> Result<gents::InferenceProfile> {
        let access = self.operator_access(node_did)?;
        let saved = mutations::select_agent_model_on(
            &access,
            node_did,
            agent_id,
            backend_id,
            model_name,
            reasoning_effort,
            service_tier,
        )
        .await?;
        if self.refresh_config_source(node_did).await.is_err() {
            tracing::warn!("Model saved; client observation will resynchronize");
            self.store.request_resync(Some(node_did));
        }
        Ok(saved)
    }

    pub async fn save_inference_profile(&self, row: &gents::InferenceProfile) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        match mutations::upsert_inference_profile_on(&access, row).await {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "inference_profile",
                    row_id = %row.profile_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save inference profile", error)),
        }
    }

    /// A disconnected runtime must not stage its reviewer in the client replica.
    /// Preparing this Task also requires an already available signer; it never
    /// unlocks a runtime identity as a side effect of selecting permissions.
    pub async fn prepare_access_review_task(
        &self,
        node: &str,
        agent: &str,
        preferred: Option<&str>,
    ) -> Result<gents::plugin::approval::workspace::ReviewTaskSelection> {
        let record = self
            .sync_state
            .records()
            .into_iter()
            .find(|record| record.node_did == node)
            .context("Connect this runtime before setting up access review.")?;
        anyhow::ensure!(gents::identity::can_mint_defradb_bearer(node),
            "Open Runtime Settings to connect this runtime's configuration before choosing Approve for me.");
        let access = crate::local_runtime::operator_endpoint(&record)
            .map(ConfigAccess::Graphql)
            .context("Access review setup requires this runtime's operator endpoint.")?;
        mutations::prepare_access_review_task_on(&access, node, agent, preferred).await
    }

    pub async fn save_task(&self, row: &gents::document_config::Task) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        match mutations::upsert_task_on(&access, row).await {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "task",
                    row_id = %row.task_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save task", error)),
        }
    }

    pub async fn save_skill(&self, row: &gents::document_config::SkillDocument) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        match mutations::upsert_skill_on(&access, row).await {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "skill",
                    row_id = %row.skill_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save skill", error)),
        }
    }

    pub async fn save_schedule(&self, row: &gents::document_config::Schedule) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        match mutations::upsert_schedule_on(&access, row).await {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "schedule",
                    row_id = %row.schedule_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save schedule", error)),
        }
    }

    pub async fn save_trigger(&self, row: &gents::document_config::Trigger) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        match mutations::upsert_trigger_on(&access, row).await {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "trigger",
                    row_id = %row.trigger_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save trigger", error)),
        }
    }

    pub async fn save_event_source(
        &self,
        document: &gents::document_config::EventSource,
    ) -> Result<()> {
        let access = self.operator_access(&document.node_did)?;
        match mutations::upsert_event_source_on(&access, document).await {
            Ok(()) => {
                self.refresh_config_source(&document.node_did).await?;
                self.clear_mutation_error();
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save event source", error)),
        }
    }

    pub async fn delete_event_source(&self, event_source_id: &str, node_did: &str) -> Result<()> {
        let access = self.operator_access(node_did)?;
        match mutations::delete_event_source_on(&access, node_did, event_source_id).await {
            Ok(0) => Err(self.record_mutation_error(
                "delete event source",
                anyhow::anyhow!("no EventSource document with event_source_id {event_source_id:?}"),
            )),
            Ok(_) => {
                self.refresh_config_source(node_did).await?;
                self.clear_mutation_error();
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("delete event source", error)),
        }
    }

    pub async fn fire_task_now(&self, task_row: &Task, args: serde_json::Value) -> Result<String> {
        let actor = identity::Did::new(self.node_identity.did().to_owned())
            .context("desktop node DID is not ACP-addressable")?;
        match mutations::fire_task_now(&self.node, actor, task_row, args).await {
            Ok(doc_id) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "manual_run",
                    task_id = %task_row.task_id,
                    request_doc_id = %doc_id,
                    "desktop manual task run enqueued"
                );
                Ok(doc_id)
            }
            Err(error) => Err(self.record_mutation_error("fire task", error)),
        }
    }

    pub async fn fire_task_now_for_node(
        &self,
        node_did: &str,
        task_id: &str,
        args: serde_json::Value,
    ) -> Result<SubmittedRequest> {
        let access = self.operator_access(node_did)?;
        let invocation = mutations::resolve_task_now_on(&access, node_did, task_id, args)
            .await
            .map_err(|error| self.record_mutation_error("resolve task", error))?;
        self.submit_manual_task_invocation(invocation).await
    }

    pub async fn fire_schedule_now(&self, row: &Schedule) -> Result<String> {
        let actor = identity::Did::new(self.node_identity.did().to_owned())
            .context("desktop node DID is not ACP-addressable")?;
        match mutations::fire_schedule_now(&self.node, actor, row).await {
            Ok(doc_id) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "schedule",
                    row_id = %row.schedule_id,
                    action = "run_now",
                    request_doc_id = %doc_id,
                    "desktop manual schedule run enqueued"
                );
                Ok(doc_id)
            }
            Err(error) => Err(self.record_mutation_error("fire schedule now", error)),
        }
    }

    pub async fn fire_schedule_now_for_node(
        &self,
        node_did: &str,
        schedule_id: &str,
    ) -> Result<SubmittedRequest> {
        let access = self.operator_access(node_did)?;
        let invocation = mutations::resolve_schedule_now_on(&access, node_did, schedule_id)
            .await
            .map_err(|error| self.record_mutation_error("resolve schedule", error))?;
        self.submit_manual_task_invocation(invocation).await
    }

    async fn submit_manual_task_invocation(
        &self,
        invocation: mutations::ManualTaskInvocation,
    ) -> Result<SubmittedRequest> {
        let snapshot = self.store.snapshot();
        let peer_record = self
            .peer_record_for_chat_write(&invocation.node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(&invocation.node_did, peer_record.as_ref(), Utc::now())?;
        let (signer, admission, requester_did) = self
            .request_authority(&invocation.node_did, peer_record.as_ref())
            .await?;
        let options = SubmitRequestOptions {
            input: RequestInput {
                initial_title: Some(SessionTitle {
                    text: invocation.session_title.clone(),
                    source: SessionTitleSource::Task,
                }),
                ..RequestInput::default()
            },
            trigger_lineage: TriggerLineage {
                trigger_kind: Some("manual".to_string()),
                ..TriggerLineage::default()
            },
            ..SubmitRequestOptions::default()
        };
        let agent_id = agent_id_for_write(Some(&invocation.agent_id));
        let access = self.operator_access(&invocation.node_did)?;
        let submitted = mutations::submit_task_request(
            self.node.as_ref(),
            snapshot.as_ref(),
            &access,
            &invocation.fire,
            &requester_did,
            signer.as_ref(),
            admission,
            &invocation.content,
            agent_id.as_deref(),
            options,
        )
        .await;
        match submitted {
            Ok(result) => {
                let result =
                    observe_submitted_request(&self.store, result, self.refresh_store()).await;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "manual_run",
                    node_did = %invocation.node_did,
                    task_id = %invocation.task_id,
                    request_id = %result.request_id,
                    requester_did,
                    "desktop canonical manual task run submitted through client authority"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error("fire task", error)),
        }
    }

    pub async fn ensure_session_hydration_started(
        &self,
        session_id: &str,
        node_did: &str,
    ) -> Result<(
        gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress,
        Option<String>,
    )> {
        let session_id = normalize_required("session_id", session_id)?;
        let node_did = normalize_required("node_did", node_did)?;
        let foreign_header = self
            .session_unreadable_reason(session_id, node_did)
            .is_some();
        let (progress, detail, _) = self
            .load_hydration_observation(session_id, node_did)
            .await?;
        if !should_start_session_hydration_request(&progress, session_id, node_did)
            || foreign_header
        {
            return Ok((progress, detail));
        }
        let evidence = load_local_hydration_start_evidence(
            self.node.as_ref(),
            self.node_identity.did(),
            session_id,
            node_did,
        )
        .await?;
        if !hydration_start_evidence_is_ready(false, &progress, &evidence) {
            return Ok((progress, detail));
        }
        self.admit_session_hydration_request(
            gents::agent::p2p_reconcile::session_hydration::HydrationAdmissionAction::PassiveStart,
            session_id,
            node_did,
            &progress,
            Some(&evidence),
        )
        .await?;
        self.load_hydration_status(session_id, node_did).await
    }

    pub fn transcript_node_scope(&self, node_did: &str) -> Option<String> {
        self.sync_state
            .records()
            .iter()
            .any(|peer| peer.node_did == node_did && peer.is_enrollment())
            .then(|| self.node_identity.did().to_string())
    }

    /// Whether this paired client reads the session through the node's operator
    /// access because the node owns it. The node refuses to hydrate such a
    /// session to this client's replica, so it is read only through the node.
    pub fn session_read_through_operator(&self, session_id: &str, node_did: &str) -> bool {
        let store = self.store.snapshot();
        store
            .sessions
            .iter()
            .find(|row| row.session_id == session_id && row.node_did == node_did)
            .is_some_and(|session| session.requester_did.as_deref() == Some(node_did))
            && self.transcript_node_scope(node_did).is_some()
            && self.operator_observation_graphql(node_did).is_some()
    }

    pub fn session_unreadable_reason(
        &self,
        session_id: &str,
        node_did: &str,
    ) -> Option<&'static str> {
        let store = self.store.snapshot();
        let session = store
            .sessions
            .iter()
            .find(|row| row.session_id == session_id && row.node_did == node_did)?;
        super::super::query::session_unreadable_reason(
            session,
            self.transcript_node_scope(node_did).as_deref(),
            self.operator_graphql(node_did).is_some(),
        )
    }

    pub async fn session_hydration_progress(
        &self,
        session_id: &str,
        node_did: &str,
    ) -> Result<gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress> {
        Ok(self.session_hydration_status(session_id, node_did).await?.0)
    }

    pub async fn session_hydration_status(
        &self,
        session_id: &str,
        node_did: &str,
    ) -> Result<(
        gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress,
        Option<String>,
    )> {
        let session_id = normalize_required("session_id", session_id)?;
        let node_did = normalize_required("node_did", node_did)?;
        self.load_hydration_status(session_id, node_did).await
    }

    pub async fn retry_session_hydration(&self, session_id: &str, node_did: &str) -> Result<()> {
        let session_id = normalize_required("session_id", session_id)?;
        let node_did = normalize_required("node_did", node_did)?;
        if let Some(reason) = self.session_unreadable_reason(session_id, node_did) {
            bail!("{reason}");
        }
        let (progress, _, request) = self
            .load_hydration_observation(session_id, node_did)
            .await?;
        if !gents::agent::p2p_reconcile::session_hydration::can_admit_hydration_request(
            gents::agent::p2p_reconcile::session_hydration::HydrationAdmissionAction::ExplicitRetry,
            &request,
            &progress,
            session_id,
            node_did,
            false,
            false,
            false,
            false,
        ) {
            bail!("session hydration retry requires a failed attempt for the selected session");
        }
        if !self
            .admit_session_hydration_request(
                gents::agent::p2p_reconcile::session_hydration::HydrationAdmissionAction::ExplicitRetry,
                session_id,
                node_did,
                &progress,
                None,
            )
            .await?
        {
            bail!("session hydration retry requires a failed attempt for the selected session");
        }
        Ok(())
    }

    async fn admit_session_hydration_request(
        &self,
        action: gents::agent::p2p_reconcile::session_hydration::HydrationAdmissionAction,
        session_id: &str,
        node_did: &str,
        observed_progress: &gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress,
        evidence: Option<&LocalHydrationStartEvidence>,
    ) -> Result<bool> {
        use gents::agent::p2p_reconcile::session_hydration::HydrationAdmissionAction as Action;

        let session_id = normalize_required("session_id", session_id)?;
        let node_did = normalize_required("node_did", node_did)?;
        let evidence = match action {
            Action::PassiveStart => Some(
                evidence.context("passive hydration admission requires local start evidence")?,
            ),
            Action::ExplicitRetry => {
                anyhow::ensure!(
                    evidence.is_none(),
                    "explicit hydration retry does not accept passive start evidence"
                );
                None
            }
        };
        let peer_record = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer_record.as_ref(), Utc::now())?;
        let request_key = format!("{}:{session_id}", self.local_peer_id());
        let expected_requester_did = self.node_identity.did().to_owned();
        let requester_did = gents::graphql::escape_graphql_string(&expected_requester_did);
        let node_did_gql = gents::graphql::escape_graphql_string(node_did);
        let session_id_gql = gents::graphql::escape_graphql_string(session_id);
        let request_key_gql = gents::graphql::escape_graphql_string(&request_key);
        let now = gents::graphql::escape_graphql_string(
            &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        let request_query = hydration_request_admission_query(&request_key_gql);
        let observed_progress = observed_progress.clone();
        let mutation = match action {
            Action::PassiveStart => format!(
                r#"mutation {{
                create_SessionHydrationRequest(input: {{
                        request_key: "{request_key_gql}",
                        requester_did: "{requester_did}",
                        node_did: "{node_did_gql}",
                        session_id: "{session_id_gql}",
                        created_at: "{now}",
                        status: "pending",
                        status_detail: "",
                        served_doc_count: 0
                }}) {{ _docID }}
            }}"#,
            ),
            Action::ExplicitRetry => format!(
                r#"mutation {{
                update_SessionHydrationRequest(
                    filter: {{
                        request_key: {{ _eq: "{request_key_gql}" }},
                        requester_did: {{ _eq: "{requester_did}" }},
                        node_did: {{ _eq: "{node_did_gql}" }},
                        session_id: {{ _eq: "{session_id_gql}" }},
                        status: {{ _eq: "rejected" }}
                    }},
                    input: {{
                        created_at: "{now}",
                        status: "pending",
                        status_detail: "",
                        served_doc_count: 0,
                        served_manifest_json: "",
                        outcome_signer_did: "",
                        outcome_signature: "",
                        processed_at: null
                    }}
                ) {{ _docID }}
            }}"#,
            ),
        };
        let operation = match action {
            Action::PassiveStart => "desktop.session_hydration.admit_initial",
            Action::ExplicitRetry => "desktop.session_hydration.retry_rejected",
        };
        gents::config_client::ConfigAccess::transact_local_write(
            &self.node,
            None,
            operation,
            move |txn| {
                let request_query = request_query.clone();
                let mutation = mutation.clone();
                let request_key = request_key.clone();
                let expected_requester_did = expected_requester_did.clone();
                let observed_progress = observed_progress.clone();
                Box::pin(async move {
                    if action == Action::PassiveStart {
                        gents::graphql::fence_arrivals(txn, "SessionHydrationRequest").await?;
                    }
                    let request_response = txn.execute(&request_query).await?;
                    let requests = transaction_rows::<HydrationAdmissionRequestRow>(
                        &request_response,
                        "SessionHydrationRequest",
                    )?;
                    anyhow::ensure!(
                        requests.len() <= 1,
                        "session hydration request key resolved to multiple rows"
                    );
                    let request = requests.first();
                    if let Some(request) = request {
                        request.ensure_target(&request_key, &expected_requester_did, node_did, session_id)?;
                    }
                    let request_state = match request {
                        None => {
                            gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState::Missing
                        }
                        Some(request) => match request.status.as_deref() {
                            Some("pending") => {
                                gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState::Pending
                            }
                            Some("served") => {
                                gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState::Served(BTreeSet::new())
                            }
                            Some("rejected") => {
                                gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState::Rejected(None)
                            }
                            Some(_) | None if action == Action::PassiveStart => {
                                gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState::Pending
                            }
                            Some(status) => anyhow::bail!("unknown session hydration request status {status:?}"),
                            None => anyhow::bail!("session hydration request is missing status"),
                        },
                    };
                    let evidence = evidence.as_ref();
                    if !gents::agent::p2p_reconcile::session_hydration::can_admit_hydration_request(
                        action,
                        &request_state,
                        &observed_progress,
                        session_id,
                        node_did,
                        false,
                        evidence.is_some_and(|evidence| evidence.owned_session_present),
                        observed_progress.merged_count > 0,
                        evidence.is_some_and(|evidence| evidence.nonterminal_request_present),
                    ) {
                        return Ok(false);
                    }
                    let response = txn.execute(&mutation).await?;
                    let field = match action {
                        Action::PassiveStart => "create_SessionHydrationRequest",
                        Action::ExplicitRetry => "update_SessionHydrationRequest",
                    };
                    let rows = gents::graphql::mutation_documents(&response, field)?;
                    let changed = rows.is_some_and(|rows| !rows.is_empty());
                    if action == Action::PassiveStart {
                        anyhow::ensure!(
                            changed,
                            "initial session hydration request create returned no document"
                        );
                    }
                    Ok(changed)
                })
            },
        )
        .await
    }

    async fn load_hydration_status(
        &self,
        session_id: &str,
        node_did: &str,
    ) -> Result<(
        gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress,
        Option<String>,
    )> {
        let (progress, detail, _) = self
            .load_hydration_observation(session_id, node_did)
            .await?;
        Ok((progress, detail))
    }

    async fn load_hydration_observation(
        &self,
        session_id: &str,
        node_did: &str,
    ) -> Result<(
        gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress,
        Option<String>,
        gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState,
    )> {
        let (request, detail) = load_hydration_server_state(
            self.node.as_ref(),
            self.local_peer_id(),
            self.node_identity.did(),
            session_id,
            node_did,
            &self.node_identity,
        )
        .await?;
        use gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState;
        let verified_manifest = match &request {
            ClientHydrationRequestState::Served(documents)
            | ClientHydrationRequestState::Rejected(Some(documents)) => Some(documents),
            _ => None,
        };
        let merged = load_local_hydration_documents(
            self.node.as_ref(),
            self.node_identity.did(),
            session_id,
            node_did,
            verified_manifest,
        )
        .await?;
        Ok((
            gents::agent::p2p_reconcile::session_hydration::project_durable_hydration_progress(
                session_id,
                node_did,
                merged,
                request.clone(),
            ),
            detail,
            request,
        ))
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

#[derive(Deserialize)]
struct HydrationDocIdRow {
    #[serde(rename = "_docID")]
    doc_id: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct LocalHydrationStartEvidence {
    owned_session_present: bool,
    nonterminal_request_present: bool,
}

#[derive(Deserialize)]
struct HydrationAdmissionRequestRow {
    request_key: Option<String>,
    requester_did: Option<String>,
    node_did: Option<String>,
    session_id: Option<String>,
    status: Option<String>,
}

impl HydrationAdmissionRequestRow {
    fn ensure_target(
        &self,
        request_key: &str,
        requester_did: &str,
        node_did: &str,
        session_id: &str,
    ) -> Result<()> {
        for (field, actual, expected) in [
            ("request_key", self.request_key.as_deref(), request_key),
            (
                "requester_did",
                self.requester_did.as_deref(),
                requester_did,
            ),
            ("node_did", self.node_did.as_deref(), node_did),
            ("session_id", self.session_id.as_deref(), session_id),
        ] {
            anyhow::ensure!(
                actual == Some(expected),
                "session hydration request {field} does not match the selected target"
            );
        }
        Ok(())
    }
}

fn transaction_rows<T>(response: &serde_json::Value, field: &str) -> Result<Vec<T>>
where
    T: serde::de::DeserializeOwned,
{
    let data = response
        .get("data")
        .and_then(serde_json::Value::as_object)
        .context("hydration admission transaction omitted response data")?;
    let rows = data
        .get(field)
        .with_context(|| format!("hydration admission transaction omitted {field}"))?;
    Vec::<T>::deserialize(rows).with_context(|| format!("decode {field} transaction rows"))
}

fn hydration_request_admission_query(request_key_gql: &str) -> String {
    format!(
        r#"{{ SessionHydrationRequest(filter: {{ request_key: {{ _eq: "{request_key_gql}" }} }}) {{
            request_key requester_did node_did session_id status
        }} }}"#
    )
}

fn should_start_session_hydration_request(
    progress: &gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress,
    session_id: &str,
    node_did: &str,
) -> bool {
    use gents::agent::p2p_reconcile::session_hydration::ClientHydrationPhase;
    progress.session_id == session_id
        && progress.node_did == node_did
        && progress.phase == ClientHydrationPhase::Idle
}

fn hydration_start_evidence_is_ready(
    foreign_header: bool,
    progress: &gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress,
    evidence: &LocalHydrationStartEvidence,
) -> bool {
    gents::agent::p2p_reconcile::session_hydration::can_start_hydration(
        foreign_header,
        evidence.owned_session_present,
        progress.merged_count > 0,
        evidence.nonterminal_request_present,
    )
}

async fn load_local_hydration_start_evidence(
    node: &EmbeddedNode,
    requester_did: &str,
    session_id: &str,
    node_did: &str,
) -> Result<LocalHydrationStartEvidence> {
    let requester_did = gents::graphql::escape_graphql_string(requester_did);
    let session_id = gents::graphql::escape_graphql_string(session_id);
    let node_did = gents::graphql::escape_graphql_string(node_did);
    let scope = format!(
        "requester_did: {{ _eq: \"{requester_did}\" }}, node_did: {{ _eq: \"{node_did}\" }}, session_id: {{ _eq: \"{session_id}\" }}"
    );
    let response = gents::graphql::graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{
                AgentSession(filter: {{ {scope} }}, limit: 1) {{ _docID }}
                AgentRequest(filter: {{ {scope} }}) {{ request_id lifecycle_state }}
            }}"#
        ),
        "query local hydration start evidence",
    )
    .await?;
    local_hydration_start_evidence_from_response(&response)
}

fn local_hydration_start_evidence_from_response(
    response: &defra_node::QueryResponse,
) -> Result<LocalHydrationStartEvidence> {
    let owned_session_present =
        !gents::graphql::rows::<HydrationDocIdRow>(response, "AgentSession")?.is_empty();
    let nonterminal_request_present =
        gents::graphql::rows::<AgentRequestRow>(response, "AgentRequest")?
            .iter()
            .any(|row| {
                !row.lifecycle_state
                    .is_some_and(RequestLifecycleState::is_terminal)
            });
    Ok(LocalHydrationStartEvidence {
        owned_session_present,
        nonterminal_request_present,
    })
}

async fn load_local_hydration_documents(
    node: &EmbeddedNode,
    requester_did: &str,
    session_id: &str,
    node_did: &str,
    verified_manifest: Option<&BTreeSet<SessionHydrationDocumentKey>>,
) -> Result<BTreeSet<SessionHydrationDocumentKey>> {
    let query = local_hydration_query(requester_did, session_id, node_did, verified_manifest);
    let response = gents::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "query local session hydration documents",
    )
    .await?;
    local_hydration_documents_from_response(&response)
}

const HYDRATION_COLLECTIONS: [(&str, SessionHydrationCollection); 5] = [
    ("AgentRequest", SessionHydrationCollection::AgentRequest),
    ("AgentMessage", SessionHydrationCollection::AgentMessage),
    (
        "AgentOutputSegment",
        SessionHydrationCollection::AgentOutputSegment,
    ),
    ("AgentToolCall", SessionHydrationCollection::AgentToolCall),
    (
        "CompactionEntry",
        SessionHydrationCollection::CompactionEntry,
    ),
];

/// Fork origin dependencies retain their source session. Only the verified
/// receipt's physical keys extend the session read; ownership filters still apply.
fn local_hydration_query(
    requester_did: &str,
    session_id: &str,
    node_did: &str,
    verified_manifest: Option<&BTreeSet<SessionHydrationDocumentKey>>,
) -> String {
    let requester_did = gents::graphql::escape_graphql_string(requester_did);
    let session_id = gents::graphql::escape_graphql_string(session_id);
    let node_did = gents::graphql::escape_graphql_string(node_did);
    let fields = HYDRATION_COLLECTIONS
        .into_iter()
        .map(|(name, collection)| {
            let ids = verified_manifest
                .into_iter()
                .flatten()
                .filter(|key| key.collection == collection)
                .map(|key| format!("\"{}\"", gents::graphql::escape_graphql_string(&key.doc_id)))
                .collect::<Vec<_>>()
                .join(",");
            let session_filter = format!("session_id: {{ _eq: \"{session_id}\" }}");
            let selected = if ids.is_empty() {
                session_filter
            } else {
                format!("_or: [{{{session_filter}}}, {{_docID: {{_in: [{ids}]}}}}]")
            };
            format!(
                "{name}(filter: {{requester_did: {{_eq: \"{requester_did}\"}}, node_did: {{_eq: \"{node_did}\"}}, {selected}}}) {{_docID}}"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{{{fields}}}")
}

fn local_hydration_documents_from_response(
    response: &defra_node::QueryResponse,
) -> Result<BTreeSet<SessionHydrationDocumentKey>> {
    let mut ids = BTreeSet::new();
    for (collection_name, collection) in HYDRATION_COLLECTIONS {
        for row in gents::graphql::rows::<HydrationDocIdRow>(response, collection_name)? {
            if let Some(doc_id) = row.doc_id.filter(|value| !value.is_empty()) {
                ids.insert(SessionHydrationDocumentKey { collection, doc_id });
            }
        }
    }
    Ok(ids)
}

async fn load_hydration_server_state(
    node: &EmbeddedNode,
    peer_id: &str,
    requester_did: &str,
    session_id: &str,
    node_did: &str,
    node_identity: &super::super::principal_identity::PrincipalIdentity,
) -> Result<(
    gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState,
    Option<String>,
)> {
    use gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState;
    let expected_request_key = format!("{peer_id}:{session_id}");
    let request_key = gents::graphql::escape_graphql_string(&expected_request_key);
    let query = format!(
        r#"{{ SessionHydrationRequest(filter: {{ request_key: {{ _eq: "{request_key}" }} }}) {{
            request_key requester_did node_did session_id status status_detail
            served_doc_count served_manifest_json processed_at
            outcome_signer_did outcome_signature
        }} }}"#
    );
    let response = gents::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "query session hydration request",
    )
    .await?;
    let Some(data) = response.data else {
        return Ok((ClientHydrationRequestState::Missing, None));
    };
    let rows = data
        .get("SessionHydrationRequest")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    if rows.len() > 1 {
        bail!("session hydration request key resolved to multiple rows");
    }
    let Some(row) = rows.first() else {
        return Ok((ClientHydrationRequestState::Missing, None));
    };
    for (field, expected) in [
        ("request_key", expected_request_key.as_str()),
        ("requester_did", requester_did),
        ("node_did", node_did),
        ("session_id", session_id),
    ] {
        if row.get(field).and_then(|value| value.as_str()) != Some(expected) {
            bail!("session hydration request {field} does not match the selected target");
        }
    }
    let status = row
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if status == "pending" {
        return Ok((ClientHydrationRequestState::Pending, None));
    }
    if !matches!(status, "served" | "rejected") {
        bail!("unknown session hydration request status {status:?}");
    }
    let served_count = row
        .get("served_doc_count")
        .and_then(|value| value.as_i64())
        .map(usize::try_from)
        .transpose()
        .context("session hydration served_doc_count must be non-negative")?;
    let manifest = decode_manifest_json(
        row.get("served_manifest_json")
            .and_then(|value| value.as_str())
            .context("terminal hydration request is missing served_manifest_json")?,
    )?;
    if served_count != Some(manifest.len()) {
        bail!("session hydration served_doc_count does not match signed manifest");
    }
    let signature = bs58::decode(
        row.get("outcome_signature")
            .and_then(|value| value.as_str())
            .context("terminal hydration request is missing outcome_signature")?,
    )
    .into_vec()
    .context("decode session hydration receipt signature")?;
    let receipt = SessionHydrationReceipt {
        version: SESSION_HYDRATION_RECEIPT_VERSION,
        request_key: expected_request_key,
        requester_did: requester_did.to_string(),
        node_did: node_did.to_string(),
        session_id: session_id.to_string(),
        status: status.to_string(),
        status_detail: row
            .get("status_detail")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        served_manifest: manifest,
        processed_at: row
            .get("processed_at")
            .and_then(|value| value.as_str())
            .context("terminal hydration request is missing processed_at")?
            .to_string(),
        signer_did: row
            .get("outcome_signer_did")
            .and_then(|value| value.as_str())
            .context("terminal hydration request is missing outcome_signer_did")?
            .to_string(),
        signature,
    };
    receipt.validate_shape()?;
    if !node_identity
        .verify(node_did, &receipt.signing_payload()?, &receipt.signature)
        .await?
    {
        bail!("session hydration receipt signature is invalid");
    }
    let documents = receipt.served_manifest.into_iter().collect::<BTreeSet<_>>();
    match status {
        "served" => Ok((ClientHydrationRequestState::Served(documents), None)),
        "rejected" => {
            let detail = Some(receipt.status_detail).filter(|detail| !detail.is_empty());
            Ok((
                ClientHydrationRequestState::Rejected(Some(documents)),
                detail,
            ))
        }
        _ => unreachable!(),
    }
}

fn retain_rows_with_sources<T>(
    rows: &mut Vec<T>,
    sources: &mut Vec<Option<String>>,
    mut keep: impl FnMut(&T) -> bool,
) {
    let mut kept_rows = Vec::with_capacity(rows.len());
    let mut kept_sources = Vec::with_capacity(rows.len());

    for (index, row) in rows.drain(..).enumerate() {
        if keep(&row) {
            kept_rows.push(row);
            kept_sources.push(sources.get(index).cloned().unwrap_or_default());
        }
    }

    *rows = kept_rows;
    *sources = kept_sources;
}

fn prune_deleted_skill_rows(rows: &mut ClientStoreRows, node_did: &str, skill_id: &str) {
    retain_rows_with_sources(&mut rows.skills, &mut rows.skill_source_node_dids, |row| {
        !(row.skill_id == skill_id && row.node_did == node_did)
    });
}

fn complete_confirmed_delete(
    store: &ObservedStore,
    last_mutation_error: &StdRwLock<Option<String>>,
    refresh_result: Result<u64>,
    action_label: &str,
    action: &str,
    row_id: &str,
    prune: impl FnOnce(&mut ClientStoreRows),
) {
    match refresh_result {
        Ok(_) => {
            *last_mutation_error
                .write()
                .expect("mutation error lock poisoned") = None;
        }
        Err(error) => {
            let warning = format!(
                "{action_label} succeeded, but refreshing the source snapshot failed: {error}"
            );
            *last_mutation_error
                .write()
                .expect("mutation error lock poisoned") = Some(warning);
            tracing::warn!(
                target: "gents_desktop_core::writes",
                action = %action,
                row_id = %row_id,
                error = %error,
                "desktop write saved, but refreshing the source snapshot failed"
            );
        }
    }

    let mut rows = store.snapshot().to_rows();
    prune(&mut rows);
    store.replace_snapshot(ClientStore::from_rows(rows));

    tracing::info!(
        target: "gents_desktop_core::writes",
        action = %action,
        row_id = %row_id,
        "desktop write saved"
    );
}

#[cfg(test)]
mod delete_source_tests {
    use super::*;
    use anyhow::anyhow;
    use serde_json::json;

    #[test]
    fn hydration_start_evidence_rejects_unknown_request_lifecycle_state() {
        let response = defra_node::QueryResponse::success(json!({
            "AgentSession": [{ "_docID": "session-1" }],
            "AgentRequest": [{
                "request_id": "request-1",
                "lifecycle_state": "notARequestState"
            }]
        }));

        let error = local_hydration_start_evidence_from_response(&response)
            .expect_err("unknown lifecycle state must fail at the row boundary");
        assert!(
            format!("{error:#}").contains("notARequestState"),
            "{error:#}"
        );
    }

    #[test]
    fn hydration_query_escapes_every_scope_value() {
        let query = local_hydration_query(
            "did:key:requester\"escaped",
            "session\"escaped",
            "did:key:node\"escaped",
            None,
        );
        assert!(query.contains(r#"requester_did: {_eq: "did:key:requester\"escaped"}"#));
        assert!(query.contains(r#"session_id: { _eq: "session\"escaped" }"#));
        assert!(query.contains(r#"node_did: {_eq: "did:key:node\"escaped"}"#));
    }

    fn task(task_id: &str) -> Task {
        serde_json::from_value(json!({
            "task_id": task_id,
            "node_did": "did:key:amy",
            "agent_id": "default",
            "prompt_template": "run"
        }))
        .expect("task")
    }

    fn peer_record(source: Option<&str>) -> PeerRecord {
        let mut record = PeerRecord::new("Amy", "endpoint-amy", "did:key:amy");
        record.source = source.map(str::to_owned);
        if source == Some("enrollment") {
            record.pairing_network_id = Some("network-a".into());
            record.enrollment_request_digest = Some("digest-a".into());
            record.enrollment_authorization_sequence = Some(1);
            record.enrollment_authorization_expires_at = Some("2099-09-29T00:00:00Z".into());
        }
        record
    }

    #[test]
    fn ux_data_requires_the_installing_local_runtime() {
        let home = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let mut record = peer_record(Some("local-standard"));
        record.graphql = Some("http://127.0.0.1:1234/api/v0/graphql".into());
        assert!(ux_local_runtime_record(&[], &record.node_did, home.path()).is_err());
        assert!(ux_local_runtime_record(&[record.clone()], &record.node_did, home.path()).is_err());
        record.local_node_home = Some(other.path().display().to_string());
        assert!(ux_local_runtime_record(&[record.clone()], &record.node_did, home.path()).is_err());
        record.local_node_home = Some(home.path().display().to_string());
        assert!(ux_local_runtime_record(&[record.clone()], "did:key:other", home.path()).is_err());
        assert!(ux_local_runtime_record(&[record.clone()], &record.node_did, home.path()).is_ok());
        record.graphql = None;
        assert!(ux_local_runtime_record(&[record.clone()], &record.node_did, home.path()).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn expired_enrollment_cannot_admit_a_request_without_waiting_for_the_sweep() {
        let mut peer = peer_record(Some("enrollment"));
        peer.pairing_ready = true;
        peer.enrollment_authorization_expires_at = Some("2026-08-30T12:00:01Z".into());
        let before = "2026-08-30T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let expired = "2026-08-30T12:00:01Z".parse::<DateTime<Utc>>().unwrap();

        assert!(ensure_peer_chat_ready_at(&peer.node_did, Some(&peer), before).is_ok());
        assert!(ensure_peer_chat_ready_at(&peer.node_did, Some(&peer), expired).is_err());
        assert!(
            peer_record_owning_node_at(std::slice::from_ref(&peer), &peer.node_did, expired,)
                .is_none()
        );
    }

    #[test]
    fn machine_pairing_never_claims_an_unlisted_node() {
        let mut machine = peer_record(Some("enrollment"));
        machine.pairing_template = Some("machine".to_string());
        machine.pairing_ready = true;
        assert!(peer_record_owning_node_at(&[machine], "did:key:child", Utc::now()).is_none());
    }

    #[test]
    fn confirmed_delete_prunes_locally_and_warns_when_refresh_fails() {
        let rows = ClientStoreRows {
            tasks: vec![task("deleted"), task("retained")],
            task_source_node_dids: vec![None, Some("did:key:remote".to_string())],
            ..ClientStoreRows::default()
        };
        let (store, _version_rx) = ObservedStore::new(ClientStore::from_rows(rows));
        let last_mutation_error = StdRwLock::new(None);

        complete_confirmed_delete(
            store.as_ref(),
            &last_mutation_error,
            Err(anyhow!("replica unavailable")),
            "delete task",
            "config_task_delete",
            "deleted",
            |rows| {
                retain_rows_with_sources(&mut rows.tasks, &mut rows.task_source_node_dids, |row| {
                    row.task_id != "deleted"
                });
            },
        );

        let snapshot = store.snapshot();
        assert_eq!(snapshot.tasks.len(), 1);
        assert_eq!(snapshot.tasks[0].task_id, "retained");
        assert_eq!(
            snapshot.task_source_node_dids,
            vec![Some("did:key:remote".to_string())]
        );
        assert_eq!(
            last_mutation_error
                .read()
                .expect("mutation error lock poisoned")
                .as_deref(),
            Some(
                "delete task succeeded, but refreshing the source snapshot failed: replica unavailable"
            )
        );
    }
}

fn strip_cli_operator_hints(message: &str) -> String {
    message
        .lines()
        .filter(|line| {
            !line.contains("gents init")
                && !line.contains("gents server")
                && !line.contains("--graphql")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

#[derive(Debug, Clone)]
pub struct NetworkStatus {
    pub local_peer_id: std::result::Result<String, String>,
    pub listen_addresses: std::result::Result<Vec<String>, String>,
    pub connected_peers: std::result::Result<Vec<String>, String>,
    pub replicators: std::result::Result<Vec<NetworkReplicator>, String>,
    pub saved_peers: Vec<super::super::peer_directory::PeerRecord>,
}

#[derive(Debug, Clone)]
pub struct NetworkReplicator {
    pub peer_id: Option<String>,
    pub address: Option<String>,
    pub collections: Vec<String>,
    pub status: Option<u8>,
    pub last_status_change: Option<String>,
}

#[cfg(test)]
mod accepted_edit_projection_tests {
    use super::*;
    use gents_protocol::session_input_edit::{
        SessionInputEditOutcome, SessionInputEditReceipt, SESSION_INPUT_EDIT_VERSION,
    };

    #[tokio::test]
    async fn submitted_request_survives_refresh_failure_and_requests_scoped_resync() {
        let (store, _version) = ObservedStore::new(ClientStore::default());
        let mut wake = store.subscribe_resync_requests();
        let receipt = SubmittedRequest {
            request_id: "accepted-request".into(),
            session_id: "accepted-session".into(),
            node_did: "did:test:node".into(),
            agent_id: Some("agent".into()),
        };
        let before = store.projection_revision();
        let accepted = observe_submitted_request(&store, receipt.clone(), async {
            anyhow::bail!("projection unavailable after commit")
        })
        .await;
        assert_eq!(accepted, receipt);
        assert!(wake.has_changed().unwrap());
        wake.borrow_and_update();
        assert_eq!(
            store.take_resync_requests(),
            (false, vec!["did:test:node".to_owned()])
        );
        assert_eq!(store.projection_revision(), before);
        assert!(store.snapshot().requests.is_empty());
    }

    #[tokio::test]
    async fn submitted_request_refresh_success_does_not_schedule_another_read() {
        let (store, _version) = ObservedStore::new(ClientStore::default());
        let wake = store.subscribe_resync_requests();
        let receipt = SubmittedRequest {
            request_id: "accepted-request".into(),
            session_id: "accepted-session".into(),
            node_did: "did:test:node".into(),
            agent_id: None,
        };
        let accepted = observe_submitted_request(&store, receipt.clone(), async { Ok(1) }).await;
        assert_eq!(accepted, receipt);
        assert!(!wake.has_changed().unwrap());
        assert_eq!(store.take_resync_requests(), (false, Vec::new()));
    }

    fn applied_receipt() -> SessionInputEditReceipt {
        SessionInputEditReceipt {
            version: SESSION_INPUT_EDIT_VERSION,
            command_id: "accepted-edit".to_string(),
            command_digest: "sha256:test".to_string(),
            requester_did: "did:test:requester".to_string(),
            node_did: "did:test:node".to_string(),
            session_id: "session".to_string(),
            outcome: SessionInputEditOutcome::Applied,
            reason: String::new(),
            request_doc_ids: vec!["replacement-doc".to_string()],
            request_ids: vec!["replacement-request".to_string()],
            processed_at: "2026-10-14T00:00:00Z".to_string(),
            signature: vec![0; 64],
        }
    }

    fn modeled_command_outcome(case: &serde_json::Value) -> SessionInputEditOutcome {
        serde_json::from_value(case["command_outcome"].clone()).expect("modeled receipt outcome")
    }

    #[tokio::test]
    async fn generated_accepted_receipt_survives_projection_refresh_failure() {
        let contracts: serde_json::Value =
            gents_lean_contract::load_contract_snapshot().expect("Lean contract snapshot");
        let case = contracts["session_input_edit_client_observation_cases"]
            .as_array()
            .and_then(|cases| {
                cases
                    .iter()
                    .find(|case| case["name"] == "applied_receipt_projection_failure")
            })
            .expect("modeled applied receipt with failed projection");
        let (store, _version) = ObservedStore::new(ClientStore::default());
        let before = store.projection_revision().store_version;
        let receipt = applied_receipt();
        let accepted = merge_accepted_edit_projection(
            &ConfigAccess::Graphql(gents::config_client::GraphqlEndpoint::anonymous(
                "invalid endpoint",
            )),
            store.as_ref(),
            receipt,
            &["selected-doc".to_string()],
            "did:test:node",
            "session",
            "did:test:requester",
        )
        .await;

        assert_eq!(
            serde_json::to_value(accepted.outcome).expect("serialize outcome"),
            serde_json::to_value(modeled_command_outcome(case)).expect("serialize modeled outcome")
        );
        assert_eq!(case["projection_fresh"], false);
        assert_eq!(store.projection_revision().store_version, before);
        assert!(store.snapshot().requests.is_empty());
    }

    #[tokio::test]
    async fn incomplete_or_foreign_edit_projection_keeps_store_and_receipt() {
        let node = Arc::new(
            crate::test_support::node_builder()
                .build()
                .await
                .expect("local projection node"),
        );
        crate::client::schema::ensure_runtime_schemas(node.as_ref())
            .await
            .expect("schemas");
        let access = ConfigAccess::local(node.clone());
        let seed = access
            .write(
                "test.foreign_edit_projection_row",
                r#"mutation {
                    create_AgentRequest(input: {
                        request_id: "foreign-projection-row",
                        purpose: "normal",
                        node_did: "did:test:other-node",
                        requester_did: "did:test:requester",
                        agent_id: "default",
                        session_id: "other-session",
                        content: "foreign row",
                        lifecycle_state: "pending",
                        execution_origin: "interactive",
                        created_at: "2026-10-14T00:00:00Z"
                    }) { _docID }
                }"#,
            )
            .await
            .expect("seed foreign row");
        let foreign_id = seed["data"]["add_AgentRequest"][0]["_docID"]
            .as_str()
            .expect("physical ID")
            .to_owned();
        let (store, _version) = ObservedStore::new(ClientStore::default());
        let before = store.projection_revision().store_version;

        let missing = merge_accepted_edit_projection(
            &access,
            store.as_ref(),
            applied_receipt(),
            &["physically-missing".to_string()],
            "did:test:node",
            "session",
            "did:test:requester",
        )
        .await;
        let foreign = merge_accepted_edit_projection(
            &access,
            store.as_ref(),
            applied_receipt(),
            &[foreign_id],
            "did:test:node",
            "session",
            "did:test:requester",
        )
        .await;
        let mut empty_receipt = applied_receipt();
        empty_receipt.request_doc_ids.clear();
        empty_receipt.request_ids.clear();
        let empty = merge_accepted_edit_projection(
            &access,
            store.as_ref(),
            empty_receipt,
            &[],
            "did:test:node",
            "session",
            "did:test:requester",
        )
        .await;

        for receipt in [missing, foreign, empty] {
            assert_eq!(receipt.outcome, SessionInputEditOutcome::Applied);
        }
        assert_eq!(store.projection_revision().store_version, before);
        assert!(store.snapshot().requests.is_empty());
    }
}

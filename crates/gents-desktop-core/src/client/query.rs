use anyhow::{bail, Context, Result};
#[cfg(test)]
use defra_node::Node as EmbeddedNode;
#[cfg(test)]
use gents_documents::InferenceBackend;
use gents_documents::Trigger;
use gents_protocol::client_protocol::SessionRowObservation;
#[cfg(test)]
use gents_protocol::graphql::escape_graphql_string;
use gents_protocol::output::reconstruction::DependencyDenial;
use gents_protocol::row::{AgentRequestRow, AgentToolCallRow, MailboxItemRow};
use gents_protocol::schemas::{
    AGENT_MESSAGE_NAME, AGENT_NAME, AGENT_OUTPUT_SEGMENT_NAME, AGENT_REQUEST_NAME,
    AGENT_SESSION_NAME, AGENT_TARGET_NAME, AGENT_TOOL_CALL_NAME, COMPACTION_ENTRY_NAME,
    DEFAULT_AGENT_SELECTION_NAME, GOAL_NAME,
    INFERENCE_BACKEND_METADATA_NAME as INFERENCE_BACKEND_NAME, INFERENCE_BACKEND_OBSERVATION_NAME,
    INFERENCE_PROFILE_NAME, MAILBOX_ITEM_NAME, NODE_NAME, NODE_READINESS_NAME, NODE_RUNTIME_NAME,
    SCHEDULE_NAME, SKILL_NAME, TASK_NAME, TOOLS_NAME, TOOL_SERVICE_REGISTRY_NAME, TRIGGER_NAME,
};
use gents_protocol::session::AgentSession;
use gents_store::config_client::TransactionFailure;
use serde_json::Value;

use super::peer_directory::PeerRecord;
use super::store::{ClientStore, ClientStoreRows};

mod document_patches;
mod request_closure;
mod session_tip;
mod session_transcript;
pub use session_tip::{
    load_request_prompt_ownership_on, load_session_live_store_on, load_session_live_text_on,
    load_session_request_live_store_on, load_session_tip_store_on, RequestPromptFact,
    RequestPromptOwnership,
};
mod snapshot_loaders;
pub use snapshot_loaders::{load_node_snapshot_on, SnapshotReadError};

#[cfg(test)]
pub use document_patches::fetch_doc_patch;
#[cfg(test)]
pub(crate) use document_patches::supports_doc_patch_collection;
pub(crate) use document_patches::{is_transcript_content_collection, OBSERVED_COLLECTIONS};
pub(crate) use request_closure::close_request_rows_on;
pub use session_transcript::{
    load_session_compaction_entries_on, load_session_context_store_on,
    load_session_diagnostics_store_on, load_session_transcript_page_on,
    load_session_transcript_window_on, TranscriptWindow,
};
pub(crate) use snapshot_loaders::*;

pub const DEFAULT_SESSION_TRANSCRIPT_PAGE_SIZE: usize = 40;
/// Bound a page's message read and the work needed to project it.
pub const MAX_SESSION_TRANSCRIPT_PAGE_SIZE: usize = 80;
/// Tool calls at a sequence are returned together; exceeding this bounded read
/// fails instead of returning a partial sequence.
pub(super) const SESSION_TRANSCRIPT_TOOL_CALL_ROW_BUDGET: usize = 320;

#[derive(Debug)]
pub struct SessionTranscriptQueryPage {
    pub store: ClientStore,
    pub canonical_dependencies: CanonicalTranscriptDependencies,
    pub query_count: u64,
    pub queried_rows: usize,
    pub message_query_limit: usize,
    pub tool_call_query_limit: usize,
    pub source_exhausted: bool,
    pub has_newer: bool,
    pub covered: TranscriptSpan,
    /// The requests the page's rows belong to, as far as they are readable.
    /// Kept beside the store so a reader merging pages into its own request
    /// state does not take them for the session's request history.
    pub requests: Vec<AgentRequestRow>,
    /// Delivery messages outside the page that carry results for its tool
    /// calls: sources for those results, never rows of their own.
    pub result_messages: Vec<gents_protocol::output::rows::TranscriptMessageRow>,
}

/// The message numbers a transcript read proves, inclusive. Every message in
/// the span is in the read, so a number inside it without a row is empty, not
/// unread. `through: None` reaches the newest message the read could see.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TranscriptSpan {
    pub from: Option<i64>,
    pub through: Option<i64>,
}

#[derive(Debug, Default)]
pub struct CanonicalTranscriptDependencies {
    pub origin_headers: Vec<gents_protocol::output::rows::TranscriptMessageRow>,
    /// Exact source request metadata supplies attribution, never child turn membership.
    pub origin_requests: Vec<AgentRequestRow>,
    pub output_segments: Vec<gents_protocol::output::rows::OutputSegmentRow>,
    pub denied_header_doc_ids: Vec<String>,
    pub denied_segment_doc_ids: Vec<String>,
    pub dependency_denials: Vec<DependencyDenial>,
}

/// What a selected-session read found in one read transaction.
#[derive(Debug)]
pub enum SelectedSessionRead {
    /// The node's canonical session row and the requests it selects.
    Visible(Box<ClientStore>),
    /// The runtime returned no visible session for this scope.
    Absent,
}

pub async fn load_chat_patch(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
    request_id: Option<&str>,
    fallback_request_id: Option<&str>,
    page_request_doc_ids: &[String],
) -> Result<SelectedSessionRead> {
    access
        .transact_readonly("desktop.selected_request_refresh", |txn| {
            Box::pin(load_chat_patch_in_txn(
                txn,
                node_did,
                session_id,
                request_id,
                fallback_request_id,
                page_request_doc_ids,
            ))
        })
        .await
}

async fn load_chat_patch_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    request_id: Option<&str>,
    fallback_request_id: Option<&str>,
    page_request_doc_ids: &[String],
) -> Result<SelectedSessionRead> {
    let node_did = node_did.trim();
    let session_id = session_id.trim();
    let request_id = request_id.map(str::trim).filter(|id| !id.is_empty());
    let fallback_request_id = fallback_request_id
        .map(str::trim)
        .filter(|id| !id.is_empty());
    if node_did.is_empty() || session_id.is_empty() {
        return Ok(SelectedSessionRead::Visible(Box::default()));
    }
    let sessions = txn
        .read_view(&gents_session::views::SessionList {
            input: gents_protocol::operations::session::SessionListInput {
                node_did: Some(node_did.to_owned()),
                session_id: Some(session_id.to_owned()),
                include_archived: true,
                ..Default::default()
            },
        })
        .await?
        .into_iter()
        .map(|row| row.session)
        .collect();
    let Some(session) = selected_session_from_rows(sessions, node_did, session_id)? else {
        return Ok(SelectedSessionRead::Absent);
    };
    let requester_did = session.requester_did.clone();
    let lookup = selected_request_lookup(
        node_did,
        session_id,
        requester_did.as_deref(),
        session.observation.as_ref(),
        request_id,
        fallback_request_id,
        page_request_doc_ids,
    )?;
    let selected = if let Some(lookup) = lookup {
        let rows = txn.read_view(&lookup.view).await?;
        lookup.decode(rows, node_did, session_id, requester_did.as_deref())?
    } else {
        Vec::new()
    };
    let scopes = request_seed_scopes(&selected);
    let seeds = read_request_seeds_in_txn(txn, selected, &scopes, page_request_doc_ids).await?;
    let requests = request_closure::close_request_rows_in_txn(txn, seeds).await?;
    let scopes = request_context_scopes(&requests);
    let views = scopes
        .iter()
        .map(|(node, session)| {
            gents_automation::views::GoalRows::new(node).sessions(vec![session.clone()])
        })
        .collect::<Vec<_>>();
    let goals = txn
        .read_views(&views)
        .await?
        .into_iter()
        .flatten()
        .collect();
    anyhow::ensure!(
        requests
            .iter()
            .all(|row| row.requester_did == requester_did),
        "request closure contains a row outside the canonical session requester scope"
    );
    Ok(SelectedSessionRead::Visible(Box::new(
        ClientStore::from_rows(ClientStoreRows {
            requests,
            sessions: vec![session],
            goals,
            ..ClientStoreRows::default()
        }),
    )))
}

pub(crate) async fn load_request_documents_on(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    doc_ids: &[String],
) -> Result<Vec<AgentRequestRow>> {
    let (rows, expected) = session_transcript::read_request_documents(
        access,
        node_did,
        doc_ids,
        "pending edit request patch",
    )
    .await?;
    anyhow::ensure!(
        rows.len() == expected,
        "pending edit request patch is incomplete"
    );
    Ok(rows)
}

fn request_view(
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> gents_session::views::requests::RequestRows {
    gents_session::views::requests::RequestRows::new(
        gents_session::views::transcript::Scope::Session(gents_protocol::session::SessionScope {
            node_did: node_did.to_owned(),
            session_id: session_id.to_owned(),
            requester_did: requester_did.map(str::to_owned),
        }),
    )
    .public()
}

struct SelectedRequestLookup {
    view: gents_session::views::requests::RequestRows,
    expected_doc_id: Option<String>,
    expected_request_id: Option<String>,
}

impl SelectedRequestLookup {
    /// Session metadata can replicate before its physical request; an empty
    /// lookup cannot establish a broken canonical reference.
    fn decode(
        self,
        selected: Vec<AgentRequestRow>,
        node_did: &str,
        session_id: &str,
        requester_did: Option<&str>,
    ) -> Result<Vec<AgentRequestRow>> {
        anyhow::ensure!(
            selected.iter().all(|row| {
                row.node_did.as_deref() == Some(node_did)
                    && row.session_id.as_deref() == Some(session_id)
                    && row.requester_did.as_deref() == requester_did
                    && row.purpose
                        == Some(gents_protocol::request_admission::RequestPurpose::Normal)
                    && self
                        .expected_doc_id
                        .as_deref()
                        .is_none_or(|doc_id| row.doc_id.as_deref() == Some(doc_id))
            }),
            "selected request query returned a row outside its exact node/session/requester scope"
        );
        if selected.is_empty() {
            // A signed client request can arrive after its independently created session.
            return Err(crate::error::BridgeError::new(
                crate::error::BridgeErrorCode::ProjectionPending,
                "selected request has not reached the session owner yet",
            )
            .into());
        }
        if let Some(expected_request_id) = self.expected_request_id.as_deref() {
            anyhow::ensure!(
                selected.len() == 1 && selected[0].request_id == expected_request_id,
                "canonical session head did not resolve to its exact physical request"
            );
        }
        Ok(selected)
    }
}

fn selected_request_lookup(
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    observation: Option<&gents_protocol::session::SessionObservation>,
    request_id: Option<&str>,
    fallback_request_id: Option<&str>,
    page_request_doc_ids: &[String],
) -> Result<Option<SelectedRequestLookup>> {
    let lookup = if let Some(request_id) = request_id {
        Some(SelectedRequestLookup {
            view: request_view(node_did, session_id, requester_did)
                .request_ids(vec![request_id.to_owned()]),
            expected_doc_id: None,
            expected_request_id: None,
        })
    } else if let Some(head) =
        observation.and_then(|observation| observation.latest_request.as_ref())
    {
        Some(SelectedRequestLookup {
            view: request_view(node_did, session_id, requester_did)
                .doc_ids(vec![head.request_doc_id.clone()]),
            expected_doc_id: Some(head.request_doc_id.clone()),
            expected_request_id: Some(head.request_id.clone()),
        })
    } else if observation.is_none() {
        fallback_request_id.map(|request_id| SelectedRequestLookup {
            view: request_view(node_did, session_id, requester_did)
                .request_ids(vec![request_id.to_owned()]),
            expected_doc_id: None,
            expected_request_id: None,
        })
    } else {
        None
    };
    anyhow::ensure!(
        lookup.is_some() || page_request_doc_ids.is_empty(),
        "session transcript has request owners but the canonical session has no latest request"
    );
    Ok(lookup)
}

fn selected_session_from_rows(
    sessions: Vec<AgentSession>,
    node_did: &str,
    session_id: &str,
) -> Result<Option<AgentSession>> {
    let in_scope = sessions
        .iter()
        .all(|session| session.node_did == node_did && session.session_id == session_id);
    match SessionRowObservation::observe(sessions.len(), in_scope) {
        SessionRowObservation::Absent => Ok(None),
        SessionRowObservation::Canonical => Ok(sessions.into_iter().next()),
        SessionRowObservation::Duplicate => bail!(
            "selected session refresh found {} canonical AgentSession rows for one node/session",
            sessions.len()
        ),
        SessionRowObservation::OutOfScope => {
            bail!("selected session refresh returned a row outside its exact node/session scope")
        }
    }
}

pub(super) async fn read_request_seeds_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    mut selected: Vec<AgentRequestRow>,
    scopes: &[(String, String, Option<String>)],
    page_request_doc_ids: &[String],
) -> Result<Vec<AgentRequestRow>> {
    let mut views = Vec::new();
    for (node, session, requester) in scopes {
        views.push(
            request_view(node, session, requester.as_deref()).seeds(page_request_doc_ids.to_vec()),
        );
        views.push(request_view(node, session, requester.as_deref()).limit(1));
    }
    selected.extend(txn.read_views(&views).await?.into_iter().flatten());
    Ok(selected)
}

fn request_seed_scopes(requests: &[AgentRequestRow]) -> Vec<(String, String, Option<String>)> {
    requests
        .iter()
        .filter_map(|row| {
            Some((
                row.node_did.clone()?,
                row.session_id.clone()?,
                row.requester_did.clone(),
            ))
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn request_context_scopes(requests: &[AgentRequestRow]) -> Vec<(String, String)> {
    requests
        .iter()
        .filter_map(|row| Some((row.node_did.clone()?, row.session_id.clone()?)))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    mod projection_and_patches;
    mod query_edges;
    mod transcript_pagination;
}

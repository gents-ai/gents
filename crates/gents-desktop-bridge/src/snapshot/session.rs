use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};
use gents_desktop_core::client::{ClientCore, ClientStore, SessionTranscriptQueryPage};
use gents_protocol::message::Message;
use gents_protocol::row::{AgentMessageRow, AgentRequestRow, AgentToolCallRow};
use gents_protocol::transcript::{
    normalize_markdown_text, present_message, present_persisted_message,
};

use super::super::cause_derivation::{
    derive_response_cause, derive_tool_call_cause, RequestEvidence, ResponseEvidence,
    ToolCallEvidence,
};
use super::super::types::{
    normalize_optional, turn_state_label, CommandDenialView, DerivedCancelCauseView,
    DesktopSessionSnapshot, GoalView, MessageView, PendingTurnView, ResponseView,
    RetryEligibilityView, SessionCompactionView, SessionContextView, SessionLiveDeltaView,
    SessionLiveTextPatchView, SessionProjectionRevisionView, SessionTimelinePageView, ToolCallView,
    ToolResultView,
};
use super::timeline::{build_rendered_timeline, has_materialized_user_owner};
use super::{request_matches_agent, source_matches_agent};

#[path = "session/command_denial.rs"]
mod command_denial;
#[path = "session/context_projection.rs"]
mod context_projection;
#[path = "session/live_delta.rs"]
mod live_delta;
#[path = "session/pending_turn.rs"]
mod pending_turn;
#[path = "session/projection.rs"]
mod projection;
#[path = "session/request_context.rs"]
mod request_context;
#[path = "session/timeline_page.rs"]
mod timeline_page;

use command_denial::command_denial_from_row;
pub use context_projection::attach_last_request_context;
use context_projection::{build_session_context_from_stores, usize_to_i64};
pub use live_delta::build_session_live_delta;
#[cfg(test)]
pub(crate) use live_delta::build_session_live_delta_from_store;
use pending_turn::{build_pending_turn, project_retry_eligibility};
use projection::build_session_snapshot_from_store_for_agent_with_transcript;
#[cfg(test)]
use request_context::decode_latest_request_context;
use request_context::load_latest_session_request_context;
pub use timeline_page::{apply_session_timeline_page, apply_session_timeline_page_with_query};

pub(super) fn message_is_runtime_control(
    message: &AgentMessageRow,
    requests_by_id: &HashMap<&str, &AgentRequestRow>,
    keyed_steering_request_ids: &std::collections::BTreeSet<String>,
) -> bool {
    let request_metadata = message
        .request_id
        .as_deref()
        .and_then(|request_id| requests_by_id.get(request_id))
        .and_then(|request| request.metadata.as_deref());
    let has_keyed_input = message
        .request_id
        .as_deref()
        .is_some_and(|request_id| keyed_steering_request_ids.contains(request_id));
    gents::lifecycle::is_runtime_control_message(
        request_metadata,
        &message.message_key,
        has_keyed_input,
    )
}

pub(super) fn keyed_steering_request_ids(
    messages: &[&AgentMessageRow],
) -> std::collections::BTreeSet<String> {
    messages
        .iter()
        .filter(|message| gents::lifecycle::is_steering_input_message_key(&message.message_key))
        .filter_map(|message| normalize_optional(message.request_id.as_deref()))
        .collect()
}

pub(super) fn request_is_background_completion(request: &AgentRequestRow) -> bool {
    gents::lifecycle::is_background_completion_request(request.metadata.as_deref())
}

struct LoadedRequestContext {
    request_id: String,
    call_id: String,
    call_sequence: i64,
    accounting: gents_protocol::rendered_request::ContextAccounting,
}

#[cfg(test)]
pub fn build_session_snapshot_from_store(
    store: &gents_desktop_core::client::ClientStore,
    session_id: &str,
    preferred_request_id: Option<&str>,
) -> Option<DesktopSessionSnapshot> {
    build_session_snapshot_from_store_for_agent(store, None, session_id, preferred_request_id)
}

#[cfg(test)]
pub fn build_session_snapshot_from_store_for_agent(
    store: &gents_desktop_core::client::ClientStore,
    agent_did: Option<&str>,
    session_id: &str,
    preferred_request_id: Option<&str>,
) -> Option<DesktopSessionSnapshot> {
    build_session_snapshot_from_store_for_agent_with_transcript(
        store,
        store,
        store,
        false,
        true,
        true,
        agent_did,
        session_id,
        preferred_request_id,
    )
}

/// Build the shared session snapshot and attach the newest durable accounting
/// row for any request in the session. This deliberately does not key the meter
/// off `latest_request_id`: a newly submitted request has no accounting until
/// its first provider dispatch, so the previous measured request remains visible.
#[cfg(test)]
pub async fn build_session_snapshot_for_agent(
    core: &ClientCore,
    agent_did: Option<&str>,
    session_id: &str,
    preferred_request_id: Option<&str>,
) -> Option<DesktopSessionSnapshot> {
    build_session_snapshot_for_agent_with_transcript(
        core,
        agent_did,
        session_id,
        preferred_request_id,
        None,
        None,
        true,
        true,
    )
    .await
}

pub async fn build_session_snapshot_for_agent_with_transcript(
    core: &ClientCore,
    agent_did: Option<&str>,
    session_id: &str,
    preferred_request_id: Option<&str>,
    transcript_store: Option<&ClientStore>,
    context_store: Option<&ClientStore>,
    context_totals_exact: bool,
    include_live_tail: bool,
) -> Option<DesktopSessionSnapshot> {
    let (store, projection_revision) = core.store().snapshot_with_revision();
    let request_ids = agent_did.map_or_else(
        || store.requests_for_session(session_id),
        |agent_did| store.requests_for_session_for_agent(session_id, agent_did),
    );
    let request_ids = request_ids
        .into_iter()
        .map(|request| request.request_id.clone())
        .collect::<Vec<_>>();
    let loaded_context = match agent_did {
        Some(agent_did) => {
            match load_latest_session_request_context(core.node(), agent_did, &request_ids).await {
                Ok(context) => context,
                Err(error) => {
                    tracing::warn!(
                        target: "gents_desktop::chat",
                        agent_did,
                        session_id,
                        error = %error,
                        "loading latest session context accounting failed"
                    );
                    None
                }
            }
        }
        None => None,
    };
    let mut snapshot = build_session_snapshot_from_store_for_agent_with_transcript(
        store.as_ref(),
        transcript_store.unwrap_or(store.as_ref()),
        context_store.or(transcript_store).unwrap_or(store.as_ref()),
        transcript_store.is_some(),
        context_totals_exact,
        include_live_tail,
        agent_did,
        session_id,
        preferred_request_id,
    );
    if let Some(snapshot) = snapshot.as_mut() {
        match loaded_context {
            Some(context) => attach_last_request_context(
                snapshot,
                context.request_id,
                context.call_id,
                context.call_sequence,
                context.accounting,
            ),
            None => {}
        }
        snapshot.projection_revision = Some(SessionProjectionRevisionView {
            store_version: projection_revision.store_version,
            reconcile_version: projection_revision.reconcile_version,
        });
        snapshot.hydration = match agent_did {
            Some(agent_did) => match core.session_hydration_progress(session_id, agent_did).await {
                Ok(progress) => Some(super::to_hydration_view(&progress)),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        session_id,
                        agent_did,
                        "loading session-keyed hydration progress failed"
                    );
                    None
                }
            },
            None => None,
        };
    }
    snapshot
}

#[cfg(test)]
#[path = "session/tests/request_context.rs"]
mod request_context_tests;
#[cfg(test)]
#[path = "session/tests/retry_eligibility.rs"]
mod retry_eligibility_tests;

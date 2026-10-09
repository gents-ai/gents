use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};
use gents::session::canonical_rows::TranscriptMessageRow;
use gents_desktop_core::client::{
    CanonicalTranscriptDependencies, ClientCore, ClientStore, SessionTranscriptQueryPage,
};
use gents_protocol::message::Message;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::{AgentRequestRow, AgentToolCallRow};
use gents_protocol::transcript::present_message;

use super::super::cause_derivation::{derive_tool_call_cause, RequestEvidence, ToolCallEvidence};
use super::super::types::{
    is_live_turn_state, normalize_optional, turn_state_label, CommandDenialView,
    DerivedCancelCauseView, DesktopSessionSnapshot, GoalView, MessageReconstructionView,
    MessageView, PendingTurnView, ReconstructionState, RequestOriginView, RequestOutcomeView,
    RetryEligibilityView, SessionCompactionView, SessionContextView, SessionHydrationView,
    SessionLiveDeltaView, SessionLiveTextPatchView, SessionProjectionRevisionView,
    SessionTimelinePageView, ToolCallView,
};
use super::timeline::build_rendered_timeline;
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
    message: &TranscriptMessageRow,
    requests_by_id: &HashMap<&str, &AgentRequestRow>,
) -> bool {
    let request_input = message
        .message
        .request_doc_id
        .as_deref()
        .and_then(|request_id| requests_by_id.get(request_id))
        .and_then(|request| request.input.as_ref())
        .cloned()
        .unwrap_or_default();
    gents::lifecycle::is_runtime_control_message(&request_input, &message.message.message_key)
}

/// Who put `request` into its session, when not the person. A session
/// message names its sender's session only when this replica holds the exact
/// causing request.
pub(super) fn request_origin_view(
    store: &ClientStore,
    request: &AgentRequestRow,
) -> Option<RequestOriginView> {
    use gents::lifecycle::RequestOrigin;
    Some(match gents::lifecycle::request_origin(request) {
        RequestOrigin::Person => return None,
        RequestOrigin::SessionMessage {
            parent_request_doc_id,
        } => {
            let parent = parent_request_doc_id.and_then(|doc_id| {
                store
                    .requests
                    .iter()
                    .find(|row| row.doc_id.as_deref() == Some(doc_id))
            });
            RequestOriginView::SessionMessage {
                sender_agent_did: parent
                    .and_then(|row| normalize_optional(row.agent_did.as_deref()))
                    .or_else(|| normalize_optional(request.requester_did.as_deref())),
                sender_session_id: parent
                    .and_then(|row| normalize_optional(row.session_id.as_deref())),
                sender_request_id: parent
                    .map(|row| row.request_id.clone())
                    .or_else(|| normalize_optional(request.caused_by_parent_request_id.as_deref())),
            }
        }
        RequestOrigin::Trigger {
            trigger_id,
            trigger_kind,
        } => RequestOriginView::Trigger {
            trigger_id: trigger_id.to_owned(),
            trigger_kind: trigger_kind.map(str::to_owned),
        },
        RequestOrigin::GoalContinuation { goal_id, sequence } => {
            RequestOriginView::GoalContinuation {
                goal_id: goal_id.map(str::to_owned),
                sequence,
            }
        }
        RequestOrigin::BackgroundCompletion => RequestOriginView::BackgroundCompletion,
    })
}

/// The origin of a user-role transcript entry: that of the request it was
/// published under, or a background completion for a delivered notification.
pub(super) fn message_origin(
    store: &ClientStore,
    message: &TranscriptMessageRow,
    requests_by_id: &HashMap<&str, &AgentRequestRow>,
) -> Option<RequestOriginView> {
    if message.message.role != gents_protocol::output::MessageRole::User {
        return None;
    }
    if gents::background_completion::is_background_completion_notification_message_key(
        &message.message.message_key,
    ) {
        return Some(RequestOriginView::BackgroundCompletion);
    }
    message
        .message
        .request_doc_id
        .as_deref()
        .and_then(|request_id| requests_by_id.get(request_id))
        .and_then(|request| request_origin_view(store, request))
}

pub(super) fn request_is_background_completion(request: &AgentRequestRow) -> bool {
    request
        .input
        .as_ref()
        .is_some_and(gents::lifecycle::is_background_completion_request)
}

/// `canonical_rows::authored_message_key` is `pub(crate)` in gents, so its
/// prompt shape is repeated here; the two reconciliations that compare keys
/// (the pending turn's owner and the message projection's owns-turn marker)
/// must not drift apart.
pub(super) fn authored_prompt_message_key(request_doc_id: &str) -> String {
    format!("authored:{request_doc_id}:prompt")
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
        None,
        None,
        false,
        true,
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
    canonical_dependencies: Option<&CanonicalTranscriptDependencies>,
    context_store: Option<&ClientStore>,
    prompt_ownership: Option<&gents_desktop_core::client::RequestPromptOwnership>,
    context_totals_exact: bool,
    include_live_tail: bool,
) -> Option<DesktopSessionSnapshot> {
    let (store, projection_revision) = core.store().snapshot_with_revision();
    let unreadable = agent_did.and_then(|agent_did| {
        core.session_unreadable_reason(session_id, agent_did)
            .map(|reason| super::unreadable_hydration_view(session_id, agent_did, reason))
    });
    let hydration = match agent_did {
        Some(_) if unreadable.is_some() => unreadable,
        Some(agent_did) => match core.session_hydration_status(session_id, agent_did).await {
            Ok((progress, detail)) => Some(super::to_hydration_view(&progress, detail)),
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
        canonical_dependencies,
        prompt_ownership,
        transcript_store.is_some(),
        context_totals_exact,
        context_totals_exact,
        include_live_tail,
        agent_did,
        session_id,
        preferred_request_id,
    );
    if snapshot.is_none() {
        snapshot = hydration
            .as_ref()
            .filter(|hydration| hydration.phase != "idle")
            .map(|hydration| {
                build_hydration_only_session_snapshot(
                    store.as_ref(),
                    session_id,
                    agent_did.expect("hydration progress is keyed by an agent DID"),
                    hydration.clone(),
                    context_totals_exact,
                )
            });
    }
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
            provenance_version: projection_revision.provenance_version,
        });
        snapshot.hydration = hydration;
    }
    snapshot
}

fn build_hydration_only_session_snapshot(
    store: &ClientStore,
    session_id: &str,
    agent_did: &str,
    hydration: SessionHydrationView,
    context_totals_exact: bool,
) -> DesktopSessionSnapshot {
    DesktopSessionSnapshot {
        live_cursor: None,
        session_id: session_id.to_string(),
        agent_did: Some(agent_did.to_string()),
        behavior_id: None,
        title: None,
        preview_text: None,
        status: None,
        goal: None,
        turn_state: None,
        latest_request_id: None,
        retry_eligibility: project_retry_eligibility(None),
        latest_request_outcome: None,
        pending_turn: None,
        queued_turns: Vec::new(),
        folded_inputs: Vec::new(),
        context: build_session_context_from_stores(
            store,
            store,
            Some(agent_did),
            None,
            session_id,
            context_totals_exact,
        ),
        timeline_items: Vec::new(),
        hydration: Some(hydration),
        timeline_page: None,
        projection_revision: None,
        messages: Vec::new(),
        tool_calls: Vec::new(),
    }
}

#[cfg(test)]
mod hydration_only_tests {
    use super::*;

    #[test]
    fn preserves_exact_requester_and_reports_no_unobserved_history() {
        let snapshot = build_hydration_only_session_snapshot(
            &ClientStore::default(),
            "session-1",
            "did:test:requester",
            SessionHydrationView {
                session_id: "session-1".to_string(),
                agent_did: "did:test:requester".to_string(),
                phase: "requested".to_string(),
                merged_count: 0,
                covered_count: 0,
                served_count: None,
                detail: None,
            },
            true,
        );

        assert_eq!(snapshot.session_id, "session-1");
        assert_eq!(snapshot.agent_did.as_deref(), Some("did:test:requester"));
        assert_eq!(
            snapshot.hydration.as_ref().map(|view| view.phase.as_str()),
            Some("requested")
        );
        assert!(snapshot.timeline_items.is_empty());
        assert_eq!(snapshot.context.durable_message_count, 0);
        assert_eq!(
            snapshot.retry_eligibility.denial_reason.as_deref(),
            Some("requestNotObserved")
        );
    }
}

#[cfg(test)]
#[path = "session/tests/request_context.rs"]
mod request_context_tests;
#[cfg(test)]
#[path = "session/tests/retry_eligibility.rs"]
mod retry_eligibility_tests;

#[cfg(test)]
mod paging_coverage_tests {
    use super::*;
    use gents_desktop_core::client::ClientStoreRows;

    #[test]
    fn paged_pending_prompt_uses_lean_read_coverage() {
        let contract: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases = contract["session_document_cases"]["projection"]
            .as_array()
            .unwrap();
        let mut count = 0;
        for case in cases
            .iter()
            .filter(|case| case["operation"] == "tip_coverage")
        {
            count += 1;
            let mut rows = ClientStoreRows {
                requests: vec![AgentRequestRow {
                    doc_id: Some("request-doc".into()),
                    request_id: "request".into(),
                    session_id: Some("session".into()),
                    agent_did: Some("agent".into()),
                    purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
                    content: Some("hello".into()),
                    lifecycle_state: Some(RequestLifecycleState::Processing),
                    ..Default::default()
                }],
                ..Default::default()
            };
            if case["materialized"].as_bool().unwrap() {
                rows.transcript_messages.push(TranscriptMessageRow {
                    doc_id: "prompt-doc".into(),
                    message: serde_json::from_value(serde_json::json!({
                        "message_key":"authored:request-doc:prompt", "session_id":"session",
                        "agent_did":"agent", "request_doc_id":"request-doc", "sequence":0,
                        "role":"user", "outcome":"complete", "blocks":[],
                        "publication":{"kind":"request_execution","execution_generation":"generation"},
                        "created_at":"2026-09-30T00:00:00Z"
                    })).unwrap(),
                });
            }
            let store = ClientStore::from_rows(rows);
            let mut ownership = gents_desktop_core::client::RequestPromptOwnership::default();
            if case["known"].as_bool().unwrap() {
                let doc = if case["observed_request"].as_u64().unwrap() == 1 {
                    "request-doc"
                } else {
                    "other-request-doc"
                };
                ownership.by_request_doc_id.insert(
                    doc.into(),
                    gents_desktop_core::client::RequestPromptFact {
                        agent_did: "agent".into(),
                        session_id: "session".into(),
                        requester_did: None,
                        materialized: case["materialized"].as_bool().unwrap(),
                        first_sequence: None,
                    },
                );
            }
            let page = ClientStore::default();
            let snapshot = build_session_snapshot_from_store_for_agent_with_transcript(
                &store,
                &page,
                &store,
                None,
                Some(&ownership),
                true,
                case["complete"].as_bool().unwrap(),
                false,
                true,
                Some("agent"),
                "session",
                Some("request"),
            )
            .unwrap();
            assert_eq!(
                snapshot.pending_turn.is_some(),
                case["pending"].as_bool().unwrap(),
                "{case}"
            );
            assert_eq!(
                snapshot.context.transcript_totals_exact,
                Some(case["complete"].as_bool().unwrap())
            );
        }
        assert!(count > 0);
    }
}

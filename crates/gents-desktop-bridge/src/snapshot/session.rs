use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use gents::session::canonical_rows::TranscriptMessageRow;
use gents::store::config_client::ConfigAccess;
use gents::tools::background::job::view::{load_job_view, JobKey};
use gents_desktop_core::client::{
    CanonicalTranscriptDependencies, ClientCore, ClientStore, SessionTranscriptQueryPage,
};
use gents_protocol::job::{JobOutcome, JobState};
use gents_protocol::message::Message;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::{AgentRequestRow, AgentToolCallRow};
use gents_protocol::transcript::present_message;

use super::super::cause_derivation::{derive_tool_call_cause, RequestEvidence, ToolCallEvidence};
use super::super::types::{
    is_live_turn_state, normalize_optional, turn_state_label, CommandDenialView,
    DerivedCancelCauseView, DesktopSessionSnapshot, GoalView, MessageReconstructionView,
    MessageView, PendingTurnView, ReconstructionState, RenderedTimelineItem, RequestOriginView,
    RequestOutcomeView, RetryEligibilityView, SessionCompactionView, SessionContextView,
    SessionHydrationView, SessionLiveDeltaView, SessionLiveTextPatchView,
    SessionProjectionRevisionView, SessionTimelinePageView, ToolCallView,
};
use super::timeline::build_rendered_timeline;
use super::{request_matches_node, source_matches_node};

#[path = "session/command_denial.rs"]
mod command_denial;
#[path = "session/context_projection.rs"]
mod context_projection;
#[path = "session/goal_provenance.rs"]
pub(super) mod goal_provenance;
#[path = "session/inference_activity.rs"]
pub(crate) mod inference_activity;
#[path = "session/live_delta.rs"]
mod live_delta;
#[path = "session/pending_turn.rs"]
mod pending_turn;
#[path = "session/projection.rs"]
mod projection;
#[path = "session/request_context.rs"]
mod request_context;
#[path = "session/terminal_tool_output.rs"]
mod terminal_tool_output;
#[path = "session/timeline_page.rs"]
mod timeline_page;

use command_denial::command_denial_from_row;
pub use context_projection::attach_last_request_context;
use context_projection::{build_session_context_from_stores, usize_to_i64};
pub use live_delta::build_session_live_delta;
#[cfg(test)]
pub(crate) use live_delta::build_session_live_delta_from_store;
use pending_turn::{project_pending_turn, project_retry_eligibility};
use projection::build_session_snapshot_from_store_for_node_with_transcript;
#[cfg(test)]
pub(crate) use projection::render_transcript_rows;
pub(crate) use projection::render_transcript_rows_with_provenance;
#[cfg(test)]
use request_context::decode_latest_request_context;
use request_context::load_latest_session_request_context;
pub use request_context::load_session_context_details;
use timeline_page::insert_timeline_item_by_sequence;
pub use timeline_page::{apply_session_timeline_page, apply_session_timeline_page_with_query};

pub(super) fn message_is_runtime_control(
    message: &gents_protocol::output::TranscriptMessage,
    input_request: Option<&AgentRequestRow>,
) -> bool {
    let request_input = input_request
        .and_then(|request| request.input.as_ref())
        .cloned()
        .unwrap_or_default();
    gents::request::lifecycle::is_runtime_control_message(&request_input, &message.message_key)
}

pub(super) fn request_origin_view(
    store: &ClientStore,
    request: &AgentRequestRow,
) -> Option<RequestOriginView> {
    use gents::request::lifecycle::RequestOrigin;
    Some(match gents::request::lifecycle::request_origin(request) {
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
                display_body: parent
                    .and_then(|parent| {
                        gents::request::lifecycle::session_message_display_body(request, parent)
                    })
                    .map(str::to_owned),
                sender_node_did: parent
                    .and_then(|row| normalize_optional(row.node_did.as_deref()))
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

pub(super) fn message_origin(
    store: &ClientStore,
    message: &gents_protocol::output::TranscriptMessage,
    input_request: Option<&AgentRequestRow>,
) -> Option<RequestOriginView> {
    if message.role != gents_protocol::output::MessageRole::User {
        return None;
    }
    if gents::tools::background::completion::is_background_completion_notification_message_key(
        &message.message_key,
    ) {
        return Some(RequestOriginView::BackgroundCompletion);
    }
    input_request.and_then(|request| request_origin_view(store, request))
}

pub(super) fn request_is_background_completion(request: &AgentRequestRow) -> bool {
    request
        .input
        .as_ref()
        .is_some_and(gents::request::lifecycle::is_background_completion_request)
}

pub(super) fn authored_prompt_message_key(request_doc_id: &str) -> String {
    format!("authored:{request_doc_id}:prompt")
}

struct LoadedRequestContext {
    request_id: String,
    request_doc_id: Option<String>,
    anchor_sequence: Option<i64>,
    call_id: String,
    call_sequence: i64,
    queued_at: Option<String>,
    accounting: gents_protocol::rendered_request::ContextAccounting,
}

#[cfg(test)]
pub fn build_session_snapshot_from_store(
    store: &gents_desktop_core::client::ClientStore,
    session_id: &str,
    preferred_request_id: Option<&str>,
) -> Option<DesktopSessionSnapshot> {
    build_session_snapshot_from_store_for_node(store, None, session_id, preferred_request_id)
}

#[cfg(test)]
pub fn build_session_snapshot_from_store_for_node(
    store: &gents_desktop_core::client::ClientStore,
    node_did: Option<&str>,
    session_id: &str,
    preferred_request_id: Option<&str>,
) -> Option<DesktopSessionSnapshot> {
    build_session_snapshot_from_store_for_node_with_transcript(
        store,
        store,
        store,
        None,
        None,
        false,
        true,
        true,
        true,
        node_did,
        session_id,
        preferred_request_id,
        None,
    )
}

#[cfg(test)]
pub async fn build_session_snapshot_for_node(
    core: &ClientCore,
    node_did: Option<&str>,
    session_id: &str,
    preferred_request_id: Option<&str>,
) -> Option<DesktopSessionSnapshot> {
    build_session_snapshot_for_node_with_transcript(
        core,
        &ConfigAccess::local(core.node_arc()),
        node_did,
        session_id,
        preferred_request_id,
        None,
        None,
        None,
        None,
        None,
        true,
        true,
        None,
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "projection inputs come from separate stores and request options"
)]
pub async fn build_session_snapshot_for_node_with_transcript(
    core: &ClientCore,
    access: &ConfigAccess,
    node_did: Option<&str>,
    session_id: &str,
    preferred_request_id: Option<&str>,
    transcript_store: Option<&ClientStore>,
    canonical_dependencies: Option<&CanonicalTranscriptDependencies>,
    context_store: Option<&ClientStore>,
    prompt_ownership: Option<&gents_desktop_core::client::RequestPromptOwnership>,
    hydration_observation: Option<(
        gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress,
        Option<String>,
    )>,
    context_totals_exact: bool,
    include_live_tail: bool,
    request_projection: Option<&[AgentRequestRow]>,
) -> Option<DesktopSessionSnapshot> {
    let (store, projection_revision) = core.store().snapshot_with_revision();
    let unreadable = node_did.and_then(|node_did| {
        core.session_unreadable_reason(session_id, node_did)
            .map(|reason| super::unreadable_hydration_view(session_id, node_did, reason))
    });
    let hydration = match node_did {
        Some(_) if unreadable.is_some() => unreadable,
        Some(node_did) if core.session_read_through_operator(session_id, node_did) => None,
        Some(node_did) => match super::scoped_hydration_observation_view(
            hydration_observation,
            session_id,
            node_did,
        ) {
            Some(view) => Some(view),
            None => match core.session_hydration_status(session_id, node_did).await {
                Ok((progress, detail)) => Some(super::to_hydration_view(&progress, detail)),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        session_id,
                        node_did,
                        "loading session-keyed hydration progress failed"
                    );
                    None
                }
            },
        },
        None => None,
    };
    let observed_queue_ids = store
        .requests
        .iter()
        .filter(|row| {
            row.node_did.as_deref() == node_did
                && row.session_id.as_deref() == Some(session_id)
                && matches!(
                    row.lifecycle_state,
                    Some(
                        RequestLifecycleState::Pending
                            | RequestLifecycleState::WorkspaceBindingPending
                    )
                )
        })
        .filter_map(|row| row.doc_id.clone())
        .collect::<Vec<_>>();
    let mut arrival_order = None;
    let pending_queue = match node_did {
        Some(node_did)
            if include_live_tail
                && core
                    .session_unreadable_reason(session_id, node_did)
                    .is_none() =>
        {
            match core
                .pending_user_queue_projection(node_did, session_id, &observed_queue_ids)
                .await
            {
                Ok(projection) => {
                    arrival_order = Some(projection.arrival_order);
                    Some(
                        projection
                            .pending
                            .entries
                            .into_iter()
                            .map(|entry| crate::types::PendingQueueEntryView {
                                request_doc_id: entry.request_doc_id,
                                request_id: entry.request_id.clone(),
                                content: entry.content,
                                selected_skill_ids: (!entry.selected_skill_ids.is_empty())
                                    .then_some(entry.selected_skill_ids),
                                captured_context: (!entry.captured_context.is_empty())
                                    .then_some(entry.captured_context),
                                attachments: (!entry.attachments.is_empty())
                                    .then_some(entry.attachments),
                                editable: entry.editable,
                                steerable: entry.steerable,
                                edit_group: entry.edit_group,
                            })
                            .collect(),
                    )
                }
                Err(error) => {
                    tracing::warn!(session_id, node_did, error = %error, "pending queue unavailable");
                    None
                }
            }
        }
        _ => None,
    };
    let session_requester = node_did.and_then(|node_did| {
        store
            .sessions
            .iter()
            .find(|session| session.node_did == node_did && session.session_id == session_id)
            .map(|session| session.requester_did.as_deref())
    });
    let loaded_context = match (node_did, session_requester) {
        (Some(node_did), Some(requester_did)) => {
            match load_latest_session_request_context(access, node_did, requester_did, session_id)
                .await
            {
                Ok(context) => context,
                Err(error) => {
                    tracing::warn!(
                        target: "gents_desktop::chat",
                        node_did,
                        session_id,
                        error = %error,
                        "loading latest session context accounting failed"
                    );
                    None
                }
            }
        }
        _ => None,
    };
    let mut snapshot = build_session_snapshot_from_store_for_node_with_transcript(
        store.as_ref(),
        transcript_store.unwrap_or(store.as_ref()),
        context_store.or(transcript_store).unwrap_or(store.as_ref()),
        canonical_dependencies,
        prompt_ownership,
        transcript_store.is_some(),
        context_totals_exact,
        context_totals_exact,
        include_live_tail,
        node_did,
        session_id,
        preferred_request_id,
        request_projection,
    );
    if snapshot.is_none() {
        snapshot = hydration
            .as_ref()
            .filter(|hydration| hydration.phase != "idle")
            .map(|hydration| {
                build_hydration_only_session_snapshot(
                    store.as_ref(),
                    session_id,
                    node_did.expect("hydration progress is keyed by a node DID"),
                    hydration.clone(),
                    context_totals_exact,
                )
            });
    }
    if let Some(snapshot) = snapshot.as_mut() {
        goal_provenance::attach(
            access,
            request_projection.unwrap_or(&store.requests),
            &transcript_store
                .unwrap_or(store.as_ref())
                .transcript_messages,
            canonical_dependencies,
            snapshot,
        )
        .await;
        fill_job_outcomes(core, snapshot).await;
        if let Some(context) = loaded_context {
            attach_last_request_context(
                snapshot,
                context.request_id,
                context.call_id,
                context.call_sequence,
                context.accounting,
            )
        }
        if let (Some(node_did), Some(requester_did)) = (node_did, session_requester) {
            let history = attach_context_history_rows(
                access,
                session_id,
                node_did,
                requester_did,
                &transcript_store
                    .unwrap_or(store.as_ref())
                    .transcript_messages,
                &mut snapshot.timeline_items,
            )
            .await;
            if let Some(entries) = history.compactions {
                snapshot.context.session_compaction_count =
                    Some(i64::try_from(entries.len()).unwrap_or(i64::MAX));
                snapshot.context.compactions = entries
                    .iter()
                    .map(|entry| SessionCompactionView {
                        compaction_key: entry.compaction_key.clone(),
                        sequence: entry.sequence,
                        messages_compacted: entry.messages_compacted.map(|value| value.max(0)),
                        original_tokens: entry.original_tokens,
                        compacted_tokens: entry.compacted_tokens,
                        created_at: entry.created_at.clone(),
                    })
                    .collect();
            }
            snapshot.context.request_reduction_history_loaded = history.reductions_loaded;
        }
        snapshot.projection_revision = Some(SessionProjectionRevisionView {
            store_version: projection_revision.store_version,
            provenance_version: projection_revision.provenance_version,
        });
        snapshot.hydration = hydration;
        snapshot.pending_queue = pending_queue;
        if let Some(arrivals) = arrival_order {
            order_queued_turns(snapshot, &arrivals);
        }
    }
    snapshot
}

/// What a read's context history found: the session's compactions when they
/// loaded, and whether the reductions of the read's requests did.
pub(crate) struct ContextHistory {
    pub(crate) compactions: Option<Vec<gents_protocol::row::CompactionEntryRow>>,
    pub(crate) reductions_loaded: bool,
}

/// Compaction and context-reduction rows for the messages a read holds,
/// placed among `items` by the message each is anchored to. A failed load
/// leaves its rows out; the read itself still stands.
pub(crate) async fn attach_context_history_rows(
    access: &ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    messages: &[TranscriptMessageRow],
    items: &mut Vec<RenderedTimelineItem>,
) -> ContextHistory {
    let page_sequences = messages
        .iter()
        .map(|row| i64::from(row.message.sequence))
        .collect::<RapidHashSet<_>>();
    let compactions = match gents_desktop_core::client::load_session_compaction_entries_on(
        access,
        session_id,
        node_did,
        requester_did,
    )
    .await
    {
        Ok(entries) => {
            attach_session_compaction_timeline_events(items, &entries, &page_sequences);
            Some(entries)
        }
        Err(error) => {
            tracing::warn!(
                target: "gents_desktop::chat",
                session_id,
                node_did,
                error = %error,
                "loading session compaction history failed"
            );
            None
        }
    };
    let request_doc_ids = messages
        .iter()
        .filter_map(|row| row.message.request_doc_id.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let reductions_loaded = match request_context::load_session_request_context_history(
        access,
        node_did,
        session_id,
        requester_did,
        &request_doc_ids,
    )
    .await
    {
        Ok(history) => {
            attach_context_reduction_timeline_events(items, history, &page_sequences);
            true
        }
        Err(error) => {
            tracing::warn!(
                target: "gents_desktop::chat",
                session_id,
                node_did,
                error = %error,
                "loading page context reduction history failed"
            );
            false
        }
    };
    ContextHistory {
        compactions,
        reductions_loaded,
    }
}

fn attach_context_reduction_timeline_events(
    items: &mut Vec<RenderedTimelineItem>,
    history: Vec<LoadedRequestContext>,
    page_sequences: &RapidHashSet<i64>,
) {
    struct RequestSummary {
        sequence: i64,
        request_id: String,
        reductions: i64,
        compacted: i64,
        repaired: i64,
        latest_order: (Option<String>, i64, String),
        latest_estimate: i64,
        latest_timestamp: Option<String>,
    }
    let mut summaries = std::collections::BTreeMap::<String, RequestSummary>::new();
    for entry in history {
        let compacted = match entry.accounting.compaction_reason {
            gents_protocol::rendered_request::ContextCompactionReason::Compacted => true,
            gents_protocol::rendered_request::ContextCompactionReason::ProviderViewRepaired => {
                false
            }
            _ => continue,
        };
        let (Some(request_doc_id), Some(sequence)) = (entry.request_doc_id, entry.anchor_sequence)
        else {
            continue;
        };
        let order = (entry.queued_at.clone(), entry.call_sequence, entry.call_id);
        let summary = summaries
            .entry(request_doc_id)
            .or_insert_with(|| RequestSummary {
                sequence,
                request_id: entry.request_id.clone(),
                reductions: 0,
                compacted: 0,
                repaired: 0,
                latest_order: (None, i64::MIN, String::new()),
                latest_estimate: 0,
                latest_timestamp: None,
            });
        summary.reductions = summary.reductions.saturating_add(1);
        if compacted {
            summary.compacted = summary.compacted.saturating_add(1);
        } else {
            summary.repaired = summary.repaired.saturating_add(1);
        }
        if order > summary.latest_order {
            summary.latest_order = order;
            summary.latest_estimate =
                i64::try_from(entry.accounting.estimated_input_tokens).unwrap_or(i64::MAX);
            summary.latest_timestamp = entry.queued_at;
        }
    }
    for (request_doc_id, summary) in summaries {
        if !page_sequences.contains(&summary.sequence) {
            continue;
        }
        let sequence = summary.sequence;
        let item = RenderedTimelineItem::RequestContextReductionSummary {
            item_key: format!("request-context-reduction:{request_doc_id}"),
            sequence,
            request_doc_id,
            request_id: summary.request_id,
            reduction_count: summary.reductions,
            compacted_count: summary.compacted,
            repaired_count: summary.repaired,
            latest_estimated_input_tokens: Some(summary.latest_estimate),
            latest_timestamp: summary.latest_timestamp,
        };
        insert_timeline_item_by_sequence(items, item, summary.sequence);
    }
}

/// The sync builder holds no `ConfigAccess`, so a failed or timed-out job
/// row's outcome (`load_job_view`) and every job row's next calls are filled
/// here, once per tool call, on both the flat rows and the rendered groups.
async fn fill_job_outcomes(core: &ClientCore, snapshot: &mut DesktopSessionSnapshot) {
    let ended =
        |state: Option<JobState>| matches!(state, Some(JobState::Failed | JobState::TimedOut));
    let mut call_ids = snapshot
        .tool_calls
        .iter()
        .filter(|tool| ended(tool.job_state))
        .filter_map(|tool| tool.tool_call_id.clone())
        .collect::<Vec<_>>();
    for item in &snapshot.timeline_items {
        if let RenderedTimelineItem::ToolGroup { tools, .. } = item {
            call_ids.extend(
                tools
                    .iter()
                    .filter(|tool| ended(tool.job_state))
                    .filter_map(|tool| tool.tool_call_id.clone()),
            );
        }
    }
    call_ids.sort();
    call_ids.dedup();
    let access = ConfigAccess::local(core.node_arc());
    let mut outcomes = RapidHashMap::new();
    for tool_call_id in call_ids {
        let key = JobKey::Address {
            session_id: snapshot.session_id.clone(),
            tool_call_id: tool_call_id.clone(),
        };
        match load_job_view(&access, None, key).await {
            Ok(view) => {
                if let Some(outcome) = view.outcome {
                    outcomes.insert(tool_call_id, outcome);
                }
            }
            Err(error) => tracing::warn!(
                session_id = %snapshot.session_id,
                tool_call_id,
                error = %error,
                "job outcome unavailable"
            ),
        }
    }
    let session_id = snapshot.session_id.as_str();
    for tool in &mut snapshot.tool_calls {
        if let Some((outcome, next_calls)) = job_row_fill(
            session_id,
            tool.tool_call_id.as_deref(),
            tool.job_state,
            &outcomes,
        ) {
            tool.job_outcome = outcome;
            tool.job_next_calls = next_calls;
        }
    }
    for item in &mut snapshot.timeline_items {
        if let RenderedTimelineItem::ToolGroup { tools, .. } = item {
            for tool in tools {
                if let Some((outcome, next_calls)) = job_row_fill(
                    session_id,
                    tool.tool_call_id.as_deref(),
                    tool.job_state,
                    &outcomes,
                ) {
                    tool.job_outcome = outcome;
                    tool.job_next_calls = next_calls;
                }
            }
        }
    }
}

fn attach_session_compaction_timeline_events(
    items: &mut Vec<RenderedTimelineItem>,
    entries: &[gents_protocol::row::CompactionEntryRow],
    page_sequences: &RapidHashSet<i64>,
) {
    for entry in entries {
        let Some(sequence) = entry
            .compacted_through_sequence
            .filter(|sequence| *sequence >= 0 && page_sequences.contains(sequence))
        else {
            continue;
        };
        let item = RenderedTimelineItem::SessionCompaction {
            item_key: format!("session-compaction:{}", entry.compaction_key),
            sequence,
            messages_compacted: entry.messages_compacted.map(|value| value.max(0)),
            original_tokens: entry.original_tokens,
            compacted_tokens: entry.compacted_tokens,
            timestamp: entry.created_at.clone(),
        };
        insert_timeline_item_by_sequence(items, item, sequence);
    }
}

fn job_row_fill(
    session_id: &str,
    tool_call_id: Option<&str>,
    state: Option<JobState>,
    outcomes: &RapidHashMap<String, JobOutcome>,
) -> Option<(Option<JobOutcome>, Vec<String>)> {
    let (state, tool_call_id) = (state?, tool_call_id?);
    let outcome = outcomes.get(tool_call_id).cloned();
    let next_calls = gents_server::jobs::next_calls(state, &format!("{session_id}:{tool_call_id}"));
    Some((outcome, next_calls))
}

pub(super) fn order_queued_turns(snapshot: &mut DesktopSessionSnapshot, arrivals: &[String]) {
    let queued = std::mem::take(&mut snapshot.queued_turns)
        .into_iter()
        .filter_map(|turn| Some((turn.request_doc_id.clone()?, turn)))
        .collect::<RapidHashMap<_, _>>();
    snapshot.queued_turns = arrivals
        .iter()
        .filter_map(|id| queued.get(id).cloned())
        .collect();
}

fn build_hydration_only_session_snapshot(
    store: &ClientStore,
    session_id: &str,
    node_did: &str,
    hydration: SessionHydrationView,
    context_totals_exact: bool,
) -> DesktopSessionSnapshot {
    DesktopSessionSnapshot {
        live_cursor: None,
        session_id: session_id.to_string(),
        node_did: Some(node_did.to_string()),
        agent_id: None,
        title: None,
        preview_text: None,
        status: None,
        goal: None,
        turn_state: None,
        inference_activity: None,
        latest_request_id: None,
        retry_eligibility: project_retry_eligibility(None),
        latest_request_outcome: None,
        pending_turn: None,
        queued_turns: Vec::new(),
        pending_queue: None,
        folded_inputs: Vec::new(),
        context: build_session_context_from_stores(
            store,
            store,
            Some(node_did),
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
                node_did: "did:test:requester".to_string(),
                phase: "requested".to_string(),
                merged_count: 0,
                covered_count: 0,
                served_count: None,
                detail: None,
            },
            true,
        );

        assert_eq!(snapshot.session_id, "session-1");
        assert_eq!(snapshot.node_did.as_deref(), Some("did:test:requester"));
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
#[path = "session/tests/job_rows.rs"]
mod job_rows_tests;
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
    fn many_reduction_rows_collapse_to_one_request_summary_that_pages() {
        let store = ClientStore::from_rows(ClientStoreRows {
            sessions: vec![gents_protocol::session::AgentSession {
                session_id: "session".into(),
                node_did: "node".into(),
                requester_did: None,
                agent_id: "agent".into(),
                created_at: "2026-10-21T00:00:00Z".into(),
                closed_at: None,
                archived_at: None,
                archive_revision: 0,
                stopped_at: None,
                title: None,
                tags: Vec::new(),
                provenance: None,
                observation: None,
            }],
            ..ClientStoreRows::default()
        });
        let mut snapshot = build_session_snapshot_from_store(&store, "session", None).unwrap();
        let history = (0..81)
            .map(|index| LoadedRequestContext {
                request_id: "request-logical".into(),
                request_doc_id: Some("AgentRequest/physical".into()),
                anchor_sequence: Some(1),
                call_id: format!("call-{index}"),
                call_sequence: index,
                queued_at: Some(format!(
                    "2026-10-21T00:{:02}:{:02}Z",
                    index / 60,
                    index % 60
                )),
                accounting: gents_protocol::rendered_request::ContextAccounting {
                    accounting_version:
                        gents_protocol::rendered_request::CONTEXT_ACCOUNTING_VERSION,
                    turn_index: index as usize,
                    attempt: 0,
                    estimator: "test".into(),
                    components: gents_protocol::rendered_request::ContextInputComponents {
                        messages: 100,
                        documents: 0,
                        tool_schemas: 0,
                        additional_parameters: 0,
                        output_schema: 0,
                    },
                    estimated_input_tokens: 100,
                    raw_estimated_input_tokens: None,
                    input_calibration_basis_points: None,
                    anchored_input_tokens: None,
                    context_window: 1_000,
                    compaction_threshold_basis_points: 8000,
                    compaction_threshold_tokens: 800,
                    configured_max_output_tokens: None,
                    effective_max_output_tokens: None,
                    compaction_reason:
                        gents_protocol::rendered_request::ContextCompactionReason::Compacted,
                    pre_compaction_input_tokens: Some(900),
                },
            })
            .collect();
        attach_context_reduction_timeline_events(
            &mut snapshot.timeline_items,
            history,
            &rapidhash::RapidHashSet::from_iter([1]),
        );
        assert!(matches!(
            snapshot.timeline_items.as_slice(),
            [RenderedTimelineItem::RequestContextReductionSummary {
                reduction_count: 81,
                compacted_count: 81,
                ..
            }]
        ));
        snapshot
            .timeline_items
            .push(RenderedTimelineItem::AssistantMessage {
                request_turn: None,
                item_key: "assistant-2".into(),
                request_id: None,
                sequence: Some(2),
                content: None,
                reasoning: None,
                timestamp: None,
                reconstruction: MessageReconstructionView {
                    state: ReconstructionState::Ready,
                    error: None,
                    denied_dependency_doc_id: None,
                },
            });
        let full_snapshot = snapshot;
        let mut newest = full_snapshot.clone();
        apply_session_timeline_page(&mut newest, None, Some(1)).unwrap();
        let cursor = newest.timeline_page.unwrap().oldest_item_key.unwrap();
        assert_eq!(cursor, "assistant-2");
        let mut older = full_snapshot;
        apply_session_timeline_page(&mut older, Some(&cursor), Some(1)).unwrap();
        assert!(matches!(
            older.timeline_items.as_slice(),
            [RenderedTimelineItem::RequestContextReductionSummary {
                reduction_count: 81,
                ..
            }]
        ));
    }

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
                    node_did: Some("node".into()),
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
                        "node_did":"node", "request_doc_id":"request-doc", "sequence":0,
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
                        node_did: "node".into(),
                        session_id: "session".into(),
                        requester_did: None,
                        materialized: case["materialized"].as_bool().unwrap(),
                        first_sequence: None,
                    },
                );
            }
            let page = ClientStore::default();
            let snapshot = build_session_snapshot_from_store_for_node_with_transcript(
                &store,
                &page,
                &store,
                None,
                Some(&ownership),
                true,
                case["complete"].as_bool().unwrap(),
                false,
                true,
                Some("node"),
                "session",
                Some("request"),
                None,
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

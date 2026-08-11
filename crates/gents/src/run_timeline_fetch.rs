//! Row fetch for [`crate::run_timeline`]: loads the persisted documents a
//! request's timeline is reconstructed from, over either transport
//! ([`ConfigAccess::Graphql`] or [`ConfigAccess::Local`]). Lifted from the
//! CLI `trace` command so the desktop client shares one fetcher.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::run_timeline::{
    build_run_timeline, RunTimeline, RunTimelineRows, TimelineCompactionRow,
    TimelineConversationRow, TimelineInferenceCallRow, TimelineMessageRow,
    TimelineRenderedRequestRef, TimelineRenderedRequestRow, TimelineRequestRow,
    TimelineResponseRow, TimelineSessionRow, TimelineToolCallRow,
};
use gents_protocol::graphql::graphql_rows_from_response;

pub async fn load_run_timeline(access: &ConfigAccess, request_id: &str) -> Result<RunTimeline> {
    Ok(build_run_timeline(
        load_run_timeline_rows(access, request_id).await?,
    ))
}

pub async fn load_run_timeline_rows(
    access: &ConfigAccess,
    request_id: &str,
) -> Result<RunTimelineRows> {
    let request = load_timeline_request_by_id(access, request_id).await?;
    let root_session_id = request.session_id.clone();

    let mut requests = match root_session_id.as_deref() {
        Some(session_id) => load_timeline_requests_for_session(access, session_id).await?,
        None => Vec::new(),
    };
    ensure_unique_timeline_request_ids(&requests)?;
    merge_timeline_request(&mut requests, request.clone())?;
    let request_doc_id = request
        .doc_id
        .as_deref()
        .context("timeline root AgentRequest has no _docID")?;
    for child in load_timeline_child_requests(access, request_doc_id).await? {
        if child.caused_by_parent_request_doc_id.as_deref() != Some(request_doc_id) {
            continue;
        }
        if child.caused_by_parent_request_id.as_deref() != Some(request.request_id.as_str()) {
            anyhow::bail!(
                "child AgentRequest {} physical parent is {}, but logical parent is {:?}",
                child.request_id,
                request_doc_id,
                child.caused_by_parent_request_id
            );
        }
        merge_timeline_request(&mut requests, child)?;
    }

    let rendered_request_refs = match request.doc_id.as_deref() {
        Some(request_doc_id) => load_timeline_rendered_request_refs(access, request_doc_id).await?,
        None => Vec::new(),
    };

    let session_ids = timeline_session_ids(&requests);
    let mut messages = Vec::new();
    let mut tool_calls = Vec::new();
    let mut responses = Vec::new();
    let mut compactions = Vec::new();
    for session_id in &session_ids {
        messages.extend(load_timeline_messages_for_session(access, session_id).await?);
        tool_calls.extend(load_timeline_tool_calls_for_session(access, session_id).await?);
        responses.extend(load_timeline_responses_for_session(access, session_id).await?);
        compactions.extend(load_timeline_compactions_for_session(access, session_id).await?);
    }
    if session_ids.is_empty() || root_session_id.is_none() {
        responses.extend(load_timeline_responses_for_request(access, request_doc_id).await?);
    }
    let mut inference_calls = Vec::new();
    for request_doc_id in timeline_request_doc_ids(&requests)? {
        inference_calls
            .extend(load_timeline_inference_calls_for_request(access, &request_doc_id).await?);
    }
    let mut rendered_requests = Vec::new();
    for session_id in &session_ids {
        rendered_requests
            .extend(load_timeline_rendered_requests_for_session(access, session_id).await?);
    }
    if session_ids.is_empty() || root_session_id.is_none() {
        rendered_requests.extend(
            load_timeline_rendered_requests_for_request(access, &request.request_id).await?,
        );
    }

    let request_bindings = timeline_request_bindings(&requests, &request)?;
    // Direct children may have their own sessions.  Session-wide queries also
    // return rows for grandchildren (and later continuation requests) that are
    // not part of this root + direct-child timeline.  Ignore those unrelated
    // rows. Logical-only rows are legacy/unverified and are quarantined from
    // provenance-bearing projections; physical-only or mismatched in-scope
    // claims still fail closed below.
    let in_scope_ids = request_bindings
        .values()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let legacy_logical_only_rows = messages
        .iter()
        .filter(|row| {
            nonempty(row.request_id.as_deref()).is_some_and(|id| in_scope_ids.contains(&id))
                && nonempty(row.request_doc_id.as_deref()).is_none()
        })
        .count()
        + tool_calls
            .iter()
            .filter(|row| {
                nonempty(row.request_id.as_deref()).is_some_and(|id| in_scope_ids.contains(&id))
                    && nonempty(row.request_doc_id.as_deref()).is_none()
            })
            .count()
        + responses
            .iter()
            .filter(|row| {
                in_scope_ids.contains(row.request_id.as_str())
                    && nonempty(row.request_doc_id.as_deref()).is_none()
            })
            .count()
        + inference_calls
            .iter()
            .filter(|row| {
                in_scope_ids.contains(row.request_id.as_str())
                    && nonempty(row.request_doc_id.as_deref()).is_none()
            })
            .count()
        + compactions
            .iter()
            .filter(|row| {
                in_scope_ids.contains(row.request_id.as_str())
                    && nonempty(row.request_doc_id.as_deref()).is_none()
            })
            .count()
        + rendered_requests
            .iter()
            .filter(|row| {
                nonempty(row.request_id.as_deref()).is_some_and(|id| in_scope_ids.contains(&id))
                    && nonempty(row.request_doc_id.as_deref()).is_none()
            })
            .count();
    if legacy_logical_only_rows > 0 {
        tracing::warn!(
            request_id = %request.request_id,
            legacy_logical_only_rows,
            "timeline omitted rows without physical AgentRequest provenance",
        );
    }
    messages.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            row.request_id.as_deref(),
            row.request_doc_id.as_deref(),
        )
    });
    tool_calls.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            row.request_id.as_deref(),
            row.request_doc_id.as_deref(),
        )
    });
    responses.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            Some(row.request_id.as_str()),
            row.request_doc_id.as_deref(),
        )
    });
    inference_calls.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            Some(row.request_id.as_str()),
            row.request_doc_id.as_deref(),
        )
    });
    compactions.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            Some(row.request_id.as_str()),
            row.request_doc_id.as_deref(),
        )
    });
    rendered_requests.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            row.request_id.as_deref(),
            row.request_doc_id.as_deref(),
        )
    });
    validate_request_scoped_rows(
        &request_bindings,
        &messages,
        &tool_calls,
        &responses,
        &inference_calls,
        &compactions,
        &rendered_requests,
    )?;
    validate_child_tool_bridges(&request, &requests, &tool_calls)?;

    let session = match root_session_id.as_deref() {
        Some(session_id) => load_timeline_session(access, session_id).await?,
        None => None,
    };
    let conversation = match root_session_id.as_deref() {
        Some(session_id) => load_timeline_conversation(access, session_id).await?,
        None => None,
    };

    Ok(RunTimelineRows {
        request,
        session,
        conversation,
        requests,
        messages,
        tool_calls,
        inference_calls,
        compactions,
        responses,
        rendered_requests,
        rendered_request_refs,
    })
}

mod context_loaders;
mod event_loaders;
mod query_helpers;
mod request_loaders;
mod validation;

use context_loaders::{load_timeline_conversation, load_timeline_session};
use event_loaders::{
    load_timeline_compactions_for_session, load_timeline_inference_calls_for_request,
    load_timeline_messages_for_session, load_timeline_rendered_request_refs,
    load_timeline_rendered_requests_for_request, load_timeline_rendered_requests_for_session,
    load_timeline_responses_for_request, load_timeline_responses_for_session,
    load_timeline_tool_calls_for_session,
};
use query_helpers::load_rows;
use request_loaders::{
    load_timeline_child_requests, load_timeline_request_by_id, load_timeline_requests_for_session,
};
use validation::{
    ensure_unique_timeline_request_ids, merge_timeline_request, nonempty,
    request_scoped_row_is_in_timeline, timeline_request_bindings, timeline_request_doc_ids,
    timeline_session_ids, validate_child_tool_bridges, validate_request_scoped_rows,
};

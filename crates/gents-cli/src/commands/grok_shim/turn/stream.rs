use rapidhash::{HashSetExt, RapidHashSet};
use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use gents::store::graphql::graphql_with_transaction_retry;
use serde_json::Value;
use tokio::sync::Mutex;

use super::super::projection::{
    AsyncCommit, CursorAdvance, NovelProjectionEvent, ProjectionEngine, RequestCursor,
    UpdateTimestamps,
};
use super::sender::{PromptSender, PromptSenderLine};
use super::{ObservedRequest, RequestUpdateTiming, TurnManager};

struct ProjectionCursorCommit<'a> {
    cursor: &'a tokio::sync::Mutex<RequestCursor>,
    advance: CursorAdvance,
}

impl AsyncCommit for ProjectionCursorCommit<'_> {
    async fn commit(&self) {
        self.cursor.lock().await.record(self.advance.clone());
    }
}

impl TurnManager {
    #[expect(
        clippy::too_many_arguments,
        reason = "per-prompt stream state borrowed from separate owners"
    )]
    pub(super) async fn stream_projection_updates(
        &self,
        session_id: &str,
        request_id: &str,
        prompt_id: Option<&str>,
        sender: &PromptSender,
        projections: &ProjectionEngine,
        cursor: &tokio::sync::Mutex<RequestCursor>,
        update_timing: &mut RequestUpdateTiming,
        include_notifications: bool,
        child_depth: usize,
    ) -> Result<()> {
        self.stream_projection_updates_mode(
            session_id,
            request_id,
            prompt_id,
            sender,
            projections,
            cursor,
            update_timing,
            include_notifications,
            child_depth,
            false,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "per-prompt stream state borrowed from separate owners"
    )]
    pub(super) async fn stream_projection_updates_mode(
        &self,
        session_id: &str,
        request_id: &str,
        prompt_id: Option<&str>,
        sender: &PromptSender,
        projections: &ProjectionEngine,
        cursor: &tokio::sync::Mutex<RequestCursor>,
        update_timing: &mut RequestUpdateTiming,
        include_notifications: bool,
        child_depth: usize,
        activity_only: bool,
    ) -> Result<()> {
        let prompt_id = if activity_only { None } else { prompt_id };
        let request = self
            .pinned_projection_request(cursor, session_id, request_id, child_depth == 0)
            .await?;
        let batch = {
            let mut cursor = cursor.lock().await;
            projections
                .project_request_updates(&request, &mut cursor, prompt_id)
                .await?
        };
        let mut deferred_notifications = false;
        let mut child_finishes = Vec::new();
        for event in batch.events {
            let event = if child_depth > 0 {
                cursor.lock().await.child_output_event(event)
            } else {
                event
            };
            if activity_only && !is_background_activity(&event.payload) {
                continue;
            }
            if event.payload.get("sessionUpdate").and_then(Value::as_str)
                == Some("subagent_finished")
            {
                child_finishes.push(event);
                continue;
            }
            if !include_notifications
                && event.payload.get("sessionUpdate").and_then(Value::as_str)
                    == Some("user_message_chunk")
            {
                deferred_notifications = true;
                continue;
            }
            self.send_projection_event(
                session_id,
                prompt_id,
                event,
                sender,
                projections,
                cursor,
                update_timing,
                activity_only,
            )
            .await?;
        }
        let deferred_children = self
            .stream_readable_child_updates(session_id, sender, projections, cursor, child_depth)
            .await?;
        for event in child_finishes {
            if event
                .payload
                .get("child_session_id")
                .and_then(Value::as_str)
                .is_some_and(|session| deferred_children.contains(session))
            {
                continue;
            }
            self.send_projection_event(
                session_id,
                prompt_id,
                event,
                sender,
                projections,
                cursor,
                update_timing,
                activity_only,
            )
            .await?;
        }
        if !activity_only && !batch.trailing_advances.is_empty() {
            let mut cursor = cursor.lock().await;
            for advance in batch.trailing_advances {
                if deferred_notifications
                    && matches!(advance, CursorAdvance::MessageHighWater { .. })
                {
                    continue;
                }
                cursor.record(advance);
            }
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "per-prompt stream state borrowed from separate owners"
    )]
    async fn send_projection_event(
        &self,
        session_id: &str,
        prompt_id: Option<&str>,
        event: NovelProjectionEvent,
        sender: &PromptSender,
        projections: &ProjectionEngine,
        cursor: &Mutex<RequestCursor>,
        timing: &mut RequestUpdateTiming,
        activity_only: bool,
    ) -> Result<()> {
        let timestamps = if activity_only {
            UpdateTimestamps::default()
        } else {
            timing.resolve(event.timing.as_ref(), chrono::Utc::now().timestamp_millis())
        };
        let commit = ProjectionCursorCommit {
            cursor,
            advance: event.advance,
        };
        projections
            .session_updates()
            .send_with_commit(
                session_id,
                move |event_id, total_tokens| {
                    let meta = super::super::projection::stamp_update_meta(
                        event_id,
                        total_tokens,
                        prompt_id,
                        None,
                        timestamps,
                    );
                    Ok(super::super::projection::session_notification_for_method(
                        event.method,
                        session_id,
                        event.payload,
                        meta,
                    ))
                },
                PromptSenderLine(sender),
                commit,
            )
            .await?;
        Ok(())
    }

    async fn stream_readable_child_updates(
        &self,
        parent_session_id: &str,
        sender: &PromptSender,
        projections: &ProjectionEngine,
        parent_cursor: &Mutex<RequestCursor>,
        depth: usize,
    ) -> Result<BTreeSet<String>> {
        let mut deferred = BTreeSet::new();
        let parent = parent_cursor
            .lock()
            .await
            .request
            .clone()
            .context("parent projection identity missing")?;
        for child in
            super::super::projection::caused_sessions::caused_by_request(&self.node, &parent)
                .await?
        {
            let session_id = child.scope.session_id.as_str();
            if session_id == parent_session_id
                || !parent_cursor
                    .lock()
                    .await
                    .subagent_spawn_was_delivered(session_id)
            {
                continue;
            }
            for row in self.readable_child_session_requests(&child.scope).await? {
                let request_id = row.request_id.clone();
                let started_at = row
                    .created_at
                    .as_deref()
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                    .map(|value| value.timestamp_millis())
                    .unwrap_or(0);
                let progress = self
                    .observed
                    .lock()
                    .await
                    .entry((session_id.to_owned(), request_id.clone()))
                    .or_insert_with(|| {
                        Arc::new(Mutex::new(ObservedRequest::new(
                            request_id.clone(),
                            started_at,
                            true,
                        )))
                    })
                    .clone();
                let Ok(mut progress) = progress.try_lock() else {
                    deferred.insert(session_id.to_owned());
                    continue;
                };
                {
                    let mut cursor = progress.cursor.lock().await;
                    if let Some(pinned) = &cursor.request {
                        anyhow::ensure!(
                            pinned.doc_id == row.doc_id
                                && pinned.node_did == row.node_did
                                && pinned.requester_did == row.requester_did,
                            "child projection cannot rebind physical request"
                        );
                    } else {
                        cursor.request = Some(row);
                    }
                }
                let projected = progress
                    .cursor
                    .lock()
                    .await
                    .request
                    .clone()
                    .expect("verified child identity pinned");
                let terminal = self.request_stop_reason(&projected).await?;
                if terminal.is_none() {
                    deferred.insert(session_id.to_owned());
                }
                let ObservedRequest { cursor, timing, .. } = &mut *progress;
                Box::pin(self.stream_projection_updates(
                    session_id,
                    &request_id,
                    None,
                    sender,
                    projections,
                    cursor,
                    timing,
                    terminal.is_some() || matches!(sender, PromptSender::Replay { .. }),
                    depth + 1,
                ))
                .await?;
            }
        }
        Ok(deferred)
    }

    async fn readable_child_session_requests(
        &self,
        child: &crate::caused_sessions::SessionScope,
    ) -> Result<Vec<gents_protocol::row::AgentRequestRow>> {
        let session = child.session_id.as_str();
        let node = child.node_did.as_str();
        let scope = gents::session::public_request_filter(&gents::session::session_scope_filter(
            node,
            session,
            child.requester_did.as_deref(),
        ));
        let mut seen = RapidHashSet::new();
        let mut requests = Vec::new();
        let mut offset = 0usize;
        loop {
            let query = format!(
                "{{AgentRequest(filter: {{{scope}}}, order: {{request_id: ASC}}, limit: 128, offset: {offset}) {{{}}}}}",
                gents::SIGNED_REQUEST_FIELDS
            );
            let response = graphql_with_transaction_retry(
                &self.node,
                &query,
                "read scoped child session requests",
            )
            .await?;
            let rows = response
                .data
                .as_ref()
                .and_then(|data| data.get("AgentRequest"))
                .and_then(Value::as_array)
                .context("child session query missing rows")?;
            anyhow::ensure!(rows.len() <= 128, "child session query exceeded page limit");
            for value in rows {
                let row: gents_protocol::row::AgentRequestRow =
                    serde_json::from_value(value.clone())?;
                anyhow::ensure!(
                    row.node_did.as_deref() == Some(node)
                        && row.session_id.as_deref() == Some(session)
                        && row.requester_did == child.requester_did
                        && row.doc_id.as_deref().is_some_and(|id| !id.is_empty())
                        && seen.insert(row.request_id.clone()),
                    "child session query crossed scope or has ambiguous request labels"
                );
                requests.push(row);
            }
            if rows.len() < 128 {
                break;
            }
            offset = offset
                .checked_add(rows.len())
                .context("child session pagination overflow")?;
        }
        requests
            .sort_by(|a, b| (&a.created_at, &a.request_id).cmp(&(&b.created_at, &b.request_id)));
        Ok(requests)
    }

    pub(super) async fn pinned_projection_request(
        &self,
        cursor: &Mutex<RequestCursor>,
        session: &str,
        request_id: &str,
        allow_root_read: bool,
    ) -> Result<gents_protocol::row::AgentRequestRow> {
        let mut cursor = cursor.lock().await;
        if cursor.request.is_none() {
            anyhow::ensure!(
                allow_root_read,
                "projection requires verified physical request receipt"
            );
            cursor.request = Some(
                self.load_projection_request(
                    &self.config.node_did,
                    session,
                    Some(&self.config.node_did),
                    request_id,
                    None,
                )
                .await?,
            );
        }
        let request = cursor.request.clone().expect("projection identity pinned");
        anyhow::ensure!(
            request.request_id == request_id && request.session_id.as_deref() == Some(session),
            "projection cursor cannot change request scope"
        );
        Ok(request)
    }
}

fn is_background_activity(payload: &Value) -> bool {
    matches!(
        payload["sessionUpdate"].as_str(),
        Some(
            "tool_call"
                | "tool_call_update"
                | "task_backgrounded"
                | "task_completed"
                | "monitor_event"
                | "subagent_spawned"
                | "subagent_progress"
                | "subagent_finished"
        )
    )
}

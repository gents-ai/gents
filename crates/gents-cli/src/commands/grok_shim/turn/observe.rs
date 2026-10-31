use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use gents::store::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::super::projection::{ProjectionEngine, UpdateTimestamps, SESSION_UPDATE_METHOD};
use super::sender::{PromptSender, PromptSenderLine};
use super::{subscribe_projected_changes, wait_for_projected_change, ObservedRequest, TurnManager};

impl TurnManager {
    pub(in crate::commands::grok_shim) async fn observe_session(
        self: &Arc<Self>,
        session_id: &str,
        sender: PromptSender,
        projections: Arc<ProjectionEngine>,
    ) {
        self.observe_session_since(
            session_id,
            chrono::Utc::now().to_rfc3339(),
            sender,
            projections,
        )
        .await;
    }

    pub(in crate::commands::grok_shim) async fn observe_session_since(
        self: &Arc<Self>,
        session_id: &str,
        attached_at: String,
        sender: PromptSender,
        projections: Arc<ProjectionEngine>,
    ) {
        let mut observers = self.observers.lock().await;
        if observers.contains_key(session_id) || self.state.lock().await.closed {
            return;
        }
        let session_id = session_id.to_owned();
        let manager = Arc::downgrade(self);
        let observed_session = session_id.clone();
        let mut changes = subscribe_projected_changes(&self.node);
        observers.insert(
            session_id,
            tokio::spawn(async move {
                let mut first_pass = true;
                let mut after = String::new();
                let mut delivery_after = String::new();
                let mut goal_cursor = super::super::goals::GoalCursor::default();
                let mut job_cursor = super::super::job_runs::JobRunCursor::default();
                let mut goal_refresh_at = tokio::time::Instant::now();
                loop {
                    if !std::mem::take(&mut first_pass) {
                        wait_for_projected_change(&mut changes).await;
                    }
                    let Some(manager) = manager.upgrade() else {
                        break;
                    };
                    if manager.state.lock().await.closed {
                        break;
                    }
                    if tokio::time::Instant::now() >= goal_refresh_at {
                        goal_refresh_at = tokio::time::Instant::now() + Duration::from_secs(1);
                        if let Err(error) = goal_cursor
                            .refresh(
                                &manager.node,
                                &manager.config.node_did,
                                &observed_session,
                                &sender,
                                &projections,
                            )
                            .await
                        {
                            tracing::warn!(%error, session_id = %observed_session,
                            "Grok goal observation failed; retrying");
                        }
                        for error in job_cursor
                            .refresh(
                                &manager.node,
                                &manager.config.node_did,
                                &observed_session,
                                &sender,
                                &projections,
                            )
                            .await
                        {
                            tracing::warn!(error = %format_args!("{error:#}"),
                            session_id = %observed_session, "Grok job observation failed");
                        }
                    }
                    if let Err(error) = manager
                        .observe_session_tick(
                            &observed_session,
                            &attached_at,
                            &mut after,
                            &mut delivery_after,
                            &sender,
                            &projections,
                        )
                        .await
                    {
                        tracing::warn!(%error, session_id = %observed_session,
                        "Grok shim session observation failed; retrying");
                    }
                }
            }),
        );
    }

    pub(in crate::commands::grok_shim) async fn replay_session(
        &self,
        session_id: &str,
        rows: &[gents_protocol::row::AgentRequestRow],
        sender: &PromptSender,
        projections: &ProjectionEngine,
    ) -> Result<Option<String>> {
        let replay = PromptSender::Replay {
            inner: Arc::new(sender.clone()),
        };
        let mut running = None;
        for row in rows {
            let started_at = row
                .created_at
                .as_deref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|time| time.timestamp_millis())
                .unwrap_or(0);
            let live_prompt_id = format!("notifications-{}", row.request_id);
            let prompt_id = if row.runtime_source_kind.is_some() {
                live_prompt_id.clone()
            } else {
                row.request_id.clone()
            };
            let mut progress = ObservedRequest::new(live_prompt_id.clone(), started_at, false);
            progress.cursor.lock().await.request = Some(row.clone());
            if let Some(content) = row.content.as_deref().filter(|value| !value.is_empty()) {
                projections.session_updates().send(session_id, |event_id, total_tokens| {
                    Ok(super::super::projection::session_notification_for_method(SESSION_UPDATE_METHOD, session_id,
                        json!({"sessionUpdate":"user_message_chunk", "content":{"type":"text", "text":content},
                            "_meta":{"hideFromScrollback":row.runtime_source_kind.is_some()}}),
                        super::super::projection::stamp_update_meta(event_id, total_tokens, Some(&prompt_id), Some(true),
                            UpdateTimestamps { agent_timestamp_ms: Some(started_at), ..Default::default() }),
                    ))
                }, PromptSenderLine(&replay)).await?;
            }
            let terminal = self.request_stop_reason(row).await?;
            self.stream_projection_updates(
                session_id,
                &row.request_id,
                Some(&prompt_id),
                &replay,
                projections,
                &progress.cursor,
                &mut progress.timing,
                true,
                0,
            )
            .await?;
            if let Some(reason) = terminal {
                projections.session_updates().send(session_id, |event_id, total_tokens| {
                    Ok(super::super::projection::session_notification_for_method("x.ai/session_notification", session_id,
                        json!({"sessionUpdate":"turn_completed", "prompt_id":prompt_id, "stop_reason":reason.wire_name()}),
                        super::super::projection::stamp_update_meta(event_id, total_tokens, Some(&prompt_id), Some(true), UpdateTimestamps::default()),
                    ))
                }, PromptSenderLine(&replay)).await?;
                progress.completion_sent = true;
                progress.prompt_id = None;
            } else if running.is_none() {
                running = Some((row.request_id.clone(), live_prompt_id));
            }
            self.observed.lock().await.insert(
                (session_id.to_owned(), row.request_id.clone()),
                Arc::new(Mutex::new(progress)),
            );
        }
        if let Some((request_id, _)) = &running {
            self.autonomous_delivery
                .lock()
                .await
                .insert(session_id.to_owned(), request_id.clone());
        }
        Ok(running.map(|(_, prompt)| prompt))
    }

    pub(super) async fn observe_session_tick(
        &self,
        session_id: &str,
        attached_at: &str,
        after: &mut String,
        delivery_after: &mut String,
        sender: &PromptSender,
        projections: &ProjectionEngine,
    ) -> Result<()> {
        const PAGE_SIZE: usize = 128;
        let attached_at = chrono::DateTime::parse_from_rfc3339(attached_at)?
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let node = escape_graphql_string(&self.config.node_did);
        let scope = gents::session::public_request_filter(&format!(
            r#"session_id: {{ _eq: "{}" }}, node_did: {{ _eq: "{node}" }}, requester_did: {{ _eq: "{node}" }}, created_at: {{ _gte: "{}" }}, request_id: {{ _gt: "{}" }}"#,
            escape_graphql_string(session_id),
            escape_graphql_string(&attached_at),
            escape_graphql_string(after),
        ));
        let query = format!(
            r#"{{ AgentRequest(filter: {{ {scope} }},
                order: [{{ request_id: ASC }}, {{ _docID: ASC }}], limit: {PAGE_SIZE}) {{ {fields} }} }}"#,
            fields = gents::SIGNED_REQUEST_FIELDS,
        );
        let response = graphql_with_transaction_retry(
            &self.node,
            &query,
            "Grok shim session request discovery",
        )
        .await?;
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .context("missing session request discovery rows")?;
        for row in rows {
            let request: gents_protocol::row::AgentRequestRow =
                serde_json::from_value(row.clone())?;
            anyhow::ensure!(
                request.node_did.as_deref() == Some(self.config.node_did.as_str())
                    && request.requester_did.as_deref() == Some(self.config.node_did.as_str())
                    && request.session_id.as_deref() == Some(session_id)
                    && request.doc_id.as_deref().is_some_and(|id| !id.is_empty()),
                "observed request query crossed physical scope"
            );
            let request_id = row
                .get("request_id")
                .and_then(Value::as_str)
                .context("session request missing request_id")?;
            let state = self.state.lock().await;
            if state.entries.iter().any(|((session, _), pending)| {
                session == session_id
                    && pending
                        .request
                        .as_ref()
                        .map(|row| row.request_id.as_str())
                        .is_none_or(|id| id == request_id)
            }) {
                continue;
            }
            let started_at = row
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.timestamp_millis())
                .unwrap_or(0);
            self.observed
                .lock()
                .await
                .entry((session_id.to_owned(), request_id.to_owned()))
                .or_insert_with(|| {
                    let mut progress = ObservedRequest::new(
                        format!("notifications-{request_id}"),
                        started_at,
                        false,
                    );
                    progress.echo_sent = false;
                    progress.cursor.get_mut().request = Some(request);
                    Arc::new(Mutex::new(progress))
                });
        }
        *after = if rows.len() == PAGE_SIZE {
            rows.last()
                .and_then(|row| row.get("request_id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        } else {
            String::new()
        };

        const DELIVERY_PAGE_SIZE: usize = 32;
        let mut candidates: Vec<_> = self
            .observed
            .lock()
            .await
            .iter()
            .filter(|((session, _), _)| session == session_id)
            .map(|((_, id), progress)| {
                let autonomous = progress
                    .try_lock()
                    .map(|p| p.prompt_id.is_some())
                    .unwrap_or(false);
                (
                    format!("{}:{id}", u8::from(autonomous)),
                    id.clone(),
                    progress.clone(),
                )
            })
            .collect();
        candidates.sort_by(|a, b| a.0.cmp(&b.0));
        candidates.retain(|(key, _, _)| key > &*delivery_after);
        candidates.truncate(DELIVERY_PAGE_SIZE);
        *delivery_after = if candidates.len() == DELIVERY_PAGE_SIZE {
            candidates
                .last()
                .map(|(key, _, _)| key.clone())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let mut requests: Vec<_> = candidates
            .into_iter()
            .map(|(_, id, progress)| (id, progress))
            .collect();
        if let Some(active) = self
            .autonomous_delivery
            .lock()
            .await
            .get(session_id)
            .cloned()
        {
            if !requests.iter().any(|(id, _)| id == &active) {
                if let Some(progress) = self
                    .observed
                    .lock()
                    .await
                    .get(&(session_id.to_owned(), active.clone()))
                    .cloned()
                {
                    requests.push((active, progress));
                }
            }
        }
        for (request_id, progress) in requests {
            let _delivery = self.delivery_gate.lock().await;
            let foreground_busy = self
                .foreground_deliveries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(session_id);
            let Ok(mut progress) = progress.try_lock() else {
                continue;
            };
            let request_busy = {
                let state = self.state.lock().await;
                if state.closed {
                    continue;
                }
                state
                    .entries
                    .keys()
                    .any(|(session, _)| session == session_id)
            };
            let active = self
                .autonomous_delivery
                .lock()
                .await
                .get(session_id)
                .cloned();
            if foreground_busy
                || request_busy
                || active.as_deref().is_some_and(|id| id != request_id)
            {
                let ObservedRequest {
                    prompt_id,
                    cursor,
                    timing,
                    ..
                } = &mut *progress;
                self.stream_projection_updates_mode(
                    session_id,
                    &request_id,
                    prompt_id.as_deref(),
                    sender,
                    projections,
                    cursor,
                    timing,
                    false,
                    0,
                    true,
                )
                .await?;
                continue;
            }
            let continuing_autonomous = active.is_some();
            if progress.prompt_id.is_some() && !continuing_autonomous {
                self.autonomous_delivery
                    .lock()
                    .await
                    .insert(session_id.to_owned(), request_id.clone());
            }
            if !progress.echo_sent {
                if let Some((prompt_id, content)) =
                    self.observed_human_prompt(session_id, &request_id).await?
                {
                    projections.session_updates().send(session_id, |event_id, total_tokens| {
                        Ok(super::super::projection::session_notification_for_method(SESSION_UPDATE_METHOD, session_id,
                            json!({"sessionUpdate":"user_message_chunk", "content":{"type":"text","text":content},
                                "_meta":{"hideFromScrollback":false,"promptIndex":0}}),
                            super::super::projection::stamp_update_meta(event_id, total_tokens, Some(&prompt_id), Some(false),
                                UpdateTimestamps { agent_timestamp_ms:Some(progress.timing.turn_start_ms),
                                    turn_start_ms:Some(progress.timing.turn_start_ms), ..Default::default() }),
                        ))
                    }, PromptSenderLine(sender)).await?;
                    progress.prompt_id = Some(prompt_id);
                }
                progress.echo_sent = true;
            }
            let projected = self
                .pinned_projection_request(&progress.cursor, session_id, &request_id, true)
                .await?;
            let terminal = self.request_stop_reason(&projected).await?;
            let ObservedRequest {
                prompt_id,
                cursor,
                timing,
                ..
            } = &mut *progress;
            self.stream_projection_updates(
                session_id,
                &request_id,
                prompt_id.as_deref(),
                sender,
                projections,
                cursor,
                timing,
                !continuing_autonomous,
                0,
            )
            .await?;
            if !progress.completion_sent {
                if let Some(reason) = terminal {
                    let prompt_id = progress
                        .prompt_id
                        .clone()
                        .context("missing autonomous prompt ID")?;
                    projections.session_updates().send(session_id, move |event_id, total_tokens| {
                        Ok(super::super::projection::session_notification_for_method(
                            "x.ai/session_notification", session_id,
                            json!({"sessionUpdate": "turn_completed", "prompt_id": prompt_id,
                                "stop_reason": reason.wire_name()}),
                            super::super::projection::stamp_update_meta(event_id, total_tokens,
                                Some(&prompt_id), None, UpdateTimestamps::default()),
                        ))
                    }, PromptSenderLine(sender)).await?;
                    progress.completion_sent = true;
                    progress.prompt_id = None;
                    self.autonomous_delivery.lock().await.remove(session_id);
                    delivery_after.clear();
                    break;
                }
            }
        }
        Ok(())
    }
}

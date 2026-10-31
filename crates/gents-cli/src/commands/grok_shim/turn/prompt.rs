use std::sync::Arc;

use anyhow::{Context as _, Result};
use gents::store::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use serde_json::{json, Value};
use tokio::sync::{oneshot, Mutex};

use super::super::projection::ProjectionEngine;
use super::sender::PromptSender;
use super::wire::{prompt_result, CancelNotification, PromptRequest, StopReason};
use super::{
    subscribe_projected_changes, CancelBeforeIdLatch, CancelLatch, ObservedRequest, PendingPrompt,
    PromptKey, TurnManager,
};

impl TurnManager {
    pub(in crate::commands::grok_shim) async fn handle_prompt(
        &self,
        request: PromptRequest,
        sender: &PromptSender,
        projections: &ProjectionEngine,
    ) -> Result<Value> {
        if request.prompt.is_empty() {
            anyhow::bail!("session/prompt requires at least one prompt block");
        }
        let turn_start_ms = chrono::Utc::now().timestamp_millis();
        let prompt_id = request
            .prompt_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let key = (request.session_id.clone(), prompt_id.clone());
        let (response_tx, response_rx) = oneshot::channel::<Result<Value>>();
        let cancel_before_id = Arc::new(Mutex::new(CancelBeforeIdLatch::default()));
        #[cfg(test)]
        {
            let gate = self.insertion_gate.lock().await.clone();
            if let Some(gate) = gate {
                gate.arrived.notify_one();
                gate.release.notified().await;
            }
        }
        {
            let _admission = self.delivery_gate.lock().await;
            let mut state = self.state.lock().await;
            if state.is_closed() {
                anyhow::bail!("connection already disconnected");
            }
            if state
                .entries
                .keys()
                .any(|(session, _)| session == &request.session_id)
            {
                anyhow::bail!("session already has a live prompt");
            }
            state.entries.insert(
                key.clone(),
                PendingPrompt {
                    response_tx: Some(response_tx),
                    request: None,
                    cancel_before_id: cancel_before_id.clone(),
                    drained: false,
                },
            );
        }

        tracing::debug!(session_id = %request.session_id, prompt_id = %prompt_id, "Grok shim submitting prompt request");
        let progress = Arc::new(Mutex::new(ObservedRequest::new(
            prompt_id.clone(),
            turn_start_ms,
            true,
        )));
        let mut progress_guard = progress.lock().await;
        let submission = self.submit_request(&request, &prompt_id).await;
        let (request_id, submitted_request) = match submission {
            Ok(row) => {
                let request_id = row.request_id.clone();
                progress_guard.cursor.lock().await.request = Some(row.clone());
                let registered_own_entry = {
                    let mut state = self.state.lock().await;
                    match state.entries.get_mut(&key) {
                        Some(entry) if Arc::ptr_eq(&entry.cancel_before_id, &cancel_before_id) => {
                            entry.request = Some(row.clone());
                            self.observed.lock().await.insert(
                                (request.session_id.clone(), request_id.clone()),
                                progress.clone(),
                            );
                            true
                        }
                        _ => false,
                    }
                };
                let cancelled_before_id = cancel_before_id.lock().await.is_cancelled();
                if !registered_own_entry || cancelled_before_id {
                    self.interrupt_submitted(&row).await;
                    drop(response_rx);
                    tracing::info!(
                        session_id = %request.session_id,
                        prompt_id = %prompt_id,
                        request_id = %request_id,
                        registered_own_entry,
                        cancelled_before_id,
                        "Grok shim prompt was drained during submission; interrupting submitted request"
                    );
                    return Ok(json!({"stopReason": StopReason::Cancelled.wire_name()}));
                }
                (request_id, row)
            }
            Err(error) => {
                let (own_entry, state_closed) =
                    self.take_entry_if_generation(&key, &cancel_before_id).await;
                let own_entry_was_present = own_entry.is_some();
                let cancelled_by_latch = cancel_before_id.lock().await.is_cancelled();
                let cancelled_by_disconnect = state_closed && !own_entry_was_present;
                let cancelled_by_drain = !own_entry_was_present;
                drop(response_rx);
                if cancelled_by_disconnect || cancelled_by_drain || cancelled_by_latch {
                    tracing::info!(
                        %error,
                        session_id = %request.session_id,
                        prompt_id = %prompt_id,
                        state_closed,
                        own_entry_was_present,
                        cancelled_by_latch,
                        "Grok shim prompt submission failed into a cancelled/disconnected turn; resolving cancelled"
                    );
                    return Ok(json!({"stopReason": StopReason::Cancelled.wire_name()}));
                }
                tracing::warn!(
                    %error,
                    session_id = %request.session_id,
                    prompt_id = %prompt_id,
                    "Grok shim prompt submission failed"
                );
                return Err(error);
            }
        };

        let drained_during_submission = {
            let state = self.state.lock().await;
            state.entries.get(&key).is_none_or(|entry| {
                !Arc::ptr_eq(&entry.cancel_before_id, &cancel_before_id) || entry.drained
            })
        };
        if drained_during_submission {
            self.interrupt_submitted(&submitted_request).await;
            drop(response_rx);
            tracing::info!(
                session_id = %request.session_id,
                prompt_id = %prompt_id,
                request_id = %request_id,
                "Grok shim prompt drained before user echo; interrupting submitted request"
            );
            return Ok(json!({"stopReason": StopReason::Cancelled.wire_name()}));
        }
        for (index, block) in request.prompt.iter().enumerate() {
            if let Err(error) = sender
                .send_user_message_chunk(
                    projections.session_updates(),
                    &request.session_id,
                    &prompt_id,
                    block,
                    index,
                )
                .await
            {
                self.interrupt_and_drain(&key, &submitted_request, &cancel_before_id)
                    .await;
                drop(response_rx);
                tracing::warn!(
                    %error,
                    session_id = %request.session_id,
                    prompt_id = %prompt_id,
                    request_id = %request_id,
                    "Grok shim user echo send failed; interrupted submitted request"
                );
                return Err(error);
            }
        }

        let outcome = self
            .watch_terminal(
                &key,
                &submitted_request,
                &request.session_id,
                &prompt_id,
                &cancel_before_id,
                sender,
                projections,
                &mut progress_guard,
                response_rx,
                subscribe_projected_changes(&self.node),
            )
            .await;
        let outcome = outcome?;
        prompt_result(outcome)
    }

    pub(super) async fn observed_human_prompt(
        &self,
        session: &str,
        request: &str,
    ) -> Result<Option<(String, String)>> {
        let node = escape_graphql_string(&self.config.node_did);
        let scope = gents::session::public_request_filter(&format!(
            r#"request_id:{{_eq:"{}"}},session_id:{{_eq:"{}"}},node_did:{{_eq:"{node}"}},requester_did:{{_eq:"{node}"}}"#,
            escape_graphql_string(request),
            escape_graphql_string(session)
        ));
        let query =
            format!(r#"{{AgentRequest(filter:{{{scope}}}) {{content runtime_source_kind}}}}"#,);
        let response =
            graphql_with_transaction_retry(&self.node, &query, "read observed human prompt")
                .await?;
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data["AgentRequest"].as_array())
            .context("missing observed request rows")?;
        anyhow::ensure!(
            rows.len() == 1,
            "observed request ownership is missing or ambiguous"
        );
        let row = &rows[0];
        if row["runtime_source_kind"].as_str().is_some() {
            return Ok(None);
        }
        Ok(Some((
            request.to_owned(),
            row["content"].as_str().unwrap_or_default().to_owned(),
        )))
    }

    pub(in crate::commands::grok_shim) async fn handle_cancel(
        &self,
        notification: CancelNotification,
    ) -> Result<()> {
        tracing::info!(
            session_id = %notification.session_id,
            prompt_id = notification.prompt_id.as_deref().unwrap_or(""),
            "Grok shim received session/cancel"
        );
        let target_prompt_id = notification.prompt_id.clone();
        let targets: Vec<(PromptKey, CancelLatch)> = {
            let state = self.state.lock().await;
            state
                .entries
                .iter()
                .filter(|((session, _), _)| session == &notification.session_id)
                .filter(|((_, prompt_id), _)| {
                    target_prompt_id
                        .as_deref()
                        .is_none_or(|expected| expected == prompt_id.as_str())
                })
                .map(|(key, entry)| (key.clone(), entry.cancel_before_id.clone()))
                .collect()
        };
        #[cfg(test)]
        {
            let gate = self.cancel_selection_gate.lock().await.clone();
            if let Some(gate) = gate {
                gate.arrived.notify_one();
                gate.release.notified().await;
            }
        }
        let had_foreground_target = !targets.is_empty();
        for (key, generation) in targets {
            let request = self
                .drain_entry(&key, &generation, StopReason::Cancelled)
                .await;
            if let Some(request) = request {
                self.interrupt_submitted(&request).await;
            }
        }
        let autonomous_target = match target_prompt_id.as_deref() {
            Some(id) if id.starts_with("notifications-") => {
                id.strip_prefix("notifications-").map(str::to_owned)
            }
            Some(id) => {
                let active = self
                    .autonomous_delivery
                    .lock()
                    .await
                    .get(&notification.session_id)
                    .cloned();
                match active {
                    Some(request)
                        if self
                            .observed_human_prompt(&notification.session_id, &request)
                            .await?
                            .is_some_and(|(prompt, _)| prompt == id) =>
                    {
                        Some(request)
                    }
                    _ => None,
                }
            }
            None if !had_foreground_target => self
                .autonomous_delivery
                .lock()
                .await
                .get(&notification.session_id)
                .cloned(),
            None => None,
        };
        if let Some(request_id) = autonomous_target.as_deref() {
            let observed = self
                .observed
                .lock()
                .await
                .contains_key(&(notification.session_id.clone(), request_id.to_owned()));
            let row = if observed {
                Some(
                    self.load_projection_request(
                        &self.config.node_did,
                        &notification.session_id,
                        Some(&self.config.node_did),
                        request_id,
                        None,
                    )
                    .await?,
                )
            } else {
                None
            };
            if let Some(row) = row.filter(|_| observed) {
                if self.request_stop_reason(&row).await?.is_some() {
                    return Ok(());
                }
                self.interrupt_submitted(&row).await;
            }
        }
        Ok(())
    }

    pub(in crate::commands::grok_shim) async fn handle_disconnect(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        state.closed = true;
        let drained: Vec<(
            PromptKey,
            CancelLatch,
            Option<gents_protocol::row::AgentRequestRow>,
        )> = std::mem::take(&mut state.entries)
            .into_iter()
            .map(|(key, mut entry)| {
                entry.resolve(Ok(json!({
                    "stopReason": StopReason::Cancelled.wire_name(),
                })));
                (key, entry.cancel_before_id.clone(), entry.request.clone())
            })
            .collect();
        drop(state);
        let observers = std::mem::take(&mut *self.observers.lock().await);
        for observer in observers.values() {
            observer.abort();
        }
        for (_, observer) in observers {
            let _ = observer.await;
        }
        #[cfg(test)]
        {
            let gate = self.disconnect_gate.lock().await.clone();
            if let Some(gate) = gate {
                gate.arrived.notify_one();
                gate.release.notified().await;
            }
        }
        for ((_session_id, _prompt_id), latch, request) in drained {
            let _first_cancel = latch.lock().await.cancel();
            if let Some(request) = request {
                self.interrupt_submitted(&request).await;
            }
        }
        Ok(())
    }

    async fn drain_entry(
        &self,
        key: &(String, String),
        generation: &Arc<Mutex<CancelBeforeIdLatch>>,
        stop_reason: StopReason,
    ) -> Option<gents_protocol::row::AgentRequestRow> {
        let (entry, _) = self.take_entry_if_generation(key, generation).await;
        let mut entry = entry?;
        #[cfg(test)]
        {
            let gate = self.cancel_drain_gate.lock().await.clone();
            if let Some(gate) = gate {
                gate.arrived.notify_one();
                gate.release.notified().await;
            }
        }
        let _first_cancel = entry.cancel_before_id.lock().await.cancel();
        entry.resolve(Ok(json!({
            "stopReason": stop_reason.wire_name(),
        })));
        entry.request.clone()
    }

    pub(super) async fn interrupt_and_drain(
        &self,
        key: &(String, String),
        request: &gents_protocol::row::AgentRequestRow,
        generation: &Arc<Mutex<CancelBeforeIdLatch>>,
    ) {
        if let (Some(mut entry), _) = self.take_entry_if_generation(key, generation).await {
            let _first_cancel = entry.cancel_before_id.lock().await.cancel();
            entry.resolve(Ok(json!({
                "stopReason": StopReason::Cancelled.wire_name(),
            })));
        }
        self.interrupt_submitted(request).await;
    }

    pub(super) async fn take_entry_if_generation(
        &self,
        key: &(String, String),
        generation: &Arc<Mutex<CancelBeforeIdLatch>>,
    ) -> (Option<PendingPrompt>, bool) {
        let mut state = self.state.lock().await;
        let owns_current_generation = state
            .entries
            .get(key)
            .is_some_and(|entry| Arc::ptr_eq(&entry.cancel_before_id, generation));
        let entry = if owns_current_generation {
            state.entries.remove(key)
        } else {
            None
        };
        (entry, state.is_closed())
    }

    pub(super) async fn interrupt_submitted(&self, request: &gents_protocol::row::AgentRequestRow) {
        let Some(physical) = request.doc_id.as_deref() else {
            tracing::error!(request_id=%request.request_id,"submitted request has no physical identity");
            return;
        };
        let Some(owner) = request.node_did.as_deref() else {
            tracing::error!(request_id=%request.request_id,"submitted request has no node identity");
            return;
        };
        if let Err(error) = gents::interrupt_request_by_doc_id(
            &self.node,
            physical,
            owner,
            request.requester_did.as_deref(),
        )
        .await
        {
            tracing::warn!(%error,request_id=%request.request_id,"Grok shim failed to interrupt submitted request");
        }
    }
}

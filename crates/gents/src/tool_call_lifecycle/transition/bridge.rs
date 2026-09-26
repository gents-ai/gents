use super::*;

impl ToolCallLifecycle {
    pub const CANCEL_DURING_RUN_OUTPUT: &'static str = "tool call cancelled";

    /// Running → Completed for bridge (subagent) tools.
    ///
    /// Lean parity: bridge_complete. Parent tool .running → .completed when
    /// the caller has verified the linked child request reached .completed.
    /// Closes the canonical output source and terminalizes lifecycle metadata
    /// atomically. Foreground delivery publishes the invocation reply;
    /// background completion retains its already-published receipt.
    ///
    /// Trust boundary: bridge_complete does NOT verify the child's terminal
    /// state internally (Lean's precondition is on the caller). R3's
    /// SubagentSource will be the natural place for that check.
    pub async fn bridge_complete(&mut self, child_result: String) -> Result<bool> {
        self.bridge_complete_inner(child_result, None).await
    }

    pub(crate) async fn bridge_complete_with_presentation(
        &mut self,
        child_result: String,
        rendered: &str,
        presentation: gents_protocol::output::PayloadPresentation,
    ) -> Result<bool> {
        self.bridge_complete_inner(child_result, Some((rendered, presentation)))
            .await
    }

    async fn bridge_complete_inner(
        &mut self,
        child_result: String,
        presented: Option<(&str, gents_protocol::output::PayloadPresentation)>,
    ) -> Result<bool> {
        // A second projector may load the already-committed terminal row while
        // holding an older Running edge. It must still verify the exact
        // canonical closure and result through the delivery replay path.
        self.ensure_state(
            &[ToolCallState::Running, ToolCallState::Completed],
            "bridge_complete",
        )?;
        if !self.is_bridge() {
            return Err(IllegalToolCallTransition::BridgeCompleteRequiresChildLink.into());
        }

        let fields = super::super::delivery::TerminalFields {
            state: ToolCallState::Completed,
            failure: None,
            cancel: None,
            remote_cancel_intent_at: None,
            completion_reason: None,
        };
        let updated = match presented {
            Some((rendered, presentation)) => {
                self.terminalize_raw_with_presentation(
                    ToolCallState::Running,
                    fields,
                    &child_result,
                    rendered,
                    presentation,
                    "tool_call.bridge_complete_delivery",
                )
                .await?
            }
            None => {
                self.terminalize_bridge_with_delivery(
                    ToolCallState::Running,
                    fields,
                    &child_result,
                    "tool_call.bridge_complete_delivery",
                )
                .await?
            }
        };
        if !updated {
            self.sync_after_lost_running_compare("bridge_complete")
                .await?;
            return Ok(false);
        }
        Ok(true)
    }

    /// Running → Failed (or Cancelled for ChildTerminal::Interrupted).
    ///
    /// Lean parity: bridge_failure. Parent tool .running → .failed (or
    /// .cancelled for ChildTerminal::Interrupted). Projection per
    /// ChildTerminal::projected_state(). Persists lifecycle_state,
    /// completed_at, latency_ms; conditionally persists tool_failure_class
    /// and result when the child reached .failed.
    ///
    /// Returns BridgeFailureRequiresChildLink for native tools (no
    /// child_request_id).
    pub async fn bridge_failure(&mut self, child_terminal: super::ChildTerminal) -> Result<bool> {
        let completion_reason = match &child_terminal {
            super::ChildTerminal::Dead => "deadline_exceeded",
            super::ChildTerminal::Interrupted => "explicit_cancel",
            super::ChildTerminal::Failed { .. } | super::ChildTerminal::Superseded => "tool_failed",
        };
        self.bridge_failure_with_completion_reason_and_presentation(
            child_terminal,
            completion_reason,
            None,
        )
        .await
    }

    pub(crate) async fn bridge_failure_with_completion_reason(
        &mut self,
        child_terminal: super::ChildTerminal,
        completion_reason: &str,
    ) -> Result<bool> {
        self.bridge_failure_with_completion_reason_and_presentation(
            child_terminal,
            completion_reason,
            None,
        )
        .await
    }

    pub(crate) async fn bridge_failure_with_presentation(
        &mut self,
        child_terminal: super::ChildTerminal,
        rendered: &str,
        presentation: gents_protocol::output::PayloadPresentation,
    ) -> Result<bool> {
        let completion_reason = match &child_terminal {
            super::ChildTerminal::Dead => "deadline_exceeded",
            super::ChildTerminal::Interrupted => "explicit_cancel",
            super::ChildTerminal::Failed { .. } | super::ChildTerminal::Superseded => "tool_failed",
        };
        self.bridge_failure_with_completion_reason_and_presentation(
            child_terminal,
            completion_reason,
            Some((rendered, presentation)),
        )
        .await
    }

    async fn bridge_failure_with_completion_reason_and_presentation(
        &mut self,
        child_terminal: super::ChildTerminal,
        completion_reason: &str,
        presented: Option<(&str, gents_protocol::output::PayloadPresentation)>,
    ) -> Result<bool> {
        let projected = child_terminal.projected_state();
        self.ensure_state(&[ToolCallState::Running, projected], "bridge_failure")?;
        if !self.is_bridge() {
            return Err(IllegalToolCallTransition::BridgeFailureRequiresChildLink.into());
        }

        let (failure_class_for_persist, reason_for_persist) = match &child_terminal {
            super::ChildTerminal::Failed {
                reason,
                failure_class,
            } => (Some(*failure_class), Some(reason.clone())),
            _ => (None, None),
        };

        let result = reason_for_persist
            .as_deref()
            .unwrap_or("linked child did not produce a completed result");
        let fields = super::super::delivery::TerminalFields {
            state: projected,
            failure: failure_class_for_persist,
            cancel: (projected == ToolCallState::Cancelled).then_some(CancelCause::Interrupted),
            remote_cancel_intent_at: None,
            completion_reason: Some(completion_reason),
        };
        let updated = match presented {
            Some((rendered, presentation)) => {
                self.terminalize_raw_with_presentation(
                    ToolCallState::Running,
                    fields,
                    result,
                    rendered,
                    presentation,
                    "tool_call.bridge_failure_delivery",
                )
                .await?
            }
            None => {
                self.terminalize_bridge_with_delivery(
                    ToolCallState::Running,
                    fields,
                    result,
                    "tool_call.bridge_failure_delivery",
                )
                .await?
            }
        };
        if !updated {
            self.sync_after_lost_running_compare("bridge_failure")
                .await?;
            return Ok(false);
        }
        Ok(true)
    }

    /// Running -> Failed(`SpawnUnclaimed`) for a cross-principal spawn whose
    /// child the parent has not observed (Lean `SpawnClaimFence.expire`). A
    /// missing child row is not proof the child will never materialize or has
    /// not already won its claim, so the same terminal write records the
    /// durable cancel intent its host mirrors onto the child's interrupt latch,
    /// and leaves the remote acknowledgement pending until the child is
    /// observed terminal. Returns false when a concurrent writer settled first.
    pub(crate) async fn abandon_unclaimed_spawn(&mut self, result: &str) -> Result<bool> {
        self.ensure_state(&[ToolCallState::Running], "abandon_unclaimed_spawn")?;
        if !self.is_bridge() {
            return Err(IllegalToolCallTransition::BridgeFailureRequiresChildLink.into());
        }
        let fields = super::super::delivery::TerminalFields {
            state: ToolCallState::Failed,
            failure: Some(FailureClass::SpawnUnclaimed),
            cancel: None,
            remote_cancel_intent_at: Some(chrono::Utc::now()),
            completion_reason: Some("unclaimed_spawn_timeout"),
        };
        let updated = self
            .terminalize_bridge_with_delivery(
                ToolCallState::Running,
                fields,
                result,
                "tool_call.abandon_unclaimed_spawn_delivery",
            )
            .await?;
        if !updated {
            self.sync_after_lost_running_compare("abandon_unclaimed_spawn")
                .await?;
        }
        Ok(updated)
    }

    /// Lean `SpawnClaimFence`: the durable cancel intent a terminal write of
    /// this spawn bridge must carry because no child row corroborating its
    /// exact physical lineage and target principal is visible. `None` when the
    /// row is not a spawn bridge or its child is observed. A lineage that
    /// cannot be resolved fences conservatively: a missing or unverifiable
    /// child row is not proof that no child will run.
    pub(crate) async fn unobserved_child_fence(
        &self,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
        if !self.is_subagent_bridge() {
            return Ok(None);
        }
        let (Some(parent), Some(bridge)) = (self.request_doc_id.as_deref(), self.doc_id.as_deref())
        else {
            return Ok(Some(chrono::Utc::now()));
        };
        let observed = crate::descendant_graph::resolve_physical_bridge_child(
            crate::descendant_graph::DescendantGraphAccess::Local(&self.node),
            parent,
            bridge,
        )
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(bridge_doc_id = bridge, %error, "spawn bridge child lineage unresolved; fencing");
            None
        });
        Ok(observed.is_none().then(chrono::Utc::now))
    }

    /// Lean parity: bridge_cancel_cascade. Pure — returns the action that should
    /// be taken on the child AgentRequest after an explicit bridge
    /// cancellation; the caller writes the child's interrupt_requested_at.
    /// Returns None for native tools and detached subagents.
    pub async fn bridge_cancel_cascade(&self) -> Result<Option<super::CascadeIntent>> {
        if self.state != ToolCallState::Cancelled {
            return Err(IllegalToolCallTransition::CascadeRequiresCancelled.into());
        }
        if self.cancel_policy != CancelPolicy::Cascade {
            return Ok(None); // detached: no cascade
        }
        let Some(child_request_id) = self.child_request_id.clone() else {
            return Ok(None); // native: no bridge edge
        };
        Ok(Some(super::CascadeIntent {
            child_request_id,
            at: chrono::Utc::now(),
        }))
    }

    /// Dispatch cascade cancellation according to child ownership. Local child
    /// requests continue through the existing interrupt path; replicated
    /// cross-deployment children are signaled through the bridge row.
    pub async fn bridge_cancel_cascade_dispatch(
        &self,
        local_did: &str,
    ) -> Result<Option<CascadeDispatch>> {
        let Some(intent) = self.bridge_cancel_cascade().await? else {
            return Ok(None);
        };

        if let Some(child) = self.locally_owned_bridge_child(local_did).await? {
            return Ok(Some(CascadeDispatch::Local { intent, child }));
        }

        self.write_bridge_cancel_cascade_intent(intent.at).await?;
        Ok(Some(CascadeDispatch::RemoteIntentWritten))
    }

    async fn write_bridge_cancel_cascade_intent(
        &self,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        let doc_id = self.doc_id.as_ref().ok_or_else(|| {
            anyhow!("bridge_cancel_cascade_dispatch called before row was persisted")
        })?;
        let escaped_doc_id = escape_graphql_string(doc_id);
        let at = escape_graphql_string(&at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        let started_at = self.started_at.ok_or_else(|| {
            anyhow!("bridge_cancel_cascade_dispatch called without started_at set")
        })?;
        let started_at = started_at.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
        let deadline_at = self
            .deadline_at
            .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
        let unclaimed_deadline_clear = self.clear_unclaimed_deadline_fragment();
        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{
                        started_at: "{started_at}",
                        deadline_at: "{deadline_at}",
                        completed_at: "{at}",
                        cancel_cascade_intent_at: "{at}",
                        cancel_pending_remote_ack: true
                        {unclaimed_deadline_clear}
                    }}
                ) {{ _docID }}
            }}"#
        );
        execute_mutation_with_retry(&self.node, &mutation, "write_bridge_cancel_cascade_intent")
            .await
            .context("write bridge cancel cascade intent mutation")?;
        Ok(())
    }

    /// Running → Cancelled. Called by request interruption handling and
    /// startup recovery for interrupted parent requests.
    ///
    pub async fn cancel_during_run(&mut self, cause: CancelCause) -> Result<bool> {
        self.cancel_during_run_inner(cause, None, None, None).await
    }

    /// Returns whether this caller won the durable running-state compare.
    /// Background completion side effects must only be projected by that
    /// winner; a loser adopts the already-terminal durable row.
    pub(crate) async fn cancel_during_run_owned(
        &mut self,
        cause: CancelCause,
        completion_reason: &str,
    ) -> Result<bool> {
        self.cancel_during_run_inner(cause, None, Some(completion_reason), None)
            .await
    }

    /// Running -> Cancelled while dispatching a cascade cancel. For remote
    /// children the durable cancel intent is written in the same bridge update
    /// that terminalizes the tool call, so recovery never observes a cancelled
    /// bridge without the remote signal.
    pub async fn cancel_during_run_with_cascade_dispatch(
        &mut self,
        cause: CancelCause,
        local_did: &str,
    ) -> Result<Option<CascadeDispatch>> {
        self.cancel_during_run_with_cascade_dispatch_and_presentation(cause, local_did, None)
            .await
    }

    pub(crate) async fn cancel_during_run_with_cascade_dispatch_and_presentation(
        &mut self,
        cause: CancelCause,
        local_did: &str,
        presented: Option<(&str, gents_protocol::output::PayloadPresentation)>,
    ) -> Result<Option<CascadeDispatch>> {
        self.ensure_state(
            &[ToolCallState::Running],
            "cancel_during_run_with_cascade_dispatch",
        )?;

        let Some(child_request_id) = self.child_request_id.clone() else {
            let _ = self
                .cancel_during_run_inner(cause, None, None, presented)
                .await?;
            return Ok(None);
        };
        if self.cancel_policy != CancelPolicy::Cascade {
            let _ = self
                .cancel_during_run_inner(cause, None, None, presented)
                .await?;
            return Ok(None);
        }

        let intent = super::CascadeIntent {
            child_request_id,
            at: chrono::Utc::now(),
        };
        if let Some(child) = self.locally_owned_bridge_child(local_did).await? {
            let won = self
                .cancel_during_run_inner(cause, None, None, presented)
                .await?;
            if won {
                return Ok(Some(CascadeDispatch::Local { intent, child }));
            }
            return Ok(None);
        }

        let won = self
            .cancel_during_run_inner(cause, Some(intent.at), None, presented)
            .await?;
        if won {
            Ok(Some(CascadeDispatch::RemoteIntentWritten))
        } else {
            Ok(None)
        }
    }

    async fn cancel_during_run_inner(
        &mut self,
        cause: CancelCause,
        remote_cancel_intent_at: Option<chrono::DateTime<chrono::Utc>>,
        completion_reason_override: Option<&str>,
        presented: Option<(&str, gents_protocol::output::PayloadPresentation)>,
    ) -> Result<bool> {
        self.ensure_state(&[ToolCallState::Running], "cancel_during_run")?;

        let completion_reason = completion_reason_override.unwrap_or(match cause {
            CancelCause::Deadline => "deadline_exceeded",
            CancelCause::Interrupted => "parent_interrupted",
            CancelCause::UserCancelled => "explicit_cancel",
        });
        let fields = super::super::delivery::TerminalFields {
            state: ToolCallState::Cancelled,
            failure: None,
            cancel: Some(cause),
            remote_cancel_intent_at,
            completion_reason: Some(completion_reason),
        };
        let raw = Self::CANCEL_DURING_RUN_OUTPUT;
        let updated = match presented {
            Some((rendered, presentation)) => {
                self.terminalize_raw_with_presentation(
                    ToolCallState::Running,
                    fields,
                    raw,
                    rendered,
                    presentation,
                    "tool_call.cancel_during_run_delivery",
                )
                .await?
            }
            None => {
                self.terminalize_raw_with_presentation(
                    ToolCallState::Running,
                    fields,
                    raw,
                    raw,
                    gents_protocol::output::PayloadPresentation::Full,
                    "tool_call.cancel_during_run_delivery",
                )
                .await?
            }
        };
        if !updated {
            self.sync_after_lost_running_compare("cancel_during_run")
                .await?;
            return Ok(false);
        }
        Ok(true)
    }
}

impl super::ToolCallLifecycle {
    async fn locally_owned_bridge_child(
        &self,
        local_did: &str,
    ) -> Result<Option<gents_protocol::row::AgentRequestRow>> {
        let parent = self
            .request_doc_id
            .as_deref()
            .context("cascade bridge missing physical parent")?;
        let bridge = self
            .doc_id
            .as_deref()
            .context("cascade bridge missing physical identity")?;
        let child = crate::descendant_graph::resolve_physical_bridge_child(
            crate::descendant_graph::DescendantGraphAccess::Local(&self.node),
            parent,
            bridge,
        )
        .await?;
        Ok(child.filter(|child| child.agent_did.as_deref() == Some(local_did)))
    }
}

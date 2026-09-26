use super::*;
use crate::tool_call_lifecycle::delivery::TerminalFields;

impl ToolCallLifecycle {
    pub(crate) fn selected_tool_fields_fragment(&self) -> String {
        match self.selected_tool_identity.as_ref() {
            Some(selected) => format!(
                "selected_service_id: \"{}\",\n                    selected_tool_name: \"{}\",",
                escape_graphql_string(&selected.service_id),
                escape_graphql_string(&selected.tool_name),
            ),
            _ => "selected_service_id: null,\n                    selected_tool_name: null,"
                .to_string(),
        }
    }

    /// Pending → Running for the exact row admitted with the accepted provider
    /// header. Admission owns create-only identity and native intent; dispatch
    /// only wins this lifecycle CAS after that publication has committed.
    pub async fn start_running(&mut self) -> Result<()> {
        self.start_running_with_time(None, None).await
    }

    #[cfg(test)]
    pub(crate) async fn start_running_at(
        &mut self,
        now: chrono::DateTime<chrono::Utc>,
        expected_generation: &str,
    ) -> Result<()> {
        self.start_running_with_time(Some(now), Some(expected_generation))
            .await
    }

    async fn start_running_with_time(
        &mut self,
        fixture_now: Option<chrono::DateTime<chrono::Utc>>,
        expected_generation: Option<&str>,
    ) -> Result<()> {
        if let Some(expected) = expected_generation {
            anyhow::ensure!(
                self.execution_generation.as_deref() == Some(expected),
                "dispatch generation differs from accepted physical tool"
            );
        }
        if self.state == ToolCallState::Running {
            anyhow::bail!(
                "start_running cannot re-dispatch an already-running physical tool; recover its registered executor instead"
            );
        }
        self.ensure_state(&[ToolCallState::Pending], "start_running")?;
        if self.is_spawned_background() {
            return self.start_running_spawned_with_time(fixture_now).await;
        }
        self.start_running_canonical_with_time(fixture_now).await
    }

    /// Running → Completed. Writes the tool result; sets completed_at,
    /// latency_ms.
    pub async fn complete(&mut self, result: &str) -> Result<()> {
        let _ = self.complete_owned(result, None).await?;
        Ok(())
    }

    pub(crate) async fn complete_with_presentation(
        &mut self,
        result: &str,
        presentation: Option<gents_protocol::output::PayloadPresentation>,
    ) -> Result<()> {
        let _ = self.complete_owned(result, presentation).await?;
        Ok(())
    }

    /// Running -> Completed while atomically persisting `raw` and exposing
    /// exactly `rendered` through the supplied presentation.
    pub(crate) async fn complete_raw_with_presentation(
        &mut self,
        raw: &str,
        rendered: &str,
        presentation: gents_protocol::output::PayloadPresentation,
    ) -> Result<bool> {
        self.complete_raw_with_presentation_with_time(raw, rendered, presentation, None)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn complete_raw_with_presentation_at(
        &mut self,
        raw: &str,
        rendered: &str,
        presentation: gents_protocol::output::PayloadPresentation,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        self.complete_raw_with_presentation_with_time(raw, rendered, presentation, Some(now))
            .await
    }

    async fn complete_raw_with_presentation_with_time(
        &mut self,
        raw: &str,
        rendered: &str,
        presentation: gents_protocol::output::PayloadPresentation,
        fixture_now: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<bool> {
        self.ensure_state(&[ToolCallState::Running], "complete")?;
        if self.is_bridge() {
            return Err(IllegalToolCallTransition::NativeCompleteOnSubagentTool.into());
        }
        let fields = TerminalFields {
            state: ToolCallState::Completed,
            failure: None,
            cancel: None,
            remote_cancel_intent_at: None,
            completion_reason: None,
            unclaimed_expired_by: None,
        };
        let updated = if let Some(now) = fixture_now {
            self.terminalize_raw_with_presentation_at(
                ToolCallState::Running,
                fields,
                raw,
                rendered,
                presentation,
                "tool_call.complete_raw_with_presentation",
                now,
            )
            .await?
        } else {
            self.terminalize_raw_with_presentation(
                ToolCallState::Running,
                fields,
                raw,
                rendered,
                presentation,
                "tool_call.complete_raw_with_presentation",
            )
            .await?
        };
        if updated {
            self.state = ToolCallState::Completed;
        } else {
            self.sync_after_lost_running_compare("complete").await?;
        }
        Ok(updated)
    }

    pub(crate) async fn complete_owned(
        &mut self,
        result: &str,
        presentation: Option<gents_protocol::output::PayloadPresentation>,
    ) -> Result<bool> {
        self.ensure_state(&[ToolCallState::Running], "complete")?;
        if self.is_bridge() {
            return Err(IllegalToolCallTransition::NativeCompleteOnSubagentTool.into());
        }

        if !self
            .terminalize_with_presentation(
                ToolCallState::Running,
                super::super::delivery::TerminalFields {
                    state: ToolCallState::Completed,
                    failure: None,
                    cancel: None,
                    remote_cancel_intent_at: None,
                    completion_reason: None,
                    unclaimed_expired_by: None,
                },
                result,
                presentation,
                "tool_call.complete_delivery",
            )
            .await?
        {
            // Interrupt/timeout won the race — adopt the durable terminal.
            self.sync_after_lost_running_compare("complete").await?;
            return Ok(false);
        }
        Ok(true)
    }

    /// Running → Failed. For tool errors during execution. Sets failure_class.
    pub async fn fail(&mut self, result: &str, failure: super::FailureClass) -> Result<()> {
        let _ = self.fail_owned(result, failure, None).await?;
        Ok(())
    }

    pub(crate) async fn fail_with_presentation(
        &mut self,
        result: &str,
        failure: super::FailureClass,
        presentation: Option<gents_protocol::output::PayloadPresentation>,
    ) -> Result<()> {
        let _ = self.fail_owned(result, failure, presentation).await?;
        Ok(())
    }

    pub(crate) async fn fail_raw_with_presentation(
        &mut self,
        raw: &str,
        rendered: &str,
        failure: super::FailureClass,
        presentation: gents_protocol::output::PayloadPresentation,
    ) -> Result<bool> {
        self.ensure_state(&[ToolCallState::Running], "fail")?;
        if self.is_bridge() {
            return Err(IllegalToolCallTransition::NativeFailOnSubagentTool.into());
        }
        let updated = self
            .terminalize_raw_with_presentation(
                ToolCallState::Running,
                TerminalFields {
                    state: ToolCallState::Failed,
                    failure: Some(failure),
                    cancel: None,
                    remote_cancel_intent_at: None,
                    completion_reason: None,
                    unclaimed_expired_by: None,
                },
                raw,
                rendered,
                presentation,
                "tool_call.fail_raw_with_presentation",
            )
            .await?;
        if updated {
            self.state = ToolCallState::Failed;
            self.failure_class = Some(failure);
        } else {
            self.sync_after_lost_running_compare("fail").await?;
        }
        Ok(updated)
    }

    pub(crate) async fn fail_with_command_denial(
        &mut self,
        result: &str,
        denial: &CommandPolicyDenial,
    ) -> Result<bool> {
        let _ = denial;
        self.fail_owned(result, FailureClass::PolicyDenied, None)
            .await
    }

    pub(crate) async fn fail_owned(
        &mut self,
        result: &str,
        failure: super::FailureClass,
        presentation: Option<gents_protocol::output::PayloadPresentation>,
    ) -> Result<bool> {
        self.fail_owned_inner(result, failure, presentation, None)
            .await
    }

    /// Running → Failed for a native row whose background completion
    /// notification must carry `completion_reason`.
    pub(crate) async fn fail_owned_with_completion_reason(
        &mut self,
        result: &str,
        failure: super::FailureClass,
        completion_reason: &str,
    ) -> Result<bool> {
        self.fail_owned_inner(result, failure, None, Some(completion_reason))
            .await
    }

    async fn fail_owned_inner(
        &mut self,
        result: &str,
        failure: super::FailureClass,
        presentation: Option<gents_protocol::output::PayloadPresentation>,
        completion_reason: Option<&str>,
    ) -> Result<bool> {
        self.ensure_state(&[ToolCallState::Running], "fail")?;
        if self.is_bridge() {
            return Err(IllegalToolCallTransition::NativeFailOnSubagentTool.into());
        }

        if !self
            .terminalize_with_presentation(
                ToolCallState::Running,
                super::super::delivery::TerminalFields {
                    state: ToolCallState::Failed,
                    failure: Some(failure),
                    cancel: None,
                    remote_cancel_intent_at: None,
                    completion_reason,
                    unclaimed_expired_by: None,
                },
                result,
                presentation,
                "tool_call.fail_delivery",
            )
            .await?
        {
            // Interrupt/timeout won the race — adopt the durable terminal.
            self.sync_after_lost_running_compare("fail").await?;
            return Ok(false);
        }
        Ok(true)
    }

    /// Pending → Failed. Used when the dispatcher cannot start the call
    /// (MCP service unreachable, argument parse failure pre-spawn).
    pub async fn spawn_failed(&mut self, failure: super::FailureClass, reason: &str) -> Result<()> {
        self.spawn_failed_with_details(failure, reason, None).await
    }

    pub(crate) async fn spawn_failed_with_command_denial(
        &mut self,
        reason: &str,
        denial: &CommandPolicyDenial,
    ) -> Result<()> {
        let _ = denial;
        self.spawn_failed_with_details(FailureClass::PolicyDenied, reason, None)
            .await
    }

    async fn spawn_failed_with_details(
        &mut self,
        failure: super::FailureClass,
        reason: &str,
        command_denial: Option<&CommandPolicyDenial>,
    ) -> Result<()> {
        self.ensure_state(&[ToolCallState::Pending], "spawn_failed")?;

        let _ = command_denial;
        let _ = self
            .terminalize_with_delivery(
                ToolCallState::Pending,
                super::super::delivery::TerminalFields {
                    state: ToolCallState::Failed,
                    failure: Some(failure),
                    cancel: None,
                    remote_cancel_intent_at: None,
                    completion_reason: None,
                    unclaimed_expired_by: None,
                },
                reason,
                "tool_call.spawn_failed_delivery",
            )
            .await?;
        Ok(())
    }

    /// Running → TimedOut. Called by the runtime deadline wrapper and startup
    /// recovery when a running tool call exceeds its effective deadline.
    ///
    /// Returns whether this caller won the durable running-state compare.
    /// A loser adopts the already-terminal durable row (another actor —
    /// interrupt, recovery sweep, or the tool itself — terminalized first),
    /// preserving that terminal's state and recorded cause.
    pub async fn timeout(&mut self) -> Result<bool> {
        self.timeout_inner(None).await
    }

    pub(crate) async fn timeout_with_presentation(
        &mut self,
        rendered: &str,
        presentation: gents_protocol::output::PayloadPresentation,
    ) -> Result<bool> {
        self.timeout_inner(Some((rendered, presentation))).await
    }

    async fn timeout_inner(
        &mut self,
        presented: Option<(&str, gents_protocol::output::PayloadPresentation)>,
    ) -> Result<bool> {
        self.ensure_state(&[ToolCallState::Running], "timeout")?;
        let message = format!(
            "tool call deadline exceeded at {}",
            self.deadline_at.to_rfc3339()
        );
        // Lean `SpawnClaimFence.deadline`: whichever deadline settles a spawn
        // bridge first, an unobserved child is fenced in this same write.
        let remote_cancel_intent_at = self.unobserved_child_fence().await?;
        let fields = super::super::delivery::TerminalFields {
            state: ToolCallState::TimedOut,
            failure: Some(FailureClass::External),
            cancel: Some(CancelCause::Deadline),
            remote_cancel_intent_at,
            completion_reason: None,
            unclaimed_expired_by: None,
        };
        let updated = match presented {
            Some((rendered, presentation)) => {
                self.terminalize_raw_with_presentation(
                    ToolCallState::Running,
                    fields,
                    &message,
                    rendered,
                    presentation,
                    "tool_call.timeout_delivery",
                )
                .await?
            }
            None => {
                self.terminalize_raw_with_presentation(
                    ToolCallState::Running,
                    fields,
                    &message,
                    &message,
                    gents_protocol::output::PayloadPresentation::Full,
                    "tool_call.timeout_delivery",
                )
                .await?
            }
        };
        if !updated {
            // Another actor terminalized first — adopt the durable terminal.
            self.sync_after_lost_running_compare("timeout").await?;
            return Ok(false);
        }
        Ok(true)
    }

    /// Pending → Cancelled. Used when a tool call is cancelled before
    /// dispatch creates a running row.
    ///
    pub async fn cancel_before_dispatch(&mut self, cause: CancelCause) -> Result<()> {
        self.ensure_state(&[ToolCallState::Pending], "cancel_before_dispatch")?;
        let _ = self
            .terminalize_with_delivery(
                ToolCallState::Pending,
                super::super::delivery::TerminalFields {
                    state: ToolCallState::Cancelled,
                    failure: None,
                    cancel: Some(cause),
                    remote_cancel_intent_at: None,
                    completion_reason: None,
                    unclaimed_expired_by: None,
                },
                "tool call cancelled before dispatch",
                "tool_call.cancel_before_dispatch_delivery",
            )
            .await?;
        Ok(())
    }
}

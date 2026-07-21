use super::*;

impl ToolCallLifecycle {
    /// Running mode-flip: await_mode Foreground → Background.
    ///
    /// Lean parity: ToolCallContext.Transition.background.
    /// Requires Running state. Returns `ModeAlreadyBackground` if already in
    /// Background mode. Persists the new await_mode to the row, then updates
    /// the in-memory field on success.
    pub async fn background(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "background")?;
        if self.await_mode == AwaitMode::Background {
            return Err(IllegalToolCallTransition::ModeAlreadyBackground.into());
        }

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("background called before start_running persisted a row"))?;
        // DefraDB requires DateTime fields to be re-supplied on update to
        // avoid a type-mismatch error when re-validating the document.
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("background called without started_at set"))?;
        let started_at_str = started_at.to_rfc3339();
        let deadline_at_str = self.deadline_at.to_rfc3339();
        let unclaimed_deadline_fragment = self.resupply_unclaimed_deadline_fragment();

        let escaped_doc_id = escape_graphql_string(doc_id);

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "running" }},
                        await_mode: {{ _eq: "foreground" }}
                    }},
                    input: {{ await_mode: "background", started_at: "{started_at_str}", deadline_at: "{deadline_at_str}"{unclaimed_deadline_fragment} }}
                ) {{ _docID }}
            }}"#
        );

        let response = execute_mutation_with_retry(&self.node, &mutation, "background")
            .await
            .context("background mutation")?;
        if !response
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentToolCall"))
            .is_some_and(response_has_documents)
        {
            self.sync_after_lost_mode_compare("background", AwaitMode::Background)
                .await?;
            return Ok(());
        }

        self.await_mode = AwaitMode::Background;
        Ok(())
    }

    /// Running mode-flip: await_mode Background → Foreground.
    ///
    /// Lean parity: ToolCallContext.Transition.foreground.
    /// Requires Running state. Returns `ModeAlreadyForeground` if already in
    /// Foreground mode. Persists the new await_mode to the row, then updates
    /// the in-memory field on success.
    pub async fn foreground(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "foreground")?;
        if self.await_mode == AwaitMode::Foreground {
            return Err(IllegalToolCallTransition::ModeAlreadyForeground.into());
        }

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("foreground called before start_running persisted a row"))?;
        // DefraDB requires DateTime fields to be re-supplied on update to
        // avoid a type-mismatch error when re-validating the document.
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("foreground called without started_at set"))?;
        let started_at_str = started_at.to_rfc3339();
        let deadline_at_str = self.deadline_at.to_rfc3339();
        let unclaimed_deadline_fragment = self.resupply_unclaimed_deadline_fragment();

        let escaped_doc_id = escape_graphql_string(doc_id);

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "running" }},
                        await_mode: {{ _eq: "background" }}
                    }},
                    input: {{ await_mode: "foreground", started_at: "{started_at_str}", deadline_at: "{deadline_at_str}"{unclaimed_deadline_fragment} }}
                ) {{ _docID }}
            }}"#
        );

        let response = execute_mutation_with_retry(&self.node, &mutation, "foreground")
            .await
            .context("foreground mutation")?;
        if !response
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentToolCall"))
            .is_some_and(response_has_documents)
        {
            self.sync_after_lost_mode_compare("foreground", AwaitMode::Foreground)
                .await?;
            return Ok(());
        }

        self.await_mode = AwaitMode::Foreground;
        Ok(())
    }

    /// Pending|Running policy-flip: cancel_policy Cascade → Detach.
    ///
    /// Lean parity: ToolCallContext.Transition.detach. Allowed in both Pending
    /// and Running states (h_live : pre.state = .pending ∨ pre.state = .running).
    /// Returns `PolicyAlreadyDetach` if already in Detach policy. One-way — no
    /// inverse method (matches Lean's structural irreversibility).
    pub async fn detach(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Pending, ToolCallState::Running], "detach")?;
        if self.cancel_policy == CancelPolicy::Detach {
            return Err(IllegalToolCallTransition::PolicyAlreadyDetach.into());
        }
        // Composed-model parity (`ComposedState.AllToolsPersistent`): a detached
        // tool must be a linked bridged subagent — `Persistent s t` requires
        // `t.childRequestId.isSome`, and the composed `tool_step` detach guard
        // (`IsDetached toolPost → Persistent post toolPost`) forbids detaching a
        // native (child-less) tool. Enforce the same precondition here so the
        // runtime cannot reach a state the invariant rules out.
        if !self.is_subagent_bridge() {
            return Err(IllegalToolCallTransition::DetachRequiresChildLink.into());
        }

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("doc_id must be set before policy-flip"))?;
        // DefraDB requires DateTime fields to be re-supplied on update to
        // avoid a type-mismatch error when re-validating the document.
        // started_at is only set once the row is in Running state; for Pending
        // state the row has not been created yet so this field will be absent.
        let started_at_fragment = if let Some(started_at) = self.started_at {
            format!(", started_at: \"{}\"", started_at.to_rfc3339())
        } else {
            String::new()
        };
        let deadline_at_str = self.deadline_at.to_rfc3339();
        let unclaimed_deadline_fragment = self.resupply_unclaimed_deadline_fragment();

        let escaped_doc_id = escape_graphql_string(doc_id);

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{ cancel_policy: "detach", deadline_at: "{deadline_at_str}"{started_at_fragment}{unclaimed_deadline_fragment} }}
                ) {{ _docID }}
            }}"#
        );

        execute_mutation_with_retry(&self.node, &mutation, "detach")
            .await
            .context("detach mutation")?;

        self.cancel_policy = CancelPolicy::Detach;
        Ok(())
    }
}

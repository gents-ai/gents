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

        let escaped_doc_id = escape_graphql_string(doc_id);

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    docID: "{escaped_doc_id}",
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "running" }},
                        await_mode: {{ _eq: "foreground" }}
                    }},
                    input: {{ await_mode: "background", started_at: "{started_at_str}", deadline_at: "{deadline_at_str}" }}
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
    /// the in-memory field on success. An `agent_new`/`agent_message` row
    /// is background-only: its result arrives only as a message.
    pub async fn foreground(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "foreground")?;
        if self.is_session_message() {
            return Err(IllegalToolCallTransition::SessionMessageIsBackgroundOnly.into());
        }
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

        let escaped_doc_id = escape_graphql_string(doc_id);

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    docID: "{escaped_doc_id}",
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "running" }},
                        await_mode: {{ _eq: "background" }}
                    }},
                    input: {{ await_mode: "foreground", started_at: "{started_at_str}", deadline_at: "{deadline_at_str}" }}
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
}

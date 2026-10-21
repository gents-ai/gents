use super::*;

impl ToolCallLifecycle {
    pub(crate) async fn clear_background_request(&self, expected: Option<&str>) -> Result<()> {
        let doc = escape_graphql_string(self.doc_id().context("tool has no physical row")?);
        let started = escape_graphql_string(
            &self
                .started_at
                .context("tool has not started")?
                .to_rfc3339(),
        );
        let deadline = escape_graphql_string(&self.deadline_at.to_rfc3339());
        let expected = expected
            .map(|value| {
                format!(
                    r#", background_requested_at: {{ _eq: "{}" }}"#,
                    escape_graphql_string(value)
                )
            })
            .unwrap_or_default();
        crate::config_client::ConfigAccess::write_local_response(&self.node,
            "tool.clear_background_request", &format!(r#"mutation {{ update_AgentToolCall(
                docID: "{doc}", filter: {{ _docID: {{ _eq: "{doc}" }}, await_mode: {{ _eq: "foreground" }}{expected} }},
                input: {{ background_requested_at: null, started_at: "{started}", deadline_at: "{deadline}" }}
            ) {{ _docID }} }}"#)).await?;
        Ok(())
    }

    pub(crate) async fn background_with_receipt(&mut self, receipt: &str) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "background")?;
        anyhow::ensure!(
            self.await_mode == AwaitMode::Foreground && self.plugin_effect.is_none(),
            "only a running direct foreground tool can be backgrounded"
        );
        let mut adopted = self.clone();
        adopted.await_mode = AwaitMode::Background;
        let binding = adopted.background_receipt_binding()?;
        let original = self.clone();
        let receipt = receipt.to_owned();
        let request_id = self.request_id.clone();
        let receipt_key = escape_graphql_string(&format!(
            "{}:background-receipt:{}",
            self.session_id,
            self.doc_id().unwrap()
        ));
        crate::config_client::ConfigAccess::transact_local_idempotent(
            &self.node, None, crate::config_client::IdempotentTransactionRetry::Standard,
            "tool.background_handoff", move |txn| {
                let binding = binding.clone();
                let request_id = request_id.clone();
                let receipt_key = receipt_key.clone();
                let original = original.clone();
                let receipt = receipt.clone();
                Box::pin(async move {
                    if original.background_in_txn(txn, original.deadline_at).await?.is_none() {
                        let existing = txn.execute(&format!(r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{receipt_key}" }} }}, limit: 2) {{ _docID }} }}"#)).await?;
                        anyhow::ensure!(existing.pointer("/data/AgentMessage").and_then(serde_json::Value::as_array)
                            .is_some_and(|rows| rows.len() == 1), "foreground tool changed before background handoff");
                        crate::tool_call_lifecycle::delivery::publish_background_receipt_in_txn(txn, &binding, &receipt).await?;
                        return Ok(());
                    }
                    anyhow::ensure!(crate::hook::persistence::count_live_backgrounded_rows_in(txn, &request_id).await?
                        <= crate::hook::persistence::MAX_BACKGROUNDED_TOOLS_PER_PARENT,
                        "background process limit reached");
                    crate::tool_call_lifecycle::delivery::publish_background_receipt_in_txn(txn, &binding, &receipt).await?;
                    Ok(())
                })
            },
        ).await?;
        self.await_mode = AwaitMode::Background;
        Ok(())
    }

    pub async fn background(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "background")?;
        anyhow::ensure!(
            self.plugin_effect.is_none(),
            "plugin effects cannot detach from their parent invocation"
        );
        if self.await_mode == AwaitMode::Background {
            return Err(IllegalToolCallTransition::ModeAlreadyBackground.into());
        }
        let original = self.clone();
        let adopted = crate::config_client::ConfigAccess::transact_local_idempotent(
            &self.node,
            None,
            crate::config_client::IdempotentTransactionRetry::Standard,
            "tool.background",
            move |txn| {
                let original = original.clone();
                Box::pin(async move { original.background_in_txn(txn, original.deadline_at).await })
            },
        )
        .await?;
        if adopted.is_none() {
            self.sync_after_lost_mode_compare("background", AwaitMode::Background)
                .await?;
        } else {
            self.await_mode = AwaitMode::Background;
        }
        Ok(())
    }

    /// [`Self::background`] inside the caller's transaction: the same
    /// running-foreground compare, also writing `deadline_at`. Returns the
    /// lifecycle as it reads once `txn` commits, or `None` when the row is no
    /// longer a running foreground call.
    async fn background_in_txn(
        &self,
        txn: &crate::config_client::ConfigApplyTxn<'_>,
        deadline_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<Self>> {
        self.ensure_state(&[ToolCallState::Running], "background")?;
        anyhow::ensure!(
            self.plugin_effect.is_none() && self.await_mode == AwaitMode::Foreground,
            "only a running foreground call can move to the background"
        );
        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("background called before start_running persisted a row"))?;
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("background called without started_at set"))?;
        let escaped_doc_id = escape_graphql_string(doc_id);
        let response = txn
            .execute(&format!(
                r#"mutation {{
                update_AgentToolCall(
                    docID: "{escaped_doc_id}",
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "running" }},
                        await_mode: {{ _eq: "foreground" }}
                    }},
                    input: {{ await_mode: "background", started_at: "{}", deadline_at: "{}" }}
                ) {{ _docID }}
            }}"#,
                escape_graphql_string(&started_at.to_rfc3339()),
                escape_graphql_string(&deadline_at.to_rfc3339())
            ))
            .await?;
        if !response
            .pointer("/data/update_AgentToolCall")
            .is_some_and(response_has_documents)
        {
            return Ok(None);
        }
        let mut adopted = self.clone();
        adopted.await_mode = AwaitMode::Background;
        adopted.deadline_at = deadline_at;
        Ok(Some(adopted))
    }

    pub(crate) async fn adopt_plugin_job_in_txn(
        &self,
        txn: &crate::config_client::ConfigApplyTxn<'_>,
        deadline_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<Self>> {
        let Some(mut adopted) = self.background_in_txn(txn, deadline_at).await? else {
            return Ok(None);
        };
        adopted.kind = ToolRowKind::PluginJob;
        Ok(Some(adopted))
    }

    pub async fn foreground(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "foreground")?;
        if !self.kind().may_foreground() {
            return Err(match self.kind() {
                ToolRowKind::PluginJob => IllegalToolCallTransition::PluginJobIsBackgroundOnly,
                _ => IllegalToolCallTransition::SessionMessageIsBackgroundOnly,
            }
            .into());
        }
        if self.await_mode == AwaitMode::Foreground {
            return Err(IllegalToolCallTransition::ModeAlreadyForeground.into());
        }

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("foreground called before start_running persisted a row"))?;
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

        let response = crate::config_client::ConfigAccess::write_local_response(
            &self.node,
            "foreground",
            &mutation,
        )
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

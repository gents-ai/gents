use super::*;

impl ToolCallLifecycle {
    pub(crate) async fn clear_background_request(&self, expected: Option<&str>) -> Result<()> {
        let doc = self.doc_id().context("tool has no physical row")?;
        let started = self
            .started_at
            .context("tool has not started")?
            .to_rfc3339();
        let deadline = self.deadline_at.to_rfc3339();
        let rendered = {
            use gents_protocol::document_query::{
                fields::agent_tool_call as fields, Filter, Input, Update,
            };
            let mut update = Update::doc(
                doc,
                Input::new()
                    .set_null(fields::BACKGROUND_REQUESTED_AT)
                    .set(fields::STARTED_AT, started)
                    .set(fields::DEADLINE_AT, deadline),
            )
            .filter(Filter::eq(fields::DOC_ID, doc))
            .filter(Filter::eq(fields::AWAIT_MODE, "foreground"));
            if let Some(value) = expected {
                update = update.filter(Filter::eq(fields::BACKGROUND_REQUESTED_AT, value));
            }
            update.render()
        };
        self.access()?
            .write_rendered("tool.clear_background_request", &rendered)
            .await?;
        Ok(())
    }

    pub(crate) async fn background_with_receipt(&mut self, receipt: &str) -> Result<()> {
        self.background_with_receipt_slot(
            receipt,
            crate::tools::call::hook::persistence::BackgroundSlot::Occupied,
        )
        .await
    }

    pub(crate) async fn background_with_recovery_receipt(&mut self, receipt: &str) -> Result<()> {
        self.background_with_receipt_slot(
            receipt,
            crate::tools::call::hook::persistence::BackgroundSlot::Recovery,
        )
        .await
    }

    async fn background_with_receipt_slot(
        &mut self,
        receipt: &str,
        slot: crate::tools::call::hook::persistence::BackgroundSlot,
    ) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "background")?;
        anyhow::ensure!(
            self.await_mode == AwaitMode::Foreground,
            "only a running direct foreground tool can be backgrounded"
        );
        let mut adopted = self.clone();
        adopted.await_mode = AwaitMode::Background;
        let binding = adopted.background_receipt_binding()?;
        let original = self.clone();
        let receipt = receipt.to_owned();
        let receipt_key = format!(
            "{}:background-receipt:{}",
            self.session_id,
            self.doc_id().unwrap()
        );
        self.access()?.transact_idempotent(
            crate::store::config_client::IdempotentTransactionRetry::Standard,
            "tool.background_handoff", move |txn| {
                let binding = binding.clone();
                let receipt_key = receipt_key.clone();
                let original = original.clone();
                let receipt = receipt.clone();
                Box::pin(async move {
                    if original.background_in_txn(txn, original.deadline_at, slot).await?.is_none() {
                        let existing = {
                            use gents_protocol::document_query::{
                                fields::agent_message as message, Filter, Query, Selection,
                            };
                            let rendered = Query::select(Selection::doc_id())
                                .filter(Filter::eq(message::MESSAGE_KEY, &receipt_key))
                                .limit(2)
                                .render();
                            txn.run(&rendered).await?
                        };
                        anyhow::ensure!(existing.pointer("/data/AgentMessage").and_then(serde_json::Value::as_array)
                            .is_some_and(|rows| rows.len() == 1), "foreground tool changed before background handoff");
                        crate::tools::call::lifecycle::delivery::publish_background_receipt_in_txn(txn, &binding, &receipt).await?;
                        return Ok(());
                    }
                    crate::tools::call::lifecycle::delivery::publish_background_receipt_in_txn(txn, &binding, &receipt).await?;
                    Ok(())
                })
            },
        ).await?;
        self.await_mode = AwaitMode::Background;
        crate::store::config_client::session_children::publish_agent_session_child_relationships_with_access(
            &self.access()?,
            &self.node_did,
            &self.actor,
            &self.session_id,
        )
        .await?;
        Ok(())
    }

    pub async fn background(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "background")?;
        if self.await_mode == AwaitMode::Background {
            return Err(IllegalToolCallTransition::ModeAlreadyBackground.into());
        }
        let original = self.clone();
        let adopted = self
            .access()?
            .transact_idempotent(
                crate::store::config_client::IdempotentTransactionRetry::Standard,
                "tool.background",
                move |txn| {
                    let original = original.clone();
                    Box::pin(async move {
                        original
                            .background_in_txn(
                                txn,
                                original.deadline_at,
                                crate::tools::call::hook::persistence::BackgroundSlot::Occupied,
                            )
                            .await
                    })
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
        txn: &crate::store::config_client::ConfigApplyTxn<'_>,
        deadline_at: chrono::DateTime<chrono::Utc>,
        slot: crate::tools::call::hook::persistence::BackgroundSlot,
    ) -> Result<Option<Self>> {
        self.ensure_state(&[ToolCallState::Running], "background")?;
        anyhow::ensure!(
            self.await_mode == AwaitMode::Foreground,
            "only a running foreground call can move to the background"
        );
        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("background called before start_running persisted a row"))?;
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("background called without started_at set"))?;
        let response = {
            use gents_protocol::document_query::{
                fields::agent_tool_call as fields, Filter, Input, Update,
            };
            let rendered = Update::doc(
                doc_id,
                Input::new()
                    .set(fields::AWAIT_MODE, "background")
                    .set(fields::STARTED_AT, started_at.to_rfc3339())
                    .set(fields::DEADLINE_AT, deadline_at.to_rfc3339()),
            )
            .filter(Filter::eq(fields::DOC_ID, doc_id))
            .filter(Filter::eq(fields::LIFECYCLE_STATE, "running"))
            .filter(Filter::eq(fields::AWAIT_MODE, "foreground"))
            .render();
            txn.run(&rendered).await?
        };
        if !response
            .pointer("/data/update_AgentToolCall")
            .is_some_and(response_has_documents)
        {
            return Ok(None);
        }
        let request_doc_id = self
            .request_doc_id
            .as_deref()
            .ok_or_else(|| anyhow!("background called without parent request document"))?;
        crate::tools::call::hook::persistence::reserve_background_slot_in_txn(
            txn,
            request_doc_id,
            slot,
            "",
        )
        .await?;
        let mut adopted = self.clone();
        adopted.await_mode = AwaitMode::Background;
        adopted.deadline_at = deadline_at;
        Ok(Some(adopted))
    }

    pub(crate) async fn adopt_plugin_job_in_txn(
        &self,
        txn: &crate::store::config_client::ConfigApplyTxn<'_>,
        deadline_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<Self>> {
        let Some(mut adopted) = self
            .background_in_txn(
                txn,
                deadline_at,
                crate::tools::call::hook::persistence::BackgroundSlot::Admitted,
            )
            .await?
        else {
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
        let rendered = {
            use gents_protocol::document_query::{
                fields::agent_tool_call as fields, Filter, Input, Update,
            };
            Update::doc(
                doc_id,
                Input::new()
                    .set(fields::AWAIT_MODE, "foreground")
                    .set(fields::STARTED_AT, started_at.to_rfc3339())
                    .set(fields::DEADLINE_AT, self.deadline_at.to_rfc3339()),
            )
            .filter(Filter::eq(fields::DOC_ID, doc_id))
            .filter(Filter::eq(fields::LIFECYCLE_STATE, "running"))
            .filter(Filter::eq(fields::AWAIT_MODE, "background"))
            .render()
        };

        let response = self
            .access()?
            .write_rendered("foreground", &rendered)
            .await
            .context("foreground mutation")?;
        let response: defra_node::QueryResponse = serde_json::from_value(response)?;
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

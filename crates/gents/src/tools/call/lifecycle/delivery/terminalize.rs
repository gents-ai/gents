#[cfg(test)]
use super::take_terminal_failure_for_test;
use super::terminal_effects::{retain_file_changes, write_terminal_progress};
use super::terminal_transaction::terminalize_transaction;
use super::{
    plugin_writes_failed, PluginWritesFailed, RawPresentation, TerminalFields, TerminalJobProgress,
    TerminalizeOptions,
};
use crate::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use crate::tools::call::lifecycle::{FailureClass, ToolCallLifecycle, ToolCallState, ToolRowKind};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::output::PayloadPresentation;

impl ToolCallLifecycle {
    pub(in crate::tools::call::lifecycle) async fn terminalize_with_delivery(
        &mut self,
        expected: ToolCallState,
        fields: TerminalFields<'_>,
        text: &str,
        operation: &'static str,
    ) -> Result<bool> {
        let publish_native_result = !(expected != ToolCallState::Pending
            && self.await_mode == crate::tools::call::lifecycle::AwaitMode::Background
            && matches!(
                self.kind(),
                ToolRowKind::SessionMessage | ToolRowKind::PluginJob
            )
            && !self.is_spawned_background());
        self.terminalize_with_options_at(
            expected,
            fields,
            TerminalizeOptions {
                text,
                pending_raw: None,
                presentation: None,
                adopt_competing_terminal: false,
                publish_native_result,
                operation,
                fixture_now: None,
            },
        )
        .await
    }

    pub(in crate::tools::call::lifecycle) async fn terminalize_bridge_with_delivery(
        &mut self,
        expected: ToolCallState,
        fields: TerminalFields<'_>,
        text: &str,
        operation: &'static str,
    ) -> Result<bool> {
        let publish_native_result = !(expected != ToolCallState::Pending
            && self.await_mode == crate::tools::call::lifecycle::AwaitMode::Background
            && matches!(
                self.kind(),
                ToolRowKind::SessionMessage | ToolRowKind::PluginJob
            )
            && !self.is_spawned_background());
        self.terminalize_with_options_at(
            expected,
            fields,
            TerminalizeOptions {
                text,
                pending_raw: None,
                presentation: None,
                adopt_competing_terminal: true,
                publish_native_result,
                operation,
                fixture_now: None,
            },
        )
        .await
    }

    /// The bridge terminal of [`Self::terminalize_bridge_with_delivery`] inside
    /// a caller's transaction: the same `terminalize_transaction` compare, then
    /// the job progress write when it wins. Changes nothing in memory.
    pub(in crate::tools::call::lifecycle) async fn terminalize_bridge_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
        fields: TerminalFields<'_>,
        text: &str,
        receipt: Option<&gents_protocol::plugin::PluginExecutionReceipt>,
        job_progress: Option<TerminalJobProgress>,
        publish_native_result: bool,
    ) -> Result<bool> {
        let doc_id = self
            .doc_id
            .as_deref()
            .context("canonical tool terminalization requires accepted physical tool identity")?;
        let request_doc_id = self
            .request_doc_id
            .as_deref()
            .context("canonical tool terminalization requires accepted request binding")?;
        let accepted_header = self
            .accepted_header_doc_id
            .as_deref()
            .context("canonical tool terminalization requires accepted header binding")?;
        if !self.is_spawned_background() {
            anyhow::ensure!(
                self.arguments.is_some(),
                "canonical direct tool terminalization requires accepted argument reference"
            );
        }
        let output_budget = if fields.state == ToolCallState::Completed {
            None
        } else {
            Some(self.output_budget().await)
        };
        let plugin_receipt = receipt.map(|receipt| {
            crate::tools::call::lifecycle::plugin_receipt::published(fields.state, receipt)
        });
        let plugin_effect = self.plugin_effect_terminal()?;
        let published = terminalize_transaction(
            txn,
            doc_id,
            request_doc_id,
            accepted_header,
            &self.node_did,
            self.requester_did.as_deref(),
            &self.session_id,
            &self.tool_call_id,
            self.call_id.as_deref(),
            self.message_sequence,
            self.arguments.as_ref(),
            self.spawned_by_tool_call_doc_id.as_deref(),
            plugin_effect.as_ref(),
            &self.tool_name,
            self.deadline_at,
            self.started_at,
            Utc::now(),
            ToolCallState::Running,
            fields,
            &self.terminal_persistence_status(fields.completion_reason),
            plugin_receipt.as_ref(),
            text,
            None,
            None,
            output_budget,
            true,
            publish_native_result,
        )
        .await?;
        #[cfg(test)]
        if take_terminal_failure_for_test(doc_id) {
            return Err(crate::config_client::classified_conflict_for_test().into());
        }
        if let (true, Some(progress)) = (published, job_progress) {
            write_terminal_progress(txn, doc_id, progress).await?;
        }
        Ok(published)
    }

    pub(in crate::tools::call::lifecycle) async fn terminalize_with_presentation(
        &mut self,
        expected: ToolCallState,
        fields: TerminalFields<'_>,
        text: &str,
        presentation: Option<PayloadPresentation>,
        operation: &'static str,
    ) -> Result<bool> {
        self.terminalize_with_options_at(
            expected,
            fields,
            TerminalizeOptions {
                text,
                pending_raw: None,
                presentation,
                adopt_competing_terminal: false,
                publish_native_result: true,
                operation,
                fixture_now: None,
            },
        )
        .await
    }

    pub(in crate::tools::call::lifecycle) async fn terminalize_raw_with_presentation(
        &mut self,
        expected: ToolCallState,
        fields: TerminalFields<'_>,
        raw: &str,
        rendered: &str,
        presentation: PayloadPresentation,
        operation: &'static str,
    ) -> Result<bool> {
        self.terminalize_with_options_at(
            expected,
            fields,
            TerminalizeOptions {
                text: rendered,
                pending_raw: Some(raw),
                presentation: Some(presentation),
                adopt_competing_terminal: true,
                publish_native_result: true,
                operation,
                fixture_now: None,
            },
        )
        .await
    }

    pub(in crate::tools::call::lifecycle) async fn terminalize_raw_with_presentation_at(
        &mut self,
        expected: ToolCallState,
        fields: TerminalFields<'_>,
        raw_presentation: RawPresentation<'_>,
        operation: &'static str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let RawPresentation {
            raw,
            rendered,
            presentation,
        } = raw_presentation;
        self.terminalize_with_options_at(
            expected,
            fields,
            TerminalizeOptions {
                text: rendered,
                pending_raw: Some(raw),
                presentation: Some(presentation),
                adopt_competing_terminal: true,
                publish_native_result: true,
                operation,
                fixture_now: Some(now),
            },
        )
        .await
    }

    async fn terminalize_with_options_at(
        &mut self,
        expected: ToolCallState,
        fields: TerminalFields<'_>,
        options: TerminalizeOptions<'_>,
    ) -> Result<bool> {
        let TerminalizeOptions {
            text,
            pending_raw,
            presentation,
            adopt_competing_terminal,
            publish_native_result,
            operation,
            fixture_now,
        } = options;
        if self.file_capture.is_some() {
            anyhow::ensure!(
                self.node.node_identity_did() == Some(self.node_did.as_str()),
                "retaining file capture requires the configured node identity"
            );
        }
        let doc_id = self
            .doc_id
            .clone()
            .context("canonical tool terminalization requires accepted physical tool identity")?;
        let request_doc_id = self
            .request_doc_id
            .clone()
            .context("canonical tool terminalization requires accepted request binding")?;
        let accepted_header = self
            .accepted_header_doc_id
            .clone()
            .context("canonical tool terminalization requires accepted header binding")?;
        let started_at = self.started_at;
        let node_did = self.node_did.clone();
        let requester_did = self.requester_did.clone();
        let session_id = self.session_id.clone();
        let tool_call_id = self.tool_call_id.clone();
        let call_id = self.call_id.clone();
        let message_sequence = self.message_sequence;
        let arguments = self.arguments.clone();
        if !self.is_spawned_background() {
            anyhow::ensure!(
                arguments.is_some(),
                "canonical direct tool terminalization requires accepted argument reference"
            );
        }
        let tool_name = self.tool_name.clone();
        let deadline_at = self.deadline_at;
        let output_budget = if fields.state == ToolCallState::Completed {
            None
        } else {
            Some(self.output_budget().await)
        };
        let terminal_status = self.terminal_persistence_status(fields.completion_reason);
        let plugin_receipt = self.plugin_receipt.as_ref().map(|receipt| {
            crate::tools::call::lifecycle::plugin_receipt::published(fields.state, receipt)
        });
        let spawned_by_tool_call_doc_id = self.spawned_by_tool_call_doc_id.clone();
        let plugin_effect = self.plugin_effect_terminal()?;
        // Only a completed call commits the writes its output carried; any
        // other terminal discards them.
        let plugin_writes = self
            .plugin_writes
            .clone()
            .filter(|_| fields.state == ToolCallState::Completed);

        let file_capture = self.file_capture.clone();
        let published = ConfigAccess::transact_local_idempotent(
            &self.node,
            None,
            IdempotentTransactionRetry::Standard,
            operation,
            move |txn| {
                let doc_id = doc_id.clone();
                let request_doc_id = request_doc_id.clone();
                let accepted_header = accepted_header.clone();
                let node_did = node_did.clone();
                let requester_did = requester_did.clone();
                let session_id = session_id.clone();
                let tool_call_id = tool_call_id.clone();
                let call_id = call_id.clone();
                let arguments = arguments.clone();
                let tool_name = tool_name.clone();
                let terminal_status = terminal_status.clone();
                let plugin_receipt = plugin_receipt.clone();
                let spawned_by_tool_call_doc_id = spawned_by_tool_call_doc_id.clone();
                let plugin_effect = plugin_effect.clone();
                let presentation = presentation.clone();
                let plugin_writes = plugin_writes.clone();
                let file_capture = file_capture.clone();
                Box::pin(async move {
                    if tool_name == crate::tools::native::SPAWN_PROCESS_TOOL_NAME {
                        txn.contend_with_request_siblings();
                    }
                    let now = fixture_now.unwrap_or_else(Utc::now);
                    let published = terminalize_transaction(
                        txn,
                        &doc_id,
                        &request_doc_id,
                        &accepted_header,
                        &node_did,
                        requester_did.as_deref(),
                        &session_id,
                        &tool_call_id,
                        call_id.as_deref(),
                        message_sequence,
                        arguments.as_ref(),
                        spawned_by_tool_call_doc_id.as_deref(),
                        plugin_effect.as_ref(),
                        &tool_name,
                        deadline_at,
                        started_at,
                        now,
                        expected,
                        fields,
                        &terminal_status,
                        plugin_receipt.as_ref(),
                        text,
                        pending_raw,
                        presentation.as_ref(),
                        output_budget,
                        adopt_competing_terminal,
                        publish_native_result,
                    )
                    .await?;
                    #[cfg(test)]
                    if take_terminal_failure_for_test(&doc_id) {
                        return Err(crate::config_client::classified_conflict_for_test().into());
                    }
                    // Applied after the terminal compare so a lost compare
                    // writes nothing; a failure rolls the terminal back too.
                    if let (true, Some(pending)) = (published, &plugin_writes) {
                        let stamps = crate::plugin::data::StampContext::ToolOutput {
                            node_did: node_did.clone(),
                            session_id: session_id.clone(),
                            request_doc_id: request_doc_id.clone(),
                            tool_call_doc_id: doc_id.clone(),
                        };
                        crate::plugin::application_write::apply_plugin_writes_in_txn(
                            txn,
                            &pending.grant,
                            &stamps,
                            pending.writes.clone(),
                        )
                        .await
                        .map_err(plugin_writes_failed)?;
                    }
                    if let (true, Some(changes)) = (published, &file_capture) {
                        match changes {
                            crate::tools::call::lifecycle::file_inventory::FileCapture::Images(images) => {
                                retain_file_changes(
                                    txn,
                                    &doc_id,
                                    &request_doc_id,
                                    &node_did,
                                    &session_id,
                                    requester_did.as_deref(),
                                    images,
                                )
                                .await?
                            }
                            crate::tools::call::lifecycle::file_inventory::FileCapture::Unchanged { tool_call_doc_id } => {
                                anyhow::ensure!(
                                    tool_call_doc_id.as_deref() == Some(doc_id.as_str()),
                                    "unchanged capture belongs to another physical tool"
                                );
                                crate::tools::call::lifecycle::file_inventory::retain_inventory(
                                    txn,
                                    crate::tools::call::lifecycle::file_inventory::Metadata::Unchanged,
                                    &doc_id,
                                    &request_doc_id,
                                    &crate::session::origin::SessionScope {
                                        node_did: node_did.clone(),
                                        session_id: session_id.clone(),
                                        requester_did: requester_did.clone(),
                                    },
                                )
                                .await?;
                            }
                        }
                    }
                    Ok(published)
                })
            },
        )
        .await;
        let published = match published {
            Ok(published) => published,
            Err(error) => match error.downcast_ref::<PluginWritesFailed>() {
                Some(refused) => {
                    let text = format!(
                        "plugin data writes were refused and nothing was committed: {refused}"
                    );
                    let fields = TerminalFields {
                        state: ToolCallState::Failed,
                        failure: Some(FailureClass::ToolReturnedError),
                        cancel: None,
                        completion_reason: None,
                    };
                    return Box::pin(self.terminalize_with_options_at(
                        expected,
                        fields,
                        TerminalizeOptions {
                            text: &text,
                            pending_raw: None,
                            presentation: None,
                            adopt_competing_terminal,
                            publish_native_result,
                            operation,
                            fixture_now,
                        },
                    ))
                    .await;
                }
                None => return Err(error),
            },
        };
        if published {
            self.state = fields.state;
            self.failure_class = fields.failure;
            self.cancel_cause = fields.cancel;
        }
        Ok(published)
    }
}

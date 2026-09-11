use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use gents_codex_protocol as codex;
use gents_codex_protocol::MessagePhase;

use super::command_projection::{
    codex_command_status, codex_mcp_status, codex_patch_status, command_execution_item,
    command_output_payload, file_change_item, ToolProjectionStatus,
};
use super::compaction_projection::{
    compaction_projection_events, context_compaction_item, CompactionProjectionEvent,
    GentsCompactionProgress,
};
use super::progress::{
    gents_tool_item, tool_completed_at_ms, tool_started_at_ms, GentsToolCallProgress,
};
use super::projection_state::{
    collab_projection_events, CollabProjection, ProjectionEvent, ProjectionStatus,
};
use super::protocol::{
    agent_message_item, agent_message_item_with_phase, now_millis, send_notification,
    turn_value_with_timing,
};
use super::subagent_projection::collab_tool_item;
use super::{Outbound, ShimState};

#[derive(Default)]
struct ReasoningCompletionTracker {
    item_ids: HashSet<String>,
}

impl ReasoningCompletionTracker {
    fn contains(&self, item_id: &str) -> bool {
        self.item_ids.contains(item_id)
    }

    fn is_empty(&self) -> bool {
        self.item_ids.is_empty()
    }

    fn record(&mut self, item_id: String) -> bool {
        self.item_ids.insert(item_id)
    }
}

pub(super) struct TurnProjection<'a> {
    state: &'a ShimState,
    pub(super) thread_id: &'a str,
    pub(super) turn_id: &'a str,
    pub(super) cwd: PathBuf,
    started_at: Option<i64>,
    completed_at: Option<i64>,
    response_started_at_ms: Option<i64>,
    response_completed_at_ms: Option<i64>,
    active_agent_item_id: Option<String>,
    active_agent_text: String,
    rendered_agent_text: String,
    active_reasoning_item_id: Option<String>,
    active_reasoning_text: String,
    completed_reasoning_items: ReasoningCompletionTracker,
}

impl<'a> TurnProjection<'a> {
    pub(super) fn new(
        state: &'a ShimState,
        thread_id: &'a str,
        turn_id: &'a str,
        cwd: PathBuf,
        started_at: Option<i64>,
    ) -> Self {
        Self {
            state,
            thread_id,
            turn_id,
            cwd,
            started_at,
            completed_at: None,
            response_started_at_ms: None,
            response_completed_at_ms: None,
            active_agent_item_id: None,
            active_agent_text: String::new(),
            rendered_agent_text: String::new(),
            active_reasoning_item_id: None,
            active_reasoning_text: String::new(),
            completed_reasoning_items: ReasoningCompletionTracker::default(),
        }
    }

    pub(super) fn observe_response_timing(
        &mut self,
        started_at_ms: Option<i64>,
        completed_at_ms: Option<i64>,
    ) {
        if self.response_started_at_ms.is_none() {
            self.response_started_at_ms = started_at_ms;
        }
        observe_latest_response_completion(&mut self.response_completed_at_ms, completed_at_ms);
    }

    pub(super) fn reset_response_timing(&mut self) {
        self.response_started_at_ms = None;
        self.response_completed_at_ms = None;
    }

    pub(super) fn set_completed_at(&mut self, completed_at: Option<i64>) {
        self.completed_at = completed_at;
    }

    pub(super) async fn append_reasoning_delta(
        &mut self,
        outbound: &Outbound,
        item_id: &str,
        delta: &str,
    ) -> Result<()> {
        if delta.is_empty() {
            return Ok(());
        }
        if self.completed_reasoning_items.contains(item_id) {
            return Ok(());
        }
        if self.active_reasoning_item_id.as_deref() != Some(item_id) {
            self.complete_active_reasoning(outbound, None).await?;
            send_notification(
                outbound,
                self.state,
                codex::ServerNotification::ItemStarted(codex::ItemStartedNotification {
                    item: reasoning_item(item_id, ""),
                    thread_id: self.thread_id.to_string(),
                    turn_id: self.turn_id.to_string(),
                    started_at_ms: self.response_started_at_ms.unwrap_or_else(now_millis),
                }),
            )
            .await?;
            self.active_reasoning_item_id = Some(item_id.to_string());
            self.active_reasoning_text.clear();
        }

        self.active_reasoning_text.push_str(delta);
        send_notification(
            outbound,
            self.state,
            codex::ServerNotification::ReasoningTextDelta(codex::ReasoningTextDeltaNotification {
                thread_id: self.thread_id.to_string(),
                turn_id: self.turn_id.to_string(),
                item_id: item_id.to_string(),
                delta: delta.to_string(),
                content_index: 0,
            }),
        )
        .await
    }

    pub(super) fn resume_reasoning(&mut self, item_id: String, text: &str) {
        self.active_reasoning_item_id = Some(item_id);
        self.active_reasoning_text = text.to_string();
    }

    pub(super) async fn finish_reasoning(
        &mut self,
        outbound: &Outbound,
        item_id: &str,
        durable_text: Option<&str>,
    ) -> Result<()> {
        if self.completed_reasoning_items.contains(item_id) {
            return Ok(());
        }
        let durable_text = durable_text.filter(|text| !text.trim().is_empty());
        if durable_text.is_some()
            && !terminal_reasoning_backfill_allowed(
                self.active_reasoning_item_id.is_some(),
                !self.completed_reasoning_items.is_empty(),
            )
        {
            return Ok(());
        }
        if self.active_reasoning_item_id.is_none() {
            if let Some(text) = durable_text {
                self.append_reasoning_delta(outbound, item_id, text).await?;
            } else {
                return Ok(());
            }
        } else if self.active_reasoning_item_id.as_deref() != Some(item_id) {
            self.complete_active_reasoning(outbound, None).await?;
            if let Some(text) = durable_text {
                self.append_reasoning_delta(outbound, item_id, text).await?;
            } else {
                return Ok(());
            }
        } else if let Some(text) = durable_text {
            if let Some(delta) = text.strip_prefix(&self.active_reasoning_text) {
                self.append_reasoning_delta(outbound, item_id, delta)
                    .await?;
            }
        }

        self.complete_active_reasoning(outbound, durable_text).await
    }

    async fn complete_active_reasoning(
        &mut self,
        outbound: &Outbound,
        completed_text: Option<&str>,
    ) -> Result<()> {
        let Some(item_id) = self.active_reasoning_item_id.take() else {
            return Ok(());
        };
        let streamed_text = std::mem::take(&mut self.active_reasoning_text);
        let text = completed_text
            .filter(|text| !text.trim().is_empty())
            .unwrap_or(&streamed_text);
        if text.trim().is_empty() {
            return Ok(());
        }
        let item = reasoning_item(&item_id, text);
        send_notification(
            outbound,
            self.state,
            codex::ServerNotification::ItemCompleted(codex::ItemCompletedNotification {
                item: item.clone(),
                thread_id: self.thread_id.to_string(),
                turn_id: self.turn_id.to_string(),
                completed_at_ms: self.response_completed_at_ms.unwrap_or_else(now_millis),
            }),
        )
        .await?;
        self.completed_reasoning_items.record(item_id);
        Ok(())
    }

    pub(super) async fn append_agent_delta(
        &mut self,
        outbound: &Outbound,
        delta: &str,
    ) -> Result<()> {
        if delta.is_empty() {
            return Ok(());
        }
        if suppress_blank_agent_delta(&self.active_agent_text, delta) {
            return Ok(());
        }
        let delta = if self.rendered_agent_text.is_empty() {
            delta.trim_start()
        } else {
            delta
        };
        if delta.is_empty() {
            return Ok(());
        }
        let item_id = if let Some(item_id) = self.active_agent_item_id.as_ref() {
            item_id.clone()
        } else {
            let item_id = self.state.next_id("gents-message");
            send_notification(
                outbound,
                self.state,
                codex::ServerNotification::ItemStarted(codex::ItemStartedNotification {
                    item: agent_message_item(&item_id, ""),
                    thread_id: self.thread_id.to_string(),
                    turn_id: self.turn_id.to_string(),
                    started_at_ms: self.response_started_at_ms.unwrap_or_else(now_millis),
                }),
            )
            .await?;
            self.active_agent_item_id = Some(item_id.clone());
            item_id
        };

        self.active_agent_text.push_str(delta);
        self.rendered_agent_text.push_str(delta);
        send_notification(
            outbound,
            self.state,
            codex::ServerNotification::AgentMessageDelta(codex::AgentMessageDeltaNotification {
                thread_id: self.thread_id.to_string(),
                turn_id: self.turn_id.to_string(),
                item_id,
                delta: delta.to_string(),
            }),
        )
        .await
    }

    pub(super) fn resume_agent_message(&mut self, item_id: String, text: &str) {
        self.active_agent_item_id = Some(item_id);
        self.active_agent_text = text.to_string();
        self.rendered_agent_text = text.to_string();
    }

    pub(super) async fn finish_agent_message_with_phase(
        &mut self,
        outbound: &Outbound,
        phase: Option<MessagePhase>,
    ) -> Result<()> {
        let Some(item_id) = self.active_agent_item_id.take() else {
            return Ok(());
        };
        let text = std::mem::take(&mut self.active_agent_text);
        if text.trim().is_empty() {
            return Ok(());
        }
        let completed_item = agent_message_item_with_phase(&item_id, &text, phase);
        send_notification(
            outbound,
            self.state,
            codex::ServerNotification::ItemCompleted(codex::ItemCompletedNotification {
                item: completed_item.clone(),
                thread_id: self.thread_id.to_string(),
                turn_id: self.turn_id.to_string(),
                completed_at_ms: self.response_completed_at_ms.unwrap_or_else(now_millis),
            }),
        )
        .await?;
        Ok(())
    }

    async fn send_tool_started(
        &mut self,
        outbound: &Outbound,
        tool: &GentsToolCallProgress,
    ) -> Result<()> {
        self.finish_agent_message_with_phase(outbound, Some(MessagePhase::Commentary))
            .await?;
        send_notification(
            outbound,
            self.state,
            codex::ServerNotification::ItemStarted(codex::ItemStartedNotification {
                item: gents_tool_item(tool, codex::McpToolCallStatus::InProgress),
                thread_id: self.thread_id.to_string(),
                turn_id: self.turn_id.to_string(),
                started_at_ms: tool_started_at_ms(tool).unwrap_or_else(now_millis),
            }),
        )
        .await
    }

    async fn send_tool_completed(
        &mut self,
        outbound: &Outbound,
        tool: &GentsToolCallProgress,
        status: codex::McpToolCallStatus,
    ) -> Result<()> {
        self.finish_agent_message_with_phase(outbound, Some(MessagePhase::Commentary))
            .await?;
        let completed_item = gents_tool_item(tool, status);
        send_notification(
            outbound,
            self.state,
            codex::ServerNotification::ItemCompleted(codex::ItemCompletedNotification {
                item: completed_item.clone(),
                thread_id: self.thread_id.to_string(),
                turn_id: self.turn_id.to_string(),
                completed_at_ms: tool_completed_at_ms(tool).unwrap_or_else(now_millis),
            }),
        )
        .await?;
        Ok(())
    }

    async fn send_command_execution_started(
        &mut self,
        outbound: &Outbound,
        tool: &GentsToolCallProgress,
        status: codex::CommandExecutionStatus,
    ) -> Result<()> {
        self.finish_agent_message_with_phase(outbound, Some(MessagePhase::Commentary))
            .await?;
        send_notification(
            outbound,
            self.state,
            codex::ServerNotification::ItemStarted(codex::ItemStartedNotification {
                item: command_execution_item(&self.cwd, tool, status),
                thread_id: self.thread_id.to_string(),
                turn_id: self.turn_id.to_string(),
                started_at_ms: tool_started_at_ms(tool).unwrap_or_else(now_millis),
            }),
        )
        .await
    }

    async fn send_command_execution_completed(
        &mut self,
        outbound: &Outbound,
        tool: &GentsToolCallProgress,
        status: codex::CommandExecutionStatus,
    ) -> Result<()> {
        self.finish_agent_message_with_phase(outbound, Some(MessagePhase::Commentary))
            .await?;
        if let Some(delta) = command_output_payload(tool) {
            send_notification(
                outbound,
                self.state,
                codex::ServerNotification::CommandExecutionOutputDelta(
                    codex::CommandExecutionOutputDeltaNotification {
                        thread_id: self.thread_id.to_string(),
                        turn_id: self.turn_id.to_string(),
                        item_id: tool.tool_call_key.clone(),
                        delta,
                    },
                ),
            )
            .await?;
        }
        let completed_item = command_execution_item(&self.cwd, tool, status);
        send_notification(
            outbound,
            self.state,
            codex::ServerNotification::ItemCompleted(codex::ItemCompletedNotification {
                item: completed_item.clone(),
                thread_id: self.thread_id.to_string(),
                turn_id: self.turn_id.to_string(),
                completed_at_ms: tool_completed_at_ms(tool).unwrap_or_else(now_millis),
            }),
        )
        .await?;
        Ok(())
    }

    async fn send_file_change_started(
        &mut self,
        outbound: &Outbound,
        tool: &GentsToolCallProgress,
    ) -> Result<()> {
        let Some(item) = file_change_item(tool, codex::PatchApplyStatus::InProgress) else {
            return Ok(());
        };
        self.finish_agent_message_with_phase(outbound, Some(MessagePhase::Commentary))
            .await?;
        send_notification(
            outbound,
            self.state,
            codex::ServerNotification::ItemStarted(codex::ItemStartedNotification {
                item,
                thread_id: self.thread_id.to_string(),
                turn_id: self.turn_id.to_string(),
                started_at_ms: tool_started_at_ms(tool).unwrap_or_else(now_millis),
            }),
        )
        .await
    }

    async fn send_collab_started(
        &mut self,
        outbound: &Outbound,
        tool: &GentsToolCallProgress,
        projection: &CollabProjection,
    ) -> Result<()> {
        self.finish_agent_message_with_phase(outbound, Some(MessagePhase::Commentary))
            .await?;
        let mut started = projection.clone();
        started.status = ProjectionStatus::InProgress;
        send_notification(
            outbound,
            self.state,
            codex::ServerNotification::ItemStarted(codex::ItemStartedNotification {
                item: collab_tool_item(self.thread_id, tool, &started),
                thread_id: self.thread_id.to_string(),
                turn_id: self.turn_id.to_string(),
                started_at_ms: tool_started_at_ms(tool).unwrap_or_else(now_millis),
            }),
        )
        .await
    }

    async fn send_collab_completed(
        &mut self,
        outbound: &Outbound,
        tool: &GentsToolCallProgress,
        projection: &CollabProjection,
    ) -> Result<()> {
        self.finish_agent_message_with_phase(outbound, Some(MessagePhase::Commentary))
            .await?;
        let item = collab_tool_item(self.thread_id, tool, projection);
        send_notification(
            outbound,
            self.state,
            codex::ServerNotification::ItemCompleted(codex::ItemCompletedNotification {
                item: item.clone(),
                thread_id: self.thread_id.to_string(),
                turn_id: self.turn_id.to_string(),
                completed_at_ms: tool_completed_at_ms(tool).unwrap_or_else(now_millis),
            }),
        )
        .await?;
        Ok(())
    }

    async fn send_file_change_completed(
        &mut self,
        outbound: &Outbound,
        tool: &GentsToolCallProgress,
        status: codex::PatchApplyStatus,
    ) -> Result<()> {
        let Some(item) = file_change_item(tool, status) else {
            return Ok(());
        };
        self.finish_agent_message_with_phase(outbound, Some(MessagePhase::Commentary))
            .await?;
        send_notification(
            outbound,
            self.state,
            codex::ServerNotification::ItemCompleted(codex::ItemCompletedNotification {
                item: item.clone(),
                thread_id: self.thread_id.to_string(),
                turn_id: self.turn_id.to_string(),
                completed_at_ms: tool_completed_at_ms(tool).unwrap_or_else(now_millis),
            }),
        )
        .await?;
        Ok(())
    }

    pub(super) async fn send_tool_projection_update(
        &mut self,
        outbound: &Outbound,
        tool: &GentsToolCallProgress,
        previous: Option<&ToolProjectionStatus>,
        current: &ToolProjectionStatus,
    ) -> Result<()> {
        if let ToolProjectionStatus::Collab(projection) = current {
            let events = collab_projection_events(previous, projection);
            for event in events {
                match event {
                    ProjectionEvent::Started => {
                        self.send_collab_started(outbound, tool, projection).await?
                    }
                    ProjectionEvent::Completed => {
                        self.send_collab_completed(outbound, tool, projection)
                            .await?
                    }
                }
            }
            return Ok(());
        }

        match (previous, current) {
            (Some(ToolProjectionStatus::Command(_)), ToolProjectionStatus::Mcp(status)) => {
                let mut foreground_tool = tool.clone();
                foreground_tool.result.clear();
                self.send_command_execution_completed(
                    outbound,
                    &foreground_tool,
                    codex::CommandExecutionStatus::Completed,
                )
                .await?;
                if *status != ProjectionStatus::InProgress {
                    self.send_tool_started(outbound, tool).await?;
                }
                match status {
                    ProjectionStatus::InProgress => self.send_tool_started(outbound, tool).await,
                    ProjectionStatus::Completed | ProjectionStatus::Failed => {
                        self.send_tool_completed(outbound, tool, codex_mcp_status(*status))
                            .await
                    }
                }
            }
            (None, ToolProjectionStatus::Mcp(status))
                if *status != ProjectionStatus::InProgress =>
            {
                self.send_tool_started(outbound, tool).await?;
                self.send_tool_completed(outbound, tool, codex_mcp_status(*status))
                    .await
            }
            (Some(ToolProjectionStatus::DeferredCollab), ToolProjectionStatus::Mcp(status))
                if *status != ProjectionStatus::InProgress =>
            {
                self.send_tool_started(outbound, tool).await?;
                self.send_tool_completed(outbound, tool, codex_mcp_status(*status))
                    .await
            }
            (_, ToolProjectionStatus::Mcp(ProjectionStatus::InProgress)) => {
                self.send_tool_started(outbound, tool).await
            }
            (_, ToolProjectionStatus::Mcp(status)) => {
                self.send_tool_completed(outbound, tool, codex_mcp_status(*status))
                    .await
            }
            (None, ToolProjectionStatus::Command(status))
                if *status != ProjectionStatus::InProgress =>
            {
                self.send_command_execution_started(
                    outbound,
                    tool,
                    codex::CommandExecutionStatus::InProgress,
                )
                .await?;
                self.send_command_execution_completed(outbound, tool, codex_command_status(*status))
                    .await
            }
            (_, ToolProjectionStatus::Command(ProjectionStatus::InProgress)) => {
                self.send_command_execution_started(
                    outbound,
                    tool,
                    codex_command_status(current.command_status()),
                )
                .await
            }
            (_, ToolProjectionStatus::Command(status)) => {
                self.send_command_execution_completed(outbound, tool, codex_command_status(*status))
                    .await
            }
            (_, ToolProjectionStatus::Collab(_)) => unreachable!("handled above"),
            (_, ToolProjectionStatus::DeferredCollab) => Ok(()),
            (_, ToolProjectionStatus::DeferredFileChange) => Ok(()),
            (None, ToolProjectionStatus::FileChange(status))
            | (
                Some(ToolProjectionStatus::DeferredFileChange),
                ToolProjectionStatus::FileChange(status),
            ) if *status != ProjectionStatus::InProgress => {
                self.send_file_change_started(outbound, tool).await?;
                self.send_file_change_completed(outbound, tool, codex_patch_status(*status))
                    .await
            }
            (_, ToolProjectionStatus::FileChange(ProjectionStatus::InProgress)) => {
                self.send_file_change_started(outbound, tool).await
            }
            (_, ToolProjectionStatus::FileChange(status)) => {
                self.send_file_change_completed(outbound, tool, codex_patch_status(*status))
                    .await
            }
        }
    }

    pub(super) async fn send_compaction_projection_update(
        &mut self,
        outbound: &Outbound,
        compaction: &GentsCompactionProgress,
        previous_state: Option<&str>,
    ) -> Result<()> {
        let events = compaction_projection_events(previous_state, &compaction.call_state);
        if events.is_empty() {
            return Ok(());
        }
        self.finish_agent_message_with_phase(outbound, Some(MessagePhase::Commentary))
            .await?;
        for event in events {
            let item = context_compaction_item(&compaction.call_id);
            match event {
                CompactionProjectionEvent::Started => {
                    send_notification(
                        outbound,
                        self.state,
                        codex::ServerNotification::ItemStarted(codex::ItemStartedNotification {
                            item,
                            thread_id: self.thread_id.to_string(),
                            turn_id: self.turn_id.to_string(),
                            started_at_ms: now_millis(),
                        }),
                    )
                    .await?;
                }
                CompactionProjectionEvent::Completed => {
                    send_notification(
                        outbound,
                        self.state,
                        codex::ServerNotification::ItemCompleted(
                            codex::ItemCompletedNotification {
                                item: item.clone(),
                                thread_id: self.thread_id.to_string(),
                                turn_id: self.turn_id.to_string(),
                                completed_at_ms: now_millis(),
                            },
                        ),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    pub(super) async fn finish_turn(
        &mut self,
        outbound: &Outbound,
        status: codex::TurnStatus,
        error_message: Option<String>,
    ) -> Result<()> {
        self.complete_active_reasoning(outbound, None).await?;
        self.finish_agent_message_with_phase(outbound, Some(MessagePhase::FinalAnswer))
            .await?;
        let turn_error = if status == codex::TurnStatus::Failed {
            Some(codex::TurnError {
                message: error_message.unwrap_or_else(|| "GENTS turn failed".to_string()),
                codex_error_info: None,
                additional_details: None,
            })
        } else {
            None
        };
        send_notification(
            outbound,
            self.state,
            codex::ServerNotification::TurnCompleted(codex::TurnCompletedNotification {
                thread_id: self.thread_id.to_string(),
                turn: turn_value_with_timing(
                    self.turn_id,
                    status,
                    Vec::new(),
                    turn_error,
                    self.started_at,
                    self.completed_at,
                ),
            }),
        )
        .await
    }

    pub(super) fn active_agent_text(&self) -> &str {
        &self.active_agent_text
    }

    pub(super) fn rendered_agent_text(&self) -> &str {
        &self.rendered_agent_text
    }
}

fn reasoning_item(item_id: &str, text: &str) -> codex::ThreadItem {
    codex::ThreadItem::Reasoning {
        id: item_id.to_string(),
        summary: Vec::new(),
        content: (!text.is_empty())
            .then(|| vec![text.to_string()])
            .unwrap_or_default(),
    }
}

fn suppress_blank_agent_delta(active_agent_text: &str, delta: &str) -> bool {
    delta.trim().is_empty() && active_agent_text.trim().is_empty()
}

fn terminal_reasoning_backfill_allowed(item_open: bool, item_completed: bool) -> bool {
    item_open || !item_completed
}

fn observe_latest_response_completion(current: &mut Option<i64>, observed: Option<i64>) {
    if observed.is_some() {
        *current = observed;
    }
}

#[cfg(test)]
#[path = "../../../../gents/src/lean_vocab_test/support.rs"]
mod lean_reasoning_contracts;

#[cfg(test)]
mod tests {
    use gents_codex_protocol as codex;

    use super::{
        observe_latest_response_completion, reasoning_item, suppress_blank_agent_delta,
        terminal_reasoning_backfill_allowed, ReasoningCompletionTracker,
    };

    // Observations begin after the stream cursor has selected a segment/delta.
    // This exercises emitted protocol notifications, not cursor delta discovery.
    #[tokio::test]
    async fn generated_reasoning_projection_cases_drive_turn_projection_notifications() {
        use super::super::{CodexSidecar, ShimState};
        use std::sync::{atomic::AtomicU64, Arc};
        use std::time::Duration;
        use tokio::sync::{mpsc, Mutex};

        let temp = tempfile::tempdir().expect("reasoning projection directory");
        let node = Arc::new(
            gents::defra_node::EmbeddedNode::builder()
                .data_path(temp.path().join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
                .build()
                .await
                .expect("embedded node"),
        );
        let state = ShimState {
            codex_home: temp.path().to_path_buf(),
            trace_path: temp.path().join("reasoning.jsonl"),
            cwd: temp.path().to_path_buf(),
            fs_root: None,
            node: node.clone(),
            background_execution_registry: gents::BackgroundExecutionRegistry::default(),
            graphql: Arc::from("http://127.0.0.1/graphql"),
            agent_did: Arc::from("did:test:reasoning"),
            behavior_id: Arc::from("reasoning"),
            id_counter: Arc::new(AtomicU64::new(1)),
            timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(10),
            sidecar: Arc::new(Mutex::new(CodexSidecar::default())),
            auth_token: None,
        };
        let cases = super::lean_reasoning_contracts::lean_codex_shim_reasoning_projection_cases();
        assert!(!cases.is_empty());
        for case in cases {
            let (outbound, mut notifications) = mpsc::unbounded_channel();
            let mut projection = super::TurnProjection::new(
                &state,
                "thread",
                "turn",
                temp.path().to_path_buf(),
                None,
            );
            // Current primed fixtures resume an already open item; cursor-only
            // priming without an open item needs a stream-owner fixture.
            assert!(!case.cursor_primed || case.item_open, "{}", case.witness);
            if case.item_open {
                projection.resume_reasoning(
                    "reasoning".to_string(),
                    case.streamed_text.as_deref().unwrap_or(""),
                );
            }
            if case.item_completed {
                projection
                    .completed_reasoning_items
                    .record("reasoning".to_string());
            }
            if let Some(delta) = case.live_delta.as_deref() {
                projection
                    .append_reasoning_delta(&outbound, "reasoning", delta)
                    .await
                    .expect("append reasoning");
            }
            if case.terminal {
                projection
                    .finish_reasoning(&outbound, "reasoning", case.durable_text.as_deref())
                    .await
                    .expect("finish reasoning");
            }
            let mut events = Vec::new();
            let mut deltas = String::new();
            let mut completed_text = None;
            while let Ok(payload) = notifications.try_recv() {
                let notification: codex::ServerNotification =
                    serde_json::from_str(&payload).expect("Codex notification");
                match notification {
                    codex::ServerNotification::ItemStarted(_) => events.push("started"),
                    codex::ServerNotification::ReasoningTextDelta(delta) => {
                        events.push("rawTextDelta");
                        deltas.push_str(&delta.delta);
                    }
                    codex::ServerNotification::ItemCompleted(completed) => {
                        events.push("completed");
                        match completed.item {
                            codex::ThreadItem::Reasoning { content, .. } => {
                                completed_text = Some(content.concat())
                            }
                            other => panic!("unexpected completed item: {other:?}"),
                        }
                    }
                    other => panic!("unexpected reasoning notification: {other:?}"),
                }
            }
            assert_eq!(events, case.projected_events, "{}", case.witness);
            assert_eq!(
                (!deltas.is_empty()).then_some(deltas),
                case.projected_delta,
                "{}",
                case.witness
            );
            assert_eq!(completed_text, case.completed_text, "{}", case.witness);
        }
        node.shutdown().await;
    }

    #[test]
    fn suppresses_blank_delta_that_would_open_phantom_agent_stream() {
        assert!(suppress_blank_agent_delta("", "\n  "));
        assert!(suppress_blank_agent_delta("  ", "\n  "));
    }

    #[test]
    fn keeps_blank_delta_inside_visible_agent_stream() {
        assert!(!suppress_blank_agent_delta("visible", "\n\n"));
    }

    #[test]
    fn keeps_visible_delta() {
        assert!(!suppress_blank_agent_delta("", "answer"));
    }

    #[test]
    fn completed_reasoning_item_is_terminally_idempotent() {
        let mut completed = ReasoningCompletionTracker::default();
        assert!(!completed.contains("reasoning-1"));
        assert!(completed.record("reasoning-1".to_string()));
        assert!(completed.contains("reasoning-1"));
        assert!(!completed.record("reasoning-1".to_string()));
    }

    #[test]
    fn reset_before_terminal_suppresses_durable_backfill() {
        assert!(terminal_reasoning_backfill_allowed(false, false));
        assert!(terminal_reasoning_backfill_allowed(true, true));
        assert!(!terminal_reasoning_backfill_allowed(false, true));
    }

    #[test]
    fn final_response_completion_overwrites_prior_materialization_time() {
        let mut completed_at_ms = None;
        observe_latest_response_completion(&mut completed_at_ms, Some(100));
        observe_latest_response_completion(&mut completed_at_ms, None);
        observe_latest_response_completion(&mut completed_at_ms, Some(250));
        assert_eq!(completed_at_ms, Some(250));
    }

    #[test]
    fn reasoning_projection_round_trips_through_pinned_codex_protocol() {
        let item = reasoning_item("reasoning-1", "durable reasoning");
        let encoded = serde_json::to_value(&item).expect("encode reasoning item");
        let decoded: codex::ThreadItem =
            serde_json::from_value(encoded).expect("decode reasoning item");
        assert!(matches!(
            decoded,
            codex::ThreadItem::Reasoning { id, summary, content }
                if id == "reasoning-1"
                    && summary.is_empty()
                    && content == ["durable reasoning"]
        ));

        let notification =
            codex::ServerNotification::ReasoningTextDelta(codex::ReasoningTextDeltaNotification {
                thread_id: "thread-1".to_string(),
                turn_id: "turn-1".to_string(),
                item_id: "reasoning-1".to_string(),
                delta: "increment".to_string(),
                content_index: 0,
            });
        let encoded = serde_json::to_value(&notification).expect("encode reasoning delta");
        let decoded: codex::ServerNotification =
            serde_json::from_value(encoded).expect("decode reasoning delta");
        assert!(matches!(
            decoded,
            codex::ServerNotification::ReasoningTextDelta(delta)
                if delta.item_id == "reasoning-1"
                    && delta.delta == "increment"
                    && delta.content_index == 0
        ));
    }

    // The Lean tool-metadata table (`Proofs/CodexShim/Projection.lean`
    // precedence theorems) drives the REAL conversion owner:
    // `gents_tool_item` + `tool_duration_ms` from `progress.rs`. A drift in
    // either side (Lean table vs adapter) must fail this test.
    #[test]
    fn generated_tool_metadata_cases_drive_the_tool_item_owner() {
        use gents_codex_protocol::ThreadItem;

        use super::super::progress::gents_tool_item;
        use super::lean_reasoning_contracts::lean_codex_shim_tool_metadata_cases;

        fn tool_from_case(
            case: &super::lean_reasoning_contracts::LeanCodexShimToolMetadataCase,
        ) -> super::super::progress::GentsToolCallProgress {
            let to_rfc3339 = |ms: i64| {
                use chrono::TimeZone;
                chrono::Utc
                    .timestamp_millis_opt(ms)
                    .single()
                    .expect("in-range timestamp")
                    .to_rfc3339()
            };
            super::super::progress::GentsToolCallProgress {
                tool_call_key: "session:call".to_string(),
                tool_name: case.fallback_tool.clone(),
                lifecycle_state: Some("completed".to_string()),
                selected_service_id: case.selected_server.clone(),
                selected_tool_name: case.selected_tool.clone(),
                denial_reason: case.denial_reason.clone(),
                cancel_cause: case.cancel_cause.clone(),
                tool_failure_class: case.failure_class.clone(),
                result: case.result_fallback.clone().unwrap_or_default(),
                latency_ms: case.latency_ms.map(|ms| ms as i64),
                started_at: case.started_at_ms.map(|ms| to_rfc3339(ms as i64)),
                completed_at: case.completed_at_ms.map(|ms| to_rfc3339(ms as i64)),
                ..Default::default()
            }
        }

        for case in lean_codex_shim_tool_metadata_cases() {
            let tool = tool_from_case(case);
            let item = gents_tool_item(&tool, codex::McpToolCallStatus::Failed);
            let ThreadItem::McpToolCall {
                server,
                tool: tool_name,
                error,
                duration_ms,
                ..
            } = item
            else {
                panic!("{}: expected an MCP tool call item", case.witness);
            };

            assert_eq!(
                server, case.projected_server,
                "{}: server identity",
                case.witness
            );
            assert_eq!(
                tool_name, case.projected_tool,
                "{}: tool identity",
                case.witness
            );
            let expected_failure = case.projected_failure.as_deref();
            match (error, expected_failure) {
                (Some(error), Some(expected)) => {
                    assert_eq!(
                        error.message, expected,
                        "{}: failure diagnostic",
                        case.witness
                    );
                }
                (Some(error), None) => {
                    // The adapter's terminal fallback message is the only
                    // difference from the Lean projection; a present error
                    // with no Lean diagnostic is the documented default.
                    assert_eq!(error.message, "GENTS tool call failed", "{}", case.witness);
                }
                (None, expected) => {
                    panic!(
                        "{}: failed tool must carry an error (expected {expected:?})",
                        case.witness
                    );
                }
            }

            // Duration (CodexShim.projectedDurationMs): persisted latency
            // first, then the timestamp difference, and never an invented
            // value from incomplete timestamps.
            assert_eq!(
                duration_ms,
                case.projected_duration_ms.map(|ms| ms as i64),
                "{}: duration",
                case.witness
            );

            // These columns carry tool start/completion observations. The separate
            // Lean event timestamp input is tested at the notification sender below.
            let started_ms = super::super::progress::tool_started_at_ms(&tool);
            let completed_ms = super::super::progress::tool_completed_at_ms(&tool);
            assert_eq!(
                (started_ms, completed_ms),
                (
                    case.started_at_ms.map(|ms| ms as i64),
                    case.completed_at_ms.map(|ms| ms as i64)
                ),
                "{}: persisted timestamps must round-trip through the RFC3339 columns",
                case.witness
            );
        }
    }

    // The Lean context-usage table drives the REAL usage owner:
    // `thread_token_usage` (thread_projection/usage.rs). It must emit the
    // cumulative totals and the latest-call totals as DISTINCT breakdowns and
    // pass the model window through — never fold the latest call into the
    // cumulative total.
    #[test]
    fn generated_context_usage_cases_drive_the_token_usage_owner() {
        use super::super::thread_projection::thread_token_usage;
        use super::super::thread_projection::TokenTotals;
        use super::lean_reasoning_contracts::lean_codex_shim_context_usage_cases;

        for case in lean_codex_shim_context_usage_cases() {
            let total = thread_token_usage(
                TokenTotals {
                    input_tokens: case.cumulative_input as i64,
                    output_tokens: case.cumulative_output as i64,
                },
                TokenTotals {
                    input_tokens: case.latest_prompt as i64,
                    output_tokens: case.latest_completion as i64,
                },
                case.model_window as i64,
            );

            assert_eq!(
                total.total.total_tokens, case.total_tokens as i64,
                "{}: cumulative breakdown drifted",
                case.witness
            );
            assert_eq!(
                total.total.input_tokens, case.cumulative_input as i64,
                "{}: cumulative input drifted",
                case.witness
            );
            assert_eq!(
                total.total.output_tokens, case.cumulative_output as i64,
                "{}: cumulative output drifted",
                case.witness
            );
            assert_eq!(
                total.last.total_tokens, case.current_context_tokens as i64,
                "{}: the latest-call breakdown must be the current context",
                case.witness
            );
            assert_eq!(
                total.last.input_tokens, case.latest_prompt as i64,
                "{}: latest-call input drifted",
                case.witness
            );
            assert_eq!(
                total.last.output_tokens, case.latest_completion as i64,
                "{}: latest-call output drifted",
                case.witness
            );
            assert_eq!(
                total.model_context_window,
                Some(case.model_window as i64),
                "{}: the model window must pass through for the client's \
                 remaining-context derivation",
                case.witness
            );
        }
    }

    async fn notification_test_state(directory: &std::path::Path) -> super::super::ShimState {
        use super::super::{CodexSidecar, ShimState};
        use std::sync::{atomic::AtomicU64, Arc};
        use std::time::Duration;
        use tokio::sync::Mutex;
        let node = Arc::new(
            gents::defra_node::EmbeddedNode::builder()
                .data_path(directory.join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
                .build()
                .await
                .expect("embedded node"),
        );
        let state = ShimState {
            codex_home: directory.to_path_buf(),
            trace_path: directory.join("notifications.jsonl"),
            cwd: directory.to_path_buf(),
            fs_root: None,
            node: node.clone(),
            background_execution_registry: gents::BackgroundExecutionRegistry::default(),
            graphql: Arc::from("http://127.0.0.1/graphql"),
            agent_did: Arc::from("did:test:notification"),
            behavior_id: Arc::from("notification"),
            id_counter: Arc::new(AtomicU64::new(1)),
            timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(10),
            sidecar: Arc::new(Mutex::new(CodexSidecar::default())),
            auth_token: None,
        };

        state
    }

    // Terminal wire owner: `finish_turn` must complete any open reasoning
    // item, close the agent message with the FinalAnswer phase, and emit a
    // `TurnCompleted` notification carrying the turn timing that
    // `set_completed_at` observed. A regression here drops the client's
    // completed/failed distinction or invents a missing completion time.
    #[tokio::test]
    async fn finish_turn_emits_terminal_turn_with_observed_timing() {
        use tokio::sync::mpsc;

        let temp = tempfile::tempdir().expect("finish turn directory");
        let state = notification_test_state(temp.path()).await;

        // Completed turn with observed timing.
        let (outbound, mut notifications) = mpsc::unbounded_channel();
        let mut projection = super::TurnProjection::new(
            &state,
            "thread",
            "turn",
            temp.path().to_path_buf(),
            Some(100),
        );
        projection.set_completed_at(Some(125));
        projection
            .finish_turn(&outbound, codex::TurnStatus::Completed, None)
            .await
            .expect("finish completed turn");
        let completed: codex::ServerNotification = serde_json::from_str(
            &notifications
                .try_recv()
                .expect("terminal notification must be sent"),
        )
        .expect("Codex notification");
        let codex::ServerNotification::TurnCompleted(turn_completed) = completed else {
            panic!("expected TurnCompleted, got {completed:?}");
        };
        assert_eq!(turn_completed.thread_id, "thread");
        assert_eq!(turn_completed.turn.id, "turn");
        assert_eq!(turn_completed.turn.status, codex::TurnStatus::Completed);
        assert!(turn_completed.turn.error.is_none());
        assert_eq!(turn_completed.turn.started_at, Some(100));
        assert_eq!(turn_completed.turn.completed_at, Some(125));
        assert_eq!(turn_completed.turn.duration_ms, Some(25_000));

        // Failed turn carries the failure message and never a completion time
        // that was never observed.
        let (outbound, mut notifications) = mpsc::unbounded_channel();
        let mut projection =
            super::TurnProjection::new(&state, "thread", "turn", temp.path().to_path_buf(), None);
        projection
            .finish_turn(&outbound, codex::TurnStatus::Failed, None)
            .await
            .expect("finish failed turn");
        let failed: codex::ServerNotification = serde_json::from_str(
            &notifications
                .try_recv()
                .expect("terminal notification must be sent"),
        )
        .expect("Codex notification");
        let codex::ServerNotification::TurnCompleted(turn_completed) = failed else {
            panic!("expected TurnCompleted, got {failed:?}");
        };
        assert_eq!(turn_completed.turn.status, codex::TurnStatus::Failed);
        assert!(turn_completed.turn.completed_at.is_none());
        assert!(turn_completed.turn.duration_ms.is_none());
        assert_eq!(
            turn_completed
                .turn
                .error
                .expect("failed turn must carry the error")
                .message,
            "GENTS turn failed",
            "an absent failure reason must use the documented default"
        );

        // An open agent message and open reasoning item are closed by the
        // terminal notification, with the agent item carrying FinalAnswer.
        let (outbound, mut notifications) = mpsc::unbounded_channel();
        let mut projection =
            super::TurnProjection::new(&state, "thread", "turn", temp.path().to_path_buf(), None);
        projection.resume_agent_message("agent-item".to_string(), "final reply");
        projection.resume_reasoning("reasoning-item".to_string(), "thinking");
        projection
            .finish_turn(&outbound, codex::TurnStatus::Completed, None)
            .await
            .expect("finish turn with open items");
        // Close the channel so the drain below terminates.
        drop(outbound);
        let mut agent_phases = Vec::new();
        let mut saw_reasoning_completed = false;
        let mut turn_completed_count = 0;
        while let Some(payload) = notifications.recv().await {
            let notification: codex::ServerNotification =
                serde_json::from_str(&payload).expect("Codex notification");
            match notification {
                codex::ServerNotification::ItemCompleted(completed) => match completed.item {
                    codex::ThreadItem::AgentMessage { phase, text, .. } => {
                        assert_eq!(text, "final reply");
                        agent_phases.push(phase);
                    }
                    codex::ThreadItem::Reasoning { .. } => saw_reasoning_completed = true,
                    other => panic!("unexpected completed item: {other:?}"),
                },
                codex::ServerNotification::TurnCompleted(_) => turn_completed_count += 1,
                other => panic!("unexpected notification: {other:?}"),
            }
        }
        assert_eq!(
            agent_phases,
            vec![Some(gents_codex_protocol::MessagePhase::FinalAnswer)],
            "the terminal agent item must carry the FinalAnswer phase"
        );
        assert!(saw_reasoning_completed, "open reasoning must be closed");
        assert_eq!(
            turn_completed_count, 1,
            "the turn must terminate exactly once"
        );
        state.node.shutdown().await;
    }

    // The persisted-vs-observation event-timestamp precedence
    // (`CodexShim.projectedEventTimestampMs`) is decided at EMIT time by
    // `send_tool_completed`: the notification carries the persisted
    // `completed_at` when present and falls back to `now_millis()` otherwise.
    // A copied predicate in a test could never catch an emit-path regression
    // (e.g. always stamping observation time), so drive the real sender.
    #[tokio::test]
    async fn tool_completion_notification_prefers_persisted_event_time() {
        use tokio::sync::mpsc;

        let temp = tempfile::tempdir().expect("tool event timestamp directory");
        let state = notification_test_state(temp.path()).await;

        let mut tool = super::super::progress::GentsToolCallProgress {
            tool_call_key: "session:call".to_string(),
            tool_name: "search".to_string(),
            lifecycle_state: Some("completed".to_string()),
            args: "{}".to_string(),
            ..Default::default()
        };

        // Persisted completion time wins over the observation time.
        tool.completed_at = Some("2026-07-23T00:00:01.250Z".to_string());
        let (outbound, mut notifications) = mpsc::unbounded_channel();
        let mut projection =
            super::TurnProjection::new(&state, "thread", "turn", temp.path().to_path_buf(), None);
        let before_send = super::super::protocol::now_millis();
        projection
            .send_tool_projection_update(
                &outbound,
                &tool,
                None,
                &super::super::projection_state::ToolProjectionStatus::Mcp(
                    super::super::projection_state::ProjectionStatus::Completed,
                ),
            )
            .await
            .expect("send tool projection");
        let after_send = super::super::protocol::now_millis();
        drop(outbound);
        let mut completed_at_ms = None;
        let mut started_at_ms = None;
        while let Some(payload) = notifications.recv().await {
            let notification: codex::ServerNotification =
                serde_json::from_str(&payload).expect("Codex notification");
            match notification {
                codex::ServerNotification::ItemStarted(started) => {
                    started_at_ms = Some(started.started_at_ms)
                }
                codex::ServerNotification::ItemCompleted(completed) => {
                    completed_at_ms = Some(completed.completed_at_ms)
                }
                other => panic!("unexpected notification: {other:?}"),
            }
        }
        let persisted_ms = super::super::progress::timestamp_millis("2026-07-23T00:00:01.250Z")
            .expect("persisted timestamp");
        // The started item has no persisted started_at, so it stamps the
        // emit-time observation; the completed item must carry the persisted
        // completion time, never the observation fallback.
        assert!(
            started_at_ms.is_some_and(|ms| (before_send..=after_send).contains(&ms)),
            "a missing started_at must fall back to the emit-time clock: {started_at_ms:?}"
        );
        assert_eq!(
            completed_at_ms,
            Some(persisted_ms),
            "the persisted event timestamp must precede the observation fallback"
        );

        // Without any persisted event time the emit path stamps the
        // observation time (`now_millis()`), never zero or an invented past.
        let tool = super::super::progress::GentsToolCallProgress {
            tool_call_key: "session:call".to_string(),
            tool_name: "search".to_string(),
            lifecycle_state: Some("completed".to_string()),
            args: "{}".to_string(),
            ..Default::default()
        };
        let (outbound, mut notifications) = mpsc::unbounded_channel();
        let mut projection =
            super::TurnProjection::new(&state, "thread", "turn", temp.path().to_path_buf(), None);
        let before_send = super::super::protocol::now_millis();
        projection
            .send_tool_projection_update(
                &outbound,
                &tool,
                None,
                &super::super::projection_state::ToolProjectionStatus::Mcp(
                    super::super::projection_state::ProjectionStatus::Completed,
                ),
            )
            .await
            .expect("send tool projection");
        let after_send = super::super::protocol::now_millis();
        drop(outbound);
        let mut completed_at_ms = None;
        while let Some(payload) = notifications.recv().await {
            let notification: codex::ServerNotification =
                serde_json::from_str(&payload).expect("Codex notification");
            if let codex::ServerNotification::ItemCompleted(completed) = notification {
                completed_at_ms = Some(completed.completed_at_ms);
            }
        }
        let observed = completed_at_ms.expect("observation fallback must stamp a time");
        assert!(
            (before_send..=after_send).contains(&observed),
            "the observation fallback must be the emit-time clock, got {observed}"
        );
        state.node.shutdown().await;
    }
}

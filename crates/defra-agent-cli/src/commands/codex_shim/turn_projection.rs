use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use codex_app_server_protocol as codex;
use codex_protocol::models::MessagePhase;

use super::command_projection::{
    command_execution_item, command_output_payload, file_change_item, ToolProjectionStatus,
};
use super::compaction_projection::{
    compaction_projection_events, context_compaction_item, CompactionProjectionEvent,
    DefraCompactionProgress,
};
use super::progress::{
    defra_tool_item, tool_completed_at_ms, tool_started_at_ms, DefraToolCallProgress,
};
use super::protocol::{
    agent_message_item, agent_message_item_with_phase, now_millis, send_notification,
    turn_value_with_timing,
};
use super::subagent_projection::{collab_tool_item, CollabProjection};
use super::{Outbound, ShimState};

#[derive(Default)]
struct ReasoningCompletionTracker {
    item_ids: HashSet<String>,
}

impl ReasoningCompletionTracker {
    fn contains(&self, item_id: &str) -> bool {
        self.item_ids.contains(item_id)
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
    completed_items: Vec<codex::ThreadItem>,
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
            completed_items: Vec::new(),
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
        if self.response_completed_at_ms.is_none() {
            self.response_completed_at_ms = completed_at_ms;
        }
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
        self.completed_items.push(item);
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
            let item_id = self.state.next_id("defra-message");
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
        self.completed_items.push(completed_item);
        Ok(())
    }

    async fn send_tool_started(
        &mut self,
        outbound: &Outbound,
        tool: &DefraToolCallProgress,
    ) -> Result<()> {
        self.finish_agent_message_with_phase(outbound, Some(MessagePhase::Commentary))
            .await?;
        send_notification(
            outbound,
            self.state,
            codex::ServerNotification::ItemStarted(codex::ItemStartedNotification {
                item: defra_tool_item(tool, codex::McpToolCallStatus::InProgress),
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
        tool: &DefraToolCallProgress,
        status: codex::McpToolCallStatus,
    ) -> Result<()> {
        self.finish_agent_message_with_phase(outbound, Some(MessagePhase::Commentary))
            .await?;
        let completed_item = defra_tool_item(tool, status);
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
        self.completed_items.push(completed_item);
        Ok(())
    }

    async fn send_command_execution_started(
        &mut self,
        outbound: &Outbound,
        tool: &DefraToolCallProgress,
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
        tool: &DefraToolCallProgress,
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
        self.completed_items.push(completed_item);
        Ok(())
    }

    async fn send_file_change_started(
        &mut self,
        outbound: &Outbound,
        tool: &DefraToolCallProgress,
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
        tool: &DefraToolCallProgress,
        projection: &CollabProjection,
    ) -> Result<()> {
        self.finish_agent_message_with_phase(outbound, Some(MessagePhase::Commentary))
            .await?;
        let mut started = projection.clone();
        started.status = codex::CollabAgentToolCallStatus::InProgress;
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
        tool: &DefraToolCallProgress,
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
        if let Some(existing) = self
            .completed_items
            .iter()
            .position(|existing| existing.id() == item.id())
        {
            self.completed_items[existing] = item;
        } else {
            self.completed_items.push(item);
        }
        Ok(())
    }

    async fn send_file_change_completed(
        &mut self,
        outbound: &Outbound,
        tool: &DefraToolCallProgress,
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
        self.completed_items.push(item);
        Ok(())
    }

    pub(super) async fn send_tool_projection_update(
        &mut self,
        outbound: &Outbound,
        tool: &DefraToolCallProgress,
        previous: Option<&ToolProjectionStatus>,
        current: &ToolProjectionStatus,
    ) -> Result<()> {
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
                if *status != codex::McpToolCallStatus::InProgress {
                    self.send_tool_started(outbound, tool).await?;
                }
                match status {
                    codex::McpToolCallStatus::InProgress => {
                        self.send_tool_started(outbound, tool).await
                    }
                    codex::McpToolCallStatus::Completed | codex::McpToolCallStatus::Failed => {
                        self.send_tool_completed(outbound, tool, status.clone())
                            .await
                    }
                }
            }
            (None, ToolProjectionStatus::Mcp(status))
                if *status != codex::McpToolCallStatus::InProgress =>
            {
                self.send_tool_started(outbound, tool).await?;
                self.send_tool_completed(outbound, tool, status.clone())
                    .await
            }
            (Some(ToolProjectionStatus::DeferredCollab), ToolProjectionStatus::Mcp(status))
                if *status != codex::McpToolCallStatus::InProgress =>
            {
                self.send_tool_started(outbound, tool).await?;
                self.send_tool_completed(outbound, tool, status.clone())
                    .await
            }
            (_, ToolProjectionStatus::Mcp(codex::McpToolCallStatus::InProgress)) => {
                self.send_tool_started(outbound, tool).await
            }
            (_, ToolProjectionStatus::Mcp(status)) => {
                self.send_tool_completed(outbound, tool, status.clone())
                    .await
            }
            (None, ToolProjectionStatus::Command(status))
                if *status != codex::CommandExecutionStatus::InProgress =>
            {
                self.send_command_execution_started(
                    outbound,
                    tool,
                    codex::CommandExecutionStatus::InProgress,
                )
                .await?;
                self.send_command_execution_completed(outbound, tool, status.clone())
                    .await
            }
            (_, ToolProjectionStatus::Command(codex::CommandExecutionStatus::InProgress)) => {
                self.send_command_execution_started(outbound, tool, current.command_status())
                    .await
            }
            (_, ToolProjectionStatus::Command(status)) => {
                self.send_command_execution_completed(outbound, tool, status.clone())
                    .await
            }
            (
                None | Some(ToolProjectionStatus::DeferredCollab),
                ToolProjectionStatus::Collab(projection),
            ) if projection.status != codex::CollabAgentToolCallStatus::InProgress => {
                self.send_collab_started(outbound, tool, projection).await?;
                self.send_collab_completed(outbound, tool, projection).await
            }
            (_, ToolProjectionStatus::Collab(projection))
                if projection.status == codex::CollabAgentToolCallStatus::InProgress =>
            {
                self.send_collab_started(outbound, tool, projection).await
            }
            (_, ToolProjectionStatus::Collab(projection)) => {
                self.send_collab_completed(outbound, tool, projection).await
            }
            (_, ToolProjectionStatus::DeferredCollab) => Ok(()),
            (_, ToolProjectionStatus::DeferredFileChange) => Ok(()),
            (None, ToolProjectionStatus::FileChange(status))
            | (
                Some(ToolProjectionStatus::DeferredFileChange),
                ToolProjectionStatus::FileChange(status),
            ) if *status != codex::PatchApplyStatus::InProgress => {
                self.send_file_change_started(outbound, tool).await?;
                self.send_file_change_completed(outbound, tool, status.clone())
                    .await
            }
            (_, ToolProjectionStatus::FileChange(codex::PatchApplyStatus::InProgress)) => {
                self.send_file_change_started(outbound, tool).await
            }
            (_, ToolProjectionStatus::FileChange(status)) => {
                self.send_file_change_completed(outbound, tool, status.clone())
                    .await
            }
        }
    }

    pub(super) async fn send_compaction_projection_update(
        &mut self,
        outbound: &Outbound,
        compaction: &DefraCompactionProgress,
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
                    self.completed_items.push(item);
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
                message: error_message.unwrap_or_else(|| "DEFRA turn failed".to_string()),
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

#[cfg(test)]
mod tests {
    use codex_app_server_protocol as codex;

    use super::{reasoning_item, suppress_blank_agent_delta, ReasoningCompletionTracker};

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
}

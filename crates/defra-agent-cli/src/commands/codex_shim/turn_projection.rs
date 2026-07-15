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
use super::progress::{defra_tool_item, DefraToolCallProgress};
use super::protocol::{
    agent_message_item, agent_message_item_with_phase, now_millis, send_notification, turn_value,
};
use super::subagent_projection::{collab_tool_item, CollabProjection};
use super::{Outbound, ShimState};

pub(super) struct TurnProjection<'a> {
    state: &'a ShimState,
    pub(super) thread_id: &'a str,
    pub(super) turn_id: &'a str,
    pub(super) cwd: PathBuf,
    active_agent_item_id: Option<String>,
    active_agent_text: String,
    rendered_agent_text: String,
    completed_items: Vec<codex::ThreadItem>,
}

impl<'a> TurnProjection<'a> {
    pub(super) fn new(
        state: &'a ShimState,
        thread_id: &'a str,
        turn_id: &'a str,
        cwd: PathBuf,
    ) -> Self {
        Self {
            state,
            thread_id,
            turn_id,
            cwd,
            active_agent_item_id: None,
            active_agent_text: String::new(),
            rendered_agent_text: String::new(),
            completed_items: Vec::new(),
        }
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
                    started_at_ms: now_millis(),
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
                completed_at_ms: now_millis(),
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
                started_at_ms: now_millis(),
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
                completed_at_ms: now_millis(),
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
                started_at_ms: now_millis(),
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
                completed_at_ms: now_millis(),
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
                started_at_ms: now_millis(),
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
                started_at_ms: now_millis(),
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
                completed_at_ms: now_millis(),
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
                completed_at_ms: now_millis(),
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
                turn: turn_value(self.turn_id, status, Vec::new(), turn_error),
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

fn suppress_blank_agent_delta(active_agent_text: &str, delta: &str) -> bool {
    delta.trim().is_empty() && active_agent_text.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::suppress_blank_agent_delta;

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
}

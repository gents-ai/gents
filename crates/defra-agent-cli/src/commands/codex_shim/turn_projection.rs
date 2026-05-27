use std::path::PathBuf;

use anyhow::Result;
use codex_app_server_protocol as codex;

use super::command_projection::{
    command_execution_item, command_output_payload, ToolProjectionStatus,
};
use super::progress::{defra_tool_item, DefraToolCallProgress};
use super::protocol::{agent_message_item, now_millis, send_notification, turn_value};
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

    pub(super) async fn finish_agent_message(&mut self, outbound: &Outbound) -> Result<()> {
        let Some(item_id) = self.active_agent_item_id.take() else {
            return Ok(());
        };
        let text = std::mem::take(&mut self.active_agent_text);
        if text.trim().is_empty() {
            return Ok(());
        }
        let completed_item = agent_message_item(&item_id, &text);
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
        self.finish_agent_message(outbound).await?;
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
        self.finish_agent_message(outbound).await?;
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
        self.finish_agent_message(outbound).await?;
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
        self.finish_agent_message(outbound).await?;
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
        }
    }

    pub(super) async fn finish_turn(
        &mut self,
        outbound: &Outbound,
        status: codex::TurnStatus,
        error_message: Option<String>,
    ) -> Result<()> {
        self.finish_agent_message(outbound).await?;
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

use anyhow::{Context, Result};
use serde_json::json;

use super::errors::invalid_params;
use super::wire::{AcpRequest, RequestOutcome};
use super::AcpService;
use crate::commands::grok_shim::turn::sender::PromptSender;
use crate::commands::grok_shim::turn::wire::parse_prompt_request;

impl AcpService {
    pub(super) async fn handle_session_prompt(
        &self,
        request: &AcpRequest,
        prompt_sender: &PromptSender,
    ) -> Result<RequestOutcome> {
        let parsed = parse_prompt_request(&request.params, request.id.clone())
            .map_err(|error| invalid_params(error.to_string()))?;
        {
            let sessions = self.sessions.lock().await;
            let session = sessions
                .get(&parsed.session_id)
                .context("session must be created or loaded before prompting")?;
            if session.resume_from.is_some() {
                return Err(invalid_params(
                    "session replay has not handed off to live observation",
                ));
            }
        }
        if let [block] = parsed.prompt.as_slice() {
            if block.kind == "text"
                && block
                    .meta
                    .as_ref()
                    .is_none_or(|meta| meta.as_object().is_some_and(|meta| meta.is_empty()))
            {
                let goal = crate::commands::grok_shim::goals::GoalCommand::parse(&block.text)
                    .map_err(|error| invalid_params(error.to_string()))?
                    .filter(|command| {
                        !matches!(
                            command,
                            crate::commands::grok_shim::goals::GoalCommand::Create { .. }
                        )
                    });
                let workflow = crate::commands::grok_shim::workflow_command::WorkflowCommand::parse(
                    &block.text,
                );
                if goal.is_some() || workflow.is_some() {
                    crate::commands::grok_shim::sessions::load(
                        &self.config.node,
                        &self.config.node_did,
                        &self.config.agent_id,
                        &parsed.session_id,
                    )
                    .await?;
                    let reply = match (goal, workflow) {
                        (Some(command), _) => {
                            command
                                .execute(
                                    &self.config.node,
                                    &self.config.node_did,
                                    &parsed.session_id,
                                )
                                .await?
                        }
                        (None, Some(command)) => {
                            command
                                .execute(
                                    &self.config.node,
                                    &self.config.node_did,
                                    &parsed.session_id,
                                )
                                .await?
                        }
                        (None, None) => unreachable!("a host command matched above"),
                    };
                    for (kind, text) in [
                        ("user_message_chunk", block.text.as_str()),
                        ("agent_message_chunk", reply.as_str()),
                    ] {
                        self.projections.session_updates().send(&parsed.session_id, |event_id, total_tokens| {
                            Ok(crate::commands::grok_shim::projection::session_notification_for_method("session/update", &parsed.session_id,
                                json!({"sessionUpdate":kind,"content":{"type":"text","text":text}, "_meta":{"hostTurn":true}}),
                                crate::commands::grok_shim::projection::stamp_update_meta(event_id, total_tokens,
                                    parsed.prompt_id.as_deref(), None, crate::commands::grok_shim::projection::UpdateTimestamps::default())))
                        }, crate::commands::grok_shim::turn::sender::PromptSenderLine(prompt_sender)).await?;
                    }
                    self.turns
                        .observe_session(
                            &parsed.session_id,
                            prompt_sender.clone(),
                            self.projections.clone(),
                        )
                        .await;
                    return Ok(RequestOutcome {
                        notifications: Vec::new(),
                        result: json!({"stopReason":"end_turn"}),
                    });
                }
            }
        }
        let stop_reason = self
            .turns
            .handle_prompt(parsed, prompt_sender, &self.projections)
            .await?;
        Ok(RequestOutcome {
            notifications: Vec::new(),
            result: stop_reason,
        })
    }
}

use std::time::{Duration, Instant};

use anyhow::Result;
use rig::agent::MultiTurnStreamItem;
use rig::completion::message::{
    AssistantContent as AssistantMessageContent, Message as CompletionMessage,
    Reasoning as AssistantReasoning, Text as CompletionText, ToolCall as AssistantToolCall,
};
use rig::one_or_many::OneOrMany;
use rig::streaming::{StreamedAssistantContent, StreamedUserContent};

use crate::hook::DefraSessionHook;
use crate::lifecycle::RequestLifecycle;
use crate::streaming::{DefraStreamWriter, StreamWriter};

pub(super) enum StreamAction {
    Continue,
    Done,
    Error(rig::agent::StreamingError),
}

pub(super) struct StreamProcessor<'a> {
    persistence_hook: &'a DefraSessionHook,
    stream_writer: &'a DefraStreamWriter,
    lifecycle: &'a mut RequestLifecycle,
    assistant_turn: AssistantTurnAccumulator,
    pub(super) streamed_text: String,
    pub(super) final_text: Option<String>,
    doc_id: &'a str,
    last_reasoning_progress_at: Option<Instant>,
}

#[cfg(test)]
mod tests;

const REASONING_PROGRESS_INTERVAL: Duration = Duration::from_millis(500);

impl<'a> StreamProcessor<'a> {
    pub(super) fn new(
        persistence_hook: &'a DefraSessionHook,
        stream_writer: &'a DefraStreamWriter,
        lifecycle: &'a mut RequestLifecycle,
        doc_id: &'a str,
    ) -> Self {
        Self {
            persistence_hook,
            stream_writer,
            lifecycle,
            assistant_turn: AssistantTurnAccumulator::default(),
            streamed_text: String::new(),
            final_text: None,
            doc_id,
            last_reasoning_progress_at: None,
        }
    }

    pub(super) async fn process_item<R>(
        &mut self,
        item: Result<MultiTurnStreamItem<R>, rig::agent::StreamingError>,
    ) -> Result<StreamAction> {
        match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(text))) => {
                self.assistant_turn.push_text(&text.text);
                self.streamed_text.push_str(&text.text);
                let flushed = self
                    .stream_writer
                    .write_tokens(self.doc_id, &text.text)
                    .await?;
                if flushed {
                    self.lifecycle.advance().await?;
                }
                Ok(StreamAction::Continue)
            }
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Reasoning(
                reasoning,
            ))) => {
                let rendered = render_reasoning_text(&reasoning);
                self.assistant_turn.push_reasoning(reasoning);
                if !rendered.is_empty() {
                    let _ = self
                        .stream_writer
                        .write_reasoning(self.doc_id, &rendered)
                        .await?;
                }
                self.mark_reasoning_progress().await?;
                Ok(StreamAction::Continue)
            }
            Ok(MultiTurnStreamItem::StreamAssistantItem(
                StreamedAssistantContent::ReasoningDelta { reasoning, id },
            )) => {
                self.assistant_turn.push_reasoning_delta(id, &reasoning);
                if !reasoning.is_empty() {
                    let _ = self
                        .stream_writer
                        .write_reasoning(self.doc_id, &reasoning)
                        .await?;
                }
                self.mark_reasoning_progress().await?;
                Ok(StreamAction::Continue)
            }
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall {
                tool_call,
                ..
            })) => {
                self.assistant_turn.push_tool_call(tool_call);
                self.lifecycle.advance().await?;
                Ok(StreamAction::Continue)
            }
            Ok(MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
                tool_result: _tool_result,
                ..
            })) => {
                if let Some(message) = self.assistant_turn.take_message() {
                    self.persistence_hook.apply_persistence_policy(
                        self.persistence_hook
                            .persist_message(&message)
                            .await
                            .map(|_| ()),
                        "persist streamed assistant turn",
                    )?;
                }
                self.lifecycle.advance().await?;
                Ok(StreamAction::Continue)
            }
            Ok(MultiTurnStreamItem::FinalResponse(response)) => {
                self.assistant_turn.reconcile_text(response.response());
                if let Some(message) = self.assistant_turn.take_message() {
                    self.persistence_hook.apply_persistence_policy(
                        self.persistence_hook
                            .persist_message(&message)
                            .await
                            .map(|_| ()),
                        "persist final assistant turn",
                    )?;
                }
                self.lifecycle.advance().await?;
                self.final_text = Some(response.response().to_string());
                Ok(StreamAction::Done)
            }
            Ok(_) => Ok(StreamAction::Continue),
            Err(error) => Ok(StreamAction::Error(error)),
        }
    }

    pub(super) fn has_observable_activity(&self) -> bool {
        self.assistant_turn.has_content()
            || !self.streamed_text.trim().is_empty()
            || self
                .final_text
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
    }

    pub(super) async fn persist_partial_turn(&mut self, context: &str) -> Result<bool> {
        let Some(message) = self.assistant_turn.take_message() else {
            return Ok(false);
        };

        self.persistence_hook.apply_persistence_policy(
            self.persistence_hook
                .persist_message(&message)
                .await
                .map(|_| ()),
            context,
        )?;

        Ok(true)
    }
}

#[derive(Default)]
struct AssistantTurnAccumulator {
    text: String,
    reasoning: Vec<AssistantReasoning>,
    pending_reasoning_delta_text: String,
    pending_reasoning_delta_id: Option<String>,
    tool_calls: Vec<AssistantToolCall>,
}

impl AssistantTurnAccumulator {
    fn push_text(&mut self, text: &str) {
        self.text.push_str(text);
    }

    fn push_reasoning(&mut self, reasoning: AssistantReasoning) {
        merge_reasoning_blocks(&mut self.reasoning, &reasoning);
    }

    fn push_reasoning_delta(&mut self, id: Option<String>, reasoning: &str) {
        self.pending_reasoning_delta_text.push_str(reasoning);
        if self.pending_reasoning_delta_id.is_none() {
            self.pending_reasoning_delta_id = id;
        }
    }

    fn push_tool_call(&mut self, tool_call: AssistantToolCall) {
        self.tool_calls.push(tool_call);
    }

    fn reconcile_text(&mut self, final_text: &str) {
        if final_text.is_empty() {
            return;
        }
        if self.text.is_empty() {
            self.text.push_str(final_text);
        } else if let Some(remainder) = final_text.strip_prefix(&self.text) {
            self.text.push_str(remainder);
        }
    }

    fn take_message(&mut self) -> Option<CompletionMessage> {
        if self.reasoning.is_empty() && !self.pending_reasoning_delta_text.is_empty() {
            let mut assembled =
                AssistantReasoning::new(&std::mem::take(&mut self.pending_reasoning_delta_text));
            if let Some(id) = self.pending_reasoning_delta_id.take() {
                assembled = assembled.with_id(id);
            }
            self.push_reasoning(assembled);
        }

        let mut content = Vec::new();
        content.extend(
            self.reasoning
                .drain(..)
                .map(AssistantMessageContent::Reasoning),
        );
        content.extend(
            self.tool_calls
                .drain(..)
                .map(AssistantMessageContent::ToolCall),
        );

        if !self.text.is_empty() {
            content.push(AssistantMessageContent::Text(CompletionText {
                text: std::mem::take(&mut self.text),
            }));
        }

        self.pending_reasoning_delta_text.clear();
        self.pending_reasoning_delta_id = None;

        OneOrMany::many(content)
            .ok()
            .map(|content| CompletionMessage::Assistant { id: None, content })
    }

    fn has_content(&self) -> bool {
        !self.text.is_empty()
            || !self.reasoning.is_empty()
            || !self.pending_reasoning_delta_text.is_empty()
            || !self.tool_calls.is_empty()
    }
}

impl<'a> StreamProcessor<'a> {
    async fn mark_reasoning_progress(&mut self) -> Result<()> {
        let should_advance = self
            .last_reasoning_progress_at
            .is_none_or(|last| last.elapsed() >= REASONING_PROGRESS_INTERVAL);
        if should_advance {
            self.lifecycle.advance().await?;
            self.last_reasoning_progress_at = Some(Instant::now());
        }
        Ok(())
    }
}

fn merge_reasoning_blocks(
    accumulated_reasoning: &mut Vec<AssistantReasoning>,
    incoming: &AssistantReasoning,
) {
    let ids_match = |existing: &AssistantReasoning| {
        matches!(
            (&existing.id, &incoming.id),
            (Some(existing_id), Some(incoming_id)) if existing_id == incoming_id
        )
    };

    if let Some(existing) = accumulated_reasoning
        .iter_mut()
        .rev()
        .find(|existing| ids_match(existing))
    {
        existing.content.extend(incoming.content.clone());
    } else {
        accumulated_reasoning.push(incoming.clone());
    }
}

fn render_reasoning_text(reasoning: &AssistantReasoning) -> String {
    use rig::completion::message::ReasoningContent;

    let mut rendered = String::new();
    for part in &reasoning.content {
        let piece = match part {
            ReasoningContent::Text { text, .. } | ReasoningContent::Summary(text) => text.as_str(),
            ReasoningContent::Encrypted(_) => "[encrypted reasoning]",
            ReasoningContent::Redacted { .. } => "[redacted reasoning]",
            _ => "[opaque reasoning]",
        };

        if piece.is_empty() {
            continue;
        }
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        rendered.push_str(piece);
    }

    rendered
}

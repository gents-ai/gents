use anyhow::{Context, Result};
use gents_protocol::message::{
    AssistantContent as AssistantMessageContent, Message as CompletionMessage,
    Reasoning as AssistantReasoning, Text as CompletionText, ToolCall as AssistantToolCall,
};
use rig::agent::MultiTurnStreamItem;
use rig::streaming::{StreamedAssistantContent, StreamedUserContent};

use crate::loop_stream::LoopStreamItem;
use crate::provider_input::ProviderInputProfile;
use crate::request_lifecycle::RequestLifecycleControl;
use crate::session_hook::{CanonicalSessionHook, SessionHook};
use crate::stream_writer::{CanonicalStreamWriter, StreamWriter};

pub enum StreamAction {
    Continue,
    Done,
    Error(rig::agent::StreamingError),
}

pub struct StreamProcessor<'a, H, W, L>
where
    L: RequestLifecycleControl,
    W: CanonicalStreamWriter<L>,
    H: CanonicalSessionHook<W::AcceptedToolCall, W::SpawnAdmissionPlan>,
{
    persistence_hook: &'a H,
    stream_writer: &'a W,
    lifecycle: &'a mut L,
    // pub, not private: gents' own stream_processor tests drive the
    // accumulator directly.
    pub assistant_turn: AssistantTurnAccumulator,
    pub streamed_text: String,
    committed_text_len: usize,
    pub final_text: Option<String>,
    pub final_message_doc_id: Option<String>,
    pending_tool_internal_ids: Vec<String>,
    active_provider_attempt: Option<(usize, u32)>,
    authored_index: u32,
    provider_profile: ProviderInputProfile,
    doc_id: &'a str,
}

impl<'a, H, W, L> StreamProcessor<'a, H, W, L>
where
    L: RequestLifecycleControl,
    W: CanonicalStreamWriter<L>,
    H: CanonicalSessionHook<W::AcceptedToolCall, W::SpawnAdmissionPlan>,
{
    pub fn new(
        persistence_hook: &'a H,
        stream_writer: &'a W,
        lifecycle: &'a mut L,
        doc_id: &'a str,
        provider_profile: ProviderInputProfile,
    ) -> Self {
        Self {
            persistence_hook,
            stream_writer,
            lifecycle,
            assistant_turn: AssistantTurnAccumulator::default(),
            streamed_text: String::new(),
            committed_text_len: 0,
            final_text: None,
            final_message_doc_id: None,
            pending_tool_internal_ids: Vec::new(),
            active_provider_attempt: None,
            authored_index: 0,
            provider_profile,
            doc_id,
        }
    }

    pub async fn validate_execution(&self) -> Result<()> {
        self.lifecycle.validate_owned_execution().await
    }

    /// When this turn's buffered tokens are next due to be written, if any
    /// are buffered. A driver waits on this instead of polling, so a stream
    /// that goes quiet mid-batch still lands its tokens on time.
    pub async fn next_flush_deadline(&self) -> Option<tokio::time::Instant> {
        self.stream_writer.next_flush_deadline(self.doc_id).await
    }

    /// Writes whatever is buffered for this turn now.
    pub async fn flush_pending(&self) -> Result<()> {
        if let Some(message) = self.assistant_turn.message_snapshot() {
            self.stream_writer
                .flush_native_partial(self.lifecycle, &message)
                .await?;
            // A successful durable flush consumes the due batching signal.
            self.stream_writer.flush_pending(self.doc_id).await?;
        }
        Ok(())
    }

    pub async fn process_item<R>(
        &mut self,
        item: Result<LoopStreamItem<R>, rig::agent::StreamingError>,
    ) -> Result<StreamAction> {
        match item {
            Ok(LoopStreamItem::ProviderAttemptStarted {
                turn,
                attempt,
                capture_scope,
            }) => {
                self.stream_writer
                    .start_provider_attempt(self.doc_id, turn, attempt, capture_scope)
                    .await;
                self.active_provider_attempt = Some((turn, attempt));
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::ProviderTurnReady {
                turn,
                attempt,
                message,
            }) => {
                let spawn_admissions = self
                    .persistence_hook
                    .preplan_spawn_admissions(&message, &self.pending_tool_internal_ids)
                    .await;
                let published = self
                    .stream_writer
                    .publish_native_turn_with_spawn_admissions(
                        self.lifecycle,
                        turn,
                        attempt,
                        &message,
                        &spawn_admissions,
                    )
                    .await?;
                // Acceptance also commits bytes still waiting for the batch
                // timer. Consume that signal before clearing the accumulator,
                // or a due timer can starve post-publication dispatch/finalization.
                self.stream_writer.flush_pending(self.doc_id).await?;
                anyhow::ensure!(
                    published.accepted_tools.len() == self.pending_tool_internal_ids.len(),
                    "accepted tool bindings do not match streamed native tool calls"
                );
                let accepted = self
                    .pending_tool_internal_ids
                    .drain(..)
                    .zip(published.accepted_tools)
                    .collect();
                self.persistence_hook
                    .adopt_accepted_tool_calls(accepted)
                    .await?;
                self.final_message_doc_id = Some(published.message_doc_id);
                self.active_provider_attempt = None;
                self.assistant_turn = AssistantTurnAccumulator::default();
                self.committed_text_len = self.streamed_text.len();
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::Item(MultiTurnStreamItem::StreamAssistantItem(
                StreamedAssistantContent::Text(text),
            ))) => {
                let had_visible_text = !self.streamed_text.trim().is_empty();
                self.assistant_turn.push_text(&text.text);
                self.streamed_text.push_str(&text.text);
                let flush_due = self
                    .stream_writer
                    .write_tokens(self.doc_id, &text.text)
                    .await?;
                let has_visible_text = !self.streamed_text.trim().is_empty();
                if flush_due || (!had_visible_text && has_visible_text) {
                    self.flush_pending().await?;
                }
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::Item(MultiTurnStreamItem::StreamAssistantItem(
                StreamedAssistantContent::Reasoning(reasoning),
            ))) => {
                let reasoning = crate::rig_compat::from_rig_reasoning(&reasoning);
                let rendered = render_reasoning_text(&reasoning);
                self.assistant_turn
                    .push_provider_reasoning(self.provider_profile, reasoning)?;
                if self.provider_profile == ProviderInputProfile::ClaudeMessages {
                    // Claude's signed final seals the preview bytes without
                    // writing them again. The signature persists with the
                    // final native turn publication, not a duplicate segment.
                    self.flush_pending().await?;
                } else if !rendered.is_empty() {
                    let flush_due = self
                        .stream_writer
                        .write_reasoning(self.doc_id, &rendered)
                        .await?;
                    if flush_due {
                        self.flush_pending().await?;
                    }
                }
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::Item(MultiTurnStreamItem::StreamAssistantItem(
                StreamedAssistantContent::ReasoningDelta { reasoning, id },
            ))) => {
                self.assistant_turn.push_provider_reasoning_delta(
                    self.provider_profile,
                    id,
                    &reasoning,
                );
                if !reasoning.is_empty() {
                    let flush_due = self
                        .stream_writer
                        .write_reasoning(self.doc_id, &reasoning)
                        .await?;
                    if flush_due {
                        self.flush_pending().await?;
                    }
                }
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::Item(MultiTurnStreamItem::StreamAssistantItem(
                StreamedAssistantContent::ToolCall {
                    tool_call,
                    internal_call_id,
                },
            ))) => {
                self.flush_pending().await?;
                self.persistence_hook
                    .register_stream_tool_call_identity(
                        &internal_call_id,
                        &tool_call.id,
                        tool_call.call_id.as_deref(),
                    )
                    .await;
                self.pending_tool_internal_ids
                    .push(internal_call_id.clone());
                self.assistant_turn
                    .push_tool_call(crate::rig_compat::from_rig_tool_call(&tool_call));
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::Item(MultiTurnStreamItem::StreamUserItem(
                StreamedUserContent::ToolResult {
                    tool_result,
                    internal_call_id,
                },
            ))) => {
                self.flush_pending().await?;
                self.assistant_turn = AssistantTurnAccumulator::default();
                self.committed_text_len = self.streamed_text.len();
                let _ = (tool_result, internal_call_id);
                self.stream_writer.reset_tail(self.doc_id).await?;
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::Item(MultiTurnStreamItem::FinalResponse(response))) => {
                self.assistant_turn.reconcile_text(response.response());
                self.final_text = Some(response.response().to_string());
                Ok(StreamAction::Done)
            }
            Ok(LoopStreamItem::TurnRetracted { turn, attempt, .. }) => {
                self.stream_writer
                    .close_provider_attempt(
                        self.lifecycle,
                        turn,
                        attempt,
                        crate::stream_writer::ProviderAttemptClose::Retracted,
                    )
                    .await?;
                self.active_provider_attempt = None;
                self.assistant_turn = AssistantTurnAccumulator::default();
                self.streamed_text.truncate(self.committed_text_len);
                self.stream_writer.reset_tail(self.doc_id).await?;
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::AuthoredInputReady { context, prompt }) => {
                if let Some(context) = context.as_ref() {
                    self.stream_writer
                        .publish_authored_message(self.lifecycle, "context", context)
                        .await?;
                }
                self.stream_writer
                    .publish_authored_message(self.lifecycle, "prompt", &prompt)
                    .await?;
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::OutputObligationPending { reminder }) => {
                self.flush_pending().await?;
                let key = format!("output-obligation:{}", self.authored_index);
                self.stream_writer
                    .publish_authored_message(self.lifecycle, &key, &reminder)
                    .await?;
                self.authored_index = self
                    .authored_index
                    .checked_add(1)
                    .context("authored message index exhausted")?;
                self.streamed_text.truncate(self.committed_text_len);
                self.stream_writer.reset_tail(self.doc_id).await?;
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::AttemptFailed {
                turn,
                attempt,
                will_retry,
                ..
            }) => {
                if !will_retry {
                    self.flush_pending().await?;
                }
                if let Some(active) = self.active_provider_attempt {
                    anyhow::ensure!(
                        active == (turn, attempt),
                        "provider failure does not match the exact active attempt"
                    );
                    self.stream_writer
                        .close_provider_attempt(
                            self.lifecycle,
                            turn,
                            attempt,
                            if will_retry {
                                crate::stream_writer::ProviderAttemptClose::Retracted
                            } else {
                                crate::stream_writer::ProviderAttemptClose::Partial
                            },
                        )
                        .await?;
                    self.active_provider_attempt = None;
                }
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::Item(_)) => Ok(StreamAction::Continue),
            Err(error) => Ok(StreamAction::Error(error)),
        }
    }

    // Not `#[cfg(test)]`: gents' own stream_processor tests call this, and a
    // cfg(test) item in this crate is invisible to a dependent crate's own
    // test build.
    pub fn has_observable_activity(&self) -> bool {
        self.assistant_turn.has_content()
            || !self.streamed_text.trim().is_empty()
            || self
                .final_text
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
    }

    pub async fn persist_partial_turn(&mut self, context: &str) -> Result<bool> {
        self.flush_pending().await?;
        let Some(message) = self.assistant_turn.take_message() else {
            return Ok(false);
        };
        let _ = (message, context);
        if let Some((turn, attempt)) = self.active_provider_attempt.take() {
            self.stream_writer
                .close_provider_attempt(
                    self.lifecycle,
                    turn,
                    attempt,
                    crate::stream_writer::ProviderAttemptClose::Partial,
                )
                .await?;
        }
        self.stream_writer.reset_tail(self.doc_id).await?;

        Ok(true)
    }
}

#[derive(Clone, Default)]
pub struct AssistantTurnAccumulator {
    content: Vec<AssistantMessageContent>,
}

impl AssistantTurnAccumulator {
    pub fn push_text(&mut self, text: &str) {
        match self.content.last_mut() {
            Some(AssistantMessageContent::Text(current)) => current.text.push_str(text),
            _ => self
                .content
                .push(AssistantMessageContent::Text(CompletionText {
                    text: text.into(),
                })),
        }
    }

    pub fn push_reasoning(&mut self, reasoning: AssistantReasoning) {
        match self.content.last_mut() {
            Some(AssistantMessageContent::Reasoning(current)) if current.id == reasoning.id => {
                current.content.extend(reasoning.content)
            }
            _ => self
                .content
                .push(AssistantMessageContent::Reasoning(reasoning)),
        }
    }

    pub fn push_reasoning_delta(&mut self, id: Option<String>, reasoning: &str) {
        self.push_reasoning(AssistantReasoning {
            id,
            content: vec![gents_protocol::message::ReasoningContent::Text {
                text: reasoning.into(),
                signature: None,
            }],
        });
    }

    pub fn push_provider_reasoning_delta(
        &mut self,
        profile: ProviderInputProfile,
        id: Option<String>,
        fragment: &str,
    ) {
        if profile != ProviderInputProfile::ClaudeMessages {
            self.push_reasoning_delta(id, fragment);
            return;
        }
        if let Some(AssistantMessageContent::Reasoning(current)) = self.content.last_mut() {
            if current.id == id {
                if let Some(gents_protocol::message::ReasoningContent::Text {
                    text,
                    signature: None,
                }) = current.content.last_mut()
                {
                    text.push_str(fragment);
                    return;
                }
            }
        }
        self.content
            .push(AssistantMessageContent::Reasoning(AssistantReasoning {
                id,
                content: vec![gents_protocol::message::ReasoningContent::Text {
                    text: fragment.to_owned(),
                    signature: None,
                }],
            }));
    }

    pub fn push_provider_reasoning(
        &mut self,
        profile: ProviderInputProfile,
        reasoning: AssistantReasoning,
    ) -> Result<()> {
        if profile != ProviderInputProfile::ClaudeMessages {
            self.push_reasoning(reasoning);
            return Ok(());
        }
        use gents_protocol::message::ReasoningContent;
        anyhow::ensure!(
            !reasoning.content.is_empty(),
            "empty Claude reasoning block"
        );
        for part in &reasoning.content {
            match part {
                ReasoningContent::Text {
                    signature: Some(signature),
                    ..
                } if !signature.is_empty() => {}
                ReasoningContent::Redacted { data } if !data.is_empty() => {}
                _ => anyhow::bail!("Claude reasoning block is unsigned or unsupported"),
            }
        }

        let first = reasoning.content.first().expect("validated nonempty");
        if let Some(AssistantMessageContent::Reasoning(current)) = self.content.last_mut() {
            if current.id == reasoning.id {
                if let Some(ReasoningContent::Text {
                    text: preview,
                    signature: None,
                }) = current.content.last_mut()
                {
                    let ReasoningContent::Text {
                        text: final_text, ..
                    } = first
                    else {
                        anyhow::bail!("Claude redacted block followed unfinished text preview");
                    };
                    anyhow::ensure!(
                        preview == final_text,
                        "Claude signed thinking changed preview bytes"
                    );
                    *current.content.last_mut().expect("preview exists") = first.clone();
                    current
                        .content
                        .extend(reasoning.content.into_iter().skip(1));
                    return Ok(());
                }
            }
        }
        self.push_reasoning(reasoning);
        Ok(())
    }

    pub fn push_tool_call(&mut self, tool_call: AssistantToolCall) {
        self.content
            .push(AssistantMessageContent::ToolCall(tool_call));
    }

    pub fn take_message(&mut self) -> Option<CompletionMessage> {
        self.build_message()
    }

    pub fn message_snapshot(&self) -> Option<CompletionMessage> {
        self.clone().build_message()
    }

    pub fn reconcile_text(&mut self, final_text: &str) {
        if final_text.is_empty() {
            return;
        }
        let accumulated = self
            .content
            .iter()
            .filter_map(|item| match item {
                AssistantMessageContent::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<String>();
        if let Some(remainder) = final_text.strip_prefix(&accumulated) {
            self.push_text(remainder);
        }
    }

    fn build_message(&mut self) -> Option<CompletionMessage> {
        let content = std::mem::take(&mut self.content);
        (!content.is_empty()).then_some(CompletionMessage::Assistant { id: None, content })
    }

    fn has_content(&self) -> bool {
        !self.content.is_empty()
    }
}

fn render_reasoning_text(reasoning: &AssistantReasoning) -> String {
    use gents_protocol::message::ReasoningContent;

    let mut rendered = String::new();
    for part in &reasoning.content {
        let piece = match part {
            ReasoningContent::Text { text, .. } | ReasoningContent::Summary(text) => text.as_str(),
            ReasoningContent::Encrypted(_) => "[encrypted reasoning]",
            ReasoningContent::Redacted { .. } => "[redacted reasoning]",
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

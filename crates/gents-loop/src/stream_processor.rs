use gents_protocol::message::{
    AssistantContent as AssistantMessageContent, Message as CompletionMessage,
    Reasoning as AssistantReasoning, Text as CompletionText, ToolCall as AssistantToolCall,
};

use crate::error::LoopStreamError;
use crate::loop_stream::LoopStreamItem;
use crate::provider_audit::AuxiliaryOutputError;
use crate::provider_input::ProviderInputProfile;
use crate::request_lifecycle::RequestLifecycleControl;
use crate::session_hook::CanonicalSessionHook;
use crate::stream_writer::CanonicalStreamWriter;

mod progress;
pub use progress::flush_while_polling;

type Result<T, E = StreamProcessorError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum StreamProcessorError {
    #[error("audit fragment belongs to another content block")]
    AuditBlockMismatch,
    #[error("audit fragment has no reasoning declaration")]
    AuditWithoutReasoning,
    #[error("redacted audit payload repeated")]
    RedactedPayloadRepeated,
    #[error("audit fragment changed reasoning field kind")]
    AuditFieldKindChanged,
    #[error("provider block order changed")]
    ProviderBlockOrderChanged,
    #[error("empty Claude reasoning block")]
    EmptyClaudeReasoning,
    #[error("Claude reasoning block is unsigned or unsupported")]
    ClaudeReasoningUnsupported,
    #[error("Claude thinking has no declared position")]
    ClaudeThinkingNoPosition,
    #[error("Claude thinking changed native kind")]
    ClaudeThinkingKindChanged,
    #[error("Claude thinking completion changed native kind")]
    ClaudeThinkingCompletionKindChanged,
    #[error("Claude thinking bytes changed")]
    ClaudeThinkingBytesChanged,
    #[error("Claude thinking signature changed")]
    ClaudeThinkingSignatureChanged,
    #[error("Claude redacted block has no declared position")]
    ClaudeRedactedNoPosition,
    #[error("Claude redacted block changed native kind")]
    ClaudeRedactedKindChanged,
    #[error("Claude redacted completion changed native kind")]
    ClaudeRedactedCompletionKindChanged,
    #[error("Claude redacted bytes changed")]
    ClaudeRedactedBytesChanged,
    #[error("Claude redacted block followed unfinished text preview")]
    ClaudeRedactedAfterPreview,
    #[error("Claude signed thinking changed preview bytes")]
    ClaudeSignedPreviewChanged,
    #[error("provider block index overflow")]
    ProviderBlockIndexOverflow,
    #[error("agent stream completed without producing any visible response content")]
    EmptyResponse,
    #[error("natural completion has no saved provider message")]
    NoSavedProviderMessage,
    #[error("reasoning audit belongs to a different provider attempt")]
    AuditDifferentAttempt,
    #[error("Claude audit reached another provider profile")]
    AuditWrongProfile,
    #[error("accepted tool bindings do not match streamed native tool calls")]
    AcceptedToolBindingsMismatch,
    #[error("authored message index exhausted")]
    AuthoredIndexExhausted,
    #[error("provider failure does not match the exact active attempt")]
    AttemptMismatch,
    #[error("queued provider audit caused an unexpected stream action")]
    QueuedAuditAction,
    #[error("stream writer failed")]
    Writer {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("session hook failed")]
    Hook {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("owned execution check failed")]
    Lifecycle {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("{context}: closing partial provider attempt")]
    PartialAttemptClose {
        context: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("{context}: resetting partial assistant tail")]
    PartialTailReset {
        context: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error(transparent)]
    Auxiliary(#[from] AuxiliaryOutputError),
}

impl StreamProcessorError {
    fn writer(source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Writer {
            source: source.into(),
        }
    }

    fn hook(source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Hook {
            source: source.into(),
        }
    }

    fn lifecycle(source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Lifecycle {
            source: source.into(),
        }
    }
}

pub enum StreamAction {
    Continue,
    Done,
    Error(LoopStreamError),
}

pub struct StreamProcessor<'a, H, W, L>
where
    L: RequestLifecycleControl,
    W: CanonicalStreamWriter<L>,
    H: CanonicalSessionHook<W::AcceptedToolCall>,
{
    persistence_hook: &'a H,
    stream_writer: &'a W,
    lifecycle: &'a mut L,
    pub assistant_turn: AssistantTurnAccumulator,
    pub streamed_text: String,
    committed_text_len: usize,
    pub final_text: Option<String>,
    pub natural_finished: bool,
    pub final_message_doc_id: Option<String>,
    pending_tool_internal_ids: Vec<String>,
    active_provider_attempt: Option<(usize, u32)>,
    active_capture_scope: Option<gents_protocol::rendered_request::CaptureScope>,
    authored_index: u32,
    provider_profile: ProviderInputProfile,
    doc_id: &'a str,
}

impl<'a, H, W, L> StreamProcessor<'a, H, W, L>
where
    L: RequestLifecycleControl,
    W: CanonicalStreamWriter<L>,
    H: CanonicalSessionHook<W::AcceptedToolCall>,
{
    async fn publish_steering(
        &self,
        candidates: Vec<crate::loop_stream::FoldedPrompt>,
    ) -> Result<Vec<crate::loop_stream::FoldedPrompt>> {
        let mut committed = Vec::new();
        for candidate in candidates {
            if self
                .stream_writer
                .try_publish_steering_message(self.lifecycle, &candidate.key, &candidate.message)
                .await
                .map_err(StreamProcessorError::writer)?
                .is_none()
            {
                break;
            }
            committed.push(candidate);
        }
        Ok(committed)
    }

    async fn finish_or_intake(&mut self, text: &str) -> Result<crate::loop_stream::FinishOrIntake> {
        loop {
            self.validate_execution()
                .await
                .map_err(StreamProcessorError::lifecycle)?;
            let Some(snapshot) = self
                .persistence_hook
                .finish_steering_snapshot()
                .await
                .map_err(StreamProcessorError::hook)?
            else {
                return Ok(crate::loop_stream::FinishOrIntake::Finished);
            };
            if !snapshot.inputs.is_empty() {
                let committed = self.publish_steering(snapshot.inputs).await?;
                if committed.is_empty() {
                    continue;
                }
                return Ok(crate::loop_stream::FinishOrIntake::Continue(committed));
            }
            if text.trim().is_empty() && self.streamed_text.trim().is_empty() {
                return Err(StreamProcessorError::EmptyResponse);
            }
            let message_doc_id = self
                .final_message_doc_id
                .as_deref()
                .ok_or(StreamProcessorError::NoSavedProviderMessage)?;
            if self
                .stream_writer
                .finish_natural_turn(
                    self.lifecycle,
                    message_doc_id,
                    &snapshot.pending_request_doc_ids,
                )
                .await
                .map_err(StreamProcessorError::writer)?
            {
                self.natural_finished = true;
                return Ok(crate::loop_stream::FinishOrIntake::Finished);
            }
        }
    }

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
            natural_finished: false,
            final_message_doc_id: None,
            pending_tool_internal_ids: Vec::new(),
            active_provider_attempt: None,
            active_capture_scope: None,
            authored_index: 0,
            provider_profile,
            doc_id,
        }
    }

    pub async fn validate_execution(&self) -> Result<(), L::Error> {
        self.lifecycle.validate_owned_execution().await
    }

    pub async fn next_flush_deadline(&self) -> Option<tokio::time::Instant> {
        self.stream_writer.next_flush_deadline(self.doc_id).await
    }

    pub async fn flush_pending(&self) -> Result<()> {
        if let Some(message) = self.assistant_turn.message_snapshot() {
            self.stream_writer
                .flush_native_partial(self.lifecycle, &message)
                .await
                .map_err(StreamProcessorError::writer)?;
            self.stream_writer
                .flush_pending(self.doc_id)
                .await
                .map_err(StreamProcessorError::writer)?;
        }
        Ok(())
    }

    pub async fn process_item(
        &mut self,
        item: Result<LoopStreamItem, LoopStreamError>,
    ) -> Result<StreamAction> {
        match item {
            Ok(LoopStreamItem::ProviderAudit(observation)) => {
                if !(self.active_provider_attempt == Some((observation.turn, observation.attempt))
                    && self.active_capture_scope == Some(observation.capture_scope))
                {
                    return Err(StreamProcessorError::AuditDifferentAttempt);
                }
                if self.provider_profile != ProviderInputProfile::ClaudeMessages {
                    return Err(StreamProcessorError::AuditWrongProfile);
                }
                if self
                    .assistant_turn
                    .apply_provider_audit(&observation.event)?
                    && self
                        .stream_writer
                        .mark_pending_output(
                            self.doc_id,
                            crate::stream_writer::PendingOutput::of_audit(&observation.event),
                        )
                        .await
                        .map_err(StreamProcessorError::writer)?
                {
                    self.flush_pending().await?;
                }
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::ProviderAttemptStarted {
                turn,
                attempt,
                capture_scope,
            }) => {
                self.stream_writer
                    .start_provider_attempt(self.doc_id, turn, attempt, capture_scope)
                    .await;
                self.active_provider_attempt = Some((turn, attempt));
                self.active_capture_scope = Some(capture_scope);
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::ProviderTurnReady {
                turn,
                attempt,
                message,
            }) => {
                let published = self
                    .stream_writer
                    .publish_native_turn(self.lifecycle, turn, attempt, &message)
                    .await
                    .map_err(StreamProcessorError::writer)?;
                self.stream_writer
                    .flush_pending(self.doc_id)
                    .await
                    .map_err(StreamProcessorError::writer)?;
                if published.accepted_tools.len() != self.pending_tool_internal_ids.len() {
                    return Err(StreamProcessorError::AcceptedToolBindingsMismatch);
                }
                let accepted = self
                    .pending_tool_internal_ids
                    .drain(..)
                    .zip(published.accepted_tools)
                    .collect();
                self.persistence_hook
                    .adopt_accepted_tool_calls(accepted)
                    .await
                    .map_err(StreamProcessorError::hook)?;
                self.final_message_doc_id = Some(published.message_doc_id);
                self.active_provider_attempt = None;
                self.active_capture_scope = None;
                self.assistant_turn = AssistantTurnAccumulator::default();
                self.committed_text_len = self.streamed_text.len();
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::Text(text)) => {
                let had_visible_text = !self.streamed_text.trim().is_empty();
                self.assistant_turn.push_text(&text);
                self.streamed_text.push_str(&text);
                let flush_due = self
                    .stream_writer
                    .write_tokens(self.doc_id, &text)
                    .await
                    .map_err(StreamProcessorError::writer)?;
                let has_visible_text = !self.streamed_text.trim().is_empty();
                if flush_due || (!had_visible_text && has_visible_text) {
                    self.flush_pending().await?;
                }
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::Reasoning(reasoning)) => {
                let rendered = render_reasoning_text(&reasoning);
                self.assistant_turn
                    .push_provider_reasoning(self.provider_profile, reasoning)?;
                if self.provider_profile == ProviderInputProfile::ClaudeMessages {
                    // The final block must agree with the received prefix;
                    // flushing retains fields absent from Rig deltas.
                    self.flush_pending().await?;
                } else if !rendered.is_empty() {
                    let flush_due = self
                        .stream_writer
                        .write_reasoning(self.doc_id, &rendered)
                        .await
                        .map_err(StreamProcessorError::writer)?;
                    if flush_due {
                        self.flush_pending().await?;
                    }
                }
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::ReasoningDelta { reasoning, id }) => {
                if self.provider_profile == ProviderInputProfile::ClaudeMessages
                    && self.assistant_turn.provider_block.is_some()
                {
                    return Ok(StreamAction::Continue);
                }
                self.assistant_turn.push_reasoning_delta(id, &reasoning);
                if !reasoning.is_empty() {
                    let flush_due = self
                        .stream_writer
                        .write_reasoning(self.doc_id, &reasoning)
                        .await
                        .map_err(StreamProcessorError::writer)?;
                    if flush_due {
                        self.flush_pending().await?;
                    }
                }
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::ToolCall {
                tool_call,
                internal_call_id,
            }) => {
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
                self.assistant_turn.push_tool_call(tool_call);
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::ToolResult {
                tool_result,
                internal_call_id,
            }) => {
                self.flush_pending().await?;
                self.assistant_turn = AssistantTurnAccumulator::default();
                self.committed_text_len = self.streamed_text.len();
                let _ = (tool_result, internal_call_id);
                self.stream_writer
                    .reset_tail(self.doc_id)
                    .await
                    .map_err(StreamProcessorError::writer)?;
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::FinishOrIntake { text, reply }) => {
                let result = self.finish_or_intake(&text).await;
                let finished = matches!(result, Ok(crate::loop_stream::FinishOrIntake::Finished));
                let _ = reply.send(result);
                if finished {
                    self.assistant_turn.reconcile_text(&text);
                    self.final_text = Some(text);
                    Ok(StreamAction::Done)
                } else {
                    Ok(StreamAction::Continue)
                }
            }
            Ok(LoopStreamItem::Final { text }) => {
                self.assistant_turn.reconcile_text(&text);
                self.final_text = Some(text);
                Ok(StreamAction::Done)
            }
            Ok(LoopStreamItem::TurnRetracted { turn, attempt, .. }) => {
                self.flush_pending().await?;
                self.stream_writer
                    .close_provider_attempt(
                        self.lifecycle,
                        turn,
                        attempt,
                        crate::stream_writer::ProviderAttemptClose::Retracted,
                    )
                    .await
                    .map_err(StreamProcessorError::writer)?;
                self.active_provider_attempt = None;
                self.active_capture_scope = None;
                self.assistant_turn = AssistantTurnAccumulator::default();
                self.pending_tool_internal_ids.clear();
                self.streamed_text.truncate(self.committed_text_len);
                self.stream_writer
                    .reset_tail(self.doc_id)
                    .await
                    .map_err(StreamProcessorError::writer)?;
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::AuthoredInputReady {
                context,
                prompt,
                folded,
                reply,
            }) => {
                if let Some(context) = context.as_ref() {
                    self.stream_writer
                        .publish_authored_message(self.lifecycle, "context", context)
                        .await
                        .map_err(StreamProcessorError::writer)?;
                }
                self.stream_writer
                    .publish_authored_message(self.lifecycle, "prompt", &prompt)
                    .await
                    .map_err(StreamProcessorError::writer)?;
                let candidates = folded
                    .into_iter()
                    .map(|(key, message)| crate::loop_stream::FoldedPrompt { key, message })
                    .collect();
                let result = self.publish_steering(candidates).await;
                let _ = reply.send(result);
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::SteeringBoundary { reply }) => {
                let result = match self.persistence_hook.pending_steering_inputs().await {
                    Ok(candidates) => self.publish_steering(candidates).await,
                    Err(error) => Err(StreamProcessorError::hook(error)),
                };
                let _ = reply.send(result);
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::OutputObligationPending { reminder }) => {
                self.flush_pending().await?;
                let key = format!("output-obligation:{}", self.authored_index);
                self.stream_writer
                    .publish_authored_message(self.lifecycle, &key, &reminder)
                    .await
                    .map_err(StreamProcessorError::writer)?;
                self.authored_index = self
                    .authored_index
                    .checked_add(1)
                    .ok_or(StreamProcessorError::AuthoredIndexExhausted)?;
                self.streamed_text.truncate(self.committed_text_len);
                self.stream_writer
                    .reset_tail(self.doc_id)
                    .await
                    .map_err(StreamProcessorError::writer)?;
                Ok(StreamAction::Continue)
            }
            Ok(LoopStreamItem::AttemptFailed {
                turn,
                attempt,
                will_retry,
                ..
            }) => {
                self.flush_pending().await?;
                if let Some(active) = self.active_provider_attempt {
                    if active != (turn, attempt) {
                        return Err(StreamProcessorError::AttemptMismatch);
                    }
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
                        .await
                        .map_err(StreamProcessorError::writer)?;
                    self.active_provider_attempt = None;
                    self.active_capture_scope = None;
                }
                Ok(StreamAction::Continue)
            }
            Err(error) => Ok(StreamAction::Error(error)),
        }
    }

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
        let had_output = self.assistant_turn.take_message().is_some();
        if let Some((turn, attempt)) = self.active_provider_attempt.take() {
            self.stream_writer
                .close_provider_attempt(
                    self.lifecycle,
                    turn,
                    attempt,
                    crate::stream_writer::ProviderAttemptClose::Partial,
                )
                .await
                .map_err(|error| StreamProcessorError::PartialAttemptClose {
                    context: context.to_owned(),
                    source: error.into(),
                })?;
            self.active_capture_scope = None;
        }
        self.stream_writer
            .reset_tail(self.doc_id)
            .await
            .map_err(|error| StreamProcessorError::PartialTailReset {
                context: context.to_owned(),
                source: error.into(),
            })?;

        Ok(had_output)
    }

    /// Persist observations already parsed by the provider before an external
    /// cancellation drops the stream that normally delivers them.
    pub async fn persist_received_partial_turn(&mut self, context: &str) -> Result<bool> {
        self.validate_execution()
            .await
            .map_err(StreamProcessorError::lifecycle)?;
        let handled_auxiliary =
            crate::rendered_request::scope::flush_received_auxiliary_partial().await?;
        if !handled_auxiliary {
            for observation in crate::rendered_request::scope::drain_ready_audit().await {
                match self
                    .process_item(Ok(LoopStreamItem::ProviderAudit(observation)))
                    .await?
                {
                    StreamAction::Continue => {}
                    _ => return Err(StreamProcessorError::QueuedAuditAction),
                }
            }
        }
        self.persist_partial_turn(context).await
    }
}

#[derive(Clone, Default)]
pub struct AssistantTurnAccumulator {
    content: Vec<AssistantMessageContent>,
    next_provider_block: u32,
    provider_block: Option<crate::provider_audit::ClaudeBlockKind>,
}

impl AssistantTurnAccumulator {
    pub fn apply_provider_audit(
        &mut self,
        event: &crate::provider_audit::ClaudeAuditEvent,
    ) -> Result<bool, StreamProcessorError> {
        use crate::provider_audit::{ClaudeAuditEvent, ClaudeBlockKind};
        use gents_protocol::message::ReasoningContent;
        if let ClaudeAuditEvent::BlockStart { index, kind } = event {
            self.begin_provider_block(*index, kind)?;
            return Ok(!matches!(kind, ClaudeBlockKind::ToolUse { .. }));
        }
        let index = match event {
            ClaudeAuditEvent::ThinkingText { index, .. }
            | ClaudeAuditEvent::Signature { index, .. }
            | ClaudeAuditEvent::RedactedData { index, .. }
            | ClaudeAuditEvent::BlockStop { index } => *index,
            ClaudeAuditEvent::BlockStart { .. } => unreachable!(),
        };
        if !(self.next_provider_block.checked_sub(1) == Some(index)) {
            return Err(StreamProcessorError::AuditBlockMismatch);
        }
        if matches!(event, ClaudeAuditEvent::BlockStop { .. }) {
            return Ok(false);
        }
        let Some(AssistantMessageContent::Reasoning(reasoning)) = self.content.last_mut() else {
            return Err(StreamProcessorError::AuditWithoutReasoning);
        };
        match (event, reasoning.content.as_mut_slice()) {
            (
                ClaudeAuditEvent::ThinkingText { fragment, .. },
                [ReasoningContent::Text {
                    text,
                    signature: None,
                }],
            ) => text.push_str(fragment),
            (
                ClaudeAuditEvent::Signature { fragment, .. },
                [ReasoningContent::Text { signature, .. }],
            ) => {
                signature.get_or_insert_with(String::new).push_str(fragment);
            }
            (
                ClaudeAuditEvent::RedactedData { data, .. },
                [ReasoningContent::Redacted { data: observed }],
            ) => {
                if !(observed.is_empty()) {
                    return Err(StreamProcessorError::RedactedPayloadRepeated);
                }
                *observed = data.clone();
            }
            _ => return Err(StreamProcessorError::AuditFieldKindChanged),
        }
        Ok(true)
    }

    pub fn begin_provider_block(
        &mut self,
        index: u32,
        kind: &crate::provider_audit::ClaudeBlockKind,
    ) -> Result<(), StreamProcessorError> {
        use crate::provider_audit::ClaudeBlockKind;
        use gents_protocol::message::ReasoningContent;
        if !(index == self.next_provider_block) {
            return Err(StreamProcessorError::ProviderBlockOrderChanged);
        }
        self.next_provider_block = index
            .checked_add(1)
            .ok_or(StreamProcessorError::ProviderBlockIndexOverflow)?;
        self.provider_block = Some(kind.clone());
        match kind {
            ClaudeBlockKind::Text => {
                self.content
                    .push(AssistantMessageContent::Text(CompletionText {
                        text: String::new(),
                    }))
            }
            ClaudeBlockKind::Thinking | ClaudeBlockKind::RedactedThinking => {
                let part = match kind {
                    ClaudeBlockKind::Thinking => ReasoningContent::Text {
                        text: String::new(),
                        signature: None,
                    },
                    _ => ReasoningContent::Redacted {
                        data: String::new(),
                    },
                };
                self.content
                    .push(AssistantMessageContent::Reasoning(AssistantReasoning {
                        id: None,
                        content: vec![part],
                    }));
            }
            ClaudeBlockKind::ToolUse { .. } => {}
        }
        Ok(())
    }

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

    pub fn push_reasoning_delta(&mut self, id: Option<String>, fragment: &str) {
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
        self.push_reasoning(AssistantReasoning {
            id,
            content: vec![gents_protocol::message::ReasoningContent::Text {
                text: fragment.to_owned(),
                signature: None,
            }],
        });
    }

    pub fn push_provider_reasoning(
        &mut self,
        profile: ProviderInputProfile,
        reasoning: AssistantReasoning,
    ) -> Result<(), StreamProcessorError> {
        if profile != ProviderInputProfile::ClaudeMessages {
            self.push_reasoning(reasoning);
            return Ok(());
        }
        use gents_protocol::message::ReasoningContent;
        if !(!reasoning.content.is_empty()) {
            return Err(StreamProcessorError::EmptyClaudeReasoning);
        }
        for part in &reasoning.content {
            match part {
                ReasoningContent::Text {
                    signature: Some(signature),
                    ..
                } if !signature.is_empty() => {}
                ReasoningContent::Redacted { data } if !data.is_empty() => {}
                _ => return Err(StreamProcessorError::ClaudeReasoningUnsupported),
            }
        }

        let first = reasoning.content.first().expect("validated nonempty");
        if matches!(
            self.provider_block,
            Some(crate::provider_audit::ClaudeBlockKind::Thinking)
        ) {
            let Some(AssistantMessageContent::Reasoning(current)) = self.content.last_mut() else {
                return Err(StreamProcessorError::ClaudeThinkingNoPosition);
            };
            let [ReasoningContent::Text {
                text: observed,
                signature: observed_signature,
            }] = current.content.as_slice()
            else {
                return Err(StreamProcessorError::ClaudeThinkingKindChanged);
            };
            let [ReasoningContent::Text {
                text: completed,
                signature: completed_signature,
            }] = reasoning.content.as_slice()
            else {
                return Err(StreamProcessorError::ClaudeThinkingCompletionKindChanged);
            };
            if !(observed == completed) {
                return Err(StreamProcessorError::ClaudeThinkingBytesChanged);
            }
            if !(observed_signature.is_none() || observed_signature == completed_signature) {
                return Err(StreamProcessorError::ClaudeThinkingSignatureChanged);
            }
            *current = reasoning;
            return Ok(());
        }
        if matches!(
            self.provider_block,
            Some(crate::provider_audit::ClaudeBlockKind::RedactedThinking)
        ) {
            let Some(AssistantMessageContent::Reasoning(current)) = self.content.last_mut() else {
                return Err(StreamProcessorError::ClaudeRedactedNoPosition);
            };
            let [ReasoningContent::Redacted { data: observed }] = current.content.as_slice() else {
                return Err(StreamProcessorError::ClaudeRedactedKindChanged);
            };
            let [ReasoningContent::Redacted { data: completed }] = reasoning.content.as_slice()
            else {
                return Err(StreamProcessorError::ClaudeRedactedCompletionKindChanged);
            };
            if !(observed.is_empty() || observed == completed) {
                return Err(StreamProcessorError::ClaudeRedactedBytesChanged);
            }
            *current = reasoning;
            return Ok(());
        }
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
                        return Err(StreamProcessorError::ClaudeRedactedAfterPreview);
                    };
                    if !(preview == final_text) {
                        return Err(StreamProcessorError::ClaudeSignedPreviewChanged);
                    }
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
            ReasoningContent::Encrypted(_) | ReasoningContent::Redacted { .. } => continue,
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

#[cfg(test)]
use gents_protocol as test_protocol;
#[cfg(test)]
#[path = "../../gents-lean-contract/tests/support/rendered_reasoning.rs"]
mod rendered_reasoning_contract;

#[cfg(test)]
mod tests {
    use super::{render_reasoning_text, AssistantMessageContent, CompletionMessage};

    #[test]
    fn generated_reasoning_visibility_matches_live_preview() {
        for case in super::rendered_reasoning_contract::generated_reasoning_cases() {
            let CompletionMessage::Assistant { content, .. } = &case.message else {
                panic!("{}: expected generated assistant message", case.name);
            };
            let [AssistantMessageContent::Reasoning(reasoning)] = content.as_slice() else {
                panic!("{}: expected generated reasoning block", case.name);
            };
            assert_eq!(
                render_reasoning_text(reasoning),
                case.expected_text,
                "{}",
                case.name
            );
        }
    }
}

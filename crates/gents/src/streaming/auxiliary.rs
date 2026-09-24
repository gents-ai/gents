use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use defra_node::EmbeddedNode;
use gents_loop::provider_audit::{
    AuxiliaryObserve, AuxiliaryOutputEvent, AuxiliaryOutputObservation, AuxiliaryOutputSink,
};
use gents_loop::stream_processor::AssistantTurnAccumulator;
use gents_protocol::output::OutputSource;
use gents_protocol::rendered_request::CaptureScope;
use tokio::sync::Mutex;

use super::{canonical::ProviderAttemptClose, DefraStreamWriter, StreamWriter};

impl DefraStreamWriter {
    pub(crate) fn auxiliary_output_sink(
        &self,
        request: crate::watcher::AgentRequest,
        generation: String,
        profile: gents_loop::provider_input::ProviderInputProfile,
    ) -> AuxiliaryOutputSink {
        sink(
            Arc::clone(&self.node),
            request,
            generation,
            profile,
            self.batch_interval,
        )
    }
}

#[derive(Default)]
struct State {
    active: Option<(CaptureScope, usize, u32)>,
    closed: Option<((CaptureScope, usize, u32), ProviderAttemptClose)>,
    content: AssistantTurnAccumulator,
    audit_seen: bool,
}

/// Auxiliary output uses the parent's live generation but never publishes a
/// transcript header. A separate writer tail prevents nested compaction from
/// replacing the main inference attempt's uncommitted prefix.
pub(crate) fn sink(
    node: Arc<EmbeddedNode>,
    request: crate::watcher::AgentRequest,
    generation: String,
    profile: gents_loop::provider_input::ProviderInputProfile,
    batch_interval: Duration,
) -> AuxiliaryOutputSink {
    let writer = Arc::new(DefraStreamWriter::new(
        node,
        &request.agent_did,
        batch_interval,
    ));
    let request = Arc::new(request);
    let state = Arc::new(Mutex::new(State::default()));
    let deadline_writer = Arc::clone(&writer);
    let deadline_request = Arc::clone(&request);
    let deadline_state = Arc::clone(&state);
    let flush_writer = Arc::clone(&writer);
    let flush_request = Arc::clone(&request);
    let flush_state = Arc::clone(&state);
    let flush_generation = generation.clone();
    let observe: AuxiliaryObserve = Arc::new(move |observation: AuxiliaryOutputObservation| {
        let writer = Arc::clone(&writer);
        let request = Arc::clone(&request);
        let state = Arc::clone(&state);
        let generation = generation.clone();
        Box::pin(async move {
            let coordinate = (
                observation.capture_scope,
                observation.turn,
                observation.attempt,
            );
            anyhow::ensure!(
                OutputSource::ProviderTurn {
                    scope: observation.capture_scope,
                    turn_index: u32::try_from(observation.turn)?,
                    attempt: observation.attempt,
                }
                .is_auxiliary_audit(),
                "auxiliary output sink received a non-compaction source"
            );
            let mut state = state.lock().await;
            if matches!(observation.event, AuxiliaryOutputEvent::AttemptStarted) {
                anyhow::ensure!(
                    state.active.is_none(),
                    "auxiliary attempt overlaps an open source"
                );
                writer.initialize_request_buffer(&request.doc_id).await?;
                writer
                    .start_provider_attempt(
                        &request.doc_id,
                        observation.turn,
                        observation.attempt,
                        observation.capture_scope,
                    )
                    .await;
                state.active = Some(coordinate);
                state.closed = None;
                state.content = AssistantTurnAccumulator::default();
                state.audit_seen = false;
                return Ok(());
            }
            if state.active.is_none()
                && state.closed.is_some_and(|(closed, _)| closed == coordinate)
            {
                match observation.event {
                    AuxiliaryOutputEvent::OutputObligationPending { .. }
                    | AuxiliaryOutputEvent::FinalText(_)
                        if state.closed
                            == Some((coordinate, ProviderAttemptClose::AuxiliaryComplete)) =>
                    {
                        return Ok(())
                    }
                    AuxiliaryOutputEvent::ClosePartial => return Ok(()),
                    _ => {}
                }
            }
            anyhow::ensure!(
                state.active == Some(coordinate),
                "auxiliary output belongs to another attempt"
            );
            let mut close = None;
            match observation.event {
                AuxiliaryOutputEvent::AttemptStarted => unreachable!(),
                AuxiliaryOutputEvent::Audit(event) => {
                    anyhow::ensure!(
                        profile == gents_loop::provider_input::ProviderInputProfile::ClaudeMessages,
                        "Claude audit reached another auxiliary provider"
                    );
                    state.content.apply_provider_audit(&event)?;
                    state.audit_seen = true;
                }
                AuxiliaryOutputEvent::TextDelta(text) => state.content.push_text(&text),
                AuxiliaryOutputEvent::ReasoningDelta { id, fragment } => {
                    if !state.audit_seen {
                        state
                            .content
                            .push_provider_reasoning_delta(profile, id, &fragment);
                    }
                }
                AuxiliaryOutputEvent::Reasoning(reasoning) => {
                    state.content.push_provider_reasoning(profile, reasoning)?
                }
                AuxiliaryOutputEvent::ToolCall(call) => state.content.push_tool_call(call),
                AuxiliaryOutputEvent::FinalText(text) => state.content.reconcile_text(&text),
                AuxiliaryOutputEvent::TurnReady { message } => {
                    writer
                        .flush_owned_partial(&request, &generation, &message)
                        .await?;
                    close = Some(ProviderAttemptClose::AuxiliaryComplete);
                }
                AuxiliaryOutputEvent::Retract => close = Some(ProviderAttemptClose::Retracted),
                AuxiliaryOutputEvent::AttemptFailed { will_retry } => {
                    close = Some(if will_retry {
                        ProviderAttemptClose::Retracted
                    } else {
                        ProviderAttemptClose::Partial
                    });
                }
                AuxiliaryOutputEvent::OutputObligationPending { .. } => {
                    anyhow::bail!("auxiliary continuation preceded accepted turn closure");
                }
                AuxiliaryOutputEvent::ClosePartial => close = Some(ProviderAttemptClose::Partial),
            }
            if !matches!(close, Some(ProviderAttemptClose::AuxiliaryComplete)) {
                if close.is_some() || writer.mark_pending_output(&request.doc_id).await? {
                    if let Some(message) = state.content.message_snapshot() {
                        writer
                            .flush_owned_partial(&request, &generation, &message)
                            .await?;
                    }
                    writer.flush_pending(&request.doc_id).await?;
                }
            }
            if let Some(close) = close {
                writer
                    .close_owned_attempt(
                        &request,
                        &generation,
                        observation.turn,
                        observation.attempt,
                        close,
                    )
                    .await
                    .context("closing auxiliary audit output")?;
                writer.reset_tail(&request.doc_id).await?;
                state.active = None;
                state.closed = Some((coordinate, close));
                state.content = AssistantTurnAccumulator::default();
                state.audit_seen = false;
            }
            Ok(())
        })
    });
    AuxiliaryOutputSink {
        observe,
        next_flush_deadline: Arc::new(move |scope, turn, attempt| {
            let writer = Arc::clone(&deadline_writer);
            let request = Arc::clone(&deadline_request);
            let state = Arc::clone(&deadline_state);
            Box::pin(async move {
                let state = state.lock().await;
                anyhow::ensure!(
                    state.active == Some((scope, turn, attempt)),
                    "auxiliary deadline belongs to another attempt"
                );
                Ok(writer.next_flush_deadline(&request.doc_id).await)
            })
        }),
        flush_pending: Arc::new(move |scope, turn, attempt| {
            let writer = Arc::clone(&flush_writer);
            let request = Arc::clone(&flush_request);
            let state = Arc::clone(&flush_state);
            let generation = flush_generation.clone();
            Box::pin(async move {
                let state = state.lock().await;
                anyhow::ensure!(
                    state.active == Some((scope, turn, attempt)),
                    "auxiliary flush belongs to another attempt"
                );
                if let Some(message) = state.content.message_snapshot() {
                    writer
                        .flush_owned_partial(&request, &generation, &message)
                        .await?;
                }
                writer.flush_pending(&request.doc_id).await?;
                Ok(())
            })
        }),
    }
}

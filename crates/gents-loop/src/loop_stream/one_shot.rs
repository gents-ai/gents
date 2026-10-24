use super::*;
use crate::provider_audit::{
    AuxiliaryOutputEvent, AuxiliaryOutputObservation, AuxiliaryOutputSink,
};
use gents_protocol::error_chain::ErrorChain;
use gents_protocol::rendered_request::CaptureScope;

#[derive(Debug, thiserror::Error)]
#[error("auxiliary output persistence failed during {operation}: {source}")]
pub struct AuxiliaryPersistenceFailure {
    operation: &'static str,
    #[source]
    source: anyhow::Error,
}

#[derive(Debug, thiserror::Error)]
#[error("one-shot provider failed: {last_error}")]
pub struct OneShotProviderFailure {
    last_error: InferenceError,
    #[source]
    source: Option<LoopStreamError>,
}

#[derive(Debug, thiserror::Error)]
#[error("one-shot completion produced no visible output")]
pub struct OneShotNoVisibleOutput {
    #[source]
    source: LoopStreamError,
}

#[derive(Debug, thiserror::Error)]
pub enum OneShotError {
    #[error(transparent)]
    Persistence(#[from] AuxiliaryPersistenceFailure),
    #[error(transparent)]
    ProviderFailure(#[from] OneShotProviderFailure),
    #[error(transparent)]
    NoVisibleOutput(#[from] OneShotNoVisibleOutput),
    #[error("one-shot loop stream error")]
    LoopStream(#[source] LoopStreamError),
    #[error("provider stream ended without an explicit terminal response")]
    MissingTerminalResponse,
    #[error(transparent)]
    Accumulator(#[from] crate::stream_processor::StreamProcessorError),
    #[error(transparent)]
    TurnIndex(#[from] std::num::TryFromIntError),
    #[error("decoding validated structured output as {type_name} failed: {error}")]
    StructuredDecode {
        type_name: &'static str,
        error: serde_json::Error,
    },
    #[error("received provider audit reached a nonpersistent auxiliary; a canonical output authority is required")]
    AuditWithoutAuthority,
    #[error("AuthoredInputReady reached the nonpersistent one-shot auxiliary; authored-input persistence belongs to the owned StreamProcessor consumer")]
    AuthoredInputReachedAuxiliary,
    #[error("auxiliary audit changed source")]
    AuditChangedSource,
    #[error("auxiliary output has no canonical sink")]
    NoCanonicalSink,
    #[error("auxiliary attempt overlaps another source")]
    AttemptOverlapsSource,
    #[error("auxiliary ready changed source")]
    ReadyChangedSource,
    #[error("auxiliary turn has no active source")]
    TurnWithoutSource,
    #[error("detached auxiliary request cannot call tools")]
    DetachedToolCall,
    #[error("auxiliary retract has no active source")]
    RetractWithoutSource,
    #[error("auxiliary retract changed source")]
    RetractChangedSource,
    #[error("auxiliary reminder has no source")]
    ReminderWithoutSource,
    #[error("auxiliary failure changed source")]
    FailureChangedSource,
    #[error("auxiliary text has no active source")]
    TextWithoutSource,
    #[error("auxiliary reasoning has no active source")]
    ReasoningWithoutSource,
    #[error("auxiliary reasoning delta has no active source")]
    ReasoningDeltaWithoutSource,
    #[error("auxiliary tool call has no active source")]
    ToolCallWithoutSource,
    #[error("auxiliary final response has no source")]
    FinalWithoutSource,
    #[error("receiving the auxiliary item after a flush failed: {flush}")]
    ReceivingAfterFlushFailed {
        flush: String,
        #[source]
        source: Box<OneShotError>,
    },
    #[error("retaining the received auxiliary item after a flush failed: {flush}")]
    RetainingAfterFlushFailed {
        flush: String,
        #[source]
        source: Box<OneShotError>,
    },
}

fn persistence_failure(operation: &'static str, source: anyhow::Error) -> OneShotError {
    OneShotError::Persistence(AuxiliaryPersistenceFailure { operation, source })
}

fn stream_failure(
    error: LoopStreamError,
    last_attempt_error: Option<InferenceError>,
) -> OneShotError {
    match last_attempt_error {
        Some(last_error) => OneShotProviderFailure {
            last_error,
            source: Some(error),
        }
        .into(),
        None if super::is_no_visible_output_failure(&error) => {
            OneShotNoVisibleOutput { source: error }.into()
        }
        None => OneShotError::LoopStream(error),
    }
}

fn missing_final_failure(last_attempt_error: Option<InferenceError>) -> OneShotError {
    match last_attempt_error {
        Some(last_error) => OneShotProviderFailure {
            last_error,
            source: None,
        }
        .into(),
        None => OneShotError::MissingTerminalResponse,
    }
}

async fn emit_auxiliary(
    identity: (CaptureScope, usize, u32),
    event: AuxiliaryOutputEvent,
) -> Result<(), OneShotError> {
    let observation = AuxiliaryOutputObservation {
        capture_scope: identity.0,
        turn: identity.1,
        attempt: identity.2,
        event,
    };
    match crate::rendered_request::scope::emit_auxiliary_output(observation).await {
        Ok(()) => Ok(()),
        Err(error) => {
            let error = persistence_failure("observation", error);
            match crate::rendered_request::scope::flush_received_auxiliary_partial().await {
                Ok(_) => Err(error),
                Err(close_error) => Err(persistence_failure(
                    "observation cleanup",
                    close_error
                        .context(format!("original auxiliary error: {}", ErrorChain(&error))),
                )),
            }
        }
    }
}

async fn close_received_auxiliary() -> Result<(), OneShotError> {
    match crate::rendered_request::scope::flush_received_auxiliary_partial().await {
        Ok(true) => Ok(()),
        Ok(false) => Err(persistence_failure(
            "close",
            anyhow::anyhow!("active auxiliary output could not be closed"),
        )),
        Err(error) => Err(persistence_failure("close", error)),
    }
}

async fn close_received_auxiliary_after_error(error: OneShotError) -> OneShotError {
    match close_received_auxiliary().await {
        Ok(()) => error,
        Err(close_error) => persistence_failure(
            "error cleanup",
            anyhow::Error::new(close_error)
                .context(format!("original one-shot error: {}", ErrorChain(&error))),
        ),
    }
}

async fn finish_one_shot_result<T>(result: Result<T, OneShotError>) -> Result<T, OneShotError> {
    match result {
        Ok(value) => Ok(value),
        Err(error) if matches!(error, OneShotError::Persistence(_)) => Err(error),
        Err(error) => {
            match crate::rendered_request::scope::flush_received_auxiliary_partial().await {
                Ok(_) => Err(error),
                Err(close_error) => Err(persistence_failure(
                    "error cleanup",
                    close_error.context(format!("original one-shot error: {}", ErrorChain(&error))),
                )),
            }
        }
    }
}

fn ensure_auxiliary_identity(
    active: Option<(CaptureScope, usize, u32)>,
    observed: (CaptureScope, usize, u32),
) -> Result<(), OneShotError> {
    if active != Some(observed) {
        return Err(OneShotError::AuditChangedSource);
    }
    Ok(())
}

async fn flush_auxiliary_at_deadline<F, T>(
    sink: &AuxiliaryOutputSink,
    identity: (CaptureScope, usize, u32),
    progress: F,
) -> (Result<(), OneShotError>, Option<Option<T>>)
where
    F: std::future::Future<Output = Option<T>>,
{
    let (flushed, item) = crate::stream_processor::flush_while_polling(
        (sink.flush_pending)(identity.0, identity.1, identity.2),
        progress,
    )
    .await;
    let flushed = flushed.map_err(|error| persistence_failure("deadline flush", error));
    (flushed, item)
}

pub async fn run_loop_to_text<M>(
    model: M,
    prompt: Message,
    history: Vec<Message>,
    tools: Arc<Vec<Box<dyn ToolDyn>>>,
    config: LoopConfig,
) -> Result<String, OneShotError>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: 'static,
{
    finish_one_shot_result(run_loop_to_text_inner(model, prompt, history, tools, config).await)
        .await
}

async fn run_loop_to_text_inner<M>(
    model: M,
    prompt: Message,
    history: Vec<Message>,
    tools: Arc<Vec<Box<dyn ToolDyn>>>,
    config: LoopConfig,
) -> Result<String, OneShotError>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: 'static,
{
    let provider_profile = config.provider_input_counter.profile();
    let mut stream = Box::pin(run_loop_stream::<M, crate::session_hook::NoopSessionHook>(
        model,
        None,
        TaggedMessage::unassociated(prompt),
        history
            .into_iter()
            .map(TaggedMessage::unassociated)
            .collect(),
        tools,
        config,
    ));
    let mut accumulator = AssistantTurnAccumulator::default();
    let auxiliary_sink = crate::rendered_request::scope::auxiliary_output_sink();
    let mut auxiliary_enabled = false;
    let mut active_auxiliary: Option<(CaptureScope, usize, u32)> = None;
    let mut last_auxiliary: Option<(CaptureScope, usize, u32)> = None;
    let mut final_text = None;
    let mut last_attempt_error: Option<InferenceError> = None;

    loop {
        let mut flush_failure = None;
        let deadline = match (active_auxiliary, auxiliary_sink.as_ref()) {
            (Some((scope, turn, attempt)), Some(sink)) => {
                match (sink.next_flush_deadline)(scope, turn, attempt).await {
                    Ok(deadline) => deadline,
                    Err(error) => {
                        drop(stream);
                        return Err(close_received_auxiliary_after_error(persistence_failure(
                            "next flush deadline",
                            error,
                        ))
                        .await);
                    }
                }
            }
            _ => None,
        };
        let item = match deadline {
            Some(deadline) => {
                let sink = auxiliary_sink
                    .as_ref()
                    .expect("deadline needs auxiliary sink");
                let (scope, turn, attempt) =
                    active_auxiliary.expect("deadline needs active source");
                tokio::select! {
                    biased;
                    _ = tokio::time::sleep_until(deadline) => {
                        let (flushed, item) = flush_auxiliary_at_deadline(
                            sink, (scope, turn, attempt),
                            stream.next(),
                        ).await;
                        if let Err(error) = flushed {
                            if matches!(&item, Some(Some(_))) {
                                flush_failure = Some(error);
                            } else {
                                drop(stream);
                                return Err(close_received_auxiliary_after_error(error).await);
                            }
                        }
                        match item {
                            Some(item) => item,
                            None => continue,
                        }
                    }
                    item = stream.next() => item,
                }
            }
            None => stream.next().await,
        };
        let Some(item) = item else {
            break;
        };
        let item = match item {
            Ok(item) => item,
            Err(error) => {
                drop(stream);
                let error = stream_failure(error, last_attempt_error.take());
                let error = match flush_failure {
                    Some(flush_error) => OneShotError::ReceivingAfterFlushFailed {
                        flush: ErrorChain(&flush_error).to_string(),
                        source: Box::new(error),
                    },
                    None => error,
                };
                return Err(if active_auxiliary.is_some() {
                    close_received_auxiliary_after_error(error).await
                } else {
                    error
                });
            }
        };
        let processed = async {
            match item {
                LoopStreamItem::ProviderAudit(observation) => {
                    if !auxiliary_enabled {
                        return Err(OneShotError::AuditWithoutAuthority);
                    }
                    let identity = (
                        observation.capture_scope,
                        observation.turn,
                        observation.attempt,
                    );
                    ensure_auxiliary_identity(active_auxiliary, identity)?;
                    emit_auxiliary(identity, AuxiliaryOutputEvent::Audit(observation.event))
                        .await?;
                }
                LoopStreamItem::SteeringBoundary { .. }
                | LoopStreamItem::AuthoredInputReady { .. }
                | LoopStreamItem::FinishOrIntake { .. } => {
                    return Err(OneShotError::AuthoredInputReachedAuxiliary);
                }
                LoopStreamItem::ProviderAttemptStarted {
                    turn,
                    attempt,
                    capture_scope,
                } => {
                    last_attempt_error = None;
                    auxiliary_enabled = OutputSource::ProviderTurn {
                        scope: capture_scope,
                        turn_index: u32::try_from(turn)?,
                        attempt,
                    }
                    .is_auxiliary_audit();
                    if auxiliary_enabled {
                        if auxiliary_sink.is_none() {
                            return Err(OneShotError::NoCanonicalSink);
                        }
                        let identity = (capture_scope, turn, attempt);
                        if active_auxiliary.is_some() {
                            return Err(OneShotError::AttemptOverlapsSource);
                        }
                        emit_auxiliary(identity, AuxiliaryOutputEvent::AttemptStarted).await?;
                        active_auxiliary = Some(identity);
                        last_auxiliary = Some(identity);
                    }
                }
                LoopStreamItem::ProviderTurnReady {
                    turn,
                    attempt,
                    message,
                } => {
                    if auxiliary_enabled {
                        let identity = active_auxiliary.ok_or(OneShotError::TurnWithoutSource)?;
                        if (identity.1, identity.2) != (turn, attempt) {
                            return Err(OneShotError::ReadyChangedSource);
                        }
                        let has_tools = matches!(&message, Message::Assistant { content, .. }
                            if content.iter().any(|block| matches!(block, AssistantContent::ToolCall(_))));
                        let accepted = crate::tool_policy::access_review::accepts_completion(
                            identity.0.kind.request_purpose(), has_tools,
                        );
                        emit_auxiliary(identity, AuxiliaryOutputEvent::TurnReady { message })
                            .await?;
                        active_auxiliary = None;
                        // The producer can dispatch tools after this yield.
                        // Retain its final usage and audit before rejecting intent.
                        if !accepted {
                            return Err(OneShotError::DetachedToolCall);
                        }
                    }
                }
                LoopStreamItem::TurnRetracted { turn, attempt, .. } => {
                    if auxiliary_enabled {
                        let identity = active_auxiliary.ok_or(OneShotError::RetractWithoutSource)?;
                        if (identity.1, identity.2) != (turn, attempt) {
                            return Err(OneShotError::RetractChangedSource);
                        }
                        emit_auxiliary(identity, AuxiliaryOutputEvent::Retract).await?;
                        active_auxiliary = None;
                    } else {
                        accumulator = AssistantTurnAccumulator::default();
                    }
                    final_text = None;
                }
                LoopStreamItem::OutputObligationPending { reminder } => {
                    if auxiliary_enabled {
                        let identity = last_auxiliary
                            .ok_or(OneShotError::ReminderWithoutSource)?;
                        emit_auxiliary(
                            identity,
                            AuxiliaryOutputEvent::OutputObligationPending { reminder },
                        )
                        .await?;
                    } else {
                        accumulator = AssistantTurnAccumulator::default();
                    }
                }
                LoopStreamItem::AttemptFailed {
                    turn,
                    attempt,
                    error,
                    will_retry,
                    ..
                } => {
                    if auxiliary_enabled {
                        if let Some(identity) = active_auxiliary {
                            if (identity.1, identity.2) != (turn, attempt) {
                            return Err(OneShotError::FailureChangedSource);
                        }
                            emit_auxiliary(
                                identity,
                                AuxiliaryOutputEvent::AttemptFailed { will_retry },
                            )
                            .await?;
                            active_auxiliary = None;
                        }
                    }
                    final_text = None;
                    last_attempt_error = Some(error);
                }
                LoopStreamItem::Text(text) => {
                    if auxiliary_enabled {
                        let identity = active_auxiliary.ok_or(OneShotError::TextWithoutSource)?;
                        emit_auxiliary(identity, AuxiliaryOutputEvent::TextDelta(text)).await?;
                    } else {
                        accumulator.push_text(&text);
                    }
                }
                LoopStreamItem::Reasoning(reasoning) => {
                    if auxiliary_enabled {
                        let identity = active_auxiliary.ok_or(OneShotError::ReasoningWithoutSource)?;
                        emit_auxiliary(identity, AuxiliaryOutputEvent::Reasoning(reasoning))
                            .await?;
                    } else {
                        accumulator.push_provider_reasoning(provider_profile, reasoning)?;
                    }
                }
                LoopStreamItem::ReasoningDelta { id, reasoning } => {
                    if auxiliary_enabled {
                        let identity = active_auxiliary.ok_or(OneShotError::ReasoningDeltaWithoutSource)?;
                        emit_auxiliary(
                            identity,
                            AuxiliaryOutputEvent::ReasoningDelta {
                                id,
                                fragment: reasoning,
                            },
                        )
                        .await?;
                    } else {
                        accumulator.push_reasoning_delta(id, &reasoning);
                    }
                }
                LoopStreamItem::ToolCall { tool_call, .. } => {
                    if auxiliary_enabled {
                        let identity = active_auxiliary.ok_or(OneShotError::ToolCallWithoutSource)?;
                        emit_auxiliary(identity, AuxiliaryOutputEvent::ToolCall(tool_call)).await?;
                    } else {
                        accumulator.push_tool_call(tool_call);
                    }
                }
                LoopStreamItem::ToolResult { .. } => {}
                LoopStreamItem::Final { text } => {
                    if auxiliary_enabled {
                        let identity = active_auxiliary.or(last_auxiliary).ok_or(OneShotError::FinalWithoutSource)?;
                        emit_auxiliary(identity, AuxiliaryOutputEvent::FinalText(text.clone()))
                            .await?;
                    } else {
                        accumulator.reconcile_text(&text);
                    }
                    final_text = Some(text);
                }
            }
            Ok::<_, OneShotError>(())
        }
        .await;
        if let Some(error) = flush_failure {
            drop(stream);
            return Err(match processed {
                Ok(()) if active_auxiliary.is_some() => {
                    close_received_auxiliary_after_error(error).await
                }
                Ok(()) => error,
                Err(receipt_error) => OneShotError::RetainingAfterFlushFailed {
                    flush: ErrorChain(&error).to_string(),
                    source: Box::new(receipt_error),
                },
            });
        }
        processed?;
    }
    if active_auxiliary.is_some() {
        close_received_auxiliary().await?;
    }
    final_text.ok_or_else(|| missing_final_failure(last_attempt_error))
}

pub async fn run_loop_to_typed<M, T>(
    model: M,
    prompt: Message,
    history: Vec<Message>,
    tools: Arc<Vec<Box<dyn ToolDyn>>>,
    mut config: LoopConfig,
) -> Result<T, OneShotError>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: 'static,
    T: DeserializeOwned + schemars::JsonSchema + 'static,
{
    config.structured_output = Some(StructuredOutputConfig::for_type::<T>());
    let raw = run_loop_to_text(model, prompt, history, tools, config).await?;
    serde_json::from_str(&raw).map_err(|error| OneShotError::StructuredDecode {
        type_name: std::any::type_name::<T>(),
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_audit::{ClaudeAuditEvent, ClaudeBlockKind};
    use crate::rendered_request::scope::{
        arm, claim_pending, current_audit_sender, flush_received_auxiliary_partial, scope_request,
        CaptureScopeKind, RequestCaptureScope,
    };
    use crate::rendered_request::{
        AssemblyBuildPath, AssemblyTrace, RenderedRequestCaptureSink, RenderedRequestContext,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[tokio::test]
    async fn failed_auxiliary_deadline_flush_drains_received_signature() {
        let captured: Arc<Mutex<Vec<AuxiliaryOutputEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&captured);
        let sink = AuxiliaryOutputSink {
            observe: Arc::new(move |observation| {
                observed
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(observation.event);
                Box::pin(async { Ok(()) })
            }),
            next_flush_deadline: Arc::new(|_, _, _| {
                Box::pin(async { Ok(Some(tokio::time::Instant::now())) })
            }),
            flush_pending: Arc::new(|_, _, _| {
                Box::pin(async { Err(anyhow::anyhow!("mock flush failure")) })
            }),
        };
        let capture_sink: RenderedRequestCaptureSink = Arc::new(|_| Box::pin(async { Ok(()) }));
        let mut scope = RequestCaptureScope::new(
            RenderedRequestContext {
                request_doc_id: "doc-aux".into(),
                request_commit_cid: "bafy-aux".into(),
                request_id: "req-aux".into(),
                node_did: "did:key:agent".into(),
                requester_did: String::new(),
                agent_id: "behavior".into(),
                session_id: "session".into(),
                model_name: "claude".into(),
                provider_family: None,
            },
            capture_sink,
        );
        scope.set_auxiliary_output_sink(sink.clone());
        scope_request(Arc::new(scope), async {
            let label = arm(
                CaptureScopeKind::Compaction,
                0,
                0,
                AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, Vec::new()),
            )
            .unwrap();
            claim_pending().expect("armed auxiliary attempt");
            let identity = (label.parse().expect("capture scope"), 0, 0);
            emit_auxiliary(identity, AuxiliaryOutputEvent::AttemptStarted)
                .await
                .unwrap();
            let sender = current_audit_sender().expect("claimed audit sender");
            let mut reservation = sender.reserve().await.expect("audit capacity");
            reservation
                .emit(ClaudeAuditEvent::BlockStart {
                    index: 0,
                    kind: ClaudeBlockKind::Thinking,
                })
                .unwrap();
            reservation
                .emit(ClaudeAuditEvent::Signature {
                    index: 0,
                    fragment: "received".into(),
                })
                .unwrap();
            drop(reservation);

            let (result, item) = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                flush_auxiliary_at_deadline(&sink, identity, std::future::pending::<Option<()>>()),
            )
            .await
            .expect("bounded flush failure cleanup");
            assert!(item.is_none());
            let error =
                close_received_auxiliary_after_error(result.expect_err("mock flush fails")).await;
            assert!(error.to_string().contains("mock flush failure"));
            assert!(matches!(error, OneShotError::Persistence(_)));
            {
                let events = captured
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                assert!(matches!(events[0], AuxiliaryOutputEvent::AttemptStarted));
                assert!(matches!(
                    events[1],
                    AuxiliaryOutputEvent::Audit(ClaudeAuditEvent::BlockStart { .. })
                ));
                assert!(matches!(
                    events[2],
                    AuxiliaryOutputEvent::Audit(ClaudeAuditEvent::Signature { .. })
                ));
                assert!(matches!(events[3], AuxiliaryOutputEvent::ClosePartial));
            }
            assert!(!flush_received_auxiliary_partial().await.unwrap());
        })
        .await;
    }

    #[tokio::test]
    async fn changed_audit_identity_closes_received_prefix_before_error() {
        let captured: Arc<Mutex<Vec<AuxiliaryOutputEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&captured);
        let sink = AuxiliaryOutputSink {
            observe: Arc::new(move |observation| {
                observed
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(observation.event);
                Box::pin(async { Ok(()) })
            }),
            next_flush_deadline: Arc::new(|_, _, _| Box::pin(async { Ok(None) })),
            flush_pending: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        };
        let capture_sink: RenderedRequestCaptureSink = Arc::new(|_| Box::pin(async { Ok(()) }));
        let mut scope = RequestCaptureScope::new(
            RenderedRequestContext {
                request_doc_id: "doc-identity".into(),
                request_commit_cid: "bafy-identity".into(),
                request_id: "req-identity".into(),
                node_did: "did:key:agent".into(),
                requester_did: String::new(),
                agent_id: "behavior".into(),
                session_id: "session".into(),
                model_name: "claude".into(),
                provider_family: None,
            },
            capture_sink,
        );
        scope.set_auxiliary_output_sink(sink);
        scope_request(Arc::new(scope), async {
            let label = arm(
                CaptureScopeKind::Compaction,
                0,
                0,
                AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, Vec::new()),
            )
            .unwrap();
            claim_pending().expect("armed auxiliary attempt");
            let identity = (label.parse().expect("capture scope"), 0, 0);
            emit_auxiliary(identity, AuxiliaryOutputEvent::AttemptStarted)
                .await
                .unwrap();
            let sender = current_audit_sender().expect("claimed audit sender");
            let mut reservation = sender.reserve().await.expect("audit capacity");
            reservation
                .emit(ClaudeAuditEvent::BlockStart {
                    index: 0,
                    kind: ClaudeBlockKind::Thinking,
                })
                .unwrap();
            reservation
                .emit(ClaudeAuditEvent::ThinkingText {
                    index: 0,
                    fragment: "reasoning".into(),
                })
                .unwrap();
            reservation
                .emit(ClaudeAuditEvent::Signature {
                    index: 0,
                    fragment: "received".into(),
                })
                .unwrap();
            drop(reservation);

            let error = ensure_auxiliary_identity(Some(identity), (identity.0, 0, 1))
                .expect_err("changed identity must fail closed");
            let error = finish_one_shot_result::<String>(Err(error))
                .await
                .expect_err("identity error must remain visible");
            assert!(error.to_string().contains("auxiliary audit changed source"));
            {
                let events = captured
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                assert!(matches!(events[0], AuxiliaryOutputEvent::AttemptStarted));
                assert!(matches!(
                    events[1],
                    AuxiliaryOutputEvent::Audit(ClaudeAuditEvent::BlockStart { .. })
                ));
                assert!(matches!(
                    &events[2],
                    AuxiliaryOutputEvent::Audit(ClaudeAuditEvent::ThinkingText { fragment, .. })
                        if fragment == "reasoning"
                ));
                assert!(matches!(
                    &events[3],
                    AuxiliaryOutputEvent::Audit(ClaudeAuditEvent::Signature { fragment, .. })
                        if fragment == "received"
                ));
                assert!(matches!(events[4], AuxiliaryOutputEvent::ClosePartial));
                assert_eq!(events.len(), 5);
            }
            assert!(!flush_received_auxiliary_partial().await.unwrap());
        })
        .await;
    }

    #[test]
    fn only_observed_attempt_failure_classifies_provider_error() {
        let stream_error =
            || StreamingError::Completion(CompletionError::ProviderError("stream failed".into()));
        let provider = stream_failure(
            crate::rig_compat::loop_stream_error(stream_error()),
            Some(InferenceError::TransientFailure {
                reason: "provider failed".into(),
            }),
        );
        assert!(matches!(provider, OneShotError::ProviderFailure(_)));
        assert!(!matches!(
            stream_failure(crate::rig_compat::loop_stream_error(stream_error()), None),
            OneShotError::ProviderFailure(_)
        ));
        let no_visible = StreamingError::Completion(CompletionError::ProviderError(format!(
            "{}transport retry budget exhausted after 0 attempt(s)",
            super::super::NO_VISIBLE_OUTPUT_PREFIX
        )));
        let no_visible = stream_failure(crate::rig_compat::loop_stream_error(no_visible), None);
        assert!(matches!(no_visible, OneShotError::NoVisibleOutput(_)));
        assert!(!matches!(no_visible, OneShotError::ProviderFailure(_)));
        assert!(matches!(
            missing_final_failure(Some(InferenceError::PermanentFailure {
                reason: "provider failed".into(),
            })),
            OneShotError::ProviderFailure(_)
        ));
        assert!(!matches!(
            missing_final_failure(None),
            OneShotError::ProviderFailure(_)
        ));
    }

    #[derive(Clone, Copy)]
    enum CleanupCase {
        ProviderError,
        NoVisibleOutput,
        IdentityError,
    }

    async fn assert_failed_auxiliary_cleanup(case: CleanupCase) {
        let closes = Arc::new(AtomicUsize::new(0));
        let observed_closes = Arc::clone(&closes);
        let sink = AuxiliaryOutputSink {
            observe: Arc::new(move |observation| {
                let closes = Arc::clone(&observed_closes);
                Box::pin(async move {
                    if matches!(observation.event, AuxiliaryOutputEvent::ClosePartial) {
                        closes.fetch_add(1, Ordering::SeqCst);
                        anyhow::bail!("mock close failure");
                    }
                    Ok(())
                })
            }),
            next_flush_deadline: Arc::new(|_, _, _| Box::pin(async { Ok(None) })),
            flush_pending: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        };
        let capture_sink: RenderedRequestCaptureSink = Arc::new(|_| Box::pin(async { Ok(()) }));
        let mut scope = RequestCaptureScope::new(
            RenderedRequestContext {
                request_doc_id: "doc-cleanup".into(),
                request_commit_cid: "bafy-cleanup".into(),
                request_id: "req-cleanup".into(),
                node_did: "did:key:agent".into(),
                requester_did: String::new(),
                agent_id: "behavior".into(),
                session_id: "session".into(),
                model_name: "claude".into(),
                provider_family: None,
            },
            capture_sink,
        );
        scope.set_auxiliary_output_sink(sink);
        scope_request(Arc::new(scope), async {
            let label = arm(
                CaptureScopeKind::Compaction,
                0,
                0,
                AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, Vec::new()),
            )
            .unwrap();
            claim_pending().expect("armed auxiliary attempt");
            let identity = (label.parse().expect("capture scope"), 0, 0);
            emit_auxiliary(identity, AuxiliaryOutputEvent::AttemptStarted)
                .await
                .unwrap();
            let error = match case {
                CleanupCase::ProviderError => {
                    close_received_auxiliary_after_error(missing_final_failure(Some(
                        InferenceError::TransientFailure {
                            reason: "provider failed".into(),
                        },
                    )))
                    .await
                }
                CleanupCase::NoVisibleOutput => {
                    let no_visible =
                        StreamingError::Completion(CompletionError::ProviderError(format!(
                            "{}transport retry budget exhausted after 0 attempt(s)",
                            super::super::NO_VISIBLE_OUTPUT_PREFIX
                        )));
                    close_received_auxiliary_after_error(stream_failure(
                        crate::rig_compat::loop_stream_error(no_visible),
                        None,
                    ))
                    .await
                }
                CleanupCase::IdentityError => {
                    let identity_error =
                        ensure_auxiliary_identity(Some(identity), (identity.0, 0, 1)).unwrap_err();
                    finish_one_shot_result::<String>(Err(identity_error))
                        .await
                        .expect_err("cleanup failure must dominate identity error")
                }
            };
            assert!(matches!(error, OneShotError::Persistence(_)));
            assert!(ErrorChain(&error)
                .to_string()
                .contains("mock close failure"));
            assert_eq!(closes.load(Ordering::SeqCst), 1);
            let error = finish_one_shot_result::<String>(Err(error))
                .await
                .expect_err("persistence failure remains fatal");
            assert!(matches!(error, OneShotError::Persistence(_)));
            assert_eq!(closes.load(Ordering::SeqCst), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn failed_auxiliary_cleanup_dominates_provider_and_identity_errors_without_retry() {
        assert_failed_auxiliary_cleanup(CleanupCase::ProviderError).await;
        assert_failed_auxiliary_cleanup(CleanupCase::NoVisibleOutput).await;
        assert_failed_auxiliary_cleanup(CleanupCase::IdentityError).await;
    }
}

use super::*;
use crate::provider_audit::{
    AuxiliaryOutputEvent, AuxiliaryOutputObservation, AuxiliaryOutputSink,
};
use gents_protocol::rendered_request::CaptureScope;

async fn emit_auxiliary(
    identity: (CaptureScope, usize, u32),
    event: AuxiliaryOutputEvent,
) -> anyhow::Result<()> {
    let observation = AuxiliaryOutputObservation {
        capture_scope: identity.0,
        turn: identity.1,
        attempt: identity.2,
        event,
    };
    match crate::rendered_request::scope::emit_auxiliary_output(observation).await {
        Ok(()) => Ok(()),
        Err(error) => {
            match crate::rendered_request::scope::flush_received_auxiliary_partial().await {
                Ok(_) => Err(error),
                Err(close_error) => Err(error.context(format!(
                    "failed to retain received auxiliary output after sink error: {close_error:#}"
                ))),
            }
        }
    }
}

async fn close_received_auxiliary() -> anyhow::Result<()> {
    anyhow::ensure!(
        crate::rendered_request::scope::flush_received_auxiliary_partial().await?,
        "active auxiliary output could not be closed"
    );
    Ok(())
}

async fn close_received_auxiliary_after_error(error: anyhow::Error) -> anyhow::Error {
    match close_received_auxiliary().await {
        Ok(()) => error,
        Err(close_error) => error.context(format!(
            "failed to retain received auxiliary output during error: {close_error:#}"
        )),
    }
}

async fn flush_auxiliary_at_deadline(
    sink: &AuxiliaryOutputSink,
    identity: (CaptureScope, usize, u32),
) -> anyhow::Result<()> {
    match (sink.flush_pending)(identity.0, identity.1, identity.2).await {
        Ok(()) => Ok(()),
        Err(error) => Err(close_received_auxiliary_after_error(error).await),
    }
}

/// Runs a text completion through the owned stream. A request-scoped auxiliary
/// sink, when installed, owns its canonical output without publishing a
/// session transcript; otherwise this remains a nonpersistent one-shot.
pub async fn run_loop_to_text<M>(
    model: M,
    prompt: Message,
    history: Vec<Message>,
    tools: Arc<Vec<Box<dyn ToolDyn>>>,
    config: LoopConfig,
) -> anyhow::Result<String>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: 'static,
{
    let provider_profile = config.provider_input_counter.profile();
    let stream = run_loop_stream::<M, crate::session_hook::NoopSessionHook>(
        model,
        None,
        TaggedMessage::unassociated(prompt),
        history
            .into_iter()
            .map(TaggedMessage::unassociated)
            .collect(),
        tools,
        config,
    );
    futures::pin_mut!(stream);
    let mut accumulator = AssistantTurnAccumulator::default();
    let auxiliary_sink = crate::rendered_request::scope::auxiliary_output_sink();
    let mut auxiliary_enabled = false;
    let mut active_auxiliary: Option<(CaptureScope, usize, u32)> = None;
    let mut last_auxiliary: Option<(CaptureScope, usize, u32)> = None;
    let mut final_text = None;
    let mut last_attempt_error: Option<InferenceError> = None;

    loop {
        let deadline = match (active_auxiliary, auxiliary_sink.as_ref()) {
            (Some((scope, turn, attempt)), Some(sink)) => {
                match (sink.next_flush_deadline)(scope, turn, attempt).await {
                    Ok(deadline) => deadline,
                    Err(error) => return Err(close_received_auxiliary_after_error(error).await),
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
                        flush_auxiliary_at_deadline(sink, (scope, turn, attempt)).await?;
                        continue;
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
                let error = anyhow::Error::new(error);
                let error = match last_attempt_error.as_ref() {
                    Some(last_error) => error.context(format!(
                        "one-shot loop stream error after retry failure ({last_error})"
                    )),
                    None => error.context("one-shot loop stream error"),
                };
                return Err(if active_auxiliary.is_some() {
                    close_received_auxiliary_after_error(error).await
                } else {
                    error
                });
            }
        };
        match item {
            LoopStreamItem::ProviderAudit(observation) => {
                if !auxiliary_enabled {
                    anyhow::bail!(
                        "received provider audit reached a nonpersistent auxiliary; \
                         a canonical output authority is required"
                    );
                }
                let identity = (
                    observation.capture_scope,
                    observation.turn,
                    observation.attempt,
                );
                anyhow::ensure!(
                    active_auxiliary == Some(identity),
                    "auxiliary audit changed source"
                );
                emit_auxiliary(identity, AuxiliaryOutputEvent::Audit(observation.event)).await?;
            }
            LoopStreamItem::AuthoredInputReady { .. } => {
                // This event carries persistence authority and may only be
                // consumed by the owned StreamProcessor. The auxiliary has no
                // hook, so observing it here means authority leaked past the
                // ownership boundary: fail loudly instead of discarding it.
                anyhow::bail!(
                    "AuthoredInputReady reached the nonpersistent one-shot auxiliary; \
                     authored-input persistence belongs to the owned StreamProcessor consumer"
                );
            }
            LoopStreamItem::ProviderAttemptStarted {
                turn,
                attempt,
                capture_scope,
            } => {
                auxiliary_enabled = OutputSource::ProviderTurn {
                    scope: capture_scope,
                    turn_index: u32::try_from(turn)?,
                    attempt,
                }
                .is_auxiliary_audit();
                if auxiliary_enabled {
                    anyhow::ensure!(
                        auxiliary_sink.is_some(),
                        "auxiliary output has no canonical sink"
                    );
                    let identity = (capture_scope, turn, attempt);
                    anyhow::ensure!(
                        active_auxiliary.is_none(),
                        "auxiliary attempt overlaps another source"
                    );
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
                    let identity = active_auxiliary
                        .ok_or_else(|| anyhow::anyhow!("auxiliary turn has no active source"))?;
                    anyhow::ensure!(
                        (identity.1, identity.2) == (turn, attempt),
                        "auxiliary ready changed source"
                    );
                    emit_auxiliary(identity, AuxiliaryOutputEvent::TurnReady { message }).await?;
                    active_auxiliary = None;
                }
            }
            LoopStreamItem::TurnRetracted { turn, attempt, .. } => {
                if auxiliary_enabled {
                    let identity = active_auxiliary
                        .ok_or_else(|| anyhow::anyhow!("auxiliary retract has no active source"))?;
                    anyhow::ensure!(
                        (identity.1, identity.2) == (turn, attempt),
                        "auxiliary retract changed source"
                    );
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
                        .ok_or_else(|| anyhow::anyhow!("auxiliary reminder has no source"))?;
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
                        anyhow::ensure!(
                            (identity.1, identity.2) == (turn, attempt),
                            "auxiliary failure changed source"
                        );
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
            LoopStreamItem::Item(item) => match item {
                MultiTurnStreamItem::StreamAssistantItem(content) => match content {
                    StreamedAssistantContent::Text(text) => {
                        if auxiliary_enabled {
                            let identity = active_auxiliary.ok_or_else(|| {
                                anyhow::anyhow!("auxiliary text has no active source")
                            })?;
                            emit_auxiliary(identity, AuxiliaryOutputEvent::TextDelta(text.text))
                                .await?;
                        } else {
                            accumulator.push_text(&text.text);
                        }
                    }
                    StreamedAssistantContent::Reasoning(reasoning) => {
                        let reasoning = rig_compat::from_rig_reasoning(&reasoning);
                        if auxiliary_enabled {
                            let identity = active_auxiliary.ok_or_else(|| {
                                anyhow::anyhow!("auxiliary reasoning has no active source")
                            })?;
                            emit_auxiliary(identity, AuxiliaryOutputEvent::Reasoning(reasoning))
                                .await?;
                        } else {
                            accumulator.push_provider_reasoning(provider_profile, reasoning)?;
                        }
                    }
                    StreamedAssistantContent::ReasoningDelta { id, reasoning } => {
                        if auxiliary_enabled {
                            let identity = active_auxiliary.ok_or_else(|| {
                                anyhow::anyhow!("auxiliary reasoning delta has no active source")
                            })?;
                            emit_auxiliary(
                                identity,
                                AuxiliaryOutputEvent::ReasoningDelta {
                                    id,
                                    fragment: reasoning,
                                },
                            )
                            .await?;
                        } else {
                            accumulator.push_provider_reasoning_delta(
                                provider_profile,
                                id,
                                &reasoning,
                            );
                        }
                    }
                    StreamedAssistantContent::ToolCall {
                        tool_call,
                        internal_call_id: _,
                    } => {
                        let tool_call = rig_compat::from_rig_tool_call(&tool_call);
                        if auxiliary_enabled {
                            let identity = active_auxiliary.ok_or_else(|| {
                                anyhow::anyhow!("auxiliary tool call has no active source")
                            })?;
                            emit_auxiliary(identity, AuxiliaryOutputEvent::ToolCall(tool_call))
                                .await?;
                        } else {
                            accumulator.push_tool_call(tool_call);
                        }
                    }
                    _ => {}
                },
                MultiTurnStreamItem::FinalResponse(final_response) => {
                    if auxiliary_enabled {
                        let identity = active_auxiliary.or(last_auxiliary).ok_or_else(|| {
                            anyhow::anyhow!("auxiliary final response has no source")
                        })?;
                        emit_auxiliary(
                            identity,
                            AuxiliaryOutputEvent::FinalText(final_response.response().to_string()),
                        )
                        .await?;
                    } else {
                        accumulator.reconcile_text(final_response.response());
                    }
                    final_text = Some(final_response.response().to_string());
                }
                _ => {}
            },
        }
    }
    if active_auxiliary.is_some() {
        close_received_auxiliary().await?;
    }
    final_text.ok_or_else(|| {
        anyhow::anyhow!("provider stream ended without an explicit terminal response")
    })
}

/// Runs a typed completion without surrendering the runtime's owned-loop
/// chokepoint to Rig's `Agent` orchestration. Rig's schema is attached to every
/// provider request, while the owned loop validates before accepting a final
/// turn and applies its normal bounded recovery policy on malformed output.
pub async fn run_loop_to_typed<M, T>(
    model: M,
    prompt: Message,
    history: Vec<Message>,
    tools: Arc<Vec<Box<dyn ToolDyn>>>,
    mut config: LoopConfig,
) -> anyhow::Result<T>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: 'static,
    T: DeserializeOwned + schemars::JsonSchema + 'static,
{
    config.structured_output = Some(StructuredOutputConfig::for_type::<T>());
    let raw = run_loop_to_text(model, prompt, history, tools, config).await?;
    serde_json::from_str(&raw).map_err(|error| {
        anyhow::anyhow!(
            "decoding validated structured output as {} failed: {error}",
            std::any::type_name::<T>()
        )
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
                agent_did: "did:key:agent".into(),
                requester_did: String::new(),
                behavior_id: "behavior".into(),
                session_id: "session".into(),
                model_name: "claude".into(),
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

            let error = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                flush_auxiliary_at_deadline(&sink, identity),
            )
            .await
            .expect("bounded flush failure cleanup")
            .expect_err("mock flush fails");
            assert!(error.to_string().contains("mock flush failure"));
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
            drop(events);
            assert!(!flush_received_auxiliary_partial().await.unwrap());
        })
        .await;
    }
}

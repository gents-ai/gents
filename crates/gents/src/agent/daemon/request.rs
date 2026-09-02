use anyhow::Result;
use tracing::Instrument;

use super::{BehaviorDaemon, HandleRequestOutcome};
use crate::admission::{self, AdmissionCallContext, CallKind};
use crate::compaction::{self, Compactor};
use crate::prompt::PromptBuilder;
use crate::runtime_trace::RequestTraceAttrs;
use crate::session;

const CANCELLATION_GRACE_PERIOD: std::time::Duration = std::time::Duration::from_millis(100);

impl<M: rig::completion::CompletionModel + 'static> BehaviorDaemon<M> {
    /// Estimate the exact first-turn provider request for session-compaction
    /// admission. The owned loop rebuilds it and remains the sole final
    /// legality/dispatch authority.
    async fn estimate_initial_provider_input(
        &self,
        request: &crate::watcher::AgentRequest,
        preamble: String,
        history: &[crate::llm::message::Message],
        skill_reminders: &[crate::llm::message::Message],
        request_context_message: Option<&crate::llm::message::Message>,
        aggregate_token_budget: Option<crate::agent::loop_stream::AggregateTokenBudget>,
    ) -> Result<usize> {
        let config = crate::completion_factory::loop_config_for_request(
            &self.behavior,
            preamble,
            request,
            aggregate_token_budget,
            self.loop_tools.len(),
        )?;
        let provider_history = skill_reminders
            .iter()
            .cloned()
            .chain(history.iter().cloned())
            .collect::<Vec<_>>();
        let context = request_context_message
            .cloned()
            .into_iter()
            .collect::<Vec<_>>();
        let provider_request = crate::agent::loop_stream::build_request(
            self.model.as_ref(),
            crate::llm::message::Message::user(request.content.clone()),
            &provider_history,
            &context,
            self.loop_tools.as_ref(),
            &config,
        )
        .await
        .map_err(anyhow::Error::new)?;
        Ok(config
            .provider_input_counter
            .estimate_request(&provider_request)?)
    }

    pub(super) async fn handle_request(
        &mut self,
        lifecycle: &mut crate::lifecycle::RequestLifecycle,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
        mut interrupt_rx: tokio::sync::watch::Receiver<Option<crate::interrupt::InterruptIntent>>,
    ) -> Result<HandleRequestOutcome> {
        let request_token = tokio_util::sync::CancellationToken::new();
        let request = lifecycle.request().clone();
        let effective_sampling =
            crate::completion_factory::sampling_for_request(self.behavior.sampling, &request);
        effective_sampling.validate_for_provider(
            self.behavior.backend_provider_kind,
            self.behavior.openai_wire_api,
        )?;
        let effective_seed = effective_sampling.seed;
        let aggregate_token_budget = crate::completion_factory::aggregate_token_budget_for_request(
            self.node.as_ref(),
            &request,
        )
        .await?;
        let trace_attrs = RequestTraceAttrs::from_request(&request);
        let behavior_name = self.behavior.behavior_id.clone();
        let admission_context = AdmissionCallContext::for_request(
            &request,
            lifecycle.behavior_id(),
            lifecycle.backend_id(),
        );
        let title_admission_context = admission_context.clone();
        // One capture scope for the whole request, installed here rather than
        // around `run_inference`.
        //
        // `run_inference` is not the only completion loop a request contains.
        // The pre-request compaction summarizer below runs ~90 lines earlier —
        // it is the *default* path for any session over its threshold
        // (`CompactionStrategy::StripThenSummarize`) — and issues one or two
        // real provider calls whose input is the entire pre-truncated
        // transcript. Scoping only the inference block left those calls with no
        // ambient scope, so their arming sink no-opped, the transport found
        // nothing pending, and the bodies went to the provider with no durable
        // row and no diagnostic. Spanning the whole request body is what makes
        // "no provider call without its fact record" true rather than aspired
        // to.
        //
        // It must stay a single scope instance: the per-kind sequence that
        // keeps the inference loop, the summarizer, and the summarizer's JSON
        // fallback from colliding on `(turn 0, attempt 0)` lives in the scope.
        let request_commit_cid = lifecycle.request_commit_cid().ok_or_else(|| {
            anyhow::anyhow!(
                "claimed AgentRequest {} has no exact DefraDB commit CID",
                request.doc_id
            )
        })?;
        let capture_context = crate::rendered_request::RenderedRequestContext::for_claimed_request(
            &request,
            request_commit_cid,
            self.behavior.model_name.clone(),
        );
        let capture_scope = crate::rendered_request::scope::scope_from_factory(
            capture_context.clone(),
            self.rendered_request_capture_factory.as_ref(),
        );
        let handled = admission::scope_request(admission_context, async {
            self.spawn_conversation_title_generation(
                &request,
                title_admission_context,
                capture_context,
            );

            let selected_skill_ids = selected_skill_ids(request.metadata.as_deref());
            let skill_reminders = self
                .prompt_builder
                .selected_skill_reminders(&selected_skill_ids);
            let overlay = crate::workspace::resolve_request_workspace_overlay(
                self.node.as_ref(),
                &request,
                self.operator_tool_root.as_deref(),
            )
            .await?;
            // Some even when empty: bound requests must not fall through to a live walk.
            let frozen_instruction_manifest =
                crate::workspace::frozen_instruction_manifest_from_overlay(overlay.as_ref())
                    .map(str::to_owned);
            let request_context_message = super::inference::render_request_context_message(
                self.node.as_ref(),
                &self.behavior,
                &request,
                frozen_instruction_manifest.as_deref(),
            )?;
            let workspace = match overlay {
                Some(overlay) => crate::tool_call_lifecycle::runtime::ToolWorkspaceScope {
                    workspace_cwd: Some(overlay.cwd),
                    workspace_root: Some(overlay.root),
                    workspace_authority: Some(overlay.authority),
                },
                None => crate::tool_call_lifecycle::runtime::ToolWorkspaceScope::cwd_only(
                    crate::workspace::request_workspace_cwd(&request),
                ),
            };
            let mut built = async {
                for assembly_attempt in 0..=1 {
                    let background_cutoff =
                        lifecycle.background_completion_input_through_sequence();
                    let compaction_state =
                        session::load_prompt_compaction_state(
                            &self.node,
                            &request.session_id,
                            background_cutoff,
                        )
                            .instrument(tracing::info_span!(
                                "request.load_prompt_compaction_state",
                                request_id = %request.request_id,
                                session_id = %request.session_id,
                                behavior_id = %behavior_name,
                                compaction_entry_count = tracing::field::Empty,
                                compacted_message_count = tracing::field::Empty,
                                summary_count = tracing::field::Empty,
                            ))
                            .await?;
                    let prior_cursor = compaction_state.compacted_through_sequence;
                    let total_compacted_messages = compaction_state.total_messages_compacted;
                    let compaction_generation = compaction_state.generation.clone();
                    let compaction_generation_is_latest = compaction_state.is_latest_generation;
                    let sequenced_history = session::load_sequenced_history_for_request(
                        &self.node,
                        &request,
                        background_cutoff,
                        prior_cursor,
                    )
                    .instrument(tracing::info_span!(
                        "request.load_active_history",
                        request_id = %request.request_id,
                        session_id = %request.session_id,
                        behavior_id = %behavior_name,
                        compacted_through_sequence = prior_cursor.map(i64::from),
                        compacted_provider_messages_skipped = total_compacted_messages as i64,
                        cursor_hit = prior_cursor.is_some(),
                        history_message_count = tracing::field::Empty,
                    ))
                    .await?;
                    let durable_history = sequenced_history
                        .iter()
                        .map(|row| row.message.clone())
                        .collect::<Vec<_>>();
                    // One canonical reduction, shared with the compaction writer:
                    // `messages_compacted` is measured against this list, so the
                    // prefix drop below must index the same one (#993).
                    let (provider_history, file_activity) =
                        compaction::provider_view(durable_history);
                    if !file_activity.is_empty() {
                        tracing::debug!(
                            behavior_id = %self.behavior.behavior_id,
                            session_id = %request.session_id,
                            files_read = ?file_activity.files_read,
                            files_modified = ?file_activity.files_modified,
                            "files referenced in stripped history"
                        );
                    }

                    // Drop in the space the count was measured in.
                    //
                    // Re-narrowing afterwards is provably free for counts this
                    // runtime wrote — their boundary is always `pair_safe_boundary`,
                    // so the drop lands on a turn boundary and the tail is already
                    // provider-valid (`Compaction.sanitize_drop_noop`). It is not
                    // free for counts written *before* the pair-safe splitter
                    // existed: those used an arbitrary budget index and carry no
                    // version marker, so an upgraded session can drop into the
                    // middle of a turn. Without this the orphan would reach
                    // `compact()`, which re-normalizes its input and would then
                    // record its count in a shifted space — reopening the very
                    // accounting defect this change closes, for exactly the sessions
                    // that predate it.
                    // A proven cursor already excluded the raw compacted prefix at
                    // query time. Legacy/null cursors retain the old full-load and
                    // provider-space drop exactly.
                    let mut history = if prior_cursor.is_some() {
                        provider_history
                    } else {
                        compaction::active_provider_history(
                            provider_history,
                            total_compacted_messages,
                        )
                    };
                    let mut summaries = compaction_state
                        .summaries
                        .into_iter()
                        .map(compaction::bounded_summary)
                        .collect::<Vec<_>>();

                    let mut built = self
                        .prompt_builder
                        .build(&history, &summaries)
                        .instrument(tracing::info_span!(
                            "request.build_prompt",
                            request_id = %request.request_id,
                            session_id = %request.session_id,
                            behavior_id = %behavior_name,
                            history_messages = history.len(),
                            summary_count = summaries.len(),
                        ))
                        .await?;
                    let complete_input_tokens = self
                        .estimate_initial_provider_input(
                            &request,
                            built.preamble.clone(),
                            &built.messages,
                            &skill_reminders,
                            request_context_message.as_ref(),
                            aggregate_token_budget.clone(),
                        )
                        .await?;
                    let over_threshold = compaction::input_exceeds_budget(
                        complete_input_tokens,
                        self.behavior.context_window,
                        self.behavior.compaction_threshold,
                    );
                    // Runtime counterpart of Lean `PromptView.safeToReduce`,
                    // resolved at session scope: while any response in this session
                    // is still streaming, a turn is still being written into the
                    // transcript and must not be summarized away. All-terminal at
                    // session scope implies terminal for every row, so this can only
                    // err toward skipping a compaction the next request retries
                    // (`boundary.compaction.safe-to-reduce-session-scope`, #993).
                    let may_reduce = if over_threshold {
                        let live_response =
                            session::session_has_live_response(&self.node, &request.session_id).await?;
                        let gate_open = if live_response {
                            compaction::safe_to_reduce(&history, &compaction::NoneKnown)
                        } else {
                            compaction::safe_to_reduce(&history, &compaction::AllTerminal)
                        };
                        if !gate_open {
                            tracing::info!(
                                request_id = %request.request_id,
                                session_id = %request.session_id,
                                behavior_id = %behavior_name,
                                "compaction skipped: a response in this session is still streaming"
                            );
                        }
                        // `Compaction.providerView_append` — the theorem that lets a
                        // recorded count still name the same rows once the
                        // transcript grows — assumes `UniqueCallIds`. Call ids come
                        // from the provider and nothing enforces that, so it is
                        // checked rather than assumed: a reused id resurrects an
                        // earlier unpaired announcement and shifts the prefix under
                        // the stored count
                        // (`Compaction.reused_call_id_breaks_prefix_stability`).
                        let unique_call_ids = compaction::has_unique_call_ids(&history);
                        if gate_open && !unique_call_ids {
                            tracing::warn!(
                                request_id = %request.request_id,
                                session_id = %request.session_id,
                                behavior_id = %behavior_name,
                                "compaction skipped: a tool-call id is announced by more than one turn, \
                                 so a recorded compacted-prefix count would not stay valid"
                            );
                        }
                        gate_open && unique_call_ids
                    } else {
                        false
                    };
                    if may_reduce {
                        let mut options = self.compaction_options_for_request(
                            lifecycle.claimed_deadline_at(),
                            aggregate_token_budget.clone(),
                            effective_seed,
                        );
                        options.keep_recent_tokens = self.compactor.retention_target(
                            options.keep_recent_tokens,
                            complete_input_tokens,
                            &history,
                            self.behavior.context_window,
                            self.behavior.compaction_threshold,
                        )?;
                        let result = admission::scope_call(
                            CallKind::Compaction,
                            1,
                            self.compactor.compact(
                                history,
                                self.behavior.context_window,
                                &options,
                            ),
                        )
                        .await?;

                        history = result.messages;
                        if let Some(summary) = result.summary {
                            let cumulative_provider_prefix = if prior_cursor.is_some() {
                                result.messages_compacted as usize
                            } else {
                                total_compacted_messages
                                    .checked_add(result.messages_compacted as usize)
                                    .ok_or_else(|| {
                                        anyhow::anyhow!(
                                            "session compaction provider-prefix count overflow: \
                                             prior={}, added={}",
                                            total_compacted_messages,
                                            result.messages_compacted,
                                        )
                                    })?
                            };
                            let candidate_cursor =
                                compaction::compacted_through_sequence(
                                    &sequenced_history
                                        .iter()
                                        .map(|row| (row.sequence, row.message.clone()))
                                        .collect::<Vec<_>>(),
                                    cumulative_provider_prefix,
                                );
                            let compacted_through_sequence = candidate_cursor.filter(|cursor| {
                                let suffix = sequenced_history
                                    .iter()
                                    .filter(|row| row.sequence > *cursor)
                                    .map(|row| row.message.clone())
                                    .collect::<Vec<_>>();
                                compaction::provider_view(suffix).0 == history
                            });
                            if compacted_through_sequence.is_none() {
                                tracing::warn!(
                                    request_id = %request.request_id,
                                    session_id = %request.session_id,
                                    provider_prefix = cumulative_provider_prefix,
                                    "compaction succeeded without a provable canonical cursor; future requests will use the legacy full-history projection"
                                );
                            }
                            if compaction_generation_is_latest {
                                match session::save_compaction_entry_with_requester_did(
                                    &self.node,
                                    &request.session_id,
                                    &request.agent_did,
                                    request.requester_did.as_deref(),
                                    &request.request_id,
                                    &request.doc_id,
                                    &summary,
                                    &result.files_read,
                                    &result.files_modified,
                                    result.messages_compacted,
                                    compacted_through_sequence,
                                    result.original_token_estimate,
                                    result.compacted_token_estimate,
                                    &compaction_generation,
                                )
                                .await
                                {
                                    Ok(entry) => {
                                        summaries.push(compaction::bounded_summary(entry.summary));
                                    }
                                    Err(error)
                                        if is_stale_compaction_generation(&error)
                                            && assembly_attempt == 0 =>
                                    {
                                        tracing::info!(
                                            request_id = %request.request_id,
                                            session_id = %request.session_id,
                                            "compaction generation advanced during prompt assembly; rebuilding from the winner"
                                        );
                                        continue;
                                    }
                                    Err(error) if is_stale_compaction_generation(&error) => {
                                        tracing::warn!(
                                            request_id = %request.request_id,
                                            session_id = %request.session_id,
                                            "compaction generation advanced twice; using the verified request-local compaction without appending it"
                                        );
                                        summaries.push(compaction::bounded_summary(summary));
                                    }
                                    Err(error) => return Err(error),
                                }
                            } else {
                                // A background request may be bound to an older
                                // compatible transcript/compaction generation. Its
                                // reduction is valid for this request's provider
                                // input but must not mutate the live session chain.
                                tracing::info!(
                                    request_id = %request.request_id,
                                    session_id = %request.session_id,
                                    "using request-local compaction for an older background snapshot"
                                );
                                summaries.push(compaction::bounded_summary(summary));
                            }
                        }

                        built = self
                            .prompt_builder
                            .build(&history, &summaries)
                            .instrument(tracing::info_span!(
                                "request.build_prompt",
                                request_id = %request.request_id,
                                session_id = %request.session_id,
                                behavior_id = %behavior_name,
                                history_messages = history.len(),
                                summary_count = summaries.len(),
                                compacted = true,
                            ))
                            .await?;
                    }

                    return Ok::<_, anyhow::Error>(built);
                }
                unreachable!("bounded prompt assembly retry returns")
            }
            .instrument(tracing::info_span!(
                "request.prepare_prompt",
                request_id = %request.request_id,
                session_id = %request.session_id,
                agent_did = %request.agent_did,
                behavior_id = %behavior_name,
                deadline_at = %trace_attrs.deadline_at,
                has_deadline = trace_attrs.has_deadline,
                subagent_depth = trace_attrs.subagent_depth,
                is_subagent = trace_attrs.is_subagent,
                selected_skill_count = trace_attrs.selected_skill_count,
                workspace_cwd_set = trace_attrs.workspace_cwd_set,
            ))
            .await?;

            if !skill_reminders.is_empty() {
                let mut reminders = skill_reminders;
                reminders.append(&mut built.messages);
                built.messages = reminders;
            }

            lifecycle.begin_execution().await?;

            let response_behavior_id = lifecycle.behavior_id().to_string();
            let doc_id = self
                .stream_writer
                .begin_with_requester_did(
                    &request.session_id,
                    &request.request_id,
                    Some(&request.doc_id),
                    lifecycle.behavior_id(),
                    request.requester_did.as_deref(),
                )
                .instrument(tracing::info_span!(
                    "request.begin_response",
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    agent_did = %request.agent_did,
                    behavior_id = %response_behavior_id,
                    subagent_depth = trace_attrs.subagent_depth,
                    is_subagent = trace_attrs.is_subagent,
                ))
                .await?;
            lifecycle.set_response_doc_id(&doc_id);
            lifecycle.advance().await?;

            let inference_behavior_id = lifecycle.behavior_id().to_string();
            let inference_backend_id = lifecycle.backend_id().to_string();
            let result = self
                .run_inference(
                    &request,
                    &doc_id,
                    &built.messages,
                    lifecycle,
                    &mut shutdown,
                    &mut interrupt_rx,
                    &request_token,
                    aggregate_token_budget,
                    effective_seed,
                    workspace,
                    request_context_message,
                )
                .instrument(tracing::info_span!(
                    "request.run_inference",
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    agent_did = %request.agent_did,
                    behavior_id = %inference_behavior_id,
                    backend_id = %inference_backend_id,
                    deadline_at = %trace_attrs.deadline_at,
                    has_deadline = trace_attrs.has_deadline,
                    subagent_depth = trace_attrs.subagent_depth,
                    is_subagent = trace_attrs.is_subagent,
                    selected_skill_count = trace_attrs.selected_skill_count,
                    workspace_cwd_set = trace_attrs.workspace_cwd_set,
                ))
                .await;

            let token_was_cancelled = request_token.is_cancelled();
            let watched_interrupt = { interrupt_rx.borrow().clone() };
            let interrupt_at = if token_was_cancelled {
                if let Some(intent) = watched_interrupt {
                    Some(intent.at.to_rfc3339())
                } else {
                    crate::interrupt::fetch_interrupt_requested_at(&self.node, &request.request_id)
                        .await?
                }
            } else if let Some(intent) = watched_interrupt {
                request_token.cancel();
                Some(intent.at.to_rfc3339())
            } else {
                let persisted =
                    crate::interrupt::fetch_interrupt_requested_at(&self.node, &request.request_id)
                        .await?;
                if persisted.is_some() {
                    request_token.cancel();
                }
                persisted
            };

            if token_was_cancelled && interrupt_at.is_none() {
                tracing::warn!(
                    request_id = %lifecycle.request().request_id,
                    "request_token was cancelled without an interrupt latch; \
                     treating as failure rather than interrupt"
                );
                return Ok(HandleRequestOutcome::FailedAfterResponse(anyhow::anyhow!(
                    "request_token cancelled without interrupt latch"
                )));
            }

            if let Some(interrupt_at) = interrupt_at {
                if !request_token.is_cancelled() {
                    request_token.cancel();
                }
                if interrupt_at.trim().is_empty() {
                    tracing::warn!(
                        request_id = %lifecycle.request().request_id,
                        "request_token was cancelled without an interrupt latch; \
                         treating as failure rather than interrupt"
                    );
                    return Ok(HandleRequestOutcome::FailedAfterResponse(anyhow::anyhow!(
                        "request_token cancelled without interrupt latch"
                    )));
                }

                let flow_span = tracing::info_span!(
                    "interrupt.flow",
                    request_id = %lifecycle.request().request_id,
                    interrupt_at = %interrupt_at,
                );
                async {
                    tokio::time::sleep(CANCELLATION_GRACE_PERIOD).await;
                    if let Err(error) = self
                        .stream_writer
                        .write_interrupted_at(&doc_id, &interrupt_at)
                        .await
                    {
                        tracing::warn!(
                            behavior_id = %self.behavior.behavior_id,
                            doc_id = %doc_id,
                            error = %error,
                            "failed to stamp interrupted_at on response; continuing to terminal transition"
                        );
                    }
                    if let Err(error) = self.stream_writer.finalize_interrupted_response(&doc_id).await
                    {
                        tracing::warn!(
                            behavior_id = %self.behavior.behavior_id,
                            doc_id = %doc_id,
                            error = %error,
                            "failed to finalize interrupted response; continuing to terminal request transition"
                        );
                    }
                    lifecycle.transition_to_interrupted().await?;
                    if let Err(error) = crate::lifecycle::queue::drain_automated_wakeups(
                        &self.node,
                        &request.session_id,
                        &request.agent_did,
                        "automated wake-up drained because active request was interrupted",
                    )
                    .await
                    {
                        tracing::warn!(
                            request_id = %request.request_id,
                            session_id = %request.session_id,
                            error = %error,
                            "failed to drain automated wake-ups after request interrupt"
                        );
                    }
                    Ok::<_, anyhow::Error>(())
                }
                .instrument(flow_span)
                .await?;
                return Ok(HandleRequestOutcome::Interrupted);
            }

            match result {
                Ok(outcome) => Ok(outcome),
                Err(error) => {
                    if let Err(finalize_error) = self
                        .stream_writer
                        .finalize_error(&doc_id, &error.to_string())
                        .await
                    {
                        tracing::error!(
                            behavior_id = %self.behavior.behavior_id,
                            doc_id = %doc_id,
                            error = %finalize_error,
                            "failed to finalize stream after error"
                        );
                    }
                    Err(error)
                }
            }
        });

        match capture_scope {
            Some(scope) => crate::rendered_request::scope::scope_request(scope, handled).await,
            None => handled.await,
        }
    }
}

fn selected_skill_ids(metadata: Option<&str>) -> Vec<String> {
    let Some(metadata) = metadata else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(metadata) else {
        return Vec::new();
    };
    value
        .get("selected_skill_ids")
        .and_then(|ids| ids.as_array())
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn is_stale_compaction_generation(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains("stale compaction generation")
}

#[cfg(test)]
mod budget_contract_tests {
    use crate::lean_vocab_test::lean_prompt_assembly_budget_cases;

    /// Drives the production compaction trigger and per-turn output clamp from
    /// Lean-generated boundaries.
    #[test]
    fn generated_budget_cases_drive_dynamic_output_compaction_trigger() {
        let cases = lean_prompt_assembly_budget_cases();
        assert!(
            !cases.is_empty(),
            "Lean emitted no PromptAssembly budget cases"
        );

        for case in cases {
            // Round-trip through the float the configuration surface actually
            // carries, so the basis-point conversion is exercised rather than
            // bypassed.
            let threshold = case.threshold_basis_points as f64 / 10_000.0;
            // Drive the production helper, not a formula duplicated here.
            let configured = crate::compaction::threshold_budget(case.context_window, threshold);
            let effective =
                crate::compaction::effective_input_budget(case.context_window, threshold);
            let input_tokens = case.prompt_tokens.saturating_add(case.request_tokens);
            let effective_output = crate::compaction::effective_output_budget(
                input_tokens,
                case.context_window,
                case.max_output_tokens,
            );

            assert_eq!(
                configured, case.configured_threshold_budget,
                "{}: configured threshold budget drifted from Lean",
                case.name
            );
            assert_eq!(
                effective, case.effective_input_budget,
                "{}: effective input budget drifted from Lean",
                case.name
            );
            assert_eq!(
                effective_output, case.effective_output_tokens,
                "{}: effective output budget drifted from Lean",
                case.name
            );
            assert_eq!(
                input_tokens.saturating_add(effective_output) <= case.context_window,
                case.provider_safe,
                "{}: provider-safety witness drifted from Lean",
                case.name
            );
            assert_eq!(
                crate::compaction::can_dispatch(
                    input_tokens,
                    case.context_window,
                    case.max_output_tokens,
                ),
                case.can_dispatch,
                "{}: provider dispatch legality drifted from Lean",
                case.name
            );
            assert_eq!(
                crate::compaction::input_exceeds_budget(
                    input_tokens,
                    case.context_window,
                    threshold,
                ),
                case.should_compact,
                "{}: production compaction trigger drifted from Lean",
                case.name
            );
        }
    }
}

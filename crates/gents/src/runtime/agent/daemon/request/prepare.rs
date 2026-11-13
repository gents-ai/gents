use anyhow::{Context, Result};
use tracing::Instrument;

use super::super::AgentDaemon;
use crate::runtime::agent::loop_stream::{AggregateTokenBudget, FoldedPrompt};
use crate::turn::prompt::{BuiltPrompt, PromptBuilder};
use crate::turn::provider_input::budget::ContextCalibration;
use crate::turn::provider_input::ProviderInputProfile;
use gents_inference::admission::{self, CallKind};
use gents_inference::compaction;
use gents_inference::llm::message::Message;
use gents_loop::loop_stream::{
    provider_messages, provider_view_tagged, LoopReplayInput, TaggedMessage,
};
use gents_observe::observe::runtime_trace::RequestTraceAttrs;
use gents_protocol::row::AgentRequest;
use gents_session::session;
use gents_tools::runtime_context::ToolWorkspaceScope;

#[derive(Clone, Copy)]
pub(super) struct RequestInputs<'a> {
    pub(super) request: &'a AgentRequest,
    pub(super) lifecycle: &'a gents_session::request::lifecycle::RequestLifecycle,
    pub(super) request_commit_cid: &'a String,
    pub(super) agent_name: &'a str,
    pub(super) trace_attrs: &'a RequestTraceAttrs,
    pub(super) aggregate_token_budget: &'a Option<AggregateTokenBudget>,
    pub(super) context_calibration: &'a ContextCalibration,
    pub(super) effective_seed: Option<i64>,
    pub(super) resume_from_history: bool,
    pub(super) resume_same_request: bool,
}

pub(super) struct PreparedRequestInput {
    pub(super) workspace: ToolWorkspaceScope,
    pub(super) request_context_message: Option<Message>,
    pub(super) built: BuiltPrompt,
    pub(super) replay: LoopReplayInput,
    pub(super) consumed_prompts: Vec<FoldedPrompt>,
    pub(super) folded_prompts: Vec<FoldedPrompt>,
    pub(super) wake_notifications: Vec<TaggedMessage>,
}

#[derive(Clone, Copy)]
struct PromptAssembly<'a> {
    skill_reminders: &'a [Message],
    request_context_message: &'a Option<Message>,
    folded_prompts: &'a [FoldedPrompt],
    wake_notifications: &'a [TaggedMessage],
}

#[derive(Clone, Copy)]
struct SessionReduction<'a> {
    replay: &'a LoopReplayInput,
    provider_profile: ProviderInputProfile,
    sequenced_history: &'a [session::SequencedMessage],
    compaction_access: &'a gents_store::config_client::ConfigAccess,
    compaction_generation: &'a str,
    compaction_generation_is_latest: bool,
    assembly_attempt: i32,
}

impl<M> AgentDaemon<M>
where
    M: gents_inference::llm::rig_compat::ProviderModel,
{
    pub(super) async fn load_admission(
        &self,
        request: &AgentRequest,
        resumed_from_backend: bool,
    ) -> Result<(bool, bool, Option<AggregateTokenBudget>, ContextCalibration)> {
        if let Some(guard) = &self.root_execution_guard {
            guard
                .validate(&self.node, self.agent_config.agent_identity.as_ref())
                .await
                .context("validating current WorkspaceRoot policy for fresh request")?;
        }
        let resume_same_request = resumed_from_backend
            && session::resumed_request_has_published_input(&self.node, request).await?;
        let resume_from_history =
            resume_same_request || session::retry_has_published_input(&self.node, request).await?;
        let access = gents_store::config_client::ConfigAccess::local_managed_agent(
            self.node.clone(),
            identity::Did::new(request.agent_did.clone())?,
            self.agent_config.agent_identity.as_ref(),
        )?;
        let aggregate_token_budget =
            crate::turn::completion_factory::aggregate_token_budget_for_request(&access, request)
                .await?;
        let context_calibration = crate::turn::completion_factory::context_calibration_for_session(
            &access,
            &self.agent_config,
            &request.node_did,
            &request.session_id,
        )
        .await;
        Ok((
            resume_from_history,
            resume_same_request,
            aggregate_token_budget,
            context_calibration,
        ))
    }

    pub(super) async fn request_capture_scope(
        &self,
        request: &AgentRequest,
        lifecycle: &gents_session::request::lifecycle::RequestLifecycle,
        stream_writer: &gents_session::request::streaming::DefraStreamWriter,
        request_commit_cid: &str,
    ) -> Result<Option<std::sync::Arc<gents_inference::rendered_request::scope::RequestCaptureScope>>>
    {
        let capture_context = gents_inference::rendered_request::context_for_claimed_request(
            request,
            request_commit_cid,
            self.agent_config.model_name.clone(),
            self.provider_family.clone(),
        );
        let mut capture_scope = gents_inference::rendered_request::scope_from_factory(
            capture_context.clone(),
            self.rendered_request_capture_factory.as_ref(),
        );
        let previous_scopes = if lifecycle.resumed_from_backend() && capture_scope.is_some() {
            gents_inference::rendered_request::load_scope_allocations(&lifecycle.access()?, request)
                .await?
        } else {
            Vec::new()
        };
        if let Some(scope) = capture_scope.as_mut() {
            let scope = std::sync::Arc::get_mut(scope)
                .context("fresh request capture scope unexpectedly shared")?;
            scope
                .restore_allocations(previous_scopes)
                .map_err(anyhow::Error::msg)?;
            if let Some(family) = &self.compaction_provider_family {
                scope.set_compaction_provider_family(family.clone());
            }
            scope.set_auxiliary_output_sink(stream_writer.auxiliary_output_sink(
                request.clone(),
                lifecycle.execution_generation()?.to_owned(),
                crate::turn::provider_input::ProviderInputProfile::resolve(
                    self.agent_config.backend_provider_kind,
                    self.agent_config.openai_wire_api,
                ),
            ));
        }
        Ok(capture_scope)
    }

    pub(super) async fn prepare_request_input(
        &self,
        inputs: RequestInputs<'_>,
    ) -> Result<PreparedRequestInput> {
        let RequestInputs {
            request,
            lifecycle,
            agent_name,
            trace_attrs,
            resume_from_history,
            ..
        } = inputs;
        self.spawn_conversation_title_generation(request);
        let selected_skill_ids = &request.input.selected_skill_ids;
        let skill_reminders = self
            .prompt_builder
            .selected_skill_reminders(selected_skill_ids);
        let overlay = gents_session::workspace::resolve_request_workspace_overlay_with_identity(
            &self.node,
            self.agent_config.agent_identity.as_ref(),
            request,
            lifecycle.execution_generation()?,
            self.tool_config.static_policy().bash.execution_mode
                == gents_tools::native::CommandExecutionMode::ArtifactWrite,
            self.operator_tool_root.as_deref(),
        )
        .await?;
        let frozen_instruction_manifest =
            gents_session::workspace::frozen_instruction_manifest_from_overlay(overlay.as_ref())
                .map(str::to_owned);
        let request_context_message = super::super::inference::render_request_context_message(
            self.node.as_ref(),
            &self.tool_config,
            request,
            frozen_instruction_manifest.as_deref(),
        )?;
        let prompt = |folded: gents_session::request::lifecycle::queue::FoldedInput| -> Result<_> {
            Ok(crate::runtime::agent::loop_stream::FoldedPrompt {
                key: folded.key(),
                message: folded.message()?,
            })
        };
        let consumed_prompts =
            gents_session::request::lifecycle::queue::load_consumed_folded_inputs(
                &lifecycle.access()?,
                request,
            )
            .await?
            .into_iter()
            .map(prompt)
            .collect::<Result<Vec<_>>>()?;
        let selection = if resume_from_history {
            &[][..]
        } else {
            lifecycle.folded_selection()
        };
        let mut folded_prompts = consumed_prompts.clone();
        folded_prompts.extend(
            selection
                .iter()
                .cloned()
                .map(prompt)
                .collect::<Result<Vec<_>>>()?,
        );
        let workspace = match overlay {
            Some(overlay) => gents_tools::runtime_context::ToolWorkspaceScope {
                runtime_home: None,
                workspace_cwd: Some(overlay.cwd),
                workspace_root: Some(overlay.root),
                workspace_authority: Some(overlay.authority),
                workspace_artifact: overlay.workspace_artifact,
            },
            None => gents_tools::runtime_context::ToolWorkspaceScope::cwd_only(
                gents_session::workspace::request_workspace_cwd(request),
            ),
        };
        let wake_notifications = if resume_from_history {
            Vec::new()
        } else {
            let access = gents_store::config_client::ConfigAccess::local_managed_agent(
                self.node.clone(),
                identity::Did::new(&request.agent_did)?,
                self.agent_config.agent_identity.as_ref(),
            )?;
            super::super::wake_input::load(
                &access,
                request,
                lifecycle.background_completion_input_through_sequence(),
            )
            .await?
        };
        let (mut built, replay) = self
            .assemble_prompt(
                inputs,
                PromptAssembly {
                    skill_reminders: &skill_reminders,
                    request_context_message: &request_context_message,
                    folded_prompts: &folded_prompts,
                    wake_notifications: &wake_notifications,
                },
            )
            .instrument(tracing::info_span!(
                "request.prepare_prompt",
                request_id = %request.request_id,
                session_id = %request.session_id,
                node_did = %request.node_did,
                agent_did = %agent_name,
                deadline_at = %trace_attrs.deadline_at,
                has_deadline = trace_attrs.has_deadline,
                request_hop = trace_attrs.request_hop,
                selected_skill_count = trace_attrs.selected_skill_count,
                workspace_cwd_set = trace_attrs.workspace_cwd_set,
            ))
            .await?;

        if !skill_reminders.is_empty() {
            let mut reminders = skill_reminders
                .into_iter()
                .map(TaggedMessage::unassociated)
                .collect::<Vec<_>>();
            reminders.append(&mut built.messages);
            built.messages = reminders;
        }

        Ok(PreparedRequestInput {
            workspace,
            request_context_message,
            built,
            replay,
            consumed_prompts,
            folded_prompts,
            wake_notifications,
        })
    }

    async fn assemble_prompt(
        &self,
        inputs: RequestInputs<'_>,
        assembly: PromptAssembly<'_>,
    ) -> Result<(BuiltPrompt, LoopReplayInput)> {
        let RequestInputs {
            request,
            lifecycle,
            request_commit_cid,
            agent_name,
            aggregate_token_budget,
            context_calibration,
            resume_from_history,
            resume_same_request,
            ..
        } = inputs;
        let PromptAssembly {
            skill_reminders,
            request_context_message,
            folded_prompts,
            wake_notifications,
        } = assembly;
        for assembly_attempt in 0..=1 {
            let background_cutoff = lifecycle.background_completion_input_through_sequence();
            let compaction_access = gents_store::config_client::ConfigAccess::local_managed_agent(
                self.node.clone(),
                identity::Did::new(&request.agent_did)?,
                self.agent_config.agent_identity.as_ref(),
            )?;
            let compaction_state = session::load_prompt_compaction_state_with_access(
                &compaction_access,
                &request.session_id,
                &request.node_did,
                request.requester_did.as_deref(),
                background_cutoff,
            )
            .instrument(tracing::info_span!(
                "request.load_prompt_compaction_state",
                request_id = %request.request_id,
                session_id = %request.session_id,
                agent_did = %agent_name,
                compaction_entry_count = tracing::field::Empty,
                compacted_message_count = tracing::field::Empty,
                summary_count = tracing::field::Empty,
            ))
            .await?;
            let prior_cursor = compaction_state.compacted_through_sequence;
            let total_compacted_messages = compaction_state.total_messages_compacted;
            let compaction_generation = compaction_state.generation.clone();
            let compaction_generation_is_latest = compaction_state.is_latest_generation;
            let provider_profile = crate::turn::provider_input::ProviderInputProfile::resolve(
                self.agent_config.backend_provider_kind,
                self.agent_config.openai_wire_api,
            );
            let sequenced_history = session::load_sequenced_history_for_request(
                &self.node,
                request,
                background_cutoff,
                prior_cursor,
                provider_profile,
                resume_same_request,
            )
            .instrument(tracing::info_span!(
                "request.load_active_history",
                request_id = %request.request_id,
                session_id = %request.session_id,
                agent_did = %agent_name,
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
            let canonical_prefix_is_stable = compaction::session_prefix_is_stable(&durable_history);
            let file_activity = compaction::history::extract_file_activity(&durable_history);
            if !file_activity.is_empty() {
                tracing::debug!(
                    agent_did = %self.agent_config.agent_did,
                    session_id = %request.session_id,
                    files_read = ?file_activity.files_read,
                    files_modified = ?file_activity.files_modified,
                    "files referenced in stripped history"
                );
            }
            let replay = crate::turn::provider_input::replay::owned_replay_input(
                self.node.clone(),
                request.clone(),
                request_commit_cid.clone(),
                gents_protocol::rendered_request::CaptureScopeKind::Inference,
                self.replay_issuer.clone(),
                provider_profile,
            );
            let tagged =
                crate::turn::provider_input::replay::tag_canonical_history(&sequenced_history);
            let provider_history = provider_view_tagged(tagged)?;

            let history = provider_history;
            let summaries = compaction_state
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
                    agent_did = %agent_name,
                    history_messages = history.len(),
                    summary_count = summaries.len(),
                ))
                .await?;
            let complete_input_tokens = self
                .estimate_initial_provider_input(
                    request,
                    built.preamble.clone(),
                    &built.messages,
                    skill_reminders,
                    request_context_message.as_ref(),
                    folded_prompts,
                    aggregate_token_budget.clone(),
                    &replay,
                    resume_from_history,
                    wake_notifications,
                )
                .await?;
            let provider_budget = crate::turn::provider_input::budget::compaction_input_budget(
                self.agent_config.context_window,
                self.agent_config.compaction_threshold(),
                crate::turn::provider_input::budget::configured_output_ceiling(
                    crate::turn::completion_factory::effective_max_tokens(
                        self.agent_config.max_output_tokens,
                        self.agent_config.sampling.max_tokens,
                    ),
                ),
            );
            let context = context_calibration.context(complete_input_tokens);
            let reduction_admission =
                compaction::ReductionAdmission::for_context(context, provider_budget);
            let over_threshold = reduction_admission.is_some();
            let may_reduce = if over_threshold {
                let gate_open = canonical_prefix_is_stable;
                if !gate_open {
                    tracing::info!(
                        request_id = %request.request_id,
                        session_id = %request.session_id,
                        agent_did = %agent_name,
                        "compaction skipped: canonical provider prefix is not a stable turn boundary"
                    );
                }
                gate_open
            } else {
                false
            };
            if may_reduce {
                let admission =
                    reduction_admission.expect("may_reduce is true only for an admitted reduction");
                let Some(rebuilt) = self
                    .reduce_and_rebuild(
                        inputs,
                        SessionReduction {
                            replay: &replay,
                            provider_profile,
                            sequenced_history: &sequenced_history,
                            compaction_access: &compaction_access,
                            compaction_generation: &compaction_generation,
                            compaction_generation_is_latest,
                            assembly_attempt,
                        },
                        admission,
                        history,
                        summaries,
                    )
                    .await?
                else {
                    continue;
                };
                built = rebuilt;
            }

            return Ok::<_, anyhow::Error>((built, replay));
        }
        unreachable!("bounded prompt assembly retry returns")
    }

    async fn reduce_and_rebuild(
        &self,
        inputs: RequestInputs<'_>,
        reduction: SessionReduction<'_>,
        admission: compaction::ReductionAdmission,
        mut history: Vec<TaggedMessage>,
        mut summaries: Vec<String>,
    ) -> Result<Option<BuiltPrompt>> {
        let RequestInputs {
            request,
            lifecycle,
            agent_name,
            aggregate_token_budget,
            context_calibration,
            effective_seed,
            ..
        } = inputs;
        let SessionReduction {
            replay,
            provider_profile,
            sequenced_history,
            compaction_access,
            compaction_generation,
            compaction_generation_is_latest,
            assembly_attempt,
        } = reduction;
        let prior_provider_prefix = 0;
        let mut options = self.compaction_options_for_request(
            lifecycle.claimed_deadline_at(),
            aggregate_token_budget.clone(),
            effective_seed,
            context_calibration.clone(),
            self.compaction_provider_params(request).await?,
        );
        let projected = provider_view_tagged(history)?;
        options.max_compacted_prefix_messages =
            crate::runtime::agent::loop_stream::replay_compaction_prefix_bound(
                &projected,
                &replay.required,
            )?;
        let projected_native = provider_messages(provider_profile, &projected);
        options.keep_recent_tokens = self.compactor.retention_target(
            options.keep_recent_tokens,
            &projected_native,
            admission,
            &options,
            self.agent_config.context_window,
        )?;
        let result = admission::scope_call(
            CallKind::Compaction,
            1,
            self.compactor.reduce(
                projected_native.clone(),
                self.agent_config.context_window,
                &options,
                admission,
            ),
        )
        .await?;

        // The seeded anchor described the unreduced history.
        context_calibration.invalidate_anchor();
        history = if let Some(exact) = result.exact_reduction() {
            anyhow::ensure!(
                projected_native
                    == exact
                        .compacted_prefix
                        .iter()
                        .chain(exact.retained_suffix)
                        .cloned()
                        .collect::<Vec<_>>(),
                "session reduction changed its exact provider source"
            );
            let (prefix, suffix) = projected.split_at(exact.compacted_prefix.len());
            let associations =
                gents_session::session::provider_context_reduction::ReplayAssociations::from_tagged_split(
                    replay.required.clone(),
                    prefix,
                    suffix,
                );
            let native = crate::runtime::agent::loop_stream::association_reduction(
                provider_profile,
                &projected,
                exact,
            )?;
            gents_session::session::provider_context_reduction::validate_replay_associations(
                &associations,
                native.exact().compacted_prefix,
                native.exact().retained_suffix,
                &request.doc_id,
            )?;
            suffix.to_vec()
        } else {
            anyhow::ensure!(
                projected_native == result.provider_messages()?,
                "provider repair changed its sourced projection"
            );
            projected
        };
        if let Some(exact) = result.exact_reduction() {
            let summary = exact.checkpoint;
            let sequence_rows = sequenced_history
                .iter()
                .map(|row| (row.sequence, row.message.clone()))
                .collect::<Vec<_>>();
            let compacted_through_sequence = compaction::session_cursor_for_reduction(
                provider_profile,
                &sequence_rows,
                prior_provider_prefix,
                exact,
            )?;
            if let (true, Some(compacted_through_sequence)) =
                (compaction_generation_is_latest, compacted_through_sequence)
            {
                match session::save_exact_compaction_entry(
                    compaction_access,
                    session::NewExactSessionCompaction {
                        agent_did: &request.agent_did,
                        session_id: &request.session_id,
                        node_did: &request.node_did,
                        requester_did: request.requester_did.as_deref(),
                        request_id: &request.request_id,
                        request_doc_id: &request.doc_id,
                        files_read: &result.files_read,
                        files_modified: &result.files_modified,
                        compacted_through_sequence,
                        original_tokens: result.original_token_estimate,
                        compacted_tokens: result.compacted_token_estimate,
                        expected_generation: compaction_generation,
                    },
                    exact,
                )
                .await
                {
                    Ok(entry) => {
                        summaries.push(compaction::bounded_summary(entry.summary));
                    }
                    Err(error)
                        if gents_session::session::StaleCompactionGeneration::occurs_in(
                            error.as_ref(),
                        ) && assembly_attempt == 0 =>
                    {
                        tracing::info!(
                            request_id = %request.request_id,
                            session_id = %request.session_id,
                            "compaction generation advanced during prompt assembly; rebuilding from the winner"
                        );
                        return Ok(None);
                    }
                    Err(error)
                        if gents_session::session::StaleCompactionGeneration::occurs_in(
                            error.as_ref(),
                        ) =>
                    {
                        tracing::warn!(
                            request_id = %request.request_id,
                            session_id = %request.session_id,
                            "compaction generation advanced twice; using the verified request-local compaction without appending it"
                        );
                        summaries.push(compaction::bounded_summary(summary.to_string()));
                    }
                    Err(error) => return Err(error),
                }
            } else if compacted_through_sequence.is_none() {
                tracing::warn!(
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    provider_prefix = prior_provider_prefix
                        .saturating_add(exact.compacted_prefix.len()),
                    "using non-durable request-local compaction because no canonical session cursor could be proven"
                );
                summaries.push(compaction::bounded_summary(summary.to_string()));
            } else {
                tracing::info!(
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    "using request-local compaction for an older background snapshot"
                );
                summaries.push(compaction::bounded_summary(summary.to_string()));
            }
        }

        let built = self
            .prompt_builder
            .build(&history, &summaries)
            .instrument(tracing::info_span!(
                "request.build_prompt",
                request_id = %request.request_id,
                session_id = %request.session_id,
                agent_did = %agent_name,
                history_messages = history.len(),
                summary_count = summaries.len(),
                compacted = true,
            ))
            .await?;
        Ok(Some(built))
    }
}
